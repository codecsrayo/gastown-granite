package quota

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/session"
)

// asstLine builds an assistant transcript line carrying a usage block stamped
// at ts. Production transcripts always timestamp each turn, and the aggregator
// uses that per-message timestamp to bucket tokens into the session/week
// windows — so test fixtures must carry one too.
func asstLine(ts time.Time, in, out, cacheRead, cacheCreate int) string {
	return fmt.Sprintf(
		`{"type":"assistant","timestamp":%q,"message":{"usage":{"input_tokens":%d,"output_tokens":%d,"cache_read_input_tokens":%d,"cache_creation_input_tokens":%d}}}`,
		ts.UTC().Format(time.RFC3339), in, out, cacheRead, cacheCreate,
	)
}

// registerGTPrefix wires the "gt-" prefix into the session registry so
// IsKnownSession matches gt-prefixed names. The package-level registry is
// shared, so we only need to do this once per test binary.
func registerGTPrefix(t *testing.T) {
	t.Helper()
	session.DefaultRegistry().Register("gt", "gastown")
}

// fakeUsageProvider implements UsageProvider for tests without touching tmux.
type fakeUsageProvider struct {
	sessions []string
	env      map[string]map[string]string // session -> key -> value
	workDirs map[string]string             // session -> workdir
}

func (f *fakeUsageProvider) ListSessions() ([]string, error) { return f.sessions, nil }
func (f *fakeUsageProvider) GetEnvironment(sess, key string) (string, error) {
	if m, ok := f.env[sess]; ok {
		if v, ok := m[key]; ok {
			return v, nil
		}
	}
	return "", os.ErrNotExist
}
func (f *fakeUsageProvider) GetPaneWorkDir(sess string) (string, error) {
	if d, ok := f.workDirs[sess]; ok {
		return d, nil
	}
	return "", os.ErrNotExist
}

// writeTranscript drops a JSONL transcript into a faked Claude project dir
// derived from the workdir, mirroring the production layout
// (<claudeRoot>/projects/<encoded-workdir>/<name>.jsonl).
func writeTranscript(t *testing.T, root, workDir, name string, lines []string) string {
	t.Helper()
	projectName := strings.ReplaceAll(workDir, "/", "-")
	projectName = strings.ReplaceAll(projectName, "_", "-")
	dir := filepath.Join(root, "projects", projectName)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	path := filepath.Join(dir, name)
	body := strings.Join(lines, "\n")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatalf("write transcript: %v", err)
	}
	return path
}

func TestAggregateUsage_GroupsByAccount(t *testing.T) {
	registerGTPrefix(t)
	root := t.TempDir()
	wd1 := "/wd1"
	wd2 := "/wd2"

	now := time.Now()
	recent := now.Add(-time.Minute) // inside both the 5h session and 7d week windows

	writeTranscript(t, root, wd1, "a.jsonl", []string{
		`{"type":"user","message":{}}`,
		asstLine(recent, 100, 50, 10, 5),
		asstLine(recent, 40, 20, 0, 0),
		`{"not":"json`, // malformed line — should skip
	})
	writeTranscript(t, root, wd2, "b.jsonl", []string{
		asstLine(recent, 7, 3, 0, 0),
	})

	// Force mod times into the future so they survive the window cutoff
	// regardless of when the test runs.
	mt := now.Add(1 * time.Hour)
	_ = os.Chtimes(filepath.Join(root, "projects", strings.ReplaceAll(wd1, "/", "-"), "a.jsonl"), mt, mt)
	_ = os.Chtimes(filepath.Join(root, "projects", strings.ReplaceAll(wd2, "/", "-"), "b.jsonl"), mt, mt)

	provider := &fakeUsageProvider{
		sessions: []string{"gt-rig-claude", "hq-mayor"},
		env: map[string]map[string]string{
			"gt-rig-claude": {"GT_QUOTA_ACCOUNT": "work"},
			"hq-mayor":      {"GT_QUOTA_ACCOUNT": "personal"},
		},
		workDirs: map[string]string{
			"gt-rig-claude": wd1,
			"hq-mayor":      wd2,
		},
	}
	accounts := &config.AccountsConfig{Accounts: map[string]config.Account{
		"work":     {ConfigDir: "/tmp/work"},
		"personal": {ConfigDir: "/tmp/personal"},
	}}
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work":     {Status: config.QuotaStatusAvailable},
		"personal": {Status: config.QuotaStatusAvailable},
	}}

	report, err := AggregateUsage(provider, state, accounts, root, now)
	if err != nil {
		t.Fatalf("AggregateUsage: %v", err)
	}

	work := report.Accounts["work"]
	if work.Counts.InputTokens != 140 || work.Counts.OutputTokens != 70 ||
		work.Counts.CacheReadTokens != 10 || work.Counts.CacheCreationTokens != 5 {
		t.Errorf("work counts = %+v", work.Counts)
	}
	// Both lines are recent, so the week window holds the same totals as the
	// session window.
	if work.WeekCounts.InputTokens != 140 || work.WeekCounts.OutputTokens != 70 {
		t.Errorf("work week counts = %+v", work.WeekCounts)
	}
	if len(work.Sessions) != 1 || work.Sessions[0].Session != "gt-rig-claude" {
		t.Errorf("work sessions = %+v", work.Sessions)
	}

	personal := report.Accounts["personal"]
	if personal.Counts.InputTokens != 7 || personal.Counts.OutputTokens != 3 {
		t.Errorf("personal counts = %+v", personal.Counts)
	}
}

