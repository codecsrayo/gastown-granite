package quota

import (
	"reflect"
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/config"
)

func TestApplyTokenExpiries_UpdatesAccountsInPlace(t *testing.T) {
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work":     {Status: config.QuotaStatusAvailable, LastUsed: "x"},
		"personal": {Status: config.QuotaStatusLimited, LimitedAt: "y"},
	}}
	ApplyTokenExpiries(state, map[string]string{
		"work":     "2026-06-01T00:00:00Z",
		"personal": "2026-05-15T00:00:00Z",
		"ghost":    "2026-07-01T00:00:00Z", // unknown handle — should not be inserted
	})

	if got := state.Accounts["work"].TokenExpiresAt; got != "2026-06-01T00:00:00Z" {
		t.Errorf("work expiry = %q", got)
	}
	if got := state.Accounts["work"].LastUsed; got != "x" {
		t.Errorf("LastUsed must be preserved, got %q", got)
	}
	if state.Accounts["work"].TokenLastChecked == "" {
		t.Errorf("TokenLastChecked must be set")
	}
	if _, exists := state.Accounts["ghost"]; exists {
		t.Errorf("unknown handle %q should have been inserted as ghost record", "ghost")
	}
}

func TestRecordLimitedSessions_OverwritesEachCall(t *testing.T) {
	state := &config.QuotaState{}
	RecordLimitedSessions(state, map[string]config.LimitedSessionState{
		"gt-a": {Account: "work", RateLimited: true},
	})
	if len(state.LimitedSessions) != 1 {
		t.Fatalf("expected 1 session, got %d", len(state.LimitedSessions))
	}
	RecordLimitedSessions(state, map[string]config.LimitedSessionState{
		"gt-b": {Account: "personal", NearLimit: true},
	})
	if _, ok := state.LimitedSessions["gt-a"]; ok {
		t.Errorf("previous session gt-a should have been cleared")
	}
	if _, ok := state.LimitedSessions["gt-b"]; !ok {
		t.Errorf("new session gt-b missing")
	}

	// Empty map clears the snapshot — confirms we never persist stale entries.
	RecordLimitedSessions(state, map[string]config.LimitedSessionState{})
	if state.LimitedSessions != nil {
		t.Errorf("empty input should clear LimitedSessions, got %v", state.LimitedSessions)
	}
}

func TestRecordRotation_BumpsCounterAndStampsTimes(t *testing.T) {
	now := time.Date(2026, 5, 24, 9, 30, 0, 0, time.UTC)
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work": {Status: config.QuotaStatusAvailable, RotationCount: 2},
	}}
	RecordRotation(state, "work", now)

	acct := state.Accounts["work"]
	if acct.RotationCount != 3 {
		t.Errorf("RotationCount = %d, want 3", acct.RotationCount)
	}
	want := now.UTC().Format(time.RFC3339)
	if acct.LastRotatedAt != want || acct.LastUsed != want {
		t.Errorf("timestamps = %q / %q, want %q", acct.LastRotatedAt, acct.LastUsed, want)
	}
}

func TestClearExpired_PreservesObservabilityFields(t *testing.T) {
	la, _ := time.LoadLocation("America/Los_Angeles")
	now := time.Date(2026, 5, 24, 15, 0, 0, 0, la)

	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work": {
			Status:           config.QuotaStatusLimited,
			ResetsAt:         "11am (America/Los_Angeles)", // already past
			LastUsed:         "u",
			TokenExpiresAt:   "2026-06-01T00:00:00Z",
			TokenLastChecked: "checked",
			RotationCount:    7,
			LastRotatedAt:    "rotated",
		},
	}}
	cleared := clearExpiredAt(nil, state, now)
	if !reflect.DeepEqual(cleared, []string{"work"}) {
		t.Fatalf("cleared = %v", cleared)
	}
	acct := state.Accounts["work"]
	if acct.Status != config.QuotaStatusAvailable {
		t.Errorf("status = %s, want available", acct.Status)
	}
	if acct.TokenExpiresAt != "2026-06-01T00:00:00Z" || acct.RotationCount != 7 || acct.LastRotatedAt != "rotated" {
		t.Errorf("observability fields lost: %+v", acct)
	}
}
