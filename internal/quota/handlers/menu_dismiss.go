package handlers

import (
	"context"
	"strings"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// MenuDismisser is the tmux surface MenuDismissLink needs. *tmux.Tmux
// satisfies it. Kept narrow so tests can fake it.
type MenuDismisser interface {
	CapturePane(session string, lines int) (string, error)
	SendKeysRaw(session, keys string) error
}

// dismissCheckLines is how many bottom pane lines we inspect to confirm the
// interactive rate-limit menu is live before sending a key. Mirrors the
// scanner's check window.
const dismissCheckLines = 20

// MenuDismissLink unblocks sessions that are hard rate-limited but have NO
// rotation target (every account is limited, so rotating is pointless). Claude
// Code's rate-limit modal is interactive and blocking; an automated agent
// can't answer it, so the session wedges forever at the menu.
//
// For each limited-but-unassigned session, this link re-captures the pane to
// confirm the modal is live RIGHT NOW, then sends a single Escape — the menu's
// documented "cancel" key. Escape is the only safe key: option 1 is "Upgrade
// your plan" (a paid action), so we never send a numbered selection. Cancelling
// drops Claude out of the modal so it idles until the limit resets, instead of
// sitting frozen on a blocking prompt.
//
// Sessions WITH an assignment are skipped — the rotation respawn (kill +
// respawn) clears their menu anyway.
type MenuDismissLink struct {
	tmux MenuDismisser
}

// NewMenuDismissLink builds the link.
func NewMenuDismissLink(t MenuDismisser) chain.Link {
	return &MenuDismissLink{tmux: t}
}

func (*MenuDismissLink) Name() string { return "menu_dismiss" }

func (l *MenuDismissLink) Handle(_ context.Context, e bus.Event, next chain.Next) error {
	rp, ok := e.(qe.RotationPlanned)
	if !ok || rp.Plan == nil || l.tmux == nil {
		return next(e)
	}

	for _, r := range rp.Plan.LimitedSessions {
		if _, assigned := rp.Plan.Assignments[r.Session]; assigned {
			continue // rotation will respawn it and clear the menu
		}
		if !l.menuIsLive(r.Session) {
			continue // not actually sitting on the modal — don't touch it
		}
		// Escape = "cancel" per the menu's own footer. Never a number.
		_ = l.tmux.SendKeysRaw(r.Session, "Escape")
	}
	return next(e)
}

// menuIsLive re-captures the pane and confirms the interactive rate-limit
// modal is currently displayed. Requires BOTH the upgrade option and the
// wait option so we don't mistake scrollback relics for a live modal.
func (l *MenuDismissLink) menuIsLive(session string) bool {
	content, err := l.tmux.CapturePane(session, dismissCheckLines)
	if err != nil {
		return false
	}
	lower := strings.ToLower(content)
	return strings.Contains(lower, "upgrade your plan") &&
		strings.Contains(lower, "stop and wait for limit to reset")
}
