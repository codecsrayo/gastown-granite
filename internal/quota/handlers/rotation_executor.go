package handlers

import (
	"context"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/sessionrestart"
	"github.com/steveyegge/gastown/internal/util"
)

// RotationExecutor is the canonical in-process rotation engine, used by
// both the daemon quota_dog patrol and the `gt quota rotate` CLI. Per
// (session, newAccount) assignment it:
//
//  1. Resolves the session's live CLAUDE_CONFIG_DIR.
//  2. Performs the keychain + oauthAccount swap once per config dir.
//  3. Rebuilds the restart command via sessionrestart.BuildCommand.
//  4. Respawns the tmux pane with the new env so /resume resumes silently.
//  5. Persists rotation/swap state atomically.
//
// Unlike KeychainExecutor (swap-only), this is safe to wire into the daemon
// orchestrator: it produces the same RotateResult shape as the cmd path,
// including ResumedSession="continue" on success.
type RotationExecutor struct {
	tmux    TmuxOps
	mgr     *quota.Manager
	accts   *config.AccountsConfig
	swapper Swapper
	builder CommandBuilder
	now     func() time.Time
	homeDir func() (string, error)
}

// TmuxOps is the subset of tmux.Tmux needed for respawn. Defined as an
// interface so tests can substitute a fake without spawning real tmux.
type TmuxOps interface {
	GetEnvironment(session, key string) (string, error)
	GetPaneID(session string) (string, error)
	SetRemainOnExit(pane string, value bool) error
	KillPaneProcesses(pane string) error
	ClearHistory(pane string) error
	RespawnPane(pane, command string) error
	SetEnvironment(session, key, value string) error
}

// CommandBuilder produces a respawn command for a session. Defaults to
// sessionrestart.BuildCommand; tests can stub it.
type CommandBuilder func(session string, opts sessionrestart.Options) (string, error)

// RotationExecutorConfig is the constructor input.
type RotationExecutorConfig struct {
	Tmux     TmuxOps
	Manager  *quota.Manager
	Accounts *config.AccountsConfig
	Swapper  Swapper         // nil → DefaultSwapper
	Builder  CommandBuilder  // nil → sessionrestart.BuildCommand
}

// NewRotationExecutor builds an executor wired with optional overrides.
func NewRotationExecutor(cfg RotationExecutorConfig) *RotationExecutor {
	if cfg.Tmux == nil {
		panic("handlers: RotationExecutor requires Tmux")
	}
	if cfg.Manager == nil {
		panic("handlers: RotationExecutor requires Manager")
	}
	if cfg.Accounts == nil {
		panic("handlers: RotationExecutor requires Accounts")
	}
	s := cfg.Swapper
	if s == nil {
		s = DefaultSwapper{}
	}
	b := cfg.Builder
	if b == nil {
		b = sessionrestart.BuildCommand
	}
	return &RotationExecutor{
		tmux:    cfg.Tmux,
		mgr:     cfg.Manager,
		accts:   cfg.Accounts,
		swapper: s,
		builder: b,
		now:     time.Now,
		homeDir: os.UserHomeDir,
	}
}

// Execute walks plan.Assignments in deterministic order and returns one
// RotateResult per session. Errors are reported in-band via RotateResult.Error;
// the executor never returns early — one failing session must not block
// rotation of the others.
func (r *RotationExecutor) Execute(_ context.Context, plan *quota.RotatePlan) []quota.RotateResult {
	if plan == nil || len(plan.Assignments) == 0 {
		return nil
	}

	swappedDirs := make(map[string]string) // configDir -> sourceHandle
	sessions := sortedKeys(plan.Assignments)
	results := make([]quota.RotateResult, 0, len(sessions))
	rotations := make([]rotationFact, 0, len(sessions)) // for persisted state

	for _, sessionName := range sessions {
		newAccount := plan.Assignments[sessionName]
		res := r.executeOne(sessionName, newAccount, plan, swappedDirs)
		results = append(results, res)
		if res.Rotated {
			oldResetsAt := lookupResetsAt(plan, sessionName)
			rotations = append(rotations, rotationFact{
				configDir:   lookupConfigDirForSession(plan, r.accts, sessionName),
				newAccount:  newAccount,
				oldAccount:  res.OldAccount,
				oldResetsAt: oldResetsAt,
			})
		}
	}

	if len(rotations) > 0 {
		now := r.now()
		_ = r.mgr.WithLock(func() error {
			state, err := r.mgr.Load()
			if err != nil {
				return err
			}
			for _, f := range rotations {
				quota.RecordRotation(state, f.newAccount, now)
				if f.configDir != "" {
					quota.RecordSwap(state, f.configDir, f.newAccount, now)
				}
				if f.oldAccount != "" && f.oldAccount != f.newAccount {
					quota.MarkLimitedState(state, f.oldAccount, f.oldResetsAt, now)
				}
			}
			return r.mgr.SaveUnlocked(state)
		})
	}

	return results
}

type rotationFact struct {
	configDir   string
	newAccount  string
	oldAccount  string
	oldResetsAt string
}

