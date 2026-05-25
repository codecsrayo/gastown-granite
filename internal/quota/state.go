// Package quota manages Claude Code account quota rotation for Gas Town.
//
// When sessions hit rate limits, the overseer can scan for blocked sessions
// and rotate them to available accounts. State is persisted to mayor/quota.json
// with crash-safe atomic writes and file-level locking.
package quota

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/gofrs/flock"
	"github.com/steveyegge/gastown/internal/atomicfile"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/util"
)

// Manager handles quota state persistence with file locking.
type Manager struct {
	townRoot string
}

// NewManager creates a new quota manager for the given town root.
func NewManager(townRoot string) *Manager {
	return &Manager{townRoot: townRoot}
}

// statePath returns the path to quota.json.
func (m *Manager) statePath() string {
	return constants.MayorQuotaPath(m.townRoot)
}

// lockPath returns the path to the flock file for quota state.
func (m *Manager) lockPath() string {
	return filepath.Join(m.townRoot, constants.DirMayor, constants.DirRuntime, "quota.lock")
}

// lock acquires an exclusive file lock for quota state operations.
// Caller must defer unlock().
func (m *Manager) lock() (func(), error) {
	lockDir := filepath.Dir(m.lockPath())
	if err := os.MkdirAll(lockDir, 0755); err != nil {
		return nil, fmt.Errorf("creating quota lock dir: %w", err)
	}
	fl := flock.New(m.lockPath())
	if err := fl.Lock(); err != nil {
		return nil, fmt.Errorf("acquiring quota lock: %w", err)
	}
	return func() { _ = fl.Unlock() }, nil
}

// Load reads the quota state from disk. Returns an empty state if the file
// doesn't exist yet (first run).
func (m *Manager) Load() (*config.QuotaState, error) {
	data, err := os.ReadFile(m.statePath())
	if os.IsNotExist(err) {
		return &config.QuotaState{
			Version:  config.CurrentQuotaVersion,
			Accounts: make(map[string]config.AccountQuotaState),
		}, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading quota state: %w", err)
	}

	var state config.QuotaState
	if err := json.Unmarshal(data, &state); err != nil {
		return nil, fmt.Errorf("parsing quota state: %w", err)
	}
	if state.Accounts == nil {
		state.Accounts = make(map[string]config.AccountQuotaState)
	}
	return &state, nil
}

// Save writes the quota state to disk atomically with file locking.
func (m *Manager) Save(state *config.QuotaState) error {
	unlock, err := m.lock()
	if err != nil {
		return err
	}
	defer unlock()

	state.Version = config.CurrentQuotaVersion
	return atomicfile.EnsureDirAndWriteJSON(m.statePath(), state)
}

// WithLock acquires the quota file lock, runs fn, then releases the lock.
// Use this to hold the lock across multiple Load/SaveUnlocked calls,
// eliminating TOCTOU races in multi-step operations like rotation.
func (m *Manager) WithLock(fn func() error) error {
	unlock, err := m.lock()
	if err != nil {
		return err
	}
	defer unlock()
	return fn()
}

// SaveUnlocked writes the quota state to disk without acquiring the lock.
// The caller MUST already hold the lock via WithLock. Using this outside
// of WithLock will corrupt state under concurrent access.
func (m *Manager) SaveUnlocked(state *config.QuotaState) error {
	state.Version = config.CurrentQuotaVersion
	return atomicfile.EnsureDirAndWriteJSON(m.statePath(), state)
}

// MarkAvailable marks an account as available (not rate-limited).
func (m *Manager) MarkAvailable(handle string) error {
	unlock, err := m.lock()
	if err != nil {
		return err
	}
	defer unlock()

	state, err := m.Load()
	if err != nil {
		return err
	}

	existing := state.Accounts[handle]
	state.Accounts[handle] = config.AccountQuotaState{
		Status:   config.QuotaStatusAvailable,
		LastUsed: existing.LastUsed,
	}

	return atomicfile.EnsureDirAndWriteJSON(m.statePath(), state)
}

// AvailableAccounts returns account handles that are not rate-limited,
// sorted by least-recently-used first.
func (m *Manager) AvailableAccounts(state *config.QuotaState) []string {
	var available []string
	for handle, acctState := range state.Accounts {
		if acctState.Status == config.QuotaStatusAvailable || acctState.Status == "" {
			available = append(available, handle)
		}
	}
	// Sort by LastUsed ascending (least recently used first)
	sortByLastUsed(available, state)
	return available
}

// LimitedAccounts returns account handles that are currently rate-limited.
func (m *Manager) LimitedAccounts(state *config.QuotaState) []string {
	var limited []string
	for handle, acctState := range state.Accounts {
		if acctState.Status == config.QuotaStatusLimited {
			limited = append(limited, handle)
		}
	}
	return limited
}

