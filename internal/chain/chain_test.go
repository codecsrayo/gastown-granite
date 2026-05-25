package chain

import (
	"context"
	"errors"
	"testing"

	"github.com/steveyegge/gastown/internal/bus"
)

type stringEvent struct{ s string }

func (stringEvent) Kind() string { return "test.str" }

func TestChainRunsLinksInOrder(t *testing.T) {
	var trace []string
	mk := func(name string) Link {
		return LinkFunc{N: name, Fn: func(_ context.Context, e bus.Event, next Next) error {
			trace = append(trace, name)
			return next(e)
		}}
	}
	c := New(mk("a"), mk("b"), mk("c"))
	if err := c.Run(context.Background(), stringEvent{s: "x"}); err != nil {
		t.Fatalf("run: %v", err)
	}
	want := []string{"a", "b", "c"}
	if len(trace) != len(want) {
		t.Fatalf("trace=%v want %v", trace, want)
	}
	for i, n := range want {
		if trace[i] != n {
			t.Fatalf("step %d = %q want %q", i, trace[i], n)
		}
	}
}

func TestChainShortCircuit(t *testing.T) {
	var trace []string
	c := New(
		LinkFunc{N: "first", Fn: func(_ context.Context, _ bus.Event, _ Next) error {
			trace = append(trace, "first")
			return nil
		}},
		LinkFunc{N: "second", Fn: func(_ context.Context, e bus.Event, next Next) error {
			trace = append(trace, "second")
			return next(e)
		}},
	)
	if err := c.Run(context.Background(), stringEvent{}); err != nil {
		t.Fatal(err)
	}
	if len(trace) != 1 || trace[0] != "first" {
		t.Fatalf("short-circuit failed: %v", trace)
	}
}

func TestChainErrorWrapsLinkName(t *testing.T) {
	boom := errors.New("boom")
	c := New(
		LinkFunc{N: "ok", Fn: func(_ context.Context, e bus.Event, next Next) error { return next(e) }},
		LinkFunc{N: "broken", Fn: func(_ context.Context, _ bus.Event, _ Next) error { return boom }},
	)
	err := c.Run(context.Background(), stringEvent{})
	if err == nil {
		t.Fatal("expected error")
	}
	if !errors.Is(err, boom) {
		t.Fatalf("expected wrap of boom, got %v", err)
	}
}

func TestChainPanicRecovered(t *testing.T) {
	c := New(
		LinkFunc{N: "panics", Fn: func(_ context.Context, _ bus.Event, _ Next) error { panic("nope") }},
	)
	err := c.Run(context.Background(), stringEvent{})
	if err == nil {
		t.Fatal("expected error from panic")
	}
}

func TestChainEnrichEvent(t *testing.T) {
	var saw string
	c := New(
		LinkFunc{N: "enrich", Fn: func(_ context.Context, _ bus.Event, next Next) error {
			return next(stringEvent{s: "enriched"})
		}},
		LinkFunc{N: "sink", Fn: func(_ context.Context, e bus.Event, _ Next) error {
			saw = e.(stringEvent).s
			return nil
		}},
	)
	if err := c.Run(context.Background(), stringEvent{s: "orig"}); err != nil {
		t.Fatal(err)
	}
	if saw != "enriched" {
		t.Fatalf("saw %q want enriched", saw)
	}
}

func TestChainAsHandler(t *testing.T) {
	b := bus.New(nil)
	var hit bool
	c := New(LinkFunc{N: "tap", Fn: func(_ context.Context, _ bus.Event, _ Next) error {
		hit = true
		return nil
	}})
	b.Subscribe("test.str", c.AsHandler())
	if err := b.Publish(context.Background(), stringEvent{}); err != nil {
		t.Fatal(err)
	}
	if !hit {
		t.Fatal("chain not invoked via bus")
	}
}
