package web

import (
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/quota"
)

func TestBuildQuotaSummary_CountsByStatusAndDetectsExpiredViaToken(t *testing.T) {
	now := time.Date(2026, 5, 24, 12, 0, 0, 0, time.UTC)
	acctCfg := &config.AccountsConfig{
		Default: "work",
		Accounts: map[string]config.Account{
			"work":     {Email: "work@example.com", ConfigDir: "/tmp/work"},
			"personal": {Email: "personal@example.com", ConfigDir: "/tmp/personal"},
			"team":     {Email: "team@example.com", ConfigDir: "/tmp/team"},
			"old":      {Email: "old@example.com", ConfigDir: "/tmp/old"},
		},
	}
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work":     {Status: config.QuotaStatusAvailable},
		"personal": {Status: config.QuotaStatusLimited, ResetsAt: "7pm"},
		"team":     {Status: config.QuotaStatusAvailable},
		// Token expired three days ago — should override the persisted
		// Available status to reflect "expired" in the summary view.
		"old": {Status: config.QuotaStatusAvailable, TokenExpiresAt: now.Add(-72 * time.Hour).Format(time.RFC3339)},
	}}
	state.LimitedSessions = map[string]config.LimitedSessionState{
		"gt-a": {Account: "personal", RateLimited: true, ResetsAt: "7pm"},
	}

	resp := BuildQuotaSummary(state, acctCfg, nil, QuotaUsageCeilings{SessionTokenCeiling: 8_000_000, WeeklyTokenCeiling: 40_000_000}, now)

	if resp.Counters.Available != 2 {
		t.Errorf("available = %d, want 2", resp.Counters.Available)
	}
	if resp.Counters.Limited != 1 {
		t.Errorf("limited = %d, want 1", resp.Counters.Limited)
	}
	if resp.Counters.Expired != 1 {
		t.Errorf("expired = %d, want 1", resp.Counters.Expired)
	}

	byHandle := map[string]QuotaSummaryAccount{}
	for _, a := range resp.Accounts {
		byHandle[a.Handle] = a
	}
	if byHandle["old"].Status != "expired" {
		t.Errorf("old.Status = %q, want expired", byHandle["old"].Status)
	}
	if byHandle["personal"].Status != string(config.QuotaStatusLimited) {
		t.Errorf("personal.Status = %q, want limited", byHandle["personal"].Status)
	}
	if got := byHandle["work"].IsDefault; !got {
		t.Errorf("work.IsDefault = false, want true (Default=%q)", acctCfg.Default)
	}

	if sessions := byHandle["personal"].ActiveSessions; len(sessions) != 1 || sessions[0] != "gt-a" {
		t.Errorf("personal.ActiveSessions = %v, want [gt-a]", sessions)
	}
	if got := resp.LimitedSessions["gt-a"].Account; got != "personal" {
		t.Errorf("limited session account = %q, want personal", got)
	}
}

func TestBuildQuotaSummary_PopulatesUnlocksAtFromResetsAt(t *testing.T) {
	// Use a noon reference so "7pm today" is unambiguously in the future
	// regardless of which zone the host happens to be in.
	now := time.Date(2026, 5, 24, 12, 0, 0, 0, time.UTC)
	acctCfg := &config.AccountsConfig{Accounts: map[string]config.Account{
		"work": {ConfigDir: "/tmp/work"},
		// "past" account: reset already happened earlier in the day.
		"past": {ConfigDir: "/tmp/past"},
	}}
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work": {Status: config.QuotaStatusLimited, ResetsAt: "7pm"},
		// 8am UTC is before the noon reference → ParseResetTime returns a past
		// time and we should NOT emit UnlocksAt for it.
		"past": {Status: config.QuotaStatusLimited, ResetsAt: "8am"},
	}}

	resp := BuildQuotaSummary(state, acctCfg, nil, QuotaUsageCeilings{SessionTokenCeiling: 8_000_000, WeeklyTokenCeiling: 40_000_000}, now)

	byHandle := map[string]QuotaSummaryAccount{}
	for _, a := range resp.Accounts {
		byHandle[a.Handle] = a
	}
	if byHandle["work"].UnlocksAt == "" {
		t.Errorf("work.UnlocksAt empty for limited+future-reset (resets_at=%q)",
			byHandle["work"].ResetsAt)
	} else {
		parsed, err := time.Parse(time.RFC3339, byHandle["work"].UnlocksAt)
		if err != nil {
			t.Errorf("UnlocksAt not RFC3339: %v (%q)", err, byHandle["work"].UnlocksAt)
		} else if !parsed.After(now) {
			t.Errorf("UnlocksAt = %v should be after now=%v", parsed, now)
		}
	}
	if byHandle["past"].UnlocksAt != "" {
		t.Errorf("past.UnlocksAt = %q, want empty (parsed reset in the past)",
			byHandle["past"].UnlocksAt)
	}
}

func TestBuildQuotaSummary_FoldsUsageWhenAvailable(t *testing.T) {
	now := time.Now()
	acctCfg := &config.AccountsConfig{Accounts: map[string]config.Account{
		"work": {ConfigDir: "/tmp/work"},
	}}
	state := &config.QuotaState{Accounts: map[string]config.AccountQuotaState{
		"work": {Status: config.QuotaStatusAvailable},
	}}
	usage := &quota.UsageReport{Accounts: map[string]quota.AccountUsage{
		"work": {Handle: "work", Counts: quota.TokenCounts{InputTokens: 42}, WeekCounts: quota.TokenCounts{InputTokens: 99}},
	}}

	ceilings := QuotaUsageCeilings{SessionTokenCeiling: 8_000_000, WeeklyTokenCeiling: 40_000_000}
	resp := BuildQuotaSummary(state, acctCfg, usage, ceilings, now)
	if len(resp.Accounts) != 1 {
		t.Fatalf("accounts = %d", len(resp.Accounts))
	}
	if resp.Accounts[0].Usage == nil || resp.Accounts[0].Usage.Counts.InputTokens != 42 {
		t.Errorf("usage not folded: %+v", resp.Accounts[0].Usage)
	}
	if resp.Accounts[0].Usage.WeekCounts.InputTokens != 99 {
		t.Errorf("week counts not folded: %+v", resp.Accounts[0].Usage)
	}
	if resp.UsageCeilings.WeeklyTokenCeiling != 40_000_000 {
		t.Errorf("ceilings not propagated: %+v", resp.UsageCeilings)
	}
}
