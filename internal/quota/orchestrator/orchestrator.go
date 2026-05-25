// Package orchestrator wires the quota bus, chain, and handlers into a
// single ready-to-use unit. Callers (daemon patrols, gt commands) get a
// configured bus and a Tick() entrypoint without needing to know which
// handlers are subscribed.
package orchestrator

import (
	"context"
	"fmt"
	"log"
	"time"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/quota/handlers"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// Orchestrator owns the bus and the scan chain.
type Orchestrator struct {
	bus     *bus.Bus
	scan    *chain.Chain
	scanner *quota.Scanner
	logger  *log.Logger
}

// Config holds dependencies for building an Orchestrator.
type Config struct {
	Scanner  *quota.Scanner
	Manager  *quota.Manager
	Accounts *config.AccountsConfig
	Logger   *log.Logger

	// Executor, when set, is wired to RotationPlanned events. It performs
	// the side-effectful half of rotation (keychain swap, respawn). Leave
	// nil for plan-only Tick() (snapshot + cooldown + plan + audit, no
	// execution) — useful for dashboards and dry runs.
	Executor handlers.Executor

	// Dismisser, when set, un-wedges hard rate-limited sessions that have no
	// rotation target by cancelling Claude Code's blocking rate-limit modal.
	// Leave nil to skip menu handling.
	Dismisser handlers.MenuDismisser
}

// New builds an Orchestrator with the default chain layout:
//
//	scan_fanout -> snapshot_persist -> cooldown_clear -> rotation_planner -> audit_emit
//
// All links are stateless across calls except through their injected
// dependencies, so the same Orchestrator may be Tick()'d repeatedly.
func New(cfg Config) (*Orchestrator, error) {
	if cfg.Scanner == nil {
		return nil, fmt.Errorf("orchestrator: nil scanner")
	}
	if cfg.Manager == nil {
		return nil, fmt.Errorf("orchestrator: nil manager")
	}
	if cfg.Accounts == nil {
		return nil, fmt.Errorf("orchestrator: nil accounts config")
	}

	b := bus.New(cfg.Logger)

	scan := chain.New(
		handlers.NewFanoutLink(b),
		handlers.NewSnapshotLink(cfg.Manager),
		handlers.NewCooldownLink(cfg.Manager, b),
		handlers.NewPlannerLink(cfg.Scanner, cfg.Manager, cfg.Accounts, b),
		handlers.NewAuditEmitter(),
	)

	// Per-event audit sink for events that are *published* (not chain-run):
	// fanout-published per-session events, planner-published RotationPlanned,
	// cooldown-published AccountCleared, plus future executor events.
	audit := handlers.AuditEmitter{}
	auditSub := func(_ context.Context, e bus.Event) error {
		audit.Emit(e)
		return nil
	}
	for _, kind := range []string{
		qe.KindSessionLimited,
		qe.KindSessionNearLimit,
		qe.KindAccountCleared,
		qe.KindAccountReactivated,
		qe.KindTokenExpired,
		qe.KindRotationPlanned,
		qe.KindKeychainSwapped,
		qe.KindSessionRespawned,
		qe.KindRotationFailed,
	} {
		b.Subscribe(kind, auditSub)
	}

	// RotationPlanned subscribers: executor (side-effects) + dismisser
	// (un-wedge no-target sessions). Composed into one chain so they run in
	// order on each plan; future links (policy gates, metrics, escalation)
	// slot in here without touching callers.
	var plannedLinks []chain.Link
	if cfg.Executor != nil {
		plannedLinks = append(plannedLinks, handlers.NewExecutorLink(cfg.Executor, b))
	}
	if cfg.Dismisser != nil {
		plannedLinks = append(plannedLinks, handlers.NewMenuDismissLink(cfg.Dismisser))
	}
	if len(plannedLinks) > 0 {
		plannedChain := chain.New(plannedLinks...)
		b.Subscribe(qe.KindRotationPlanned, plannedChain.AsHandler())
	}

	return &Orchestrator{
		bus:     b,
		scan:    scan,
		scanner: cfg.Scanner,
		logger:  cfg.Logger,
	}, nil
}

// Bus exposes the underlying bus so callers can subscribe additional
// handlers (policy, metrics, escalation) without re-building the chain.
func (o *Orchestrator) Bus() *bus.Bus { return o.bus }

// Summary is the lightweight return value of Tick — callers (the daemon
// patrol, gt commands) use it to decide whether to invoke a downstream
// executor without re-walking the full results slice.
type Summary struct {
	Total     int
	Limited   int
	NearLimit int
	Available int
}

// Tick runs one scan cycle and drives the chain. It is safe to call from
// any goroutine; the bus serializes access internally.
func (o *Orchestrator) Tick(ctx context.Context) (Summary, error) {
	results, err := o.scanner.ScanAll()
	if err != nil {
		return Summary{}, fmt.Errorf("orchestrator scan: %w", err)
	}
	limited, near, available := classify(results)
	evt := qe.ScanCompleted{
		Total:     len(results),
		Limited:   limited,
		NearLimit: near,
		Available: available,
		Results:   results,
		At:        time.Now().UTC(),
	}
	if err := o.scan.Run(ctx, evt); err != nil {
		return Summary{Total: len(results), Limited: limited, NearLimit: near, Available: available}, err
	}
	return Summary{Total: len(results), Limited: limited, NearLimit: near, Available: available}, nil
}

func classify(results []quota.ScanResult) (limited, near, available int) {
	for _, r := range results {
		switch {
		case r.RateLimited:
			limited++
		case r.NearLimit:
			near++
		default:
			available++
		}
	}
	return
}
