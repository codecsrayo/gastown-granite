// Package handlers contains the chain-of-responsibility links and bus
// subscribers that implement the quota rotation pipeline.
//
// Each link or subscriber is small enough to test in isolation. Composition
// happens in the orchestrator (see quota/orchestrator package), which wires
// the chain into the bus.
package handlers

import (
	"context"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/events"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// AuditEmitter mirrors quota domain events into the JSONL audit log so
// existing dashboards keep working unchanged. It is a terminal Link: it
// calls next so the chain continues but never short-circuits.
type AuditEmitter struct{}

// NewAuditEmitter returns a Link that emits one events.LogFeed call per
// recognized quota event type.
func NewAuditEmitter() chain.Link { return AuditEmitter{} }

func (AuditEmitter) Name() string { return "audit_emit" }

func (a AuditEmitter) Handle(_ context.Context, e bus.Event, next chain.Next) error {
	a.Emit(e)
	return next(e)
}

// Emit writes a single audit row for the event. Safe to call standalone
// when the caller is not using a chain.
func (AuditEmitter) Emit(e bus.Event) {
	switch ev := e.(type) {
	case qe.ScanCompleted:
		_ = events.LogFeed(events.TypeQuotaScanned, "quota",
			events.QuotaScannedPayload(ev.Total, ev.Limited, ev.NearLimit, ev.Available))
	case qe.SessionLimitedDetected:
		_ = events.LogFeed(events.TypeQuotaLimited, "quota",
			events.QuotaLimitedPayload(ev.Session, ev.Account, ev.ResetsAt))
	case qe.SessionNearLimitDetected:
		_ = events.LogFeed(events.TypeQuotaNearLimit, "quota",
			events.QuotaNearLimitPayload(ev.Session, ev.Account, ev.MatchedLine))
	case qe.TokenExpiredDetected:
		_ = events.LogFeed(events.TypeQuotaTokenExpired, "quota",
			events.QuotaTokenExpiredPayload(ev.Account, ev.ExpiresAt, ev.Reason))
	case qe.AccountReactivated:
		_ = events.LogFeed(events.TypeQuotaReactivated, "quota",
			events.QuotaReactivatedPayload(ev.Account, ev.PreviousResetsAt))
	case qe.AccountCleared:
		_ = events.LogFeed(events.TypeQuotaCleared, "quota",
			events.QuotaClearedPayload(ev.Account, ev.PreviousResetsAt))
	case qe.SessionRespawned:
		_ = events.LogFeed(events.TypeQuotaRotated, "quota",
			events.QuotaRotatedPayload(ev.Session, ev.FromAccount, ev.ToAccount, ev.Resumed, ev.KeychainSwap))
	case qe.RotationFailed:
		_ = events.LogFeed(events.TypeQuotaSwapFailed, "quota",
			events.QuotaSwapFailedPayload(ev.Session, ev.ToAccount, ev.Reason))
	case qe.RotationPlanned:
		if ev.Plan != nil && len(ev.Plan.Assignments) > 0 {
			_ = events.LogFeed(events.TypeQuotaAssigned, "quota",
				events.QuotaAssignedPayload(ev.Plan.Assignments, ev.Plan.AvailableAccounts))
		}
	}
}
