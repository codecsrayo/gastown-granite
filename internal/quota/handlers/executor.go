package handlers

import (
	"context"
	"time"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/quota"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// Executor performs the side-effectful half of a rotation: keychain swap,
// session respawn, state persistence. ExecutorLink delegates to it, then
// publishes one event per result (SessionRespawned / RotationFailed) so
// downstream subscribers (audit, metrics, escalation) see uniform signal
// regardless of which executor implementation ran.
//
// Implementations:
//   - KeychainExecutor (this package): swap + state persist, no respawn.
//     Safe to call from daemon — does not need tmux command-line generation.
//   - cmdRotateExecutor (gt cmd path, future): full swap + respawn pipeline.
//     Lives in cmd because it depends on buildRestartCommand, which still
//     pulls in session/config/cli helpers. Extracting that to a shared
//     package is step 4.
type Executor interface {
	Execute(ctx context.Context, plan *quota.RotatePlan) []quota.RotateResult
}

// ExecutorLink runs RotationPlanned events through an Executor and turns
// each per-session result into the appropriate domain event.
type ExecutorLink struct {
	exec      Executor
	publisher Publisher
	now       func() time.Time
}

// NewExecutorLink wires an Executor to the chain.
func NewExecutorLink(exec Executor, pub Publisher) chain.Link {
	return &ExecutorLink{exec: exec, publisher: pub, now: time.Now}
}

func (*ExecutorLink) Name() string { return "rotation_executor" }

func (l *ExecutorLink) Handle(ctx context.Context, e bus.Event, next chain.Next) error {
	rp, ok := e.(qe.RotationPlanned)
	if !ok {
		return next(e)
	}
	if rp.Plan == nil || len(rp.Plan.Assignments) == 0 || l.exec == nil {
		return next(e)
	}

	results := l.exec.Execute(ctx, rp.Plan)
	at := l.now().UTC()

	for _, r := range results {
		if r.Rotated {
			_ = l.publisher.Publish(ctx, qe.SessionRespawned{
				Session:      r.Session,
				FromAccount:  r.OldAccount,
				ToAccount:    r.NewAccount,
				Resumed:      r.ResumedSession != "",
				KeychainSwap: r.KeychainSwap,
				At:           at,
			})
			continue
		}
		if r.Error != "" {
			_ = l.publisher.Publish(ctx, qe.RotationFailed{
				Session:   r.Session,
				ToAccount: r.NewAccount,
				Reason:    r.Error,
				At:        at,
			})
		}
	}
	return next(e)
}
