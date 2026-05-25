// Package patrol runs a set of periodic patrols off independent tickers.
//
// Each Patrol has a name, an enable flag, an interval, and a Tick function.
// The Registry spawns one goroutine per enabled patrol; on every tick the
// goroutine invokes Tick with a context derived from the Run context.
// Patrols are skipped when the shutdown flag (provided by the caller) is
// set, so the daemon's existing in-flight protection still applies.
//
// This is intentionally minimal: no fanout, no scheduling priorities, no
// retries. It replaces ~200 lines of boilerplate (declare ticker, defer
// stop, add select case) in daemon.go with a flat table.
package patrol

import (
	"context"
	"log"
	"sync"
	"time"
)

// Tick is the per-cycle function a patrol runs. It is invoked on the
// patrol's own goroutine; context is the Run context.
type Tick func(ctx context.Context)

// Patrol is one entry in the registry.
type Patrol struct {
	// Name is for logging only.
	Name string

	// Enabled gates whether the patrol's goroutine starts at all.
	Enabled bool

	// Interval is how often Tick fires. Zero or negative disables.
	Interval time.Duration

	// Tick runs on every interval. Must respect ctx cancellation.
	Tick Tick

	// ShouldSkip is an optional gate evaluated at the top of every tick.
	// Returning true skips this tick (without stopping the goroutine).
	// Used by the daemon to suppress patrols during shutdown.
	ShouldSkip func() bool
}

// Registry owns the patrol goroutines and their tickers.
type Registry struct {
	logger  *log.Logger
	patrols []Patrol

	mu      sync.Mutex
	started bool
	wg      sync.WaitGroup
}

// New returns an empty registry. Patrols are added via Add, then Run.
func New(logger *log.Logger) *Registry {
	return &Registry{logger: logger}
}

// Add registers a patrol. Calling Add after Run is a no-op.
func (r *Registry) Add(p Patrol) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.started {
		if r.logger != nil {
			r.logger.Printf("patrol: Add(%q) ignored — registry already running", p.Name)
		}
		return
	}
	r.patrols = append(r.patrols, p)
}

// Run starts one goroutine per enabled patrol and blocks until ctx is
// cancelled. All goroutines exit on context cancellation, then Run returns.
func (r *Registry) Run(ctx context.Context) {
	r.mu.Lock()
	r.started = true
	patrols := r.patrols
	r.mu.Unlock()

	for _, p := range patrols {
		if !p.Enabled || p.Interval <= 0 || p.Tick == nil {
			continue
		}
		r.wg.Add(1)
		go r.runOne(ctx, p)
	}
	r.wg.Wait()
}

// runOne is the per-patrol loop.
func (r *Registry) runOne(ctx context.Context, p Patrol) {
	defer r.wg.Done()

	t := time.NewTicker(p.Interval)
	defer t.Stop()

	if r.logger != nil {
		r.logger.Printf("patrol: %s started (interval %v)", p.Name, p.Interval)
	}

	for {
		select {
		case <-ctx.Done():
			if r.logger != nil {
				r.logger.Printf("patrol: %s stopping (ctx done)", p.Name)
			}
			return
		case <-t.C:
			if p.ShouldSkip != nil && p.ShouldSkip() {
				continue
			}
			r.safeTick(ctx, p)
		}
	}
}

// safeTick runs Tick with panic recovery so one bad patrol can't crash
// the daemon. Panics are logged and the goroutine continues on the next
// interval.
func (r *Registry) safeTick(ctx context.Context, p Patrol) {
	defer func() {
		if rec := recover(); rec != nil && r.logger != nil {
			r.logger.Printf("patrol: %s panic recovered: %v", p.Name, rec)
		}
	}()
	p.Tick(ctx)
}
