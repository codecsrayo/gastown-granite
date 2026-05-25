package handlers

import (
	"context"
	"fmt"
	"os/exec"
	"sort"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
)

// ProbeReport is one row in the probe pipeline output. Mirrors the JSON
// shape that `gt quota probe --json` historically emitted so dashboards
// and scripts keep working unchanged.
type ProbeReport struct {
	Handle       string `json:"handle"`
	ConfigDir    string `json:"config_dir,omitempty"`
	Reactivated  bool   `json:"reactivated"`
	StillLimited bool   `json:"still_limited"`
	Skipped      bool   `json:"skipped,omitempty"`
	Reason       string `json:"reason,omitempty"`
}

// ProbeExecutor runs the live-probe loop against rate-limited accounts.
// Reactivated accounts are flipped to available in quota state and
// announced via AuditEmitter.Emit so the dashboard sees the change.
//
// Same surface as RotationExecutor: dependencies inject so tests run
// without a live claude binary, and the daemon + `gt quota probe`
// command share one code path.
type ProbeExecutor struct {
	mgr     *quota.Manager
	accts   *config.AccountsConfig
	runner  quota.ProbeRunner
	lead    time.Duration
	all     bool
	now     func() time.Time
	emit    func(handle, prevResetsAt string)
}

// ProbeExecutorConfig is the constructor input.
type ProbeExecutorConfig struct {
	Manager  *quota.Manager
	Accounts *config.AccountsConfig

	// Runner runs one probe against a config dir. Pass
	// quota.NewClaudeProbeRunner(...) for production; pass a stub for tests.
	Runner quota.ProbeRunner

	// Lead is how far ahead of an account's ResetsAt the executor will
	// consider it "due" for probing. Ignored when All=true.
	Lead time.Duration

	// All disables the due-window gate; every limited account is probed.
	All bool

	// Emit is invoked once per reactivated handle so the caller can fan
	// out an audit event. Nil → no emit.
	Emit func(handle, prevResetsAt string)
}

// NewProbeExecutor wires a probe executor.
func NewProbeExecutor(cfg ProbeExecutorConfig) *ProbeExecutor {
	return &ProbeExecutor{
		mgr:    cfg.Manager,
		accts:  cfg.Accounts,
		runner: cfg.Runner,
		lead:   cfg.Lead,
		all:    cfg.All,
		now:    time.Now,
		emit:   cfg.Emit,
	}
}

// Probe runs the probe loop. When explicitHandles is empty, every account
// with status=limited is considered; due-window gating still applies unless
// the executor was constructed with All=true. When explicitHandles is
// non-empty, those handles are probed unconditionally (operator override).
func (p *ProbeExecutor) Probe(ctx context.Context, explicitHandles []string) ([]ProbeReport, error) {
	if p.mgr == nil || p.accts == nil || p.runner == nil {
		return nil, fmt.Errorf("probe executor: missing dependencies")
	}

	state, err := p.mgr.Load()
	if err != nil {
		return nil, fmt.Errorf("loading quota state: %w", err)
	}

	explicit := len(explicitHandles) > 0
	targets := explicitHandles
	if !explicit {
		for handle, acctState := range state.Accounts {
			if acctState.Status == config.QuotaStatusLimited {
				targets = append(targets, handle)
			}
		}
		sort.Strings(targets)
	}

	now := p.now()
	reports := make([]ProbeReport, 0, len(targets))

	for _, handle := range targets {
		acctState := state.Accounts[handle]
		rep := ProbeReport{Handle: handle}

		if acctState.Status != config.QuotaStatusLimited {
			rep.Skipped = true
			rep.Reason = "not limited"
			reports = append(reports, rep)
			continue
		}
		if !explicit && !p.all && !quota.ShouldProbe(acctState, now, p.lead) {
			rep.Skipped = true
			rep.Reason = "not due (before reset window)"
			reports = append(reports, rep)
			continue
		}

		acct := p.accts.GetAccount(handle)
		if acct == nil || acct.ConfigDir == "" {
			rep.Skipped = true
			rep.Reason = "no config dir registered"
			reports = append(reports, rep)
			continue
		}
		rep.ConfigDir = acct.ConfigDir

		output, runErr := p.runner(ctx, acct.ConfigDir)
		outcome := quota.ClassifyProbe(output, runErr)

		if outcome.Enabled {
			if err := p.markAvailable(handle); err != nil {
				return nil, fmt.Errorf("marking %s available: %w", handle, err)
			}
			if p.emit != nil {
				p.emit(handle, acctState.ResetsAt)
			}
			rep.Reactivated = true
		} else {
			rep.StillLimited = true
			if outcome.Err != nil && !outcome.RateLimited {
				rep.Reason = "probe failed: " + outcome.Err.Error()
			}
		}
		reports = append(reports, rep)
	}
	return reports, nil
}

// markAvailable flips one account to available inside a locked write.
// Inlined here (rather than reusing the public quota.Manager API) so the
// executor owns one consistent persistence path.
func (p *ProbeExecutor) markAvailable(handle string) error {
	return p.mgr.WithLock(func() error {
		state, err := p.mgr.Load()
		if err != nil {
			return err
		}
		existing := state.Accounts[handle]
		state.Accounts[handle] = config.AccountQuotaState{
			Status:        config.QuotaStatusAvailable,
			LastUsed:      existing.LastUsed,
			WindowResetAt: time.Now().UTC().Format(time.RFC3339),
		}
		return p.mgr.SaveUnlocked(state)
	})
}

// ResolveClaudeBinary returns the path to the `claude` binary on the
// caller's PATH. Helper kept here so daemon callers don't have to import
// os/exec just to wire up a default runner.
func ResolveClaudeBinary() (string, error) {
	return exec.LookPath("claude")
}