// sortByLastUsed sorts handles by their LastUsed timestamp ascending.
func sortByLastUsed(handles []string, state *config.QuotaState) {
	// Simple insertion sort — handles list is small (3-5 accounts)
	for i := 1; i < len(handles); i++ {
		key := handles[i]
		j := i - 1
		for j >= 0 && state.Accounts[handles[j]].LastUsed > state.Accounts[key].LastUsed {
			handles[j+1] = handles[j]
			j--
		}
		handles[j+1] = key
	}
}

// EnsureAccountsTracked adds any registered accounts that are missing from
// quota state. Called during scan to keep state in sync with accounts.json.
func (m *Manager) EnsureAccountsTracked(state *config.QuotaState, accounts map[string]config.Account) {
	for handle := range accounts {
		if _, exists := state.Accounts[handle]; !exists {
			state.Accounts[handle] = config.AccountQuotaState{
				Status: config.QuotaStatusAvailable,
			}
		}
	}
}

// RecordSwap records a keychain swap mapping in quota state.
// targetConfigDir is the config dir whose keychain entry was overwritten.
// sourceHandle is the account handle whose token was swapped in.
// The caller must hold the quota lock or call this within WithLock.
func RecordSwap(state *config.QuotaState, targetConfigDir, sourceHandle string) {
	if state.ActiveSwaps == nil {
		state.ActiveSwaps = make(map[string]string)
	}
	state.ActiveSwaps[targetConfigDir] = sourceHandle
}

// ResolveSwapSourceDirs resolves activeSwaps (targetConfigDir -> accountHandle)
// to targetConfigDir -> sourceConfigDir using the accounts config.
func ResolveSwapSourceDirs(activeSwaps map[string]string, accounts map[string]config.Account) map[string]string {
	resolved := make(map[string]string, len(activeSwaps))
	for targetDir, handle := range activeSwaps {
		acct, ok := accounts[handle]
		if !ok {
			continue
		}
		resolved[targetDir] = util.ExpandHome(acct.ConfigDir)
	}
	return resolved
}

// ClearExpired checks all limited accounts and marks them available if their
// ResetsAt time has passed. Returns the handles of accounts that were cleared
// (in deterministic input order is not preserved — caller should sort if needed).
// The caller is responsible for persisting state if changes were made.
func (m *Manager) ClearExpired(state *config.QuotaState) []string {
	return clearExpiredAt(m, state, time.Now())
}

// clearExpiredAt is the testable core of ClearExpired, accepting a reference time.
func clearExpiredAt(_ *Manager, state *config.QuotaState, now time.Time) []string {
	var cleared []string
	for handle, acctState := range state.Accounts {
		if acctState.Status != config.QuotaStatusLimited {
			continue
		}
		if acctState.ResetsAt == "" {
			continue
		}
		resetTime, err := ParseResetTime(acctState.ResetsAt, now)
		if err != nil {
			continue // can't parse — leave as-is
		}
		if now.After(resetTime) {
			preserved := config.AccountQuotaState{
				Status:           config.QuotaStatusAvailable,
				LastUsed:         acctState.LastUsed,
				TokenExpiresAt:   acctState.TokenExpiresAt,
				TokenLastChecked: acctState.TokenLastChecked,
				RotationCount:    acctState.RotationCount,
				LastRotatedAt:    acctState.LastRotatedAt,
			}
			state.Accounts[handle] = preserved
			cleared = append(cleared, handle)
		}
	}
	return cleared
}

// RefreshTokenExpiries re-inspects every registered account's stored token
// and updates state.Accounts[handle].TokenExpiresAt / TokenLastChecked.
// Returns true when at least one TokenExpiresAt changed — callers persist
// state only in that case. Inspection failures (missing credentials file,
// parse error) leave the existing TokenExpiresAt alone to avoid clobbering
// known-good data on a transient read failure. Caller must hold the quota
// lock (or call inside WithLock) before invoking — Save handles the lock
// when used as a standalone refresh.
func RefreshTokenExpiries(state *config.QuotaState, accounts map[string]config.Account) bool {
	if state == nil || len(accounts) == 0 {
		return false
	}
	changed := false
	now := time.Now().UTC().Format(time.RFC3339)
	for handle, acct := range accounts {
		acctState, ok := state.Accounts[handle]
		if !ok {
			continue
		}
		configDir := util.ExpandHome(acct.ConfigDir)
		// Skip accounts whose config dir currently holds a *borrowed* token
		// from a quota rotation swap. The on-disk .credentials.json belongs to
		// the swap source, not this account — reading it would make every
		// swapped account mirror the source's login/expiry state. Preserve this
		// account's own last-known TokenExpiresAt instead.
		if _, swapped := state.ActiveSwaps[configDir]; swapped {
			continue
		}
		exp, err := InspectKeychainToken(configDir)
		if err != nil {
			// Inspection failed — keep the existing TokenExpiresAt.
			continue
		}
		var newExp string
		if !exp.IsZero() {
			newExp = exp.UTC().Format(time.RFC3339)
		}
		if acctState.TokenExpiresAt != newExp {
			acctState.TokenExpiresAt = newExp
			changed = true
		}
		acctState.TokenLastChecked = now
		state.Accounts[handle] = acctState
	}
	return changed
}

