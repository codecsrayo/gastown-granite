package handlers

import (
	"context"
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/quota"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

type recordingPublisher struct {
	events []bus.Event
}

func (r *recordingPublisher) Publish(_ context.Context, e bus.Event) error {
	r.events = append(r.events, e)
	return nil
}

func TestFanoutEmitsPerSessionEvents(t *testing.T) {
	pub := &recordingPublisher{}
	link := NewFanoutLink(pub).(*FanoutLink)
	link.now = func() time.Time { return time.Unix(1700000000, 0).UTC() }

	c := chain.New(link)
	evt := qe.ScanCompleted{
		Total:     3,
		Limited:   1,
		NearLimit: 1,
		Available: 1,
		Results: []quota.ScanResult{
			{Session: "rig-a", AccountHandle: "alpha", RateLimited: true, ResetsAt: "7pm"},
			{Session: "rig-b", AccountHandle: "beta", NearLimit: true, MatchedLine: "approaching"},
			{Session: "rig-c", AccountHandle: "gamma"},
		},
	}
	if err := c.Run(context.Background(), evt); err != nil {
		t.Fatalf("run: %v", err)
	}

	if got := len(pub.events); got != 2 {
		t.Fatalf("published %d events, want 2 (1 limited + 1 near)", got)
	}
	if _, ok := pub.events[0].(qe.SessionLimitedDetected); !ok {
		t.Fatalf("event 0 = %T, want SessionLimitedDetected", pub.events[0])
	}
	if _, ok := pub.events[1].(qe.SessionNearLimitDetected); !ok {
		t.Fatalf("event 1 = %T, want SessionNearLimitDetected", pub.events[1])
	}
}

func TestFanoutPassesThroughNonScan(t *testing.T) {
	pub := &recordingPublisher{}
	link := NewFanoutLink(pub)
	c := chain.New(link)
	if err := c.Run(context.Background(), qe.AccountCleared{Account: "x"}); err != nil {
		t.Fatalf("run: %v", err)
	}
	if len(pub.events) != 0 {
		t.Fatalf("expected no fanout on non-scan event, got %v", pub.events)
	}
}
