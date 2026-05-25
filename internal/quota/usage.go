package quota

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/session"
	"github.com/steveyegge/gastown/internal/util"
)

// UsageWindow is the assumed length of an Anthropic rate-limit window. Used
// as the default cutoff when computing "tokens in current window" for an
// account whose state has no other anchor (LimitedAt, LastRotatedAt).
const UsageWindow = 5 * time.Hour

// WeeklyUsageWindow is the rolling window for the dashboard's weekly usage bar,
// mirroring the "Current week" view in the Claude Code /status Usage tab.
const WeeklyUsageWindow = 7 * 24 * time.Hour

// UsageProvider is the minimum tmux surface needed by the aggregator. The
// scan.go TmuxClient already satisfies GetEnvironment; the extra methods are
// needed to walk sessions and locate transcripts.
type UsageProvider interface {
	ListSessions() ([]string, error)
	GetEnvironment(session, key string) (string, error)
	GetPaneWorkDir(session string) (string, error)
}

// TokenCounts holds aggregated token counts for one or more transcripts.
type TokenCounts struct {
	InputTokens         int64 `json:"input_tokens"`
	OutputTokens        int64 `json:"output_tokens"`
	CacheReadTokens     int64 `json:"cache_read_tokens"`
	CacheCreationTokens int64 `json:"cache_creation_tokens"`
}

// Total returns the sum of input + output + cache tokens.
func (t TokenCounts) Total() int64 {
	return t.InputTokens + t.OutputTokens + t.CacheReadTokens + t.CacheCreationTokens
}

// Add accumulates another TokenCounts into the receiver.
func (t *TokenCounts) Add(o TokenCounts) {
	t.InputTokens += o.InputTokens
	t.OutputTokens += o.OutputTokens
	t.CacheReadTokens += o.CacheReadTokens
	t.CacheCreationTokens += o.CacheCreationTokens
}

// SessionUsage captures the per-session contribution to an account's totals.
type SessionUsage struct {
	Session     string      `json:"session"`
	ConfigDir   string      `json:"config_dir,omitempty"`
	Transcript  string      `json:"transcript,omitempty"`
	LastModTime string      `json:"last_mod_time,omitempty"` // RFC3339
	Counts      TokenCounts `json:"counts"`
}

// AccountUsage aggregates token consumption for a single account across all
// of its currently attached sessions. Counts cover the rolling session (5h)
// window; WeekCounts cover the rolling 7-day window. Both are filtered by each
// transcript message's own timestamp, not by file modtime, so a long-lived
// transcript only contributes the tokens emitted inside each window.
type AccountUsage struct {
	Handle          string         `json:"handle"`
	WindowStart     string         `json:"window_start,omitempty"`      // RFC3339, session window
	WeekWindowStart string         `json:"week_window_start,omitempty"` // RFC3339, 7-day window
	Sessions        []SessionUsage `json:"sessions,omitempty"`
	Counts          TokenCounts    `json:"counts"`      // session (5h) window
	WeekCounts      TokenCounts    `json:"week_counts"` // 7-day window
}

// UsageReport bundles per-account aggregates plus a snapshot of unmatched
// sessions (those whose account could not be resolved). The map key is the
// account handle.
type UsageReport struct {
	GeneratedAt    string                  `json:"generated_at"`
	Accounts       map[string]AccountUsage `json:"accounts"`
	OrphanSessions []SessionUsage          `json:"orphan_sessions,omitempty"`
}

