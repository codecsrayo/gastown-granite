package handlers

import (
	"context"
	"testing"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/quota"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

type fakeExecutor struct {
	results []quota.RotateResult
	called  bool
}

func (f *fakeExecutor) Execute(_ context.Context, _ *quota.RotatePlan) []quota.RotateResult {
	f.called = true
	return f.results
}

func TestExecutorLinkPublishesPerResultEvents(t *testing.T) {
	pub := &recordingPublisher{}
	exec := &fakeExecutor{
		results: []quota.RotateResult{
			{Session: "rig-a", OldAccount: "alpha", NewAccount: "beta", Rotated: true, KeychainSwap: true, ResumedSession: "continue"},
			{Session: "rig-b", OldAccount: "alpha", NewAccount: "beta", Error: "swap failed"},
		},
	}
	link := NewExecutorLink(exec, pub)
	c := chain.New(link)

	plan := &quota.RotatePlan{
		Assignments: map[string]string{"rig-a": "beta", "rig-b": "beta"},
	}
	if err := c.Run(context.Background(), qe.RotationPlanned{Plan: plan}); err != nil {
		t.Fatalf("run: %v", err)
	}

	if !exec.called {
		t.Fatal("executor not invoked")
	}
	if got := len(pub.events); got != 2 {
		t.Fatalf("published %d events, want 2", got)
	}
	respawn, ok := pub.events[0].(qe.SessionRespawned)
	if !ok {
		t.Fatalf("event 0 = %T, want SessionRespawned", pub.events[0])
	}
	if respawn.Session != "rig-a" || !respawn.Resumed || !respawn.KeychainSwap {
		t.Fatalf("respawn fields wrong: %+v", respawn)
	}
	failed, ok := pub.events[1].(qe.RotationFailed)
	if !ok {
		t.Fatalf("event 1 = %T, want RotationFailed", pub.events[1])
	}
	if failed.Session != "rig-b" || failed.Reason != "swap failed" {
		t.Fatalf("failed fields wrong: %+v", failed)
	}
}

func TestExecutorLinkSkipsEmptyPlan(t *testing.T) {
	pub := &recordingPublisher{}
	exec := &fakeExecutor{}
	link := NewExecutorLink(exec, pub)
	c := chain.New(link)

	if err := c.Run(context.Background(), qe.RotationPlanned{Plan: nil}); err != nil {
		t.Fatal(err)
	}
	if exec.called {
		t.Fatal("executor should not run on nil plan")
	}
}

func TestExecutorLinkPassesThroughNonPlanEvents(t *testing.T) {
	pub := &recordingPublisher{}
	exec := &fakeExecutor{}
	link := NewExecutorLink(exec, pub)
	c := chain.New(link)

	if err := c.Run(context.Background(), qe.AccountCleared{Account: "x"}); err != nil {
		t.Fatal(err)
	}
	if exec.called {
		t.Fatal("executor should not run on AccountCleared")
	}
	if len(pub.events) != 0 {
		t.Fatalf("expected no published events, got %v", pub.events)
	}
}

func TestExecutorLinkOnBusViaOrchestratorSubscription(t *testing.T) {
	// Smoke test: chain.AsHandler integrates with bus subscription
	// (mirrors what orchestrator.New does for KindRotationPlanned).
	b := bus.New(nil)
	exec := &fakeExecutor{
		results: []quota.RotateResult{
			{Session: "rig-a", NewAccount: "beta", Rotated: true, KeychainSwap: true},
		},
	}
	link := NewExecutorLink(exec, b)
	c := chain.New(link)
	b.Subscribe(qe.KindRotationPlanned, c.AsHandler())

	var got bus.Event
	b.Subscribe(qe.KindSessionRespawned, func(_ context.Context, e bus.Event) error {
		got = e
		return nil
	})

	plan := &quota.RotatePlan{Assignments: map[string]string{"rig-a": "beta"}}
	if err := b.Publish(context.Background(), qe.RotationPlanned{Plan: plan}); err != nil {
		t.Fatal(err)
	}
	if got == nil {
		t.Fatal("expected SessionRespawned to be published")
	}
	if _, ok := got.(qe.SessionRespawned); !ok {
		t.Fatalf("got %T, want SessionRespawned", got)
	}
}

