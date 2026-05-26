package quota

import (
	"fmt"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/util"
)

// RotateResult holds the result of rotating a single session.
type RotateResult struct {
	Session        string `json:"session"`                  // tmux session name
	OldAccount     string `json:"old_account,omitempty"`    // previous account handle
	NewAccount     string `json:"new_account,omitempty"`    // new account handle
	Rotated        bool   `json:"rotated"`                  // whether rotation occurred
	ResumedSession string `json:"resumed_session,omitempty"` // session ID that was resumed (empty if fresh start)
	KeychainSwap   bool   `json:"keychain_swap,omitempty"`   // whether keychain was swapped
	Error          string `json:"error,omitempty"`          // error message if rotation failed
}

// RotatePlan describes what the rotator will do.
type RotatePlan struct {
	// LimitedSessions are sessions detected as hard rate-limited.
	LimitedSessions []ScanResult

	// NearLimitSessions are sessions approaching their rate limit.
	// Only populated when PlanOpts.IncludeNearLimit is true.
	NearLimitSessions []ScanResult `json:"near_limit_sessions,omitempty"`

	// AvailableAccounts are accounts that can be rotated to.
	AvailableAccounts []string

	// Assignments maps session -> new account handle.
	Assignments map[string]string

	// ConfigDirSwaps maps config_dir -> new account handle.
	// One keychain swap per config dir, not per session.
	// All sessions sharing a config dir get the same assignment.
	ConfigDirSwaps map[string]string

	// SpreadConfigDirs maps session -> new CLAUDE_CONFIG_DIR for sessions
	// being redirected to a different account's config dir (spread rotation).
	// Used when multiple sessions share the same config dir and need to be
	// fanned out across multiple accounts. The executor skips keychain swap
	// and instead respawns each session pointing at its target account's dir.
	// Only populated during preemptive --from rotation with multiple accounts.
	SpreadConfigDirs map[string]string `json:"spread_config_dirs,omitempty"`

	// SkippedAccounts maps handle -> reason for accounts that were
	// available by quota status but had invalid/expired tokens.
	SkippedAccounts map[string]string `json:"skipped_accounts,omitempty"`

	// TokenExpiries maps handle -> RFC3339 token expiry parsed during planning.
	// Empty string for accounts whose token format is opaque. Populated for
	// every registered account that the planner inspected, regardless of
	// rotation outcome. Lets the caller persist the data without re-reading
	// the keychain.
	TokenExpiries map[string]string `json:"token_expiries,omitempty"`

	// PlannedAt is the RFC3339 timestamp when PlanRotation finished.
	PlannedAt string `json:"planned_at,omitempty"`
}

// PlanOpts configures the rotation planning behavior.
type PlanOpts struct {
	// FromAccount targets all sessions using this account regardless of
	// rate-limit status (preemptive rotation). Empty string = default behavior.
	FromAccount string

	// IncludeNearLimit includes sessions approaching their rate limit
	// (not just hard-limited sessions) as rotation candidates.
	IncludeNearLimit bool
}

