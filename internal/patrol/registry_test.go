package patrol

import (
	"context"
	"sync/atomic"
	"testing"
	"time"
)

func TestRegistryRunsEnabledPatrols(t *testing.T) {
	r := New(nil)

	var a, b int32
	r.Add(Patrol{
		Name:     "a",
		Enabled:  true,
		Interval: 5 * time.Millisecond,
		Tick:     func(_ context.Context) { atomic.AddInt32(&a, 1) },
	})
	r.Add(Patrol{
		Name:     "b",
		Enabled:  true,
		Interval: 5 * time.Millisecond,
		Tick:     func(_ context.Context) { atomic.AddInt32(&b, 1) },
	})

	ctx, cancel := context.WithTimeout(context.Background(), 40*time.Millisecond)
	defer cancel()
	r.Run(ctx)

	if atomic.LoadInt32(&a) == 0 || atomic.LoadInt32(&b) == 0 {
		t.Fatalf("expected both patrols to tick at least once, got a=%d b=%d", a, b)
	}
}

func TestRegistrySkipsDisabled(t *testing.T) {
	r := New(nil)
	var hits int32
	r.Add(Patrol{
		Name:     "off",
		Enabled:  false,
		Interval: 1 * time.Millisecond,
		Tick:     func(_ context.Context) { atomic.AddInt32(&hits, 1) },
	})
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	r.Run(ctx)
	if atomic.LoadInt32(&hits) != 0 {
		t.Fatalf("disabled patrol fired %d times", hits)
	}
}

func TestRegistryShouldSkipSuppressesTick(t *testing.T) {
	r := New(nil)
	var ran int32
	var skip int32 = 1 // start in "skip" state
	r.Add(Patrol{
		Name:       "gated",
		Enabled:    true,
		Interval:   2 * time.Millisecond,
		ShouldSkip: func() bool { return atomic.LoadInt32(&skip) == 1 },
		Tick:       func(_ context.Context) { atomic.AddInt32(&ran, 1) },
	})

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer func() {
		atomic.StoreInt32(&skip, 0) // unblock — but ctx will already be done
		cancel()
	}()
	r.Run(ctx)

	if atomic.LoadInt32(&ran) != 0 {
		t.Fatalf("patrol ran %d times despite ShouldSkip=true", ran)
	}
}

func TestRegistryPanicDoesNotCrash(t *testing.T) {
	r := New(nil)
	var afterPanic int32
	r.Add(Patrol{
		Name:     "panicky",
		Enabled:  true,
		Interval: 2 * time.Millisecond,
		Tick: func(_ context.Context) {
			atomic.AddInt32(&afterPanic, 1)
			if afterPanic == 1 {
				panic("first tick")
			}
		},
	})
	ctx, cancel := context.WithTimeout(context.Background(), 40*time.Millisecond)
	defer cancel()
	r.Run(ctx)

	if atomic.LoadInt32(&afterPanic) < 2 {
		t.Fatalf("expected goroutine to continue after panic, hits=%d", afterPanic)
	}
}

func TestRegistryAddAfterRunIgnored(t *testing.T) {
	r := New(nil)
	r.Add(Patrol{Name: "a", Enabled: true, Interval: 50 * time.Millisecond, Tick: func(_ context.Context) {}})

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		r.Run(ctx)
		close(done)
	}()
	// Give Run time to flip started=true.
	time.Sleep(5 * time.Millisecond)

	var late int32
	r.Add(Patrol{
		Name:     "late",
		Enabled:  true,
		Interval: 1 * time.Millisecond,
		Tick:     func(_ context.Context) { atomic.AddInt32(&late, 1) },
	})
	time.Sleep(10 * time.Millisecond)
	cancel()
	<-done

	if atomic.LoadInt32(&late) != 0 {
		t.Fatalf("Add-after-Run started a patrol: %d hits", late)
	}
}
