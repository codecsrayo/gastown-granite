package handlers

import (
	"context"
	"errors"
	"path/filepath"
	"sync"
	"testing"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/sessionrestart"
)

type fakeTmuxOps struct {
	mu       sync.Mutex
	envs     map[string]map[string]string
	panes    map[string]string
	respawn  []string // pane args order
	envSets  []envSet
	panicGet bool
	respawnErr error
}

type envSet struct{ session, key, value string }

func newFakeTmuxOps() *fakeTmuxOps {
	return &fakeTmuxOps{
		envs:  map[string]map[string]string{},
		panes: map[string]string{},
	}
}
func (f *fakeTmuxOps) setEnv(session, key, val string) {
	if f.envs[session] == nil {
		f.envs[session] = map[string]string{}
	}
	f.envs[session][key] = val
}
func (f *fakeTmuxOps) GetEnvironment(session, key string) (string, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if e, ok := f.envs[session]; ok {
		if v, ok2 := e[key]; ok2 {
			return v, nil
		}
	}
	return "", errors.New("not set")
}
func (f *fakeTmuxOps) GetPaneID(session string) (string, error) {
	if p, ok := f.panes[session]; ok {
		return p, nil
	}
	return "", errors.New("no pane")
}
func (f *fakeTmuxOps) SetRemainOnExit(_ string, _ bool) error { return nil }
func (f *fakeTmuxOps) KillPaneProcesses(_ string) error        { return nil }
func (f *fakeTmuxOps) ClearHistory(_ string) error              { return nil }
func (f *fakeTmuxOps) RespawnPane(pane, _ string) error {
	if f.respawnErr != nil {
		return f.respawnErr
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	f.respawn = append(f.respawn, pane)
	return nil
}
func (f *fakeTmuxOps) SetEnvironment(session, key, value string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.envSets = append(f.envSets, envSet{session, key, value})
	return nil
}

func TestRotationExecutorHappyPath(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)
	targetDir := filepath.Join(townRoot, "alpha-dir")
	accts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: targetDir},
			"beta":  {ConfigDir: filepath.Join(townRoot, "beta-dir")},
		},
	}

	plan := &quota.RotatePlan{
		Assignments:    map[string]string{"gt-rig-witness": "beta"},
		ConfigDirSwaps: map[string]string{targetDir: "beta"},
		LimitedSessions: []quota.ScanResult{
			{Session: "gt-rig-witness", AccountHandle: "alpha", ConfigDir: targetDir, RateLimited: true, ResetsAt: "7pm"},
		},
	}

	tx := newFakeTmuxOps()
	tx.setEnv("gt-rig-witness", "CLAUDE_CONFIG_DIR", targetDir)
	tx.panes["gt-rig-witness"] = "%0"

	swapper := &fakeSwapper{}
	exec := NewRotationExecutor(RotationExecutorConfig{
		Tmux:     tx,
		Manager:  mgr,
		Accounts: accts,
		Swapper:  swapper,
		Builder: func(_ string, _ sessionrestart.Options) (string, error) {
			return "exec claude --continue", nil
		},
	})

	results := exec.Execute(context.Background(), plan)
	if len(results) != 1 {
		t.Fatalf("len(results) = %d, want 1", len(results))
	}
	r := results[0]
	if !r.Rotated || !r.KeychainSwap || r.ResumedSession != "continue" {
		t.Fatalf("result wrong: %+v", r)
	}
	if r.OldAccount != "alpha" || r.NewAccount != "beta" {
		t.Fatalf("account fields wrong: %+v", r)
	}
	if len(swapper.keySwaps) != 1 || len(swapper.oauthSwaps) != 1 {
		t.Fatalf("expected 1 keychain + 1 oauth swap, got key=%d oauth=%d", len(swapper.keySwaps), len(swapper.oauthSwaps))
	}
	if len(tx.respawn) != 1 || tx.respawn[0] != "%0" {
		t.Fatalf("expected respawn on %%0, got %v", tx.respawn)
	}
	// GT_QUOTA_ACCOUNT must be set on the session env so the scanner can
	// resolve the active account on the next pass.
	found := false
	for _, e := range tx.envSets {
		if e.session == "gt-rig-witness" && e.key == "GT_QUOTA_ACCOUNT" && e.value == "beta" {
			found = true
		}
	}
	if !found {
		t.Fatalf("GT_QUOTA_ACCOUNT not set in tmux env: %+v", tx.envSets)
	}

	// State must record the rotation and mark the old account limited.
	state, _ := mgr.Load()
	if state.Accounts["beta"].RotationCount != 1 {
		t.Fatalf("beta rotation count = %d, want 1", state.Accounts["beta"].RotationCount)
	}
	if state.Accounts["alpha"].Status != config.QuotaStatusLimited {
		t.Fatalf("alpha status = %s, want limited", state.Accounts["alpha"].Status)
	}
	if state.ActiveSwaps[targetDir] != "beta" {
		t.Fatalf("ActiveSwaps not recorded: %+v", state.ActiveSwaps)
	}
}