// AggregateUsage walks every Gas Town tmux session, resolves which account
// is active in each, locates the most recent Claude Code transcript for that
// session's working directory, sums the token counts emitted since the
// account's current usage window started, and returns the aggregated report.
//
// claudeConfigRoot is typically ~/.claude (the user's Claude Code config root
// for transcript storage, NOT the per-account config dirs). Pass an empty
// string to fall back to config.ClaudeConfigDir().
func AggregateUsage(
	provider UsageProvider,
	state *config.QuotaState,
	accounts *config.AccountsConfig,
	claudeConfigRoot string,
	now time.Time,
) (*UsageReport, error) {
	if claudeConfigRoot == "" {
		root, err := config.ClaudeConfigDir()
		if err != nil {
			return nil, fmt.Errorf("resolving claude config root: %w", err)
		}
		claudeConfigRoot = root
	}

	sessions, err := provider.ListSessions()
	if err != nil {
		return nil, fmt.Errorf("listing tmux sessions: %w", err)
	}

	report := &UsageReport{
		GeneratedAt: now.UTC().Format(time.RFC3339),
		Accounts:    make(map[string]AccountUsage),
	}

	for _, sess := range sessions {
		if !session.IsKnownSession(sess) {
			continue
		}

		handle, configDir := resolveSessionAccount(provider, sess, accounts)
		sessionStart := windowStartFor(handle, state, now)
		weekStart := now.Add(-WeeklyUsageWindow)

		workDir, wderr := provider.GetPaneWorkDir(sess)
		if wderr != nil || workDir == "" {
			continue
		}

		// Claude Code writes transcripts under the session's own
		// CLAUDE_CONFIG_DIR/projects, not a single shared root. Under quota
		// rotation those dirs differ per session (keychain swaps keep the
		// session's original config dir), so resolve the root per session and
		// only fall back to the global root when the session didn't expose a
		// config dir.
		transcriptRoot := claudeConfigRoot
		if configDir != "" {
			transcriptRoot = util.ExpandHome(configDir)
		}

		// Sum every transcript for this workdir touched within the weekly
		// window, splitting tokens into the session (5h) and week (7d) buckets
		// by each message's own timestamp.
		sessionCounts, weekCounts, transcriptPath, modTime, scanErr := sumProjectWindows(transcriptRoot, workDir, sessionStart, weekStart)
		if scanErr != nil || weekCounts.Total() == 0 {
			continue
		}

		su := SessionUsage{
			Session:     sess,
			ConfigDir:   configDir,
			Transcript:  transcriptPath,
			LastModTime: modTime.UTC().Format(time.RFC3339),
			Counts:      sessionCounts,
		}

		if handle == "" {
			report.OrphanSessions = append(report.OrphanSessions, su)
			continue
		}

		entry := report.Accounts[handle]
		entry.Handle = handle
		entry.WindowStart = sessionStart.UTC().Format(time.RFC3339)
		entry.WeekWindowStart = weekStart.UTC().Format(time.RFC3339)
		entry.Sessions = append(entry.Sessions, su)
		entry.Counts.Add(sessionCounts)
		entry.WeekCounts.Add(weekCounts)
		report.Accounts[handle] = entry
	}

	for handle, entry := range report.Accounts {
		sort.Slice(entry.Sessions, func(i, j int) bool {
			return entry.Sessions[i].Session < entry.Sessions[j].Session
		})
		report.Accounts[handle] = entry
	}

	return report, nil
}

// resolveSessionAccount mirrors scan.go's lookup: prefer GT_QUOTA_ACCOUNT,
// then match CLAUDE_CONFIG_DIR against the registered accounts. Returns the
// account handle (empty if unresolved) plus the raw config dir for display.
func resolveSessionAccount(provider UsageProvider, sess string, accounts *config.AccountsConfig) (handle, configDir string) {
	if override, err := provider.GetEnvironment(sess, "GT_QUOTA_ACCOUNT"); err == nil {
		override = strings.TrimSpace(override)
		if override != "" && accounts != nil {
			if _, ok := accounts.Accounts[override]; ok {
				if cd, derr := provider.GetEnvironment(sess, "CLAUDE_CONFIG_DIR"); derr == nil {
					configDir = strings.TrimSpace(cd)
				}
				return override, configDir
			}
		}
	}

	cd, err := provider.GetEnvironment(sess, "CLAUDE_CONFIG_DIR")
	if err != nil {
		return "", ""
	}
	cd = strings.TrimSpace(cd)
	configDir = cd
	if cd == "" || accounts == nil {
		return "", configDir
	}
	for h, acct := range accounts.Accounts {
		if acct.ConfigDir == cd || util.ExpandHome(acct.ConfigDir) == cd {
			return h, configDir
		}
	}
	return "", configDir
}

// windowStartFor returns the earliest time we count tokens against an account
// for "current window" reporting. Anchored to (in priority): LimitedAt (the
// limit started counting from here), LastRotatedAt (we picked this token at
// rotation), or now - UsageWindow as fallback.
func windowStartFor(handle string, state *config.QuotaState, now time.Time) time.Time {
	fallback := now.Add(-UsageWindow)
	if handle == "" || state == nil {
		return fallback
	}
	acct, ok := state.Accounts[handle]
	if !ok {
		return fallback
	}

	candidates := []string{acct.LimitedAt, acct.LastRotatedAt}
	var best time.Time
	for _, c := range candidates {
		if c == "" {
			continue
		}
		t, err := time.Parse(time.RFC3339, c)
		if err != nil {
			continue
		}
		if t.After(best) {
			best = t
		}
	}
	if best.IsZero() || best.Before(fallback) {
		return fallback
	}
	return best
}

