package daemon

import (
	"bytes"
	"context"
	"os/exec"
	"time"
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

// runQuotaProber probes limited accounts by shelling out to `gt quota probe`.
// The provider's shown reset time is imprecise, so instead of trusting it the
// prober actively tests each due account and, on a clean probe, marks it
// available and emits a quota_reactivated event.
//
// Like quota_dog, this follows the daemon's "dumb scheduler" principle: the
// daemon schedules, `gt quota probe` does the work (gating, probing, state
// update, event emission).
func (d *Daemon) runQuotaProber() {
	if !d.isPatrolActive("quota_prober") {
		return
	}

	d.logger.Printf("quota_prober: starting probe cycle")

	ctx, cancel := context.WithTimeout(d.ctx, quotaProberTimeout)
	defer cancel()

	cmd := exec.CommandContext(ctx, d.gtPath, "quota", "probe", "--json") //nolint:gosec // G204: gtPath resolved at daemon init
	cmd.Dir = d.config.TownRoot

	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		// Non-fatal: probe failure shouldn't crash the daemon.
		// Common expected failures: no accounts configured, claude not in PATH.
		stderrStr := stderr.String()
		if stderrStr != "" {
			d.logger.Printf("quota_prober: probe failed (non-fatal): %v: %s", err, stderrStr)
		} else {
			d.logger.Printf("quota_prober: probe failed (non-fatal): %v", err)
		}
		return
	}

	outStr := stdout.String()
	if outStr != "" && outStr != "[]\n" && outStr != "[]" {
		d.logger.Printf("quota_prober: probe result: %s", outStr)
	} else {
		d.logger.Printf("quota_prober: no limited accounts due for probing")
	}
}