// PlanRotation scans for limited sessions and plans account assignments.
// The opts parameter controls targeting behavior:
//   - opts.FromAccount: targets all sessions using that account regardless of limit status
//   - opts.IncludeNearLimit: also targets sessions approaching their limit
//
// Returns a plan that can be reviewed before execution.
func PlanRotation(scanner *Scanner, mgr *Manager, acctCfg *config.AccountsConfig, opts PlanOpts) (*RotatePlan, error) {
	// Scan for rate-limited and near-limit sessions
	results, err := scanner.ScanAll()
	if err != nil {
		return nil, fmt.Errorf("scanning sessions: %w", err)
	}

	// Load quota state
	state, err := mgr.Load()
	if err != nil {
		return nil, fmt.Errorf("loading quota state: %w", err)
	}
	mgr.EnsureAccountsTracked(state, acctCfg.Accounts)

	// Auto-clear accounts whose reset time has passed so they
	// become available for rotation.
	mgr.ClearExpired(state)

	// Find target sessions based on opts.
	var limitedSessions []ScanResult
	var nearLimitSessions []ScanResult
	for _, r := range results {
		if opts.FromAccount != "" {
			// Preemptive: target all sessions using the specified account
			if r.AccountHandle == opts.FromAccount {
				limitedSessions = append(limitedSessions, r)
			}
		} else {
			// Reactive: target rate-limited sessions
			if r.RateLimited {
				limitedSessions = append(limitedSessions, r)
			} else if r.NearLimit {
				nearLimitSessions = append(nearLimitSessions, r)
			}
		}
	}

	// Combine limited + near-limit sessions for assignment planning
	targetSessions := limitedSessions
	if opts.IncludeNearLimit {
		targetSessions = append(targetSessions, nearLimitSessions...)
	}

	// Available accounts come from persisted state only — NOT from scan
	// detections. Stale sessions (e.g., parked rigs with old rate-limit
	// messages still in the pane) would otherwise mark their accounts as
	// limited, shrinking the available pool and blocking rotation of
	// sessions that actually need it.
	//
	// The caller persists confirmed rate-limit state after execution.
	available := mgr.AvailableAccounts(state)

	// Exclude accounts that are the live active account of a hard rate-limited
	// session in THIS scan. resolveAccountHandle is GT_QUOTA_ACCOUNT-aware, so
	// r.AccountHandle names the account whose token is actually limited right
	// now. Persisted status stays "available" (we never persist scan-detected
	// limits — see above), so without this the planner would keep assigning
	// other sessions ONTO a currently-limited account, respawn them straight
	// into the rate-limit menu, and thrash. This exclusion is transient (per
	// scan), so it can't poison the pool across cycles the way a persisted
	// status flip would.
	liveLimited := make(map[string]bool)
	for _, r := range results {
		if r.RateLimited && r.AccountHandle != "" {
			liveLimited[r.AccountHandle] = true
		}
	}

	// Inspect every registered account's token so callers can persist the
	// expiry on the dashboard, then validate the candidate pool — skip
	// accounts whose tokens are known expired so we don't swap a bad token
	// into a target's keychain.
	expiries := make(map[string]string)
	for handle, acct := range acctCfg.Accounts {
		configDir := util.ExpandHome(acct.ConfigDir)
		if exp, _ := InspectKeychainToken(configDir); !exp.IsZero() {
			expiries[handle] = exp.UTC().Format(time.RFC3339)
		}
	}

	skipped := make(map[string]string)
	var validAvailable []string
	for _, handle := range available {
		if handle == opts.FromAccount {
			continue // rotating away from this account, not a candidate
		}
		if liveLimited[handle] {
			// Active account of a currently rate-limited session — not a
			// safe rotation target even though its persisted status says
			// available.
			skipped[handle] = "currently rate-limited (live scan)"
			continue
		}
		acct, ok := acctCfg.Accounts[handle]
		if !ok {
			continue
		}
		configDir := util.ExpandHome(acct.ConfigDir)
		if err := ValidateKeychainToken(configDir); err != nil {
			skipped[handle] = err.Error()
			continue
		}
		validAvailable = append(validAvailable, handle)
	}
	available = validAvailable

	// Collect unique config dirs from target sessions.
	// Multiple sessions can share the same config dir (via the same account).
	// We only need one keychain swap per config dir.
	// Sessions with unknown accounts are included if they have a CLAUDE_CONFIG_DIR.
	type configDirInfo struct {
		configDir     string // resolved config dir path
		accountHandle string // the limited account using this config dir (may be empty)
	}
	uniqueConfigDirs := make(map[string]*configDirInfo) // configDir -> info
	for _, r := range targetSessions {
		var configDir string
		if r.AccountHandle != "" {
			acct, ok := acctCfg.Accounts[r.AccountHandle]
			if !ok {
				continue
			}
			configDir = util.ExpandHome(acct.ConfigDir)
		} else if r.ConfigDir != "" {
			// Unknown account but we have the config dir from tmux
			configDir = r.ConfigDir
		} else {
			continue // No account and no config dir — can't rotate
		}
		if _, exists := uniqueConfigDirs[configDir]; !exists {
			uniqueConfigDirs[configDir] = &configDirInfo{
				configDir:     configDir,
				accountHandle: r.AccountHandle,
			}
		}
	}

	// Assign available accounts to unique config dirs (round-robin, skip same-account).
	configDirSwaps := make(map[string]string) // configDir -> new account handle
	availIdx := 0
	for configDir, info := range uniqueConfigDirs {
		if availIdx >= len(available) {
			break
		}
		candidate := available[availIdx]
		if candidate == info.accountHandle {
			availIdx++
			if availIdx >= len(available) {
				break
			}
			candidate = available[availIdx] // re-read after skip
		}
		configDirSwaps[configDir] = candidate
		availIdx++
	}

	// Expand config dir assignments to session-level assignments.
	assignments := make(map[string]string)
	for _, r := range targetSessions {
		var configDir string
		if r.AccountHandle != "" {
			acct, ok := acctCfg.Accounts[r.AccountHandle]
			if !ok {
				continue
			}
			configDir = util.ExpandHome(acct.ConfigDir)
		} else if r.ConfigDir != "" {
			configDir = r.ConfigDir
		} else {
			continue
		}
		if newAccount, ok := configDirSwaps[configDir]; ok {
			assignments[r.Session] = newAccount
		}
	}

	// For preemptive rotation (--from), fan out sessions across multiple accounts.
	// When there are more available accounts than unique config dirs, sessions
	// sharing a config dir can each be redirected to a DIFFERENT account's config
	// dir instead of all piling onto a single keychain-swap destination. Each
	// session's CLAUDE_CONFIG_DIR is redirected to the target account's own dir so
	// no keychain swap is needed — sessions use the target account's credentials
	// file directly (SpreadConfigDirs signals this to the executor).
	var spreadConfigDirs map[string]string
	if opts.FromAccount != "" && len(available) > 1 && len(targetSessions) > 1 {
		// Collect all sessions on the from-account (they share a config dir).
		// Assign them round-robin across available accounts.
		spreadConfigDirs = make(map[string]string)
		ai := 0
		for _, r := range targetSessions {
			if ai >= len(available) {
				ai = 0 // wrap around if more sessions than accounts
			}
			// Skip same-account (already excluded from available, but be defensive)
			candidate := available[ai]
			ai++
			targetAcct, ok := acctCfg.Accounts[candidate]
			if !ok {
				continue
			}
			assignments[r.Session] = candidate
			spreadConfigDirs[r.Session] = util.ExpandHome(targetAcct.ConfigDir)
		}
	}

	return &RotatePlan{
		LimitedSessions:   limitedSessions,
		NearLimitSessions: nearLimitSessions,
		AvailableAccounts: available,
		Assignments:       assignments,
		ConfigDirSwaps:    configDirSwaps,
		SpreadConfigDirs:  spreadConfigDirs,
		SkippedAccounts:   skipped,
		TokenExpiries:     expiries,
		PlannedAt:         time.Now().UTC().Format(time.RFC3339),
	}, nil
}