func TestRotationExecutorRespawnFailureReturnsError(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)
	targetDir := filepath.Join(townRoot, "alpha-dir")
	accts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: targetDir},
			"beta":  {ConfigDir: filepath.Join(townRoot, "beta-dir")},
		},
	}
	plan := &quota.RotatePlan{
		Assignments:    map[string]string{"gt-rig-witness": "beta"},
		ConfigDirSwaps: map[string]string{targetDir: "beta"},
		LimitedSessions: []quota.ScanResult{
			{Session: "gt-rig-witness", AccountHandle: "alpha", ConfigDir: targetDir, RateLimited: true},
		},
	}

	tx := newFakeTmuxOps()
	tx.setEnv("gt-rig-witness", "CLAUDE_CONFIG_DIR", targetDir)
	tx.panes["gt-rig-witness"] = "%0"
	tx.respawnErr = errors.New("pane gone")

	exec := NewRotationExecutor(RotationExecutorConfig{
		Tmux:     tx,
		Manager:  mgr,
		Accounts: accts,
		Swapper:  &fakeSwapper{},
		Builder: func(_ string, _ sessionrestart.Options) (string, error) {
			return "claude", nil
		},
	})

	results := exec.Execute(context.Background(), plan)
	if len(results) != 1 {
		t.Fatalf("len = %d", len(results))
	}
	if results[0].Rotated {
		t.Fatal("expected Rotated=false on respawn failure")
	}
	if results[0].Error == "" {
		t.Fatal("expected Error to be populated")
	}

	// Persistence must NOT have happened — rotation didn't complete.
	state, _ := mgr.Load()
	if state.Accounts["beta"].RotationCount != 0 {
		t.Fatalf("rotation count = %d, want 0 on respawn failure", state.Accounts["beta"].RotationCount)
	}
}

func TestRotationExecutorBuildCommandFailureStillRotates(t *testing.T) {
	// Build failure (e.g., unparseable session name) is non-fatal — the
	// keychain has already been swapped, so the session is reported as
	// Rotated=true with the build error captured.
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)
	targetDir := filepath.Join(townRoot, "alpha-dir")
	accts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: targetDir},
			"beta":  {ConfigDir: filepath.Join(townRoot, "beta-dir")},
		},
	}
	plan := &quota.RotatePlan{
		Assignments:    map[string]string{"weird-session": "beta"},
		ConfigDirSwaps: map[string]string{targetDir: "beta"},
		LimitedSessions: []quota.ScanResult{
			{Session: "weird-session", AccountHandle: "alpha", ConfigDir: targetDir, RateLimited: true},
		},
	}
	tx := newFakeTmuxOps()
	tx.setEnv("weird-session", "CLAUDE_CONFIG_DIR", targetDir)

	exec := NewRotationExecutor(RotationExecutorConfig{
		Tmux:     tx,
		Manager:  mgr,
		Accounts: accts,
		Swapper:  &fakeSwapper{},
		Builder: func(_ string, _ sessionrestart.Options) (string, error) {
			return "", errors.New("unknown session type")
		},
	})

	results := exec.Execute(context.Background(), plan)
	if len(results) != 1 {
		t.Fatalf("len = %d", len(results))
	}
	r := results[0]
	if !r.Rotated {
		t.Fatal("expected Rotated=true even when restart command fails (keychain swap landed)")
	}
	if r.Error == "" {
		t.Fatal("expected Error to surface the restart failure reason")
	}
}