// sumProjectWindows walks every .jsonl transcript for a working directory's
// Claude project that was modified at or after weekStart, and sums the token
// counts from assistant messages into two buckets keyed by each message's own
// timestamp: tokens since sessionStart (the session window) and tokens since
// weekStart (the 7-day window). Since sessionStart is always >= weekStart, the
// session bucket is a subset of the week bucket. It also reports the newest
// transcript path + modtime for display. Files last modified before weekStart
// are skipped (their final append predates the window, so no message qualifies).
func sumProjectWindows(claudeConfigRoot, workDir string, sessionStart, weekStart time.Time) (session, week TokenCounts, newestPath string, newestMod time.Time, err error) {
	projectName := strings.ReplaceAll(workDir, "/", "-")
	projectName = strings.ReplaceAll(projectName, "_", "-")
	projectDir := filepath.Join(claudeConfigRoot, "projects", projectName)

	werr := filepath.WalkDir(projectDir, func(path string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			if os.IsNotExist(walkErr) {
				return fs.SkipDir
			}
			return walkErr
		}
		if d.IsDir() {
			if path != projectDir {
				return fs.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(path, ".jsonl") {
			return nil
		}
		info, statErr := d.Info()
		if statErr != nil {
			return nil
		}
		mt := info.ModTime()
		if mt.Before(weekStart) {
			return nil
		}
		if mt.After(newestMod) {
			newestMod = mt
			newestPath = path
		}
		sessC, weekC, ferr := sumTranscriptWindows(path, sessionStart, weekStart)
		if ferr != nil {
			return nil // best effort: skip an unreadable transcript
		}
		session.Add(sessC)
		week.Add(weekC)
		return nil
	})
	if werr != nil {
		return TokenCounts{}, TokenCounts{}, "", time.Time{}, werr
	}
	return session, week, newestPath, newestMod, nil
}

// sumTranscriptWindows parses a Claude Code transcript JSONL file and sums the
// token counts from assistant messages into two timestamp-bucketed totals: the
// session window (>= sessionStart) and the week window (>= weekStart). Messages
// without a parseable RFC3339 timestamp are skipped — they can't be attributed
// to a window.
func sumTranscriptWindows(path string, sessionStart, weekStart time.Time) (session, week TokenCounts, err error) {
	f, ferr := os.Open(path) //nolint:gosec // G304: transcript path derived from local filesystem walk
	if ferr != nil {
		return session, week, ferr
	}
	defer func() { _ = f.Close() }()

	scanner := bufio.NewScanner(f)
	buf := make([]byte, 0, 256*1024)
	scanner.Buffer(buf, 4*1024*1024)

	for scanner.Scan() {
		raw := scanner.Bytes()
		if len(raw) == 0 {
			continue
		}
		// Declare per iteration: json.Unmarshal into a non-nil pointer field
		// reuses the pointed-to struct, which would let cache token fields
		// from the previous line bleed into a turn that omitted them.
		var line struct {
			Type      string `json:"type"`
			Timestamp string `json:"timestamp"`
			Message   struct {
				Usage *struct {
					InputTokens              int `json:"input_tokens"`
					OutputTokens             int `json:"output_tokens"`
					CacheReadInputTokens     int `json:"cache_read_input_tokens"`
					CacheCreationInputTokens int `json:"cache_creation_input_tokens"`
				} `json:"usage,omitempty"`
			} `json:"message,omitempty"`
		}
		if err := json.Unmarshal(raw, &line); err != nil {
			continue
		}
		if line.Type != "assistant" || line.Message.Usage == nil {
			continue
		}
		ts, terr := time.Parse(time.RFC3339, line.Timestamp)
		if terr != nil || ts.Before(weekStart) {
			continue
		}
		u := line.Message.Usage
		c := TokenCounts{
			InputTokens:         int64(u.InputTokens),
			OutputTokens:        int64(u.OutputTokens),
			CacheReadTokens:     int64(u.CacheReadInputTokens),
			CacheCreationTokens: int64(u.CacheCreationInputTokens),
		}
		week.Add(c)
		if !ts.Before(sessionStart) {
			session.Add(c)
		}
	}
	return session, week, scanner.Err()
}
