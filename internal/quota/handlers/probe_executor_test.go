package handlers

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
)

func TestProbeExecutorReactivatesCleanAccount(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)

	// Seed a limited account.
	require := func(err error) {
		t.Helper()
		if err != nil {
			t.Fatal(err)
		}
	}
	require(mgr.WithLock(func() error {
		state := &config.QuotaState{
			Version: config.CurrentQuotaVersion,
			Accounts: map[string]config.AccountQuotaState{
				"alpha": {Status: config.QuotaStatusLimited, ResetsAt: "7pm"},
			},
		}
		return mgr.SaveUnlocked(state)
	}))

	accts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/tmp/alpha"},
		},
	}

	var emitted []string
	exec := NewProbeExecutor(ProbeExecutorConfig{
		Manager:  mgr,
		Accounts: accts,
		Runner: func(_ context.Context, _ string) (string, error) {
			return "ok\n", nil // clean probe → enabled
		},
		All: true,
		Emit: func(handle, _ string) {
			emitted = append(emitted, handle)
		},
	})

	reports, err := exec.Probe(context.Background(), nil)
	if err != nil {
		t.Fatalf("probe: %v", err)
	}
	if len(reports) != 1 || !reports[0].Reactivated {
		t.Fatalf("expected 1 reactivated, got %+v", reports)
	}
	if len(emitted) != 1 || emitted[0] != "alpha" {
		t.Fatalf("expected emit for alpha, got %v", emitted)
	}

	state, _ := mgr.Load()
	if state.Accounts["alpha"].Status != config.QuotaStatusAvailable {
		t.Fatalf("alpha status = %s, want available", state.Accounts["alpha"].Status)
	}
}

func TestProbeExecutorRespectsDueGate(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)

	// Limited account with reset time far in the future → not yet due.
	future := time.Now().Add(6 * time.Hour).Format("3pm")
	if err := mgr.WithLock(func() error {
		return mgr.SaveUnlocked(&config.QuotaState{
			Accounts: map[string]config.AccountQuotaState{
				"alpha": {Status: config.QuotaStatusLimited, ResetsAt: future},
			},
		})
	}); err != nil {
		t.Fatal(err)
	}

	accts := &config.AccountsConfig{Accounts: map[string]config.Account{"alpha": {ConfigDir: "/x"}}}
	ranProbe := false
	exec := NewProbeExecutor(ProbeExecutorConfig{
		Manager:  mgr,
		Accounts: accts,
		Runner: func(_ context.Context, _ string) (string, error) {
			ranProbe = true
			return "ok", nil
		},
		Lead: 1 * time.Minute, // tiny window so alpha is not due
	})

	reports, err := exec.Probe(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	if ranProbe {
		t.Fatal("runner should not fire when account is not due")
	}
	if len(reports) != 1 || !reports[0].Skipped {
		t.Fatalf("expected skipped report, got %+v", reports)
	}
}

func TestProbeExecutorRateLimitedKeepsLimited(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)
	if err := mgr.WithLock(func() error {
		return mgr.SaveUnlocked(&config.QuotaState{
			Accounts: map[string]config.AccountQuotaState{
				"alpha": {Status: config.QuotaStatusLimited},
			},
		})
	}); err != nil {
		t.Fatal(err)
	}

	accts := &config.AccountsConfig{Accounts: map[string]config.Account{"alpha": {ConfigDir: "/x"}}}
	exec := NewProbeExecutor(ProbeExecutorConfig{
		Manager:  mgr,
		Accounts: accts,
		Runner: func(_ context.Context, _ string) (string, error) {
			return "Claude AI usage limit reached", errors.New("rate limit hit")
		},
		All: true,
	})

	reports, err := exec.Probe(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(reports) != 1 || !reports[0].StillLimited {
		t.Fatalf("expected still_limited, got %+v", reports)
	}
	state, _ := mgr.Load()
	if state.Accounts["alpha"].Status != config.QuotaStatusLimited {
		t.Fatalf("alpha should remain limited, got %s", state.Accounts["alpha"].Status)
	}
}

func TestProbeExecutorExplicitArgsBypassDueGate(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)
	future := time.Now().Add(6 * time.Hour).Format("3pm")
	if err := mgr.WithLock(func() error {
		return mgr.SaveUnlocked(&config.QuotaState{
			Accounts: map[string]config.AccountQuotaState{
				"alpha": {Status: config.QuotaStatusLimited, ResetsAt: future},
			},
		})
	}); err != nil {
		t.Fatal(err)
	}

	accts := &config.AccountsConfig{Accounts: map[string]config.Account{"alpha": {ConfigDir: "/x"}}}
	exec := NewProbeExecutor(ProbeExecutorConfig{
		Manager:  mgr,
		Accounts: accts,
		Runner: func(_ context.Context, _ string) (string, error) { return "ok", nil },
		Lead:   1 * time.Minute,
	})

	reports, err := exec.Probe(context.Background(), []string{"alpha"})
	if err != nil {
		t.Fatal(err)
	}
	if len(reports) != 1 || !reports[0].Reactivated {
		t.Fatalf("explicit arg should bypass due-gate, got %+v", reports)
	}
}