// TestRotationExecutorSpreadRotation verifies that when plan.SpreadConfigDirs
// is populated, the executor redirects CLAUDE_CONFIG_DIR to the target
// account's dir and skips the keychain swap.
func TestRotationExecutorSpreadRotation(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)
	alphaDir := filepath.Join(townRoot, "alpha-dir")
	betaDir := filepath.Join(townRoot, "beta-dir")
	gammaDir := filepath.Join(townRoot, "gamma-dir")

	accts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: alphaDir},
			"beta":  {ConfigDir: betaDir},
			"gamma": {ConfigDir: gammaDir},
		},
	}

	// Two sessions on alpha, spread: bear → beta dir, wolf → gamma dir.
	plan := &quota.RotatePlan{
		Assignments: map[string]string{
			"gt-rig-bear": "beta",
			"gt-rig-wolf": "gamma",
		},
		ConfigDirSwaps: map[string]string{alphaDir: "beta"},
		// SpreadConfigDirs overrides CLAUDE_CONFIG_DIR per session.
		SpreadConfigDirs: map[string]string{
			"gt-rig-bear": betaDir,
			"gt-rig-wolf": gammaDir,
		},
		LimitedSessions: []quota.ScanResult{
			{Session: "gt-rig-bear", AccountHandle: "alpha", ConfigDir: alphaDir},
			{Session: "gt-rig-wolf", AccountHandle: "alpha", ConfigDir: alphaDir},
		},
	}

	tx := newFakeTmuxOps()
	tx.setEnv("gt-rig-bear", "CLAUDE_CONFIG_DIR", alphaDir)
	tx.setEnv("gt-rig-wolf", "CLAUDE_CONFIG_DIR", alphaDir)
	tx.panes["gt-rig-bear"] = "%1"
	tx.panes["gt-rig-wolf"] = "%2"

	swapper := &fakeSwapper{}
	exec := NewRotationExecutor(RotationExecutorConfig{
		Tmux:     tx,
		Manager:  mgr,
		Accounts: accts,
		Swapper:  swapper,
		Builder: func(_ string, _ sessionrestart.Options) (string, error) {
			return "exec claude --continue", nil
		},
	})

	results := exec.Execute(context.Background(), plan)
	if len(results) != 2 {
		t.Fatalf("expected 2 results, got %d", len(results))
	}

	for _, r := range results {
		if !r.Rotated {
			t.Errorf("session %s: expected Rotated=true, got false (error: %q)", r.Session, r.Error)
		}
		// Spread rotation must NOT do a keychain swap.
		if r.KeychainSwap {
			t.Errorf("session %s: expected no keychain swap in spread rotation", r.Session)
		}
	}

	// No keychain swaps should have occurred for spread sessions.
	if len(swapper.keySwaps) != 0 {
		t.Errorf("expected 0 keychain swaps for spread rotation, got %d: %v", len(swapper.keySwaps), swapper.keySwaps)
	}

	// Both sessions should have been respawned.
	if len(tx.respawn) != 2 {
		t.Errorf("expected 2 respawns, got %d", len(tx.respawn))
	}

	// GT_QUOTA_ACCOUNT must be set on each session.
	quotaAccts := make(map[string]string)
	for _, e := range tx.envSets {
		if e.key == "GT_QUOTA_ACCOUNT" {
			quotaAccts[e.session] = e.value
		}
	}
	if quotaAccts["gt-rig-bear"] != "beta" {
		t.Errorf("gt-rig-bear GT_QUOTA_ACCOUNT=%q, want beta", quotaAccts["gt-rig-bear"])
	}
	if quotaAccts["gt-rig-wolf"] != "gamma" {
		t.Errorf("gt-rig-wolf GT_QUOTA_ACCOUNT=%q, want gamma", quotaAccts["gt-rig-wolf"])
	}
}
