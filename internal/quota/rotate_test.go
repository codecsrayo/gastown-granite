package quota

import (
	"testing"

	"github.com/steveyegge/gastown/internal/config"
)

func TestPlanRotation_NoLimitedSessions(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear", "gt-witness"},
		paneContent: map[string]string{
			"gt-crew-bear": "working normally...",
			"gt-witness":   "watching...",
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"work":     {ConfigDir: "/home/user/.claude-accounts/work"},
			"personal": {ConfigDir: "/home/user/.claude-accounts/personal"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	if len(plan.LimitedSessions) != 0 {
		t.Errorf("expected 0 limited sessions, got %d", len(plan.LimitedSessions))
	}
	if len(plan.Assignments) != 0 {
		t.Errorf("expected 0 assignments, got %d", len(plan.Assignments))
	}
}

func TestPlanRotation_AssignsAvailableAccount(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear", "gt-witness"},
		paneContent: map[string]string{
			"gt-crew-bear": "You've hit your limit · resets 7pm (America/Los_Angeles)",
			"gt-witness":   "watching...",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/work"},
			"gt-witness":   {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/personal"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"work":     {ConfigDir: "/home/user/.claude-accounts/work"},
			"personal": {ConfigDir: "/home/user/.claude-accounts/personal"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)

	// Pre-seed quota state with both accounts available
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"work":     {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"personal": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	if len(plan.LimitedSessions) != 1 {
		t.Fatalf("expected 1 limited session, got %d", len(plan.LimitedSessions))
	}
	if plan.LimitedSessions[0].Session != "gt-crew-bear" {
		t.Errorf("expected limited session gt-crew-bear, got %s", plan.LimitedSessions[0].Session)
	}

	if len(plan.Assignments) != 1 {
		t.Fatalf("expected 1 assignment, got %d", len(plan.Assignments))
	}

	newAccount, ok := plan.Assignments["gt-crew-bear"]
	if !ok {
		t.Fatal("expected assignment for gt-crew-bear")
	}
	// Should assign "personal" since "work" is now limited
	if newAccount != "personal" {
		t.Errorf("expected assignment to 'personal', got %q", newAccount)
	}
}

func TestPlanRotation_NoAvailableAccounts(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear"},
		paneContent: map[string]string{
			"gt-crew-bear": "You've hit your limit",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/work"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"work": {ConfigDir: "/home/user/.claude-accounts/work"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)

	// Only one account and it's limited
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"work": {Status: config.QuotaStatusAvailable},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	if len(plan.LimitedSessions) != 1 {
		t.Fatalf("expected 1 limited session, got %d", len(plan.LimitedSessions))
	}
	// No assignments because there's no other account to rotate to
	if len(plan.Assignments) != 0 {
		t.Errorf("expected 0 assignments (no available accounts), got %d", len(plan.Assignments))
	}
}

func TestPlanRotation_SkipsSameAccount(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear"},
		paneContent: map[string]string{
			"gt-crew-bear": "You've hit your limit",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)

	// alpha is LRU (oldest) but is the session's current account
	// Should skip alpha and assign beta (next LRU)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"gamma": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	newAccount, ok := plan.Assignments["gt-crew-bear"]
	if !ok {
		t.Fatal("expected assignment for gt-crew-bear")
	}
	// Should skip alpha (same account), assign beta
	if newAccount != "beta" {
		t.Errorf("expected assignment to 'beta' (skipping same account), got %q", newAccount)
	}
}

func TestPlanRotation_MultipleLimitedSessions(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"hq-mayor", "gt-crew-bear", "gt-crew-wolf"},
		paneContent: map[string]string{
			"hq-mayor":     "You've hit your limit · resets 7pm",
			"gt-crew-bear": "You've hit your limit · resets 7pm",
			"gt-crew-wolf": "working fine...",
		},
		envVars: map[string]map[string]string{
			"hq-mayor":     {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-wolf": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/beta"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)

	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"gamma": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	if len(plan.LimitedSessions) != 2 {
		t.Fatalf("expected 2 limited sessions, got %d", len(plan.LimitedSessions))
	}

	// Both limited sessions should be assigned to the same account (beta, LRU available)
	if len(plan.Assignments) != 2 {
		t.Fatalf("expected 2 assignments, got %d", len(plan.Assignments))
	}
	for session, acct := range plan.Assignments {
		if acct != "beta" {
			t.Errorf("expected session %s assigned to 'beta', got %q", session, acct)
		}
	}
}

// --- Config dir grouping tests ---

func TestPlanRotation_ConfigDirGrouping_SameDir(t *testing.T) {
	setupTestRegistry(t)

	// Two sessions on the same config dir (alpha) should produce one config dir swap.
	tmux := &mockTmux{
		sessions: []string{"hq-mayor", "gt-crew-bear"},
		paneContent: map[string]string{
			"hq-mayor":     "You've hit your limit · resets 7pm",
			"gt-crew-bear": "You've hit your limit · resets 7pm",
		},
		envVars: map[string]map[string]string{
			"hq-mayor":     {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	// One config dir swap entry (alpha's dir -> beta)
	if len(plan.ConfigDirSwaps) != 1 {
		t.Fatalf("expected 1 config dir swap, got %d: %v", len(plan.ConfigDirSwaps), plan.ConfigDirSwaps)
	}

	alphaDir := "/home/user/.claude-accounts/alpha"
	newAccount, ok := plan.ConfigDirSwaps[alphaDir]
	if !ok {
		t.Fatalf("expected config dir swap for %s", alphaDir)
	}
	if newAccount != "beta" {
		t.Errorf("expected config dir swap to 'beta', got %q", newAccount)
	}

	// Both sessions should get the same assignment (beta)
	if len(plan.Assignments) != 2 {
		t.Fatalf("expected 2 session assignments, got %d", len(plan.Assignments))
	}
	for session, assigned := range plan.Assignments {
		if assigned != "beta" {
			t.Errorf("session %s: expected assignment 'beta', got %q", session, assigned)
		}
	}
}

func TestPlanRotation_ConfigDirGrouping_DifferentDirs(t *testing.T) {
	setupTestRegistry(t)

	// Two sessions on different config dirs should produce separate swap entries.
	tmux := &mockTmux{
		sessions: []string{"hq-mayor", "gt-crew-bear"},
		paneContent: map[string]string{
			"hq-mayor":     "You've hit your limit · resets 7pm",
			"gt-crew-bear": "You've hit your limit · resets 7pm",
		},
		envVars: map[string]map[string]string{
			"hq-mayor":     {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/beta"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
			"delta": {ConfigDir: "/home/user/.claude-accounts/delta"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"gamma": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
			"delta": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T04:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	// Two different config dirs = two swap entries
	if len(plan.ConfigDirSwaps) != 2 {
		t.Fatalf("expected 2 config dir swaps, got %d: %v", len(plan.ConfigDirSwaps), plan.ConfigDirSwaps)
	}

	// Each session should have an assignment
	if len(plan.Assignments) != 2 {
		t.Fatalf("expected 2 session assignments, got %d", len(plan.Assignments))
	}

	// The two assignments should be different accounts (not alpha or beta, since those are limited)
	assigned := make(map[string]bool)
	for _, acct := range plan.Assignments {
		assigned[acct] = true
	}
	if len(assigned) != 2 {
		t.Errorf("expected 2 distinct assigned accounts, got %d: %v", len(assigned), plan.Assignments)
	}
}

// --- State persistence tests ---

func TestPlanRotation_MarksLimitedAccountsInState(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear"},
		paneContent: map[string]string{
			"gt-crew-bear": "You've hit your limit · resets 7pm (America/Los_Angeles)",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable},
			"beta":  {Status: config.QuotaStatusAvailable},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	// PlanRotation should detect alpha as limited
	if len(plan.LimitedSessions) != 1 {
		t.Fatalf("expected 1 limited session, got %d", len(plan.LimitedSessions))
	}
	if plan.LimitedSessions[0].AccountHandle != "alpha" {
		t.Errorf("expected limited account alpha, got %q", plan.LimitedSessions[0].AccountHandle)
	}

	// PlanRotation does NOT mark accounts as limited in state — the caller
	// is responsible for persisting after execution. Verify the plan output
	// contains enough info for the caller to persist.
	if plan.LimitedSessions[0].ResetsAt == "" {
		t.Errorf("expected non-empty ResetsAt for rate-limited session")
	}
}

func TestPlanRotation_DryRunReturnsValidPlan(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear"},
		paneContent: map[string]string{
			"gt-crew-bear": "You've hit your limit · resets 7pm",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	// PlanRotation returns a complete plan suitable for JSON serialization
	// (used by --dry-run --json). Verify all fields are populated.
	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	if plan.LimitedSessions == nil {
		t.Error("plan.LimitedSessions should not be nil")
	}
	if plan.AvailableAccounts == nil {
		t.Error("plan.AvailableAccounts should not be nil")
	}
	if plan.Assignments == nil {
		t.Error("plan.Assignments should not be nil")
	}
	if plan.ConfigDirSwaps == nil {
		t.Error("plan.ConfigDirSwaps should not be nil")
	}
}

// --- Preemptive rotation tests ---

func TestPlanRotation_PreemptiveFromAccount(t *testing.T) {
	setupTestRegistry(t)

	// Two sessions: one on alpha (not rate-limited), one on beta.
	// --from alpha should target the alpha session regardless of rate-limit status.
	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear", "gt-crew-wolf"},
		paneContent: map[string]string{
			"gt-crew-bear": "working normally...",
			"gt-crew-wolf": "also working...",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-wolf": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/beta"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"gamma": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{FromAccount: "alpha"})
	if err != nil {
		t.Fatal(err)
	}

	// Should target the alpha session even though it's not rate-limited
	if len(plan.LimitedSessions) != 1 {
		t.Fatalf("expected 1 targeted session, got %d", len(plan.LimitedSessions))
	}
	if plan.LimitedSessions[0].Session != "gt-crew-bear" {
		t.Errorf("expected session gt-crew-bear, got %s", plan.LimitedSessions[0].Session)
	}

	// Should assign a different account (gamma is LRU)
	if len(plan.Assignments) != 1 {
		t.Fatalf("expected 1 assignment, got %d", len(plan.Assignments))
	}
	newAccount := plan.Assignments["gt-crew-bear"]
	if newAccount != "gamma" {
		t.Errorf("expected assignment to 'gamma' (LRU), got %q", newAccount)
	}
}

func TestPlanRotation_PreemptiveFromAccount_NoSessions(t *testing.T) {
	setupTestRegistry(t)

	// No sessions use the "gamma" account — --from gamma should find nothing.
	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear"},
		paneContent: map[string]string{
			"gt-crew-bear": "working normally...",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{FromAccount: "gamma"})
	if err != nil {
		t.Fatal(err)
	}

	if len(plan.LimitedSessions) != 0 {
		t.Errorf("expected 0 targeted sessions, got %d", len(plan.LimitedSessions))
	}
	if len(plan.Assignments) != 0 {
		t.Errorf("expected 0 assignments, got %d", len(plan.Assignments))
	}
}

// --- Near-limit proactive rotation tests ---

func TestPlanRotation_IncludeNearLimit(t *testing.T) {
	setupTestRegistry(t)

	// bear is near-limit (warning pattern), wolf is fine
	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear", "gt-crew-wolf"},
		paneContent: map[string]string{
			"gt-crew-bear": "85% of your daily usage consumed",
			"gt-crew-wolf": "working fine...",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/work"},
			"gt-crew-wolf": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/personal"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"work":     {ConfigDir: "/home/user/.claude-accounts/work"},
			"personal": {ConfigDir: "/home/user/.claude-accounts/personal"},
			"backup":   {ConfigDir: "/home/user/.claude-accounts/backup"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}
	if err := scanner.WithWarningPatterns(nil); err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"work":     {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
			"personal": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"backup":   {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	// Without IncludeNearLimit — near-limit sessions NOT included
	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}
	if len(plan.LimitedSessions) != 0 {
		t.Errorf("expected 0 hard-limited sessions, got %d", len(plan.LimitedSessions))
	}
	if len(plan.NearLimitSessions) != 1 {
		t.Errorf("expected 1 near-limit session, got %d", len(plan.NearLimitSessions))
	}
	if len(plan.Assignments) != 0 {
		t.Errorf("expected 0 assignments without IncludeNearLimit, got %d", len(plan.Assignments))
	}

	// With IncludeNearLimit — near-limit sessions ARE included
	plan, err = PlanRotation(scanner, mgr, accounts, PlanOpts{IncludeNearLimit: true})
	if err != nil {
		t.Fatal(err)
	}
	if len(plan.NearLimitSessions) != 1 {
		t.Fatalf("expected 1 near-limit session, got %d", len(plan.NearLimitSessions))
	}
	if plan.NearLimitSessions[0].Session != "gt-crew-bear" {
		t.Errorf("expected near-limit session gt-crew-bear, got %s", plan.NearLimitSessions[0].Session)
	}
	if len(plan.Assignments) != 1 {
		t.Fatalf("expected 1 assignment with IncludeNearLimit, got %d", len(plan.Assignments))
	}
	newAccount := plan.Assignments["gt-crew-bear"]
	if newAccount != "backup" {
		t.Errorf("expected assignment to 'backup' (LRU), got %q", newAccount)
	}
}

func TestPlanRotation_MixedHardAndNearLimit(t *testing.T) {
	setupTestRegistry(t)

	// bear is hard-limited, wolf is near-limit
	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear", "gt-crew-wolf"},
		paneContent: map[string]string{
			"gt-crew-bear": "You've hit your limit · resets 7pm",
			"gt-crew-wolf": "90% of your daily usage consumed",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-wolf": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/beta"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
			"delta": {ConfigDir: "/home/user/.claude-accounts/delta"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}
	if err := scanner.WithWarningPatterns(nil); err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"gamma": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
			"delta": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T04:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{IncludeNearLimit: true})
	if err != nil {
		t.Fatal(err)
	}

	// Both hard-limited and near-limited should be in the plan
	if len(plan.LimitedSessions) != 1 {
		t.Errorf("expected 1 hard-limited session, got %d", len(plan.LimitedSessions))
	}
	if len(plan.NearLimitSessions) != 1 {
		t.Errorf("expected 1 near-limit session, got %d", len(plan.NearLimitSessions))
	}

	// Both should get assignments
	if len(plan.Assignments) != 2 {
		t.Fatalf("expected 2 assignments, got %d", len(plan.Assignments))
	}
}

// TestPlanRotation_ExcludesLiveLimitedAccounts verifies the planner never
// rotates a session ONTO an account that another session is currently
// hard rate-limited on, even when that account's persisted status is still
// "available". Without this, sessions thrash by respawning straight into the
// rate-limit menu (see hq-boot wedge investigation).
func TestPlanRotation_ExcludesLiveLimitedAccounts(t *testing.T) {
	setupTestRegistry(t)

	// Two sessions, each hard-limited on a different account (alpha, beta).
	// Only gamma is genuinely free. The planner must assign exclusively to
	// gamma — never alpha or beta, which are live-limited right now.
	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear", "gt-crew-wolf"},
		paneContent: map[string]string{
			"gt-crew-bear": "Stop and wait for limit to reset",
			"gt-crew-wolf": "You've hit your limit · resets 8pm",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-wolf": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/beta"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	// All three accounts persisted as available — alpha/beta are limited only
	// in the live scan, not in state.
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"gamma": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{})
	if err != nil {
		t.Fatal(err)
	}

	// Every assignment must target gamma; alpha and beta are live-limited.
	for session, target := range plan.Assignments {
		if target == "alpha" || target == "beta" {
			t.Errorf("session %s assigned to live-limited account %q", session, target)
		}
	}

	// alpha and beta should be reported as skipped with the live-limit reason.
	if reason := plan.SkippedAccounts["alpha"]; reason == "" {
		t.Error("expected alpha to be skipped (live rate-limited)")
	}
	if reason := plan.SkippedAccounts["beta"]; reason == "" {
		t.Error("expected beta to be skipped (live rate-limited)")
	}

	// gamma must remain a valid candidate.
	foundGamma := false
	for _, h := range plan.AvailableAccounts {
		if h == "gamma" {
			foundGamma = true
		}
	}
	if !foundGamma {
		t.Errorf("expected gamma in available pool, got %v", plan.AvailableAccounts)
	}
}

// TestPlanRotation_PreemptiveSpreadDistribution verifies that when multiple
// sessions share the same config dir and multiple accounts are available,
// preemptive --from rotation fans them out across different accounts instead
// of piling all onto a single keychain-swap destination.
func TestPlanRotation_PreemptiveSpreadDistribution(t *testing.T) {
	setupTestRegistry(t)

	// Three sessions on alpha (all share alpha's config dir), two other accounts free.
	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear", "gt-crew-wolf", "gt-crew-fox"},
		paneContent: map[string]string{
			"gt-crew-bear": "working normally...",
			"gt-crew-wolf": "working normally...",
			"gt-crew-fox":  "working normally...",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-wolf": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
			"gt-crew-fox":  {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
			"gamma": {ConfigDir: "/home/user/.claude-accounts/gamma"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T03:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"gamma": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{FromAccount: "alpha"})
	if err != nil {
		t.Fatal(err)
	}

	if len(plan.LimitedSessions) != 3 {
		t.Fatalf("expected 3 targeted sessions, got %d", len(plan.LimitedSessions))
	}
	if len(plan.Assignments) != 3 {
		t.Fatalf("expected 3 assignments, got %d", len(plan.Assignments))
	}

	// Spread: at least 2 distinct target accounts (with 3 sessions and 2 available).
	seen := make(map[string]bool)
	for _, acct := range plan.Assignments {
		seen[acct] = true
		if acct == "alpha" {
			t.Errorf("session assigned to source account %q", acct)
		}
	}
	if len(seen) < 2 {
		t.Errorf("expected spread across >=2 accounts, got %d distinct: %v", len(seen), plan.Assignments)
	}

	// SpreadConfigDirs must be populated for every assigned session.
	if len(plan.SpreadConfigDirs) != 3 {
		t.Errorf("expected 3 SpreadConfigDirs entries, got %d: %v", len(plan.SpreadConfigDirs), plan.SpreadConfigDirs)
	}

	// Each SpreadConfigDir must point to the target account's config dir.
	for sess, newAcct := range plan.Assignments {
		wantDir := "/home/user/.claude-accounts/" + newAcct
		if plan.SpreadConfigDirs[sess] != wantDir {
			t.Errorf("session %s: SpreadConfigDir=%q, want %q", sess, plan.SpreadConfigDirs[sess], wantDir)
		}
	}
}

// TestPlanRotation_PreemptiveNoSpreadSingleSession verifies that a single
// preemptive session does NOT populate SpreadConfigDirs (falls through to
// existing config-dir-swap path).
func TestPlanRotation_PreemptiveNoSpreadSingleSession(t *testing.T) {
	setupTestRegistry(t)

	tmux := &mockTmux{
		sessions: []string{"gt-crew-bear"},
		paneContent: map[string]string{
			"gt-crew-bear": "working normally...",
		},
		envVars: map[string]map[string]string{
			"gt-crew-bear": {"CLAUDE_CONFIG_DIR": "/home/user/.claude-accounts/alpha"},
		},
	}

	accounts := &config.AccountsConfig{
		Accounts: map[string]config.Account{
			"alpha": {ConfigDir: "/home/user/.claude-accounts/alpha"},
			"beta":  {ConfigDir: "/home/user/.claude-accounts/beta"},
		},
	}

	scanner, err := NewScanner(tmux, nil, accounts)
	if err != nil {
		t.Fatal(err)
	}

	townRoot := setupTestTown(t)
	mgr := NewManager(townRoot)
	state := &config.QuotaState{
		Version: config.CurrentQuotaVersion,
		Accounts: map[string]config.AccountQuotaState{
			"alpha": {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T02:00:00Z"},
			"beta":  {Status: config.QuotaStatusAvailable, LastUsed: "2025-01-01T01:00:00Z"},
		},
	}
	if err := mgr.Save(state); err != nil {
		t.Fatal(err)
	}

	plan, err := PlanRotation(scanner, mgr, accounts, PlanOpts{FromAccount: "alpha"})
	if err != nil {
		t.Fatal(err)
	}

	if len(plan.Assignments) != 1 {
		t.Fatalf("expected 1 assignment, got %d", len(plan.Assignments))
	}
	// Single preemptive session: no spread needed.
	if len(plan.SpreadConfigDirs) != 0 {
		t.Errorf("single-session preemptive: expected no SpreadConfigDirs, got %v", plan.SpreadConfigDirs)
	}
}
