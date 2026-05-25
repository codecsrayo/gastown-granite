package bus

import (
	"context"
	"errors"
	"sync/atomic"
	"testing"
)

type pingEvent struct{ payload string }

func (pingEvent) Kind() string { return "test.ping" }

type pongEvent struct{}

func (pongEvent) Kind() string { return "test.pong" }

func TestBusPublishDispatchesToSubscribers(t *testing.T) {
	b := New(nil)
	var hits int32
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error {
		atomic.AddInt32(&hits, 1)
		return nil
	})
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error {
		atomic.AddInt32(&hits, 1)
		return nil
	})
	b.Subscribe("test.pong", func(_ context.Context, _ Event) error {
		atomic.AddInt32(&hits, 100)
		return nil
	})

	if err := b.Publish(context.Background(), pingEvent{}); err != nil {
		t.Fatalf("publish: %v", err)
	}
	if got := atomic.LoadInt32(&hits); got != 2 {
		t.Fatalf("ping hits = %d, want 2", got)
	}
}

func TestBusPublishJoinsErrors(t *testing.T) {
	b := New(nil)
	e1 := errors.New("one")
	e2 := errors.New("two")
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error { return e1 })
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error { return e2 })
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error { return nil })

	err := b.Publish(context.Background(), pingEvent{})
	if err == nil {
		t.Fatal("expected joined error")
	}
	if !errors.Is(err, e1) || !errors.Is(err, e2) {
		t.Fatalf("expected joined error to wrap both, got %v", err)
	}
}

func TestBusPublishRecoversPanics(t *testing.T) {
	b := New(nil)
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error { panic("boom") })
	var reached bool
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error {
		reached = true
		return nil
	})
	if err := b.Publish(context.Background(), pingEvent{}); err == nil {
		t.Fatal("expected panic error")
	}
	if !reached {
		t.Fatal("fan-out aborted by panic")
	}
}

func TestBusUnsubscribe(t *testing.T) {
	b := New(nil)
	var hits int32
	un := b.Subscribe("test.ping", func(_ context.Context, _ Event) error {
		atomic.AddInt32(&hits, 1)
		return nil
	})
	if err := b.Publish(context.Background(), pingEvent{}); err != nil {
		t.Fatal(err)
	}
	un()
	if err := b.Publish(context.Background(), pingEvent{}); err != nil {
		t.Fatal(err)
	}
	if got := atomic.LoadInt32(&hits); got != 1 {
		t.Fatalf("hits = %d, want 1 after unsubscribe", got)
	}
}

func TestBusPublishNoSubscribers(t *testing.T) {
	b := New(nil)
	if err := b.Publish(context.Background(), pingEvent{}); err != nil {
		t.Fatalf("publish to empty kind: %v", err)
	}
}

func TestBusContextCancelStopsFanout(t *testing.T) {
	b := New(nil)
	var hits int32
	ctx, cancel := context.WithCancel(context.Background())
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error {
		atomic.AddInt32(&hits, 1)
		cancel()
		return nil
	})
	b.Subscribe("test.ping", func(_ context.Context, _ Event) error {
		atomic.AddInt32(&hits, 1)
		return nil
	})
	_ = b.Publish(ctx, pingEvent{})
	if got := atomic.LoadInt32(&hits); got != 1 {
		t.Fatalf("hits = %d, want 1 (second handler should be skipped)", got)
	}
}
