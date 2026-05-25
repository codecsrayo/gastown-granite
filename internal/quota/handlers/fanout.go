package handlers

import (
	"context"
	"time"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// FanoutLink converts a ScanCompleted event into one SessionLimitedDetected
// or SessionNearLimitDetected per affected session and publishes them on the
// bus. Downstream handlers (planner, executor) subscribe to those finer
// events rather than re-walking the scan results.
//
// FanoutLink calls next first so any pre-fanout link (e.g. token refresh)
// still observes the raw scan summary before per-session events fire.
type FanoutLink struct {
	publisher Publisher
	now       func() time.Time
}

// Publisher is the subset of bus.Bus used by FanoutLink. Decoupling it lets
// tests assert published events without spinning up a real Bus.
type Publisher interface {
	Publish(ctx context.Context, e bus.Event) error
}

// NewFanoutLink returns a Link that fans ScanCompleted out into per-session
// events on the given publisher.
func NewFanoutLink(pub Publisher) chain.Link {
	return &FanoutLink{publisher: pub, now: time.Now}
}

func (*FanoutLink) Name() string { return "scan_fanout" }

func (f *FanoutLink) Handle(ctx context.Context, e bus.Event, next chain.Next) error {
	if err := next(e); err != nil {
		return err
	}
	sc, ok := e.(qe.ScanCompleted)
	if !ok {
		return nil
	}
	at := f.now().UTC()
	for _, r := range sc.Results {
		switch {
		case r.RateLimited:
			_ = f.publisher.Publish(ctx, qe.SessionLimitedDetected{
				Session:     r.Session,
				Account:     r.AccountHandle,
				ConfigDir:   r.ConfigDir,
				MatchedLine: r.MatchedLine,
				ResetsAt:    r.ResetsAt,
				At:          at,
			})
		case r.NearLimit:
			_ = f.publisher.Publish(ctx, qe.SessionNearLimitDetected{
				Session:     r.Session,
				Account:     r.AccountHandle,
				ConfigDir:   r.ConfigDir,
				MatchedLine: r.MatchedLine,
				At:          at,
			})
		}
	}
	return nil
}
