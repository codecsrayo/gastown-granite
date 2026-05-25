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

// CooldownLink clears accounts whose ResetsAt window has elapsed. It runs
// against a ScanCompleted event so reactivation happens once per scan cycle
// rather than per session.
//
// Side effects: holds the quota lock, persists the cleared status, and
// publishes one AccountCleared event per handle on the supplied Publisher.
type CooldownLink struct {
	mgr       *quota.Manager
	publisher Publisher
	now       func() time.Time
}

// NewCooldownLink returns a Link that clears expired limited accounts.
func NewCooldownLink(mgr *quota.Manager, pub Publisher) chain.Link {
	return &CooldownLink{mgr: mgr, publisher: pub, now: time.Now}
}

func (*CooldownLink) Name() string { return "cooldown_clear" }

func (c *CooldownLink) Handle(ctx context.Context, e bus.Event, next chain.Next) error {
	if _, ok := e.(qe.ScanCompleted); !ok {
		return next(e)
	}

	var cleared []clearedRec
	err := c.mgr.WithLock(func() error {
		state, err := c.mgr.Load()
		if err != nil {
			return err
		}
		// Snapshot ResetsAt per handle before clearing so AccountCleared can
		// carry the previous value for audit.
		prev := make(map[string]string, len(state.Accounts))
		for h, s := range state.Accounts {
			if s.Status == config.QuotaStatusLimited {
				prev[h] = s.ResetsAt
			}
		}
		handles := c.mgr.ClearExpired(state)
		if len(handles) == 0 {
			return nil
		}
		for _, h := range handles {
			cleared = append(cleared, clearedRec{handle: h, prevResetsAt: prev[h]})
		}
		return c.mgr.SaveUnlocked(state)
	})
	if err != nil {
		return err
	}

	at := c.now().UTC()
	for _, r := range cleared {
		_ = c.publisher.Publish(ctx, qe.AccountCleared{
			Account:          r.handle,
			PreviousResetsAt: r.prevResetsAt,
			At:               at,
		})
	}
	return next(e)
}

type clearedRec struct {
	handle       string
	prevResetsAt string
}
