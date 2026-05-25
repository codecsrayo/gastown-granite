package quota

import (
	"fmt"
	"os"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/util"
)

// PickSpawnAccount resolves which account a newly-spawned session should use.
//
// Resolution priority:
//  1. GT_ACCOUNT env var (explicit operator override)
//  2. accountFlag (--account)
//  3. Least-recently-used AVAILABLE account from quota state (load spread)
//  4. Default account from accounts.json (last resort)
//
// Step 3 is the difference from config.ResolveAccountConfigDir, which jumps
// straight to the default account — piling every role session onto one
// account. By picking the LRU available account and stamping its LastUsed,
// consecutive spawns fan out across the pool, so the 4 accounts get used in
// parallel from the start instead of relying on post-hoc quota rotation.
//
// Returns the resolved config dir (~-expanded), the account handle, and an
// error only for an explicitly-requested account that doesn't exist. When no
// accounts are configured it returns ("", "", nil) — callers fall back to the
// ambient CLAUDE_CONFIG_DIR, matching the previous behaviour.
func PickSpawnAccount(townRoot, accountFlag string) (configDir, handle string, err error) {
	accountsPath := constants.MayorAccountsPath(townRoot)
	acctCfg, loadErr := config.LoadAccountsConfig(accountsPath)
	if loadErr != nil || acctCfg == nil || len(acctCfg.Accounts) == 0 {
		return "", "", nil // no accounts — caller uses ambient env
	}

	// Priority 1: GT_ACCOUNT
	if env := os.Getenv("GT_ACCOUNT"); env != "" {
		acct, ok := acctCfg.Accounts[env]
		if !ok {
			return "", "", fmt.Errorf("GT_ACCOUNT %q not found in accounts config", env)
		}
		return util.ExpandHome(acct.ConfigDir), env, nil
	}

	// Priority 2: explicit flag
	if accountFlag != "" {
		acct, ok := acctCfg.Accounts[accountFlag]
		if !ok {
			return "", "", fmt.Errorf("account %q not found in accounts config", accountFlag)
		}
		return util.ExpandHome(acct.ConfigDir), accountFlag, nil
	}

	// Priority 3: LRU available account, stamped so the next spawn differs.
	mgr := NewManager(townRoot)
	var picked string
	_ = mgr.WithLock(func() error {
		state, err := mgr.Load()
		if err != nil {
			return err
		}
		mgr.EnsureAccountsTracked(state, acctCfg.Accounts)
		avail := mgr.AvailableAccounts(state) // LRU-sorted ascending
		if len(avail) == 0 {
			return nil
		}
		picked = avail[0]
		acct := state.Accounts[picked]
		acct.LastUsed = time.Now().UTC().Format(time.RFC3339)
		state.Accounts[picked] = acct
		return mgr.SaveUnlocked(state)
	})
	if picked != "" {
		if acct, ok := acctCfg.Accounts[picked]; ok {
			return util.ExpandHome(acct.ConfigDir), picked, nil
		}
	}

	// Priority 4: default account.
	if acct := acctCfg.GetDefaultAccount(); acct != nil {
		return util.ExpandHome(acct.ConfigDir), acctCfg.Default, nil
	}
	return "", "", nil
}
