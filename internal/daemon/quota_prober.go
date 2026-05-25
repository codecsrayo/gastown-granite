package daemon

import (
	"context"
	"fmt"
	"time"

	agentconfig "github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/events"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/quota/handlers"
)

const (
	// defaultQuotaProberInterval is how often the prober checks limited accounts.
	// Tighter than the rotation cadence: probing is gated to the reset window, so
	// most ticks do nothing, and a tight interval means a reactivated account is
	// detected promptly once it actually returns.
	defaultQuotaProberInterval = 2 * time.Minute

	// quotaProberTimeout bounds a single probe cycle. Each account probe has its
	// own inner timeout; this caps the whole batch.
	quotaProberTimeout = 3 * time.Minute

	// quotaProberLead is the look-ahead window: an account is probed when its
	// ResetsAt is within this duration of now. Mirrors the historical
	// `gt quota probe --lead 5m` default.
	quotaProberLead = 5 * time.Minute
)

// QuotaProberConfig holds configuration for the quota_prober patrol.
type QuotaProberConfig struct {
	// Enabled controls whether the prober runs.
	Enabled bool `json:"enabled"`

	// IntervalStr is how often to run, as a string (e.g., "2m").
	IntervalStr string `json:"interval,omitempty"`
}

// quotaProberInterval returns the configured interval, or the default (2m).
func quotaProberInterval(config *DaemonPatrolConfig) time.Duration {
	if config != nil && config.Patrols != nil && config.Patrols.QuotaProber != nil {
		if config.Patrols.QuotaProber.IntervalStr != "" {
			if d, err := time.ParseDuration(config.Patrols.QuotaProber.IntervalStr); err == nil && d > 0 {
				return d
			}
		}
	}
	return defaultQuotaProberInterval
}

// runQuotaProber probes limited accounts in-process via handlers.ProbeExecutor.
// The provider's shown reset time is imprecise, so the prober actively tests
// each due account; clean probes mark the account available and emit
// quota_reactivated events. No shell-out — same execution path as the
// `gt quota probe` CLI.
func (d *Daemon) runQuotaProber() {
	if !d.isPatrolActive("quota_prober") {
		return
	}

	d.logger.Printf("quota_prober: starting probe cycle")

	ctx, cancel := context.WithTimeout(d.ctx, quotaProberTimeout)
	defer cancel()

	reports, err := d.probeAccounts(ctx)
	if err != nil {
		d.logger.Printf("quota_prober: probe failed (non-fatal): %v", err)
		return
	}

	reactivated, stillLimited := 0, 0
	for _, r := range reports {
		switch {
		case r.Reactivated:
			reactivated++
		case r.StillLimited:
			stillLimited++
		}
	}
	if reactivated == 0 && stillLimited == 0 {
		d.logger.Printf("quota_prober: no limited accounts due for probing")
		return
	}
	d.logger.Printf("quota_prober: probed %d (reactivated=%d still_limited=%d)",
		reactivated+stillLimited, reactivated, stillLimited)
}

func (d *Daemon) probeAccounts(ctx context.Context) ([]handlers.ProbeReport, error) {
	acctCfg, err := agentconfig.LoadAccountsConfig(constants.MayorAccountsPath(d.config.TownRoot))
	if err != nil {
		return nil, fmt.Errorf("loading accounts config: %w", err)
	}

	claudePath, err := handlers.ResolveClaudeBinary()
	if err != nil {
		return nil, fmt.Errorf("claude binary not in PATH: %w", err)
	}
	runner := quota.NewClaudeProbeRunner(claudePath, "", quota.DefaultProbePrompt, quota.DefaultProbeTimeout)

	mgr := quota.NewManager(d.config.TownRoot)
	exec := handlers.NewProbeExecutor(handlers.ProbeExecutorConfig{
		Manager:  mgr,
		Accounts: acctCfg,
		Runner:   runner,
		Lead:     quotaProberLead,
		Emit: func(handle, prevResetsAt string) {
			_ = events.LogFeed(events.TypeQuotaReactivated, "quota",
				events.QuotaReactivatedPayload(handle, prevResetsAt))
		},
	})
	return exec.Probe(ctx, nil)
}
