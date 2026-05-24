package quota

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/session"
)

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

	writeTranscript(t, root, wd1, "a.jsonl", []string{
		`{"type":"user","message":{}}`,
		`{"type":"assistant","message":{"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}`,
		`{"type":"assistant","message":{"usage":{"input_tokens":40,"output_tokens":20}}}`,
		`{"not":"json`, // malformed line — should skip
	})
	writeTranscript(t, root, wd2, "b.jsonl", []string{
		`{"type":"assistant","message":{"usage":{"input_tokens":7,"output_tokens":3}}}`,
	})

	// Force mod times into the future so they survive the window cutoff
	// regardless of when the test runs.
	now := time.Now()
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
		`{"type":"assistant","message":{"usage":{"input_tokens":1,"output_tokens":1}}}`,
	})
	mt := time.Now().Add(time.Hour)
	_ = os.Chtimes(filepath.Join(root, "projects", strings.ReplaceAll(wd, "/", "-"), "x.jsonl"), mt, mt)

	provider := &fakeUsageProvider{
		sessions: []string{"gt-rig-claude"},
		env:      map[string]map[string]string{"gt-rig-claude": {"CLAUDE_CONFIG_DIR": "/who/knows"}},
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
