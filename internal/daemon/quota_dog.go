package daemon

import (
	"context"
	"fmt"
	"time"

	agentconfig "github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/quota/handlers"
	"github.com/steveyegge/gastown/internal/quota/orchestrator"
	"github.com/steveyegge/gastown/internal/tmux"
)

const (
	defaultQuotaDogInterval = 5 * time.Minute
	// quotaDogTimeout is the maximum time allowed for a single rotation cycle.
	quotaDogTimeout = 2 * time.Minute
)

// QuotaDogConfig holds configuration for the quota_dog patrol.
type QuotaDogConfig struct {
	// Enabled controls whether the quota dog runs.
	Enabled bool `json:"enabled"`

	// IntervalStr is how often to run, as a string (e.g., "5m").
	IntervalStr string `json:"interval,omitempty"`
}

// quotaDogInterval returns the configured interval, or the default (5m).
func quotaDogInterval(config *DaemonPatrolConfig) time.Duration {
	if config != nil && config.Patrols != nil && config.Patrols.QuotaDog != nil {
		if config.Patrols.QuotaDog.IntervalStr != "" {
			if d, err := time.ParseDuration(config.Patrols.QuotaDog.IntervalStr); err == nil && d > 0 {
				return d
			}
		}
	}
	return defaultQuotaDogInterval
}

// runQuotaDog executes a quota rotation cycle entirely in-process via the
// quota orchestrator: scan → snapshot → cooldown clear → plan → keychain
// swap → respawn → state persist → audit. No shell-out. All steps run on
// the daemon goroutine inside a bounded context.
func (d *Daemon) runQuotaDog() {
	if !d.isPatrolActive("quota_dog") {
		return
	}

	d.logger.Printf("quota_dog: starting rotation cycle")

	ctx, cancel := context.WithTimeout(d.ctx, quotaDogTimeout)
	defer cancel()

	summary, err := d.tickQuotaOrchestrator(ctx)
	if err != nil {
		d.logger.Printf("quota_dog: tick failed (non-fatal): %v", err)
		return
	}
	d.logger.Printf("quota_dog: scan summary total=%d limited=%d near=%d available=%d",
		summary.Total, summary.Limited, summary.NearLimit, summary.Available)
}

// tickQuotaOrchestrator builds the quota orchestrator (bus + chain + full
// RotationExecutor) and runs a single Tick. Construction is per-call so
// account/config reloads are picked up without a daemon restart; the cost
// is dominated by the tmux walk in ScanAll, not the wiring.
func (d *Daemon) tickQuotaOrchestrator(ctx context.Context) (orchestrator.Summary, error) {
	acctCfg, err := agentconfig.LoadAccountsConfig(constants.MayorAccountsPath(d.config.TownRoot))
	if err != nil {
		return orchestrator.Summary{}, fmt.Errorf("loading accounts config: %w", err)
	}

	t := tmux.NewTmux()
	scanner, err := quota.NewScanner(t, nil, acctCfg)
	if err != nil {
		return orchestrator.Summary{}, fmt.Errorf("building scanner: %w", err)
	}
	if err := scanner.WithWarningPatterns(nil); err != nil {
		return orchestrator.Summary{}, fmt.Errorf("enabling near-limit patterns: %w", err)
	}

	mgr := quota.NewManager(d.config.TownRoot)
	executor := handlers.NewRotationExecutor(handlers.RotationExecutorConfig{
		Tmux:     t,
		Manager:  mgr,
		Accounts: acctCfg,
	})

	orch, err := orchestrator.New(orchestrator.Config{
		Scanner:   scanner,
		Manager:   mgr,
		Accounts:  acctCfg,
		Logger:    d.logger,
		Executor:  executor,
		Dismisser: t, // *tmux.Tmux satisfies MenuDismisser
	})
	if err != nil {
		return orchestrator.Summary{}, fmt.Errorf("building orchestrator: %w", err)
	}
	return orch.Tick(ctx)
}