func TestAggregateUsage_OrphanWhenAccountUnknown(t *testing.T) {
	registerGTPrefix(t)
	root := t.TempDir()
	wd := "/orphan"
	writeTranscript(t, root, wd, "x.jsonl", []string{
		asstLine(time.Now().Add(-time.Minute), 1, 1, 0, 0),
	})
	mt := time.Now().Add(time.Hour)
	_ = os.Chtimes(filepath.Join(root, "projects", strings.ReplaceAll(wd, "/", "-"), "x.jsonl"), mt, mt)

	// CLAUDE_CONFIG_DIR points at the transcript root (where the session
	// actually stores transcripts) but matches no registered account, so the
	// session resolves as an orphan.
	provider := &fakeUsageProvider{
		sessions: []string{"gt-rig-claude"},
		env:      map[string]map[string]string{"gt-rig-claude": {"CLAUDE_CONFIG_DIR": root}},
		workDirs: map[string]string{"gt-rig-claude": wd},
	}
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{}}
	accounts := &config.AccountsConfig{Accounts: map[string]config.Account{}}

	report, err := AggregateUsage(provider, state, accounts, root, time.Now())
	if err != nil {
		t.Fatalf("AggregateUsage: %v", err)
	}
	if len(report.OrphanSessions) != 1 {
		t.Fatalf("expected 1 orphan, got %d (accounts=%v)", len(report.OrphanSessions), report.Accounts)
	}
}

// TestAggregateUsage_PerSessionConfigDirRoot proves transcripts are located
// under each session's own CLAUDE_CONFIG_DIR, not a single global root. Under
// quota rotation, sessions live in different config dirs; a transcript stored
// there must still be counted even though the global root has no copy.
func TestAggregateUsage_PerSessionConfigDirRoot(t *testing.T) {
	registerGTPrefix(t)
	globalRoot := t.TempDir() // dashboard's root — intentionally empty
	sessRoot := t.TempDir()   // the session's actual CLAUDE_CONFIG_DIR
	wd := "/work/rig"

	writeTranscript(t, sessRoot, wd, "a.jsonl", []string{
		asstLine(time.Now().Add(-time.Minute), 80, 20, 0, 0),
	})
	mt := time.Now().Add(time.Hour)
	_ = os.Chtimes(filepath.Join(sessRoot, "projects", strings.ReplaceAll(wd, "/", "-"), "a.jsonl"), mt, mt)

	provider := &fakeUsageProvider{
		sessions: []string{"gt-rig-claude"},
		env: map[string]map[string]string{
			"gt-rig-claude": {"GT_QUOTA_ACCOUNT": "work", "CLAUDE_CONFIG_DIR": sessRoot},
		},
		workDirs: map[string]string{"gt-rig-claude": wd},
	}
	accounts := &config.AccountsConfig{Accounts: map[string]config.Account{
		"work": {ConfigDir: "/tmp/work"},
	}}
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work": {Status: config.QuotaStatusAvailable},
	}}

	report, err := AggregateUsage(provider, state, accounts, globalRoot, time.Now())
	if err != nil {
		t.Fatalf("AggregateUsage: %v", err)
	}
	work := report.Accounts["work"]
	if work.Counts.InputTokens != 80 || work.Counts.OutputTokens != 20 {
		t.Errorf("work counts = %+v (transcript under session config dir not found)", work.Counts)
	}
}

