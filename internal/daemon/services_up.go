package daemon

import (
	"bytes"
	"context"
	"os/exec"
	"time"
)

const (
	defaultServicesUpInterval = 10 * time.Minute
	// servicesUpTimeout bounds a single bring-up cycle. `gt up --restore` spawns
	// tmux sessions and returns once each is launched; the heavy lifting happens
	// inside the sessions, so the wrapping command itself should complete in
	// well under a minute even for large rigs.
	servicesUpTimeout = 5 * time.Minute
)

// ServicesUpConfig holds configuration for the services_up patrol.
//
// The patrol periodically runs `gt up --restore` to re-launch any expected
// service that is no longer running (daemon, deacon, mayor, witnesses,
// refineries, crew, pinned polecats). `gt up` is idempotent — running
// services are not touched.
type ServicesUpConfig struct {
	// Enabled controls whether the services_up patrol runs.
	Enabled bool `json:"enabled"`

	// IntervalStr is how often to run, as a string (e.g., "10m").
	IntervalStr string `json:"interval,omitempty"`

	// Restore controls whether `--restore` is passed to `gt up`. When true,
	// crew and pinned polecats are also brought back; when false, only
	// infrastructure agents (mayor/deacon/witness/refinery) are restored.
	Restore bool `json:"restore,omitempty"`
}

// servicesUpInterval returns the configured interval, or the default (10m).
func servicesUpInterval(config *DaemonPatrolConfig) time.Duration {
	if config != nil && config.Patrols != nil && config.Patrols.ServicesUp != nil {
		if config.Patrols.ServicesUp.IntervalStr != "" {
			if d, err := time.ParseDuration(config.Patrols.ServicesUp.IntervalStr); err == nil && d > 0 {
				return d
			}
		}
	}
	return defaultServicesUpInterval
}

func servicesUpRestore(config *DaemonPatrolConfig) bool {
	if config != nil && config.Patrols != nil && config.Patrols.ServicesUp != nil {
		return config.Patrols.ServicesUp.Restore
	}
	return false
}

// runServicesUp executes one bring-up cycle by shelling out to `gt up`.
// `gt up` is idempotent — running services are not touched, only down ones
// are restarted. This follows the same dumb-scheduler pattern as quota_dog:
// the daemon schedules, an existing command does the work.
func (d *Daemon) runServicesUp() {
	if !d.isPatrolActive("services_up") {
		return
	}

	args := []string{"up", "--quiet"}
	if servicesUpRestore(d.patrolConfig) {
		args = append(args, "--restore")
	}

	d.logger.Printf("services_up: starting bring-up cycle (gt %s)", joinArgs(args))

	ctx, cancel := context.WithTimeout(d.ctx, servicesUpTimeout)
	defer cancel()

	cmd := exec.CommandContext(ctx, d.gtPath, args...) //nolint:gosec // G204: gtPath resolved at daemon init
	cmd.Dir = d.config.TownRoot

	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		stderrStr := stderr.String()
		if stderrStr != "" {
			d.logger.Printf("services_up: bring-up failed (non-fatal): %v: %s", err, stderrStr)
		} else {
			d.logger.Printf("services_up: bring-up failed (non-fatal): %v", err)
		}
		return
	}

	if out := stdout.String(); out != "" {
		d.logger.Printf("services_up: %s", out)
	} else {
		d.logger.Printf("services_up: cycle complete (no changes)")
	}
}

func joinArgs(args []string) string {
	out := ""
	for i, a := range args {
		if i > 0 {
			out += " "
		}
		out += a
	}
	return out
}
