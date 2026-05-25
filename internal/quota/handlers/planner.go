package handlers

import (
	"context"
	"time"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// PlannerLink consumes a ScanCompleted event and, when at least one session
// is rate-limited, computes a rotation plan and publishes RotationPlanned.
//
// PlannerLink delegates the heavy lifting to quota.PlanRotation. That keeps
// the migration incremental — the chain composes existing pieces today,
// future links can split planning further (e.g. PolicyLink, CapLink) by
// inserting between PlannerLink and the executor.
type PlannerLink struct {
	scanner   *quota.Scanner
	mgr       *quota.Manager
	acctCfg   *config.AccountsConfig
	publisher Publisher
	now       func() time.Time
}

// NewPlannerLink builds a planner Link.
func NewPlannerLink(scanner *quota.Scanner, mgr *quota.Manager, acctCfg *config.AccountsConfig, pub Publisher) chain.Link {
	return &PlannerLink{
		scanner:   scanner,
		mgr:       mgr,
		acctCfg:   acctCfg,
		publisher: pub,
		now:       time.Now,
	}
}

func (*PlannerLink) Name() string { return "rotation_planner" }

func (p *PlannerLink) Handle(ctx context.Context, e bus.Event, next chain.Next) error {
	sc, ok := e.(qe.ScanCompleted)
	if !ok {
		return next(e)
	}
	if sc.Limited == 0 {
		return next(e)
	}

	plan, err := quota.PlanRotation(p.scanner, p.mgr, p.acctCfg, quota.PlanOpts{})
	if err != nil || plan == nil {
		return next(e)
	}
	// Publish whenever there are limited sessions — even with zero
	// assignments. The executor no-ops on an empty assignment set, but the
	// menu-dismiss link still needs the event to un-wedge limited sessions
	// that have no rotation target (all accounts limited).
	if len(plan.Assignments) == 0 && len(plan.LimitedSessions) == 0 {
		return next(e)
	}

	_ = p.publisher.Publish(ctx, qe.RotationPlanned{
		Plan: plan,
		At:   p.now().UTC(),
	})
	return next(e)
}
