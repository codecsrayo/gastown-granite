package handlers

import (
	"context"
	"time"

	"github.com/steveyegge/gastown/internal/bus"
	"github.com/steveyegge/gastown/internal/chain"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
)

// SnapshotLink persists the LimitedSessions snapshot to quota.json on
// every ScanCompleted. The dashboard reads this snapshot to render the
// "waiting for unlock" panel without re-scanning tmux.
type SnapshotLink struct {
	mgr *quota.Manager
	now func() time.Time
}

// NewSnapshotLink returns a Link that overwrites the LimitedSessions
// snapshot from the latest scan results.
func NewSnapshotLink(mgr *quota.Manager) chain.Link {
	return &SnapshotLink{mgr: mgr, now: time.Now}
}

func (*SnapshotLink) Name() string { return "snapshot_persist" }

func (s *SnapshotLink) Handle(_ context.Context, e bus.Event, next chain.Next) error {
	sc, ok := e.(qe.ScanCompleted)
	if !ok {
		return next(e)
	}
	if len(sc.Results) == 0 {
		return next(e)
	}

	at := s.now().UTC().Format(time.RFC3339)
	sessions := make(map[string]config.LimitedSessionState)
	for _, r := range sc.Results {
		if !r.RateLimited && !r.NearLimit {
			continue
		}
		sessions[r.Session] = config.LimitedSessionState{
			Account:     r.AccountHandle,
			ConfigDir:   r.ConfigDir,
			RateLimited: r.RateLimited,
			NearLimit:   r.NearLimit,
			ResetsAt:    r.ResetsAt,
			MatchedLine: r.MatchedLine,
			DetectedAt:  at,
		}
	}

	_ = s.mgr.WithLock(func() error {
		state, err := s.mgr.Load()
		if err != nil {
			return err
		}
		quota.RecordLimitedSessions(state, sessions)
		return s.mgr.SaveUnlocked(state)
	})
	return next(e)
}
