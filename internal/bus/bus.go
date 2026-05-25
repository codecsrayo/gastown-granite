// Package bus provides an in-process typed event bus for Gas Town subsystems.
//
// Events are typed Go values (anything implementing Kind() string).
// Subscribers register a handler against a Kind() string. Publish dispatches
// synchronously to every registered handler, fan-out, in registration order.
// Handler errors are collected and joined into one returned error.
//
// This is deliberately small and synchronous: lifecycle is easy to reason
// about, tests don't need fake schedulers, and chain-of-responsibility logic
// runs on the publishing goroutine. Callers that need async dispatch wrap
// their handler in a goroutine; the bus stays predictable.
package bus

import (
	"context"
	"errors"
	"fmt"
	"log"
	"sync"
)

// Event is anything dispatchable on the bus.
// Kind is the routing key — subscribers register against it.
type Event interface {
	Kind() string
}

// Handler reacts to an event. Returning an error does not abort fan-out:
// every subscriber for a Kind runs, errors are joined at the end.
type Handler func(ctx context.Context, e Event) error

// Unsubscribe removes a previously-registered handler.
type Unsubscribe func()

// Bus is a synchronous in-process pub/sub dispatcher.
type Bus struct {
	mu     sync.RWMutex
	subs   map[string][]subscription
	nextID uint64
	logger *log.Logger
}

type subscription struct {
	id uint64
	h  Handler
}

// New returns a bus that logs handler errors via logger when non-nil.
func New(logger *log.Logger) *Bus {
	return &Bus{
		subs:   make(map[string][]subscription),
		logger: logger,
	}
}

// Subscribe registers h against kind. The returned Unsubscribe removes it.
func (b *Bus) Subscribe(kind string, h Handler) Unsubscribe {
	if h == nil {
		return func() {}
	}
	b.mu.Lock()
	b.nextID++
	id := b.nextID
	b.subs[kind] = append(b.subs[kind], subscription{id: id, h: h})
	b.mu.Unlock()
	return func() { b.unsubscribe(kind, id) }
}

func (b *Bus) unsubscribe(kind string, id uint64) {
	b.mu.Lock()
	defer b.mu.Unlock()
	list := b.subs[kind]
	for i, s := range list {
		if s.id == id {
			b.subs[kind] = append(list[:i], list[i+1:]...)
			return
		}
	}
}

// Publish dispatches e to every subscriber of e.Kind() in registration order.
// Errors from individual handlers are joined and returned; one bad handler
// never stops the fan-out.
func (b *Bus) Publish(ctx context.Context, e Event) error {
	if e == nil {
		return errors.New("bus: nil event")
	}
	kind := e.Kind()

	b.mu.RLock()
	list := make([]subscription, len(b.subs[kind]))
	copy(list, b.subs[kind])
	b.mu.RUnlock()

	if len(list) == 0 {
		return nil
	}

	var errs []error
	for _, s := range list {
		if ctx.Err() != nil {
			errs = append(errs, ctx.Err())
			break
		}
		if err := safeInvoke(ctx, s.h, e); err != nil {
			if b.logger != nil {
				b.logger.Printf("bus: handler error for %s: %v", kind, err)
			}
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

// safeInvoke runs h and converts panics into errors so a single panicking
// subscriber cannot crash the publishing goroutine.
func safeInvoke(ctx context.Context, h Handler, e Event) (err error) {
	defer func() {
		if r := recover(); r != nil {
			err = fmt.Errorf("bus: handler panic: %v", r)
		}
	}()
	return h(ctx, e)
}