func (r *RotationExecutor) executeOne(sessionName, newAccount string, plan *quota.RotatePlan, swappedDirs map[string]string) quota.RotateResult {
	res := quota.RotateResult{Session: sessionName, NewAccount: newAccount}

	currentConfigDir := r.resolveCurrentConfigDir(sessionName)
	if currentConfigDir == "" {
		res.Error = "cannot resolve CLAUDE_CONFIG_DIR"
		return res
	}

	res.OldAccount = r.resolveOldAccount(plan, sessionName, currentConfigDir)

	newAcct, ok := r.accts.Accounts[newAccount]
	if !ok {
		res.Error = fmt.Sprintf("account %q not found in config", newAccount)
		return res
	}
	sourceConfigDir := util.ExpandHome(newAcct.ConfigDir)

	// Spread rotation: session is being redirected to a different account's
	// config dir (no keychain swap needed — target account's credentials are
	// already in its own dir). Only triggered during preemptive --from spread.
	targetConfigDir := currentConfigDir
	if spreadDir, ok := plan.SpreadConfigDirs[sessionName]; ok && spreadDir != "" {
		targetConfigDir = spreadDir
	} else {
		// Keychain-swap rotation: overwrite the token in the current config dir.
		if _, alreadySwapped := swappedDirs[currentConfigDir]; !alreadySwapped {
			if _, err := r.swapper.SwapKeychain(currentConfigDir, sourceConfigDir); err != nil {
				res.Error = fmt.Sprintf("keychain swap failed: %v", err)
				return res
			}
			// oauthAccount swap is best-effort — keychain token alone is enough
			// for quota purposes. Warn via a side log but do not fail the rotation.
			_ = r.swapper.SwapOAuth(currentConfigDir, sourceConfigDir)
			swappedDirs[currentConfigDir] = newAccount
			res.KeychainSwap = true
		}
	}

	restartCmd, err := r.builder(sessionName, sessionrestart.Options{ContinueSession: true})
	if err != nil {
		// Session types that can't be restarted (e.g., hq-boot/deacon) still
		// benefit from the keychain swap. Mirror the cmd path: mark rotated
		// without restart and surface the reason.
		res.Rotated = true
		res.Error = fmt.Sprintf("keychain swapped but could not restart: %v", err)
		return res
	}

	restartCmd = config.PrependEnv(restartCmd, map[string]string{
		"CLAUDE_CONFIG_DIR": targetConfigDir,
		"GT_QUOTA_ACCOUNT":  newAccount,
	})

	pane, err := r.tmux.GetPaneID(sessionName)
	if err != nil {
		res.Error = fmt.Sprintf("getting pane: %v", err)
		return res
	}
	_ = r.tmux.SetRemainOnExit(pane, true)
	_ = r.tmux.KillPaneProcesses(pane)
	_ = r.tmux.ClearHistory(pane)
	if err := r.tmux.RespawnPane(pane, restartCmd); err != nil {
		res.Error = fmt.Sprintf("respawning pane: %v", err)
		return res
	}
	_ = r.tmux.SetEnvironment(sessionName, "GT_QUOTA_ACCOUNT", newAccount)

	res.ResumedSession = "continue"
	res.Rotated = true
	return res
}

// resolveCurrentConfigDir reads CLAUDE_CONFIG_DIR from the tmux session
// environment, falling back to ~/.claude (Claude Code's default).
func (r *RotationExecutor) resolveCurrentConfigDir(sessionName string) string {
	if dir, err := r.tmux.GetEnvironment(sessionName, "CLAUDE_CONFIG_DIR"); err == nil {
		dir = strings.TrimSpace(dir)
		if dir != "" {
			return dir
		}
	}
	home, err := r.homeDir()
	if err != nil || home == "" {
		return ""
	}
	return home + "/.claude"
}

// resolveOldAccount prefers the scanner-resolved active account from the
// plan (which is GT_QUOTA_ACCOUNT-aware). Falls back to matching the
// session's config dir against registered accounts.
func (r *RotationExecutor) resolveOldAccount(plan *quota.RotatePlan, sessionName, currentConfigDir string) string {
	if handle := lookupOldAccount(plan, sessionName); handle != "" {
		return handle
	}
	for handle, acct := range r.accts.Accounts {
		if acct.ConfigDir == currentConfigDir || util.ExpandHome(acct.ConfigDir) == currentConfigDir {
			return handle
		}
	}
	return ""
}

// lookupResetsAt returns the parsed reset time for the session's old account.
func lookupResetsAt(plan *quota.RotatePlan, sessionName string) string {
	for _, r := range plan.LimitedSessions {
		if r.Session == sessionName {
			return r.ResetsAt
		}
	}
	return ""
}

// lookupConfigDirForSession is the same shape as configDirForSession in
// keychain_executor.go but kept separate here so the two files remain
// independently movable.
func lookupConfigDirForSession(plan *quota.RotatePlan, accts *config.AccountsConfig, session string) string {
	return configDirForSession(plan, accts, session)
}
