// Package chain implements the Chain of Responsibility pattern over bus events.
//
// A Chain is an ordered list of Links. Run invokes the first Link with a
// next callback that advances to the following Link. Each Link decides
// whether to short-circuit (skip calling next), abort with an error, or
// pass control along. Links may also mutate the event before forwarding
// — the chain passes the same Event interface value through, so links
// that want to enrich an event should return a new typed value via next.
//
// Chains compose naturally with bus.Bus: a Chain can be exposed as a
// bus.Handler so a single subscription drives a multi-step pipeline.
package chain

import (
	"context"
	"errors"
	"fmt"

	"github.com/steveyegge/gastown/internal/bus"
)

// Next advances the chain. Links call next(e) to invoke the following link.
// Passing a different event value lets links enrich/replace the payload.
// Not calling next short-circuits the chain (subsequent links don't run).
type Next func(bus.Event) error

// Link is one step in a Chain.
type Link interface {
	Name() string
	Handle(ctx context.Context, e bus.Event, next Next) error
}

// LinkFunc adapts a function into a Link with the given name.
type LinkFunc struct {
	N  string
	Fn func(ctx context.Context, e bus.Event, next Next) error
}

func (l LinkFunc) Name() string { return l.N }
func (l LinkFunc) Handle(ctx context.Context, e bus.Event, next Next) error {
	return l.Fn(ctx, e, next)
}

// Chain executes Links in order.
type Chain struct {
	links []Link
}

// New builds a Chain. Order matters: the first link receives the event first.
func New(links ...Link) *Chain {
	cp := make([]Link, len(links))
	copy(cp, links)
	return &Chain{links: cp}
}

// Links returns the registered links (read-only view).
func (c *Chain) Links() []Link { return c.links }

// Run invokes the chain. Returns the joined error of any link.
func (c *Chain) Run(ctx context.Context, e bus.Event) error {
	if len(c.links) == 0 {
		return nil
	}
	var errs []error
	var step func(idx int, evt bus.Event) error
	step = func(idx int, evt bus.Event) error {
		if idx >= len(c.links) {
			return nil
		}
		if ctx.Err() != nil {
			return ctx.Err()
		}
		link := c.links[idx]
		err := safeRun(ctx, link, evt, func(next bus.Event) error {
			return step(idx+1, next)
		})
		if err != nil {
			errs = append(errs, fmt.Errorf("%s: %w", link.Name(), err))
		}
		return err
	}
	_ = step(0, e)
	return errors.Join(errs...)
}

// AsHandler returns a bus.Handler that runs the chain for each event.
func (c *Chain) AsHandler() bus.Handler {
	return func(ctx context.Context, e bus.Event) error {
		return c.Run(ctx, e)
	}
}

func safeRun(ctx context.Context, link Link, e bus.Event, next Next) (err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("panic in %s: %v", link.Name(), r)
		}
	}()
	return link.Handle(ctx, e, next)
}
