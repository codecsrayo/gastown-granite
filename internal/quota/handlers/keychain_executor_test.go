package handlers

import (
	"context"
	"errors"
	"path/filepath"
	"sync"
	"testing"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
)

type fakeSwapper struct {
	mu          sync.Mutex
	swapKeyErr  map[string]error // keyed by targetDir
	oauthErr    map[string]error
	keySwaps    []swapCall
	oauthSwaps  []swapCall
}

type swapCall struct{ target, source string }

func (f *fakeSwapper) SwapKeychain(target, source string) (*quota.KeychainCredential, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.keySwaps = append(f.keySwaps, swapCall{target, source})
	if err := f.swapKeyErr[target]; err != nil {
		return nil, err
	}
	return &quota.KeychainCredential{}, nil
}

func (f *fakeSwapper) SwapOAuth(target, source string) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.oauthSwaps = append(f.oauthSwaps, swapCall{target, source})
	return f.oauthErr[target]
}

func TestKeychainExecutorHappyPath(t *testing.T) {
	townRoot := t.TempDir()
	mgr := quota.NewManager(townRoot)

	accts := &config.AccountsConfig{
		Version: config.CurrentAccountsVersion,
		Accounts: map[string]config.Account{
			"alpha": {Email: "a@x", ConfigDir: filepath.Join(townRoot, "alpha-dir")},
			"beta":  {Email: "b@x", ConfigDir: filepath.Join(townRoot, "beta-dir")},
		},
	}

	plan := &quota.RotatePlan{
		Assignments:    map[string]string{"rig-1": "beta"},
		ConfigDirSwaps: map[string]string{filepath.Join(townRoot, "alpha-dir"): "beta"},
		LimitedSessions: []quota.ScanResult{
			{Session: "rig-1", AccountHandle: "alpha", ConfigDir: filepath.Join(townRoot, "alpha-dir"), RateLimited: true},
		},
	}

	swapper := &fakeSwapper{}
	exec := NewKeychainExecutor(mgr, accts).WithSwapper(swapper)

	results := exec.Execute(context.Background(), plan)
	if len(results) != 1 {
		t.Fatalf("len(results) = %d, want 1", len(results))
	}
	r := results[0]
	if r.Session != "rig-1" || !r.Rotated || !r.KeychainSwap {
		t.Fatalf("result wrong: %+v", r)
	}
	if r.OldAccount != "alpha" || r.NewAccount != "beta" {
		t.Fatalf("account fields wrong: %+v", r)
	}
	if len(swapper.keySwaps) != 1 {
		t.Fatalf("keychain swaps = %d, want 1", len(swapper.keySwaps))
	}
	if len(swapper.oauthSwaps) != 1 {
		t.Fatalf("oauth swaps = %d, want 1", len(swapper.oauthSwaps))
	}

	// Verify state persistence: source account got rotation recorded.
	state, err := mgr.Load()
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if state.Accounts["beta"].RotationCount != 1 {
		t.Fatalf("beta RotationCount = %d, want 1", state.Accounts["beta"].RotationCount)
	}
	if state.ActiveSwaps[filepath.Join(townRoot, "alpha-dir")] != "beta" {
		t.Fatalf("ActiveSwaps not recorded: %+v", state.ActiveSwaps)
	}
}

func TestKeychainExecutorSwapFailure(t *testing.T) {
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
		Assignments:    map[string]string{"rig-1": "beta"},
		ConfigDirSwaps: map[string]string{targetDir: "beta"},
		LimitedSessions: []quota.ScanResult{
			{Session: "rig-1", AccountHandle: "alpha", ConfigDir: targetDir, RateLimited: true},
		},
	}

	swapper := &fakeSwapper{
		swapKeyErr: map[string]error{targetDir: errors.New("keychain locked")},
	}
	exec := NewKeychainExecutor(mgr, accts).WithSwapper(swapper)

	results := exec.Execute(context.Background(), plan)
	if len(results) != 1 {
		t.Fatalf("len(results) = %d", len(results))
	}
	r := results[0]
	if r.Rotated {
		t.Fatal("expected Rotated=false on swap failure")
	}
	if r.Error == "" {
		t.Fatal("expected Error to be populated")
	}

	state, _ := mgr.Load()
	if state.Accounts["beta"].RotationCount != 0 {
		t.Fatalf("expected no rotation recorded on failure, got %d", state.Accounts["beta"].RotationCount)
	}
}

func TestKeychainExecutorDeduplicatesConfigDir(t *testing.T) {
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
		Assignments: map[string]string{
			"rig-1": "beta",
			"rig-2": "beta",
		},
		ConfigDirSwaps: map[string]string{targetDir: "beta"},
		LimitedSessions: []quota.ScanResult{
			{Session: "rig-1", AccountHandle: "alpha", ConfigDir: targetDir, RateLimited: true},
			{Session: "rig-2", AccountHandle: "alpha", ConfigDir: targetDir, RateLimited: true},
		},
	}

	swapper := &fakeSwapper{}
	exec := NewKeychainExecutor(mgr, accts).WithSwapper(swapper)

	results := exec.Execute(context.Background(), plan)
	if len(results) != 2 {
		t.Fatalf("results = %d, want 2", len(results))
	}
	if len(swapper.keySwaps) != 1 {
		t.Fatalf("expected 1 keychain swap for shared config dir, got %d", len(swapper.keySwaps))
	}
	for _, r := range results {
		if !r.Rotated || !r.KeychainSwap {
			t.Fatalf("expected both sessions rotated, got %+v", r)
		}
	}
}
