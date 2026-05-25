package handlers

import (
	"context"
	"testing"

	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/quota"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

type fakeDismisser struct {
	panes    map[string]string
	sentKeys map[string][]string
}

func newFakeDismisser() *fakeDismisser {
	return &fakeDismisser{panes: map[string]string{}, sentKeys: map[string][]string{}}
}
func (f *fakeDismisser) CapturePane(session string, _ int) (string, error) {
	return f.panes[session], nil
}
func (f *fakeDismisser) SendKeysRaw(session, keys string) error {
	f.sentKeys[session] = append(f.sentKeys[session], keys)
	return nil
}

const liveMenu = `What do you want to do?
  1. Upgrade your plan
  2. Upgrade to Team plan
  3. Stop and wait for limit to reset`

func TestMenuDismissEscapesUnassignedWedgedSession(t *testing.T) {
	f := newFakeDismisser()
	f.panes["pl-witness"] = liveMenu

	c := chain.New(NewMenuDismissLink(f))
	plan := &quota.RotatePlan{
		LimitedSessions: []quota.ScanResult{{Session: "pl-witness", RateLimited: true}},
		Assignments:     map[string]string{}, // no rotation target
	}
	if err := c.Run(context.Background(), qe.RotationPlanned{Plan: plan}); err != nil {
		t.Fatal(err)
	}
	keys := f.sentKeys["pl-witness"]
	if len(keys) != 1 || keys[0] != "Escape" {
		t.Fatalf("expected single Escape, got %v", keys)
	}
}

func TestMenuDismissSkipsAssignedSession(t *testing.T) {
	f := newFakeDismisser()
	f.panes["pl-witness"] = liveMenu

	c := chain.New(NewMenuDismissLink(f))
	plan := &quota.RotatePlan{
		LimitedSessions: []quota.ScanResult{{Session: "pl-witness", RateLimited: true}},
		Assignments:     map[string]string{"pl-witness": "beta"}, // will be rotated
	}
	if err := c.Run(context.Background(), qe.RotationPlanned{Plan: plan}); err != nil {
		t.Fatal(err)
	}
	if len(f.sentKeys["pl-witness"]) != 0 {
		t.Fatalf("assigned session should not be dismissed, got %v", f.sentKeys["pl-witness"])
	}
}

func TestMenuDismissSkipsWhenMenuNotLive(t *testing.T) {
	f := newFakeDismisser()
	// Pane shows old limit text but NOT the live interactive modal.
	f.panes["pl-witness"] = "You've hit your limit · resets 7pm\n(agent kept working)"

	c := chain.New(NewMenuDismissLink(f))
	plan := &quota.RotatePlan{
		LimitedSessions: []quota.ScanResult{{Session: "pl-witness", RateLimited: true}},
		Assignments:     map[string]string{},
	}
	if err := c.Run(context.Background(), qe.RotationPlanned{Plan: plan}); err != nil {
		t.Fatal(err)
	}
	if len(f.sentKeys["pl-witness"]) != 0 {
		t.Fatalf("should not send keys when modal not live, got %v", f.sentKeys["pl-witness"])
	}
}

func TestMenuDismissNeverSendsNumberedOption(t *testing.T) {
	f := newFakeDismisser()
	f.panes["s"] = liveMenu
	c := chain.New(NewMenuDismissLink(f))
	plan := &quota.RotatePlan{
		LimitedSessions: []quota.ScanResult{{Session: "s", RateLimited: true}},
		Assignments:     map[string]string{},
	}
	_ = c.Run(context.Background(), qe.RotationPlanned{Plan: plan})
	for _, k := range f.sentKeys["s"] {
		if k == "1" || k == "2" || k == "3" || k == "Enter" {
			t.Fatalf("sent unsafe key %q — could trigger paid upgrade", k)
		}
	}
}