// TestAggregateUsage_SplitsSessionAndWeekWindows proves tokens are bucketed by
// each message's own timestamp: a turn from 3 days ago lands in the week window
// only, while a recent turn lands in both. This is the core of the "remaining
// until block" bars mirroring the /status session vs week views.
func TestAggregateUsage_SplitsSessionAndWeekWindows(t *testing.T) {
	registerGTPrefix(t)
	root := t.TempDir()
	wd := "/wd"

	now := time.Now()
	writeTranscript(t, root, wd, "a.jsonl", []string{
		asstLine(now.Add(-3*24*time.Hour), 1000, 500, 0, 0), // week only (older than 5h)
		asstLine(now.Add(-time.Minute), 100, 50, 0, 0),       // session + week
	})
	mt := now.Add(time.Hour)
	_ = os.Chtimes(filepath.Join(root, "projects", strings.ReplaceAll(wd, "/", "-"), "a.jsonl"), mt, mt)

	provider := &fakeUsageProvider{
		sessions: []string{"gt-rig-claude"},
		env:      map[string]map[string]string{"gt-rig-claude": {"GT_QUOTA_ACCOUNT": "work"}},
		workDirs: map[string]string{"gt-rig-claude": wd},
	}
	accounts := &config.AccountsConfig{Accounts: map[string]config.Account{"work": {ConfigDir: "/tmp/work"}}}
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{"work": {Status: config.QuotaStatusAvailable}}}

	report, err := AggregateUsage(provider, state, accounts, root, now)
	if err != nil {
		t.Fatalf("AggregateUsage: %v", err)
	}
	work := report.Accounts["work"]
	// Session window: only the recent turn.
	if work.Counts.InputTokens != 100 || work.Counts.OutputTokens != 50 {
		t.Errorf("session counts = %+v, want input=100 output=50", work.Counts)
	}
	// Week window: both turns.
	if work.WeekCounts.InputTokens != 1100 || work.WeekCounts.OutputTokens != 550 {
		t.Errorf("week counts = %+v, want input=1100 output=550", work.WeekCounts)
	}
}

func TestWindowStartFor_PrefersLastRotatedOverFallback(t *testing.T) {
	now := time.Date(2026, 5, 24, 12, 0, 0, 0, time.UTC)
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work": {LastRotatedAt: now.Add(-2 * time.Hour).Format(time.RFC3339)},
	}}
	got := windowStartFor("work", state, now)
	want := now.Add(-2 * time.Hour)
	if !got.Equal(want) {
		t.Errorf("window start = %v, want %v", got, want)
	}
}

func TestWindowStartFor_FallsBackBeyondFiveHours(t *testing.T) {
	now := time.Date(2026, 5, 24, 12, 0, 0, 0, time.UTC)
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		// LastRotatedAt 10h ago — older than the 5h window, so fallback wins.
		"work": {LastRotatedAt: now.Add(-10 * time.Hour).Format(time.RFC3339)},
	}}
	got := windowStartFor("work", state, now)
	want := now.Add(-UsageWindow)
	if !got.Equal(want) {
		t.Errorf("window start = %v, want %v (UsageWindow fallback)", got, want)
	}
}
