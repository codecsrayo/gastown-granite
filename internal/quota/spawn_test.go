package quota

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/steveyegge/gastown/internal/config"
)

func writeAccounts(t *testing.T, townRoot string, handles ...string) {
	t.Helper()
	cfg := &config.AccountsConfig{
		Version:  config.CurrentAccountsVersion,
		Accounts: map[string]config.Account{},
		Default:  handles[0],
	}
	for _, h := range handles {
		cfg.Accounts[h] = config.Account{ConfigDir: "/home/u/.claude-accounts/" + h}
	}
	path := filepath.Join(townRoot, "mayor", "accounts.json")
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		t.Fatal(err)
	}
	if err := config.SaveAccountsConfig(path, cfg); err != nil {
		t.Fatal(err)
	}
}

func TestPickSpawnAccount_LRUSpread(t *testing.T) {
	t.Setenv("GT_ACCOUNT", "")
	townRoot := setupTestTown(t)
	writeAccounts(t, townRoot, "alpha", "beta", "gamma")

	seen := map[string]bool{}
	// Three consecutive spawns must pick three distinct accounts (LRU rotates
	// through the pool as each pick stamps LastUsed).
	for i := 0; i < 3; i++ {
		_, handle, err := PickSpawnAccount(townRoot, "")
		if err != nil {
			t.Fatalf("pick %d: %v", i, err)
		}
		if handle == "" {
			t.Fatalf("pick %d returned empty handle", i)
		}
		if seen[handle] {
			t.Fatalf("pick %d repeated handle %q before exhausting pool: %v", i, handle, seen)
		}
		seen[handle] = true
	}
	if len(seen) != 3 {
		t.Fatalf("expected 3 distinct accounts across spawns, got %v", seen)
	}
}

func TestPickSpawnAccount_GTAccountOverride(t *testing.T) {
	townRoot := setupTestTown(t)
	writeAccounts(t, townRoot, "alpha", "beta", "gamma")
	t.Setenv("GT_ACCOUNT", "beta")

	dir, handle, err := PickSpawnAccount(townRoot, "")
	if err != nil {
		t.Fatal(err)
	}
	if handle != "beta" {
		t.Fatalf("GT_ACCOUNT override ignored: got %q", handle)
	}
	if dir == "" {
		t.Fatal("expected non-empty config dir")
	}
}

func TestPickSpawnAccount_FlagOverride(t *testing.T) {
	t.Setenv("GT_ACCOUNT", "")
	townRoot := setupTestTown(t)
	writeAccounts(t, townRoot, "alpha", "beta", "gamma")

	_, handle, err := PickSpawnAccount(townRoot, "gamma")
	if err != nil {
		t.Fatal(err)
	}
	if handle != "gamma" {
		t.Fatalf("flag override ignored: got %q", handle)
	}
}

func TestPickSpawnAccount_UnknownFlagErrors(t *testing.T) {
	t.Setenv("GT_ACCOUNT", "")
	townRoot := setupTestTown(t)
	writeAccounts(t, townRoot, "alpha")

	if _, _, err := PickSpawnAccount(townRoot, "nonexistent"); err == nil {
		t.Fatal("expected error for unknown account flag")
	}
}

func TestPickSpawnAccount_NoAccountsReturnsEmpty(t *testing.T) {
	t.Setenv("GT_ACCOUNT", "")
	townRoot := setupTestTown(t)
	// No accounts.json written.
	dir, handle, err := PickSpawnAccount(townRoot, "")
	if err != nil {
		t.Fatalf("expected nil error with no accounts, got %v", err)
	}
	if dir != "" || handle != "" {
		t.Fatalf("expected empty result with no accounts, got dir=%q handle=%q", dir, handle)
	}
}
