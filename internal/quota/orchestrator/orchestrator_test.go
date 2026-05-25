package orchestrator

import (
	"context"
	"errors"
	"testing"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// fakeTmux is a minimal TmuxClient stub. ScanAll iterates sessions and
// captures pane content; we hand back canned values.
type fakeTmux struct {
	sessions []string
	panes    map[string]string
	envs     map[string]map[string]string
}

func (f *fakeTmux) ListSessions() ([]string, error)             { return f.sessions, nil }
func (f *fakeTmux) CapturePane(s string, _ int) (string, error) { return f.panes[s], nil }
func (f *fakeTmux) GetEnvironment(s, k string) (string, error) {
	if e, ok := f.envs[s]; ok {
		if v, ok2 := e[k]; ok2 {
			return v, nil
		}
	}
	return "", errors.New("not set")
}

type fakeExecutor struct {
	called  bool
	results []quota.RotateResult
}

func (f *fakeExecutor) Execute(_ context.Context, _ *quota.RotatePlan) []quota.RotateResult {
	f.called = true
	return f.results
}

func TestOrchestratorTickWithoutExecutorDoesNotExecute(t *testing.T) {
	tmux := &fakeTmux{
		sessions: []string{"hq-mayor"},
		panes:    map[string]string{"hq-mayor": "ready"},
		envs:     map[string]map[string]string{"hq-mayor": {"CLAUDE_CONFIG_DIR": "/tmp/cfg"}},
	}
	accts := &config.AccountsConfig{Accounts: map[string]config.Account{}}
	scanner, err := quota.NewScanner(tmux, []string{"non-matching-pattern"}, accts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)

	orch, err := New(Config{Scanner: scanner, Manager: mgr, Accounts: accts})
	if err != nil {
		t.Fatal(err)
	}

	summary, err := orch.Tick(context.Background())
	if err != nil {
		t.Fatalf("tick: %v", err)
	}
	if summary.Limited != 0 {
		t.Fatalf("limited = %d, want 0", summary.Limited)
	}
}

func TestOrchestratorTickWiresExecutorOnRotationPlanned(t *testing.T) {
	// Verify the orchestrator subscribes the executor to RotationPlanned
	// events. We bypass Tick (which requires a real scan) by publishing
	// directly on the bus.
	tmux := &fakeTmux{}
	accts := &config.AccountsConfig{Accounts: map[string]config.Account{}}
	scanner, _ := quota.NewScanner(tmux, nil, accts)

	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)
	exec := &fakeExecutor{
		results: []quota.RotateResult{{Session: "x", NewAccount: "y", Rotated: true, KeychainSwap: true}},
	}

	orch, err := New(Config{
		Scanner: scanner, Manager: mgr, Accounts: accts,
		Executor: exec,
	})
	if err != nil {
		t.Fatal(err)
	}

	var respawnSeen bool
	orch.Bus().Subscribe(qe.KindSessionRespawned, func(_ context.Context, _ bus.Event) error {
		respawnSeen = true
		return nil
	})

	plan := &quota.RotatePlan{Assignments: map[string]string{"x": "y"}}
	if err := orch.Bus().Publish(context.Background(), qe.RotationPlanned{Plan: plan}); err != nil {
		t.Fatal(err)
	}
	if !exec.called {
		t.Fatal("executor not called via bus subscription")
	}
	if !respawnSeen {
		t.Fatal("SessionRespawned not observed downstream")
	}
}