// ApplyTokenExpiries records inspected token expiries onto state.Accounts.
// Accounts whose handle is missing from the inspection map are left untouched.
// Handles unknown to state.Accounts are ignored — we never invent entries.
// Caller must hold the quota lock or invoke this within WithLock.
func ApplyTokenExpiries(state *config.QuotaState, expiries map[string]string) {
	if len(expiries) == 0 {
		return
	}
	now := time.Now().UTC().Format(time.RFC3339)
	for handle, expiresAt := range expiries {
		acct, ok := state.Accounts[handle]
		if !ok {
			continue
		}
		acct.TokenExpiresAt = expiresAt
		acct.TokenLastChecked = now
		state.Accounts[handle] = acct
	}
}

// RecordLimitedSessions overwrites the LimitedSessions snapshot on state.
// `sessions` is the canonical map keyed by tmux session name. Pass an empty
// map to clear the snapshot — quota.json should not retain stale entries.
// Caller must hold the quota lock or invoke this within WithLock.
func RecordLimitedSessions(state *config.QuotaState, sessions map[string]config.LimitedSessionState) {
	if len(sessions) == 0 {
		state.LimitedSessions = nil
		return
	}
	state.LimitedSessions = sessions
}

// RecordLastPlan overwrites the LastPlan snapshot. Pass nil to clear.
// Caller must hold the quota lock or invoke this within WithLock.
func RecordLastPlan(state *config.QuotaState, plan *config.RotationPlanSnapshot) {
	state.LastPlan = plan
}

// RecordRotation bumps the rotation counter and stamps the rotation time
// for the source account. Caller must hold the quota lock.
func RecordRotation(state *config.QuotaState, sourceHandle string, now time.Time) {
	acct := state.Accounts[sourceHandle]
	acct.RotationCount++
	acct.LastRotatedAt = now.UTC().Format(time.RFC3339)
	acct.LastUsed = acct.LastRotatedAt
	state.Accounts[sourceHandle] = acct
}

// MarkLimitedState flips an account to limited in-memory, preserving token
// and rotation fields. Used on the rotate path so the account a session was
// rotated *away* from is recorded as limited — otherwise the respawn clears
// the rate-limit text from the pane and the next scan never re-detects it,
// leaving the blocked account showing "available" on the dashboard.
// Caller must hold the quota lock or invoke this within WithLock.
func MarkLimitedState(state *config.QuotaState, handle, resetsAt string, now time.Time) {
	if handle == "" {
		return
	}
	acct := state.Accounts[handle]
	acct.Status = config.QuotaStatusLimited
	acct.LimitedAt = now.UTC().Format(time.RFC3339)
	acct.ResetsAt = resetsAt
	state.Accounts[handle] = acct
}

// parseResetTimePattern matches formats like "7pm", "11am", "3:30pm", "7:00pm"
var parseResetTimePattern = regexp.MustCompile(`(?i)^(\d{1,2})(?::(\d{2}))?\s*(am|pm)\b`)

// ParseResetTime parses a human-readable reset time string into a time.Time.
// Supported formats:
//
//	"7pm (America/Los_Angeles)" → today at 7pm in that timezone
//	"11am (America/Los_Angeles)" → today at 11am in that timezone
//	"3:30pm (America/Los_Angeles)" → today at 3:30pm in that timezone
//	"7pm" → today at 7pm in local timezone
//
// The reference time is used to determine "today".
func ParseResetTime(resetsAt string, reference time.Time) (time.Time, error) {
	resetsAt = strings.TrimSpace(resetsAt)

	// Extract timezone if present: "7pm (America/Los_Angeles)" or "7pm"
	loc := reference.Location()
	if idx := strings.Index(resetsAt, "("); idx != -1 {
		end := strings.Index(resetsAt, ")")
		if end > idx {
			tzName := strings.TrimSpace(resetsAt[idx+1 : end])
			parsed, err := time.LoadLocation(tzName)
			if err == nil {
				loc = parsed
			}
			resetsAt = strings.TrimSpace(resetsAt[:idx])
		}
	}

	// Parse the time portion: "7pm", "11am", "3:30pm"
	m := parseResetTimePattern.FindStringSubmatch(resetsAt)
	if len(m) < 4 {
		return time.Time{}, fmt.Errorf("cannot parse reset time: %q", resetsAt)
	}

	hour := 0
	fmt.Sscanf(m[1], "%d", &hour)
	minute := 0
	if m[2] != "" {
		fmt.Sscanf(m[2], "%d", &minute)
	}

	ampm := strings.ToLower(m[3])
	if ampm == "pm" && hour != 12 {
		hour += 12
	} else if ampm == "am" && hour == 12 {
		hour = 0
	}

	// Build the reset time using today's date in the target timezone
	refInLoc := reference.In(loc)
	resetTime := time.Date(refInLoc.Year(), refInLoc.Month(), refInLoc.Day(),
		hour, minute, 0, 0, loc)

	return resetTime, nil
}
