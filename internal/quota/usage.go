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

// WalkAccountUsage walks each registered account's CLAUDE_CONFIG_DIR/projects
// tree directly and sums every assistant message's tokens that fall inside the
// session (5h) and week (7d) windows. Mirrors what `claude /status` reports
// from inside that config dir — independent of tmux state, so accounts with
// no currently-attached sessions still surface their historical contribution.
//
// When state carries an active keychain swap for an account's config dir, the
// transcripts physically written under that dir burn against the *source*
// account's token, not the host's. Tokens whose message timestamp is at or
// after state.ActiveSwapStarts[hostConfigDir] are re-attributed to the source
// handle; earlier tokens stay on the host. A swap entry whose start time is
// missing causes everything in that dir to flow to the source — pre-swap
// history rarely dominates a rolling 5h/7d window, and "attribute to source"
// matches the dashboard's prediction goal (which quota will block first).
//
// Pass a nil state to disable swap relabel and bucket strictly by config dir.
func WalkAccountUsage(accounts *config.AccountsConfig, state *config.QuotaState, now time.Time) (*UsageReport, error) {
	report := &UsageReport{
		GeneratedAt: now.UTC().Format(time.RFC3339),
		Accounts:    make(map[string]AccountUsage),
	}
	if accounts == nil {
		return report, nil
	}
	sessionStart := now.Add(-UsageWindow)
	weekStart := now.Add(-WeeklyUsageWindow)

	swapSources, swapStarts := resolveSwapAttribution(accounts, state)

	for handle, acct := range accounts.Accounts {
		configDir := util.ExpandHome(acct.ConfigDir)
		if configDir == "" {
			continue
		}
		sourceHandle, hasSwap := swapSources[configDir]
		var splitAt time.Time
		if hasSwap {
			splitAt = swapStarts[configDir] // zero ⇒ everything counts as post-split (i.e. source)
		}

		preSess, preWeek, postSess, postWeek, err := sumConfigDirPartitioned(configDir, sessionStart, weekStart, splitAt)
		if err != nil {
			continue
		}

		// Host attribution: tokens written before the swap (or all tokens when
		// no swap is active on this dir).
		hostSess, hostWeek := preSess, preWeek
		if !hasSwap {
			hostSess.Add(postSess)
			hostWeek.Add(postWeek)
		}
		if hostWeek.Total() > 0 {
			entry := report.Accounts[handle]
			entry.Handle = handle
			entry.WindowStart = sessionStart.UTC().Format(time.RFC3339)
			entry.WeekWindowStart = weekStart.UTC().Format(time.RFC3339)
			entry.Counts.Add(hostSess)
			entry.WeekCounts.Add(hostWeek)
			report.Accounts[handle] = entry
		}

		// Source attribution: tokens written under this dir while it hosts the
		// source's borrowed token. Fold them onto the source's own entry so the
		// source's bar reflects burn against its real quota.
		if hasSwap && postWeek.Total() > 0 && sourceHandle != "" {
			src := report.Accounts[sourceHandle]
			src.Handle = sourceHandle
			src.WindowStart = sessionStart.UTC().Format(time.RFC3339)
			src.WeekWindowStart = weekStart.UTC().Format(time.RFC3339)
			src.Counts.Add(postSess)
			src.WeekCounts.Add(postWeek)
			report.Accounts[sourceHandle] = src
		}
	}
	return report, nil
}

// resolveSwapAttribution builds two maps keyed by expanded host config dir:
//   - swapSources[dir] = source account handle (when a swap is active)
//   - swapStarts[dir]  = parsed RFC3339 start time (zero if missing/unparsable)
//
// Both maps key on the expanded config dir of each registered host account so
// the walker can look them up using its own expanded paths. Entries whose
// target dir is not the config dir of any registered account are dropped —
// they can't be attributed to a host handle anyway.
func resolveSwapAttribution(accounts *config.AccountsConfig, state *config.QuotaState) (sources map[string]string, starts map[string]time.Time) {
	sources = map[string]string{}
	starts = map[string]time.Time{}
	if state == nil || len(state.ActiveSwaps) == 0 || accounts == nil {
		return sources, starts
	}
	knownDirs := map[string]bool{}
	for _, acct := range accounts.Accounts {
		knownDirs[util.ExpandHome(acct.ConfigDir)] = true
	}
	for rawDir, sourceHandle := range state.ActiveSwaps {
		dir := util.ExpandHome(rawDir)
		if !knownDirs[dir] {
			continue
		}
		if _, ok := accounts.Accounts[sourceHandle]; !ok {
			continue // stale swap entry pointing at an unknown source
		}
		sources[dir] = sourceHandle
		if started := state.ActiveSwapStarts[rawDir]; started != "" {
			if t, err := time.Parse(time.RFC3339, started); err == nil {
				starts[dir] = t
			}
		}
	}
	return sources, starts
}

// sumConfigDirPartitioned walks configDir/projects/**.jsonl and sums tokens
// into four buckets: pre-split session/week and post-split session/week.
// A zero splitAt routes every in-window token to the post bucket (callers use
// this when a swap is active but its start time is unknown). When the caller
// is not interested in a split it can sum pre+post.
func sumConfigDirPartitioned(configDir string, sessionStart, weekStart, splitAt time.Time) (preSess, preWeek, postSess, postWeek TokenCounts, err error) {
	projectsRoot := filepath.Join(configDir, "projects")
	werr := filepath.WalkDir(projectsRoot, func(path string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			if os.IsNotExist(walkErr) {
				return fs.SkipDir
			}
			return walkErr
		}
		if d.IsDir() {
			return nil
		}
		if !strings.HasSuffix(path, ".jsonl") {
			return nil
		}
		info, statErr := d.Info()
		if statErr != nil || info.ModTime().Before(weekStart) {
			return nil
		}
		pSess, pWeek, qSess, qWeek, ferr := sumTranscriptPartitioned(path, sessionStart, weekStart, splitAt)
		if ferr != nil {
			return nil // best effort: skip an unreadable transcript
		}
		preSess.Add(pSess)
		preWeek.Add(pWeek)
		postSess.Add(qSess)
		postWeek.Add(qWeek)
		return nil
	})
	if werr != nil {
		return TokenCounts{}, TokenCounts{}, TokenCounts{}, TokenCounts{}, werr
	}
	return preSess, preWeek, postSess, postWeek, nil
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

// sumTranscriptPartitioned parses a Claude Code transcript JSONL and produces
// four token totals: tokens before splitAt (pre) and at-or-after splitAt
// (post), each split again into the session (5h) and week (7d) windows.
//
// splitAt = zero time routes every in-window token into the post bucket, so
// callers that only want raw windowed totals can sum pre+post — and callers
// that intend a swap partition but lack a recorded start time conservatively
// treat all in-window history as post-split.
func sumTranscriptPartitioned(path string, sessionStart, weekStart, splitAt time.Time) (preSess, preWeek, postSess, postWeek TokenCounts, err error) {
	f, ferr := os.Open(path) //nolint:gosec // G304: transcript path derived from local filesystem walk
	if ferr != nil {
		return preSess, preWeek, postSess, postWeek, ferr
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
		if jerr := json.Unmarshal(raw, &line); jerr != nil {
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
		post := splitAt.IsZero() || !ts.Before(splitAt)
		if post {
			postWeek.Add(c)
			if !ts.Before(sessionStart) {
				postSess.Add(c)
			}
		} else {
			preWeek.Add(c)
			if !ts.Before(sessionStart) {
				preSess.Add(c)
			}
		}
	}
	return preSess, preWeek, postSess, postWeek, scanner.Err()
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
