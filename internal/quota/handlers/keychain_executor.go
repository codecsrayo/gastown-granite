package handlers

import (
	"context"
	"fmt"
	"sort"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/util"
)

// KeychainExecutor performs the keychain-only half of rotation: for every
// (configDir, sourceHandle) pair in the plan it swaps the keychain token,
// swaps the oauthAccount identity, and records the swap + rotation in
// quota state. It does NOT respawn tmux sessions.
//
// This is the half that is safe to run from anywhere — daemon, cmd, web —
// because it only touches the keychain and quota.json. The respawn half
// still lives in cmd (depends on buildRestartCommand). Once that helper is
// extracted (step 4), a CompositeExecutor will wrap KeychainExecutor +
// a RespawnExecutor for full in-process rotation from the daemon.
//
// Results returned by Execute mirror the shape of quota.RotateResult so
// callers cannot tell the in-process executor from the legacy cmd path.
// Sessions covered by a successful swap are reported with Rotated=true and
// KeychainSwap=true; ResumedSession stays empty (no respawn happened).
type KeychainExecutor struct {
	mgr     *quota.Manager
	accts   *config.AccountsConfig
	now     func() time.Time
	swapper Swapper
}

// Swapper isolates the keychain side effects so tests can substitute a fake.
// Both methods return the same types as the package-level quota helpers
// (SwapKeychainCredential, SwapOAuthAccount) — DefaultSwapper just calls
// through to them.
type Swapper interface {
	SwapKeychain(targetConfigDir, sourceConfigDir string) (*quota.KeychainCredential, error)
	SwapOAuth(targetConfigDir, sourceConfigDir string) error
}

// DefaultSwapper calls quota.SwapKeychainCredential / SwapOAuthAccount.
type DefaultSwapper struct{}

// SwapKeychain implements Swapper.
func (DefaultSwapper) SwapKeychain(target, source string) (*quota.KeychainCredential, error) {
	return quota.SwapKeychainCredential(target, source)
}

// SwapOAuth implements Swapper.
func (DefaultSwapper) SwapOAuth(target, source string) error {
	_, err := quota.SwapOAuthAccount(target, source)
	return err
}

// NewKeychainExecutor builds an executor that uses the real keychain.
func NewKeychainExecutor(mgr *quota.Manager, accts *config.AccountsConfig) *KeychainExecutor {
	return &KeychainExecutor{
		mgr:     mgr,
		accts:   accts,
		now:     time.Now,
		swapper: DefaultSwapper{},
	}
}

// WithSwapper substitutes the Swapper. Returns the executor for chaining.
func (k *KeychainExecutor) WithSwapper(s Swapper) *KeychainExecutor {
	k.swapper = s
	return k
}

// Execute walks plan.ConfigDirSwaps once (one keychain operation per config
// dir, regardless of how many sessions share it) then maps the swap outcome
// back onto plan.Assignments for the per-session result list.
func (k *KeychainExecutor) Execute(_ context.Context, plan *quota.RotatePlan) []quota.RotateResult {
	if plan == nil || len(plan.Assignments) == 0 {
		return nil
	}

	swapErrors := make(map[string]string)  // configDir -> error message
	swappedDirs := make(map[string]string) // configDir -> sourceHandle (successful swap)
	now := k.now()

	// Walk config dirs in deterministic order so test output and audit logs
	// don't churn from map iteration randomness.
	dirs := sortedKeys(plan.ConfigDirSwaps)
	for _, dir := range dirs {
		sourceHandle := plan.ConfigDirSwaps[dir]
		srcAcct, ok := k.accts.Accounts[sourceHandle]
		if !ok {
			swapErrors[dir] = fmt.Sprintf("account %q not found in config", sourceHandle)
			continue
		}
		sourceDir := util.ExpandHome(srcAcct.ConfigDir)
		if _, err := k.swapper.SwapKeychain(dir, sourceDir); err != nil {
			swapErrors[dir] = fmt.Sprintf("keychain swap failed: %v", err)
			continue
		}
		// oauthAccount swap is best-effort — Claude Code still identifies as
		// the new account for *quota* purposes once the keychain token is
		// in place. Log via the returned result but do not fail the swap.
		if err := k.swapper.SwapOAuth(dir, sourceDir); err != nil {
			swapErrors[dir] = fmt.Sprintf("oauth identity swap warning: %v", err)
		}
		swappedDirs[dir] = sourceHandle
	}

	// Persist state mutations in one locked window.
	_ = k.mgr.WithLock(func() error {
		state, err := k.mgr.Load()
		if err != nil {
			return err
		}
		for dir, sourceHandle := range swappedDirs {
			quota.RecordRotation(state, sourceHandle, now)
			quota.RecordSwap(state, dir, sourceHandle)
		}
		return k.mgr.SaveUnlocked(state)
	})

	// Build per-session results.
	results := make([]quota.RotateResult, 0, len(plan.Assignments))
	sessions := sortedKeys(plan.Assignments)
	for _, session := range sessions {
		newAcct := plan.Assignments[session]
		oldAcct := lookupOldAccount(plan, session)
		dir := configDirForSession(plan, k.accts, session)

		res := quota.RotateResult{
			Session:    session,
			NewAccount: newAcct,
			OldAccount: oldAcct,
		}
		if errMsg, bad := swapErrors[dir]; bad {
			res.Error = errMsg
			results = append(results, res)
			continue
		}
		if _, swapped := swappedDirs[dir]; swapped {
			res.Rotated = true
			res.KeychainSwap = true
		}
		results = append(results, res)
	}
	return results
}

// lookupOldAccount returns the account handle the session was rotating away
// from, by walking the plan's limited+near-limit slices.
func lookupOldAccount(plan *quota.RotatePlan, session string) string {
	for _, r := range plan.LimitedSessions {
		if r.Session == session {
			return r.AccountHandle
		}
	}
	for _, r := range plan.NearLimitSessions {
		if r.Session == session {
			return r.AccountHandle
		}
	}
	return ""
}

// configDirForSession resolves the resolved (~/-expanded) config dir for a
// session by joining session→account (from plan) →accounts config.
func configDirForSession(plan *quota.RotatePlan, accts *config.AccountsConfig, session string) string {
	for _, r := range plan.LimitedSessions {
		if r.Session == session {
			if r.ConfigDir != "" {
				return r.ConfigDir
			}
			if r.AccountHandle != "" {
				if acct, ok := accts.Accounts[r.AccountHandle]; ok {
					return util.ExpandHome(acct.ConfigDir)
				}
			}
		}
	}
	for _, r := range plan.NearLimitSessions {
		if r.Session == session {
			if r.ConfigDir != "" {
				return r.ConfigDir
			}
			if r.AccountHandle != "" {
				if acct, ok := accts.Accounts[r.AccountHandle]; ok {
					return util.ExpandHome(acct.ConfigDir)
				}
			}
		}
	}
	return ""
}

func sortedKeys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}
