package cmd

import (
	"testing"

	"github.com/steveyegge/gastown/internal/quota"
)

// TestOldAccountBySession verifies the execute path stamps "limited" on the
// account the scanner resolved as active (GT_QUOTA_ACCOUNT-aware), not the one
// inferred from CLAUDE_CONFIG_DIR. After a keychain swap the config dir still
// maps to the original account while a different account's token is live; if we
// marked the config-dir owner, the truly rate-limited account would stay
// "available" and remain eligible for the next rotation.
func TestOldAccountBySession(t *testing.T) {
	limited := []quota.ScanResult{
		// Post-swap: config dir belongs to "alpha" but "bravo" is the active
		// (and now rate-limited) account per GT_QUOTA_ACCOUNT.
		{Session: "gt-crew-bear", AccountHandle: "bravo", ConfigDir: "/home/u/.claude-alpha", RateLimited: true},
		// Unresolved account (unregistered config dir) — must be omitted so the
		// execute path falls back to its config-dir lookup.
		{Session: "gt-crew-fox", AccountHandle: "", ConfigDir: "/home/u/.claude-x", RateLimited: true},
	}

	got := oldAccountBySession(limited)

	if got["gt-crew-bear"] != "bravo" {
		t.Errorf("gt-crew-bear: got %q, want active account %q", got["gt-crew-bear"], "bravo")
	}
	if _, ok := got["gt-crew-fox"]; ok {
		t.Errorf("gt-crew-fox: unresolved handle must be omitted, got %q", got["gt-crew-fox"])
	}
}

// TestResetsBySession verifies reset times are threaded per session and that
// sessions without a parsed reset time are omitted.
func TestResetsBySession(t *testing.T) {
	limited := []quota.ScanResult{
		{Session: "gt-crew-bear", ResetsAt: "7pm (America/Los_Angeles)"},
		{Session: "gt-crew-fox", ResetsAt: ""},
	}

	got := resetsBySession(limited)

	if got["gt-crew-bear"] != "7pm (America/Los_Angeles)" {
		t.Errorf("gt-crew-bear: got %q, want %q", got["gt-crew-bear"], "7pm (America/Los_Angeles)")
	}
	if _, ok := got["gt-crew-fox"]; ok {
		t.Errorf("gt-crew-fox: empty reset must be omitted, got %q", got["gt-crew-fox"])
	}
}
