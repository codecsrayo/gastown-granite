package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"maps"
	"os"
	"os/exec"
	"os/signal"
	"slices"
	"sort"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/events"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/quota/handlers"
	qe "github.com/steveyegge/gastown/internal/quota/quotaevents"
	"github.com/steveyegge/gastown/internal/style"
	ttmux "github.com/steveyegge/gastown/internal/tmux"
	"github.com/steveyegge/gastown/internal/web"
	"github.com/steveyegge/gastown/internal/workspace"
)

// quotaBlockedAlertCooldown is the minimum time between successive
// "rotation blocked" escalation emissions from `gt quota watch`. A blocked
// incident persists across many polling cycles; without throttling the watcher
// would emit a new escalation every interval.
const quotaBlockedAlertCooldown = 30 * time.Minute

// Quota command flags
var (
	quotaJSON bool
)

var quotaCmd = &cobra.Command{
	Use:     "quota",
	GroupID: GroupServices,
	Short:   "Manage account quota rotation",
	RunE:    requireSubcommand,
	Long: `Manage Claude Code account quota rotation for Gas Town.

When sessions hit rate limits, quota commands help detect blocked sessions
and rotate them to available accounts from the pool.

Commands:
  gt quota status            Show account quota status
  gt quota scan              Detect rate-limited sessions
  gt quota rotate            Swap blocked sessions to available accounts
  gt quota clear             Mark account(s) as available again`,
}

var quotaStatusCmd = &cobra.Command{
	Use:   "status",
	Short: "Show account quota status",
	Long: `Show the quota status of all registered accounts.

Displays which accounts are available, rate-limited, or in cooldown,
along with timestamps for limit detection and estimated reset times.

Examples:
  gt quota status           # Text output
  gt quota status --json    # JSON output`,
	RunE: runQuotaStatus,
}

func runQuotaStatus(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil {
		return fmt.Errorf("finding town root: %w", err)
	}

	// Load accounts
	accountsPath := constants.MayorAccountsPath(townRoot)
	acctCfg, err := config.LoadAccountsConfig(accountsPath)
	if err != nil {
		fmt.Println("No accounts configured.")
		fmt.Println("\nTo add an account:")
		fmt.Println("  gt account add <handle>")
		return nil
	}

	if len(acctCfg.Accounts) == 0 {
		fmt.Println("No accounts configured.")
		return nil
	}

	// Load quota state
	mgr := quota.NewManager(townRoot)
	state, err := mgr.Load()
	if err != nil {
		return fmt.Errorf("loading quota state: %w", err)
	}

	// Ensure all accounts are tracked
	mgr.EnsureAccountsTracked(state, acctCfg.Accounts)

	// Auto-clear accounts whose reset time has passed
	if cleared := mgr.ClearExpired(state); len(cleared) > 0 {
		if err := mgr.Save(state); err != nil {
			style.PrintWarning("could not persist expired account clearance: %v", err)
		}
		for _, handle := range cleared {
			_ = events.LogFeed(events.TypeQuotaCleared, "quota",
				events.QuotaClearedPayload(handle, state.Accounts[handle].ResetsAt))
		}
	}

	if quotaJSON {
		return printQuotaStatusJSON(acctCfg, state)
	}
	return printQuotaStatusText(acctCfg, state)
}

// printQuotaStatusJSON emits the full observability snapshot — the same shape
// served by GET /api/quota/summary — so scripts and the dashboard share one
// source of truth. Token usage is aggregated best-effort; if the tmux walk
// trips, the snapshot still carries status, token expiry, rotation counts, the
// waiting limited sessions, and the last rotation plan.
func printQuotaStatusJSON(acctCfg *config.AccountsConfig, state *config.QuotaState) error {
	var usageReport *quota.UsageReport
	if report, err := quota.AggregateUsage(ttmux.NewTmux(), state, acctCfg, "", time.Now()); err == nil {
		usageReport = report
	}
	resp := web.BuildQuotaSummary(state, acctCfg, usageReport, time.Now())
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(resp)
}

func printQuotaStatusText(acctCfg *config.AccountsConfig, state *config.QuotaState) error {
	available := 0
	limited := 0
	inUse := 0

	// Aggregate live usage so we can mark which accounts have active sessions
	// right now. Best-effort: if the tmux walk fails, sessionsByAccount stays
	// empty and we simply omit the in-use markers.
	sessionsByAccount := make(map[string]int)
	if report, err := quota.AggregateUsage(ttmux.NewTmux(), state, acctCfg, "", time.Now()); err == nil && report != nil {
		for handle, usage := range report.Accounts {
			if n := len(usage.Sessions); n > 0 {
				sessionsByAccount[handle] = n
			}
		}
	}

	fmt.Println(style.Bold.Render("Account Quota Status"))
	fmt.Println()

	for _, handle := range slices.Sorted(maps.Keys(acctCfg.Accounts)) {
		acct := acctCfg.Accounts[handle]
		qs := state.Accounts[handle]
		status := qs.Status
		if status == "" {
			status = config.QuotaStatusAvailable
		}

		// Left marker: ▸ if the account currently has active sessions,
		// otherwise * for the default account, else blank. In-use takes
		// precedence because it's the more actionable signal.
		sessionCount := sessionsByAccount[handle]
		marker := " "
		switch {
		case sessionCount > 0:
			marker = style.Success.Render("▸")
			inUse++
		case handle == acctCfg.Default:
			marker = "*"
		}

		// Status badge
		var badge string
		switch status {
		case config.QuotaStatusAvailable:
			badge = style.Success.Render("available")
			available++
		case config.QuotaStatusLimited:
			badge = style.Error.Render("limited")
			limited++
			if qs.ResetsAt != "" {
				badge += style.Dim.Render(" (resets " + qs.ResetsAt + ")")
			}
		case config.QuotaStatusCooldown:
			badge = style.Warning.Render("cooldown")
			limited++
		default:
			badge = style.Dim.Render("unknown")
		}

		email := ""
		if acct.Email != "" {
			email = style.Dim.Render(" <" + acct.Email + ">")
		}

		// In-use tag: session count, plus the default tag when applicable.
		var tags string
		if sessionCount > 0 {
			noun := "session"
			if sessionCount > 1 {
				noun = "sessions"
			}
			tags += style.Info.Render(fmt.Sprintf("  in use · %d %s", sessionCount, noun))
		}
		if handle == acctCfg.Default {
			tags += style.Dim.Render("  (default)")
		}

		fmt.Printf(" %s %-12s %s%s%s\n", marker, handle, badge, email, tags)
	}

	fmt.Println()
	fmt.Printf(" %s %d available, %d limited, %d in use\n",
		style.Info.Render("Summary:"), available, limited, inUse)

	return nil
}

// Scan command flags
var (
	scanUpdate bool
)

var quotaScanCmd = &cobra.Command{
	Use:   "scan",
	Short: "Detect rate-limited sessions",
	Long: `Scan all Gas Town tmux sessions for rate-limit indicators.

Captures recent pane output from each session and checks for rate-limit
messages. Reports which sessions are blocked and which account they use.

Use --update to automatically update quota state with detected limits.

Examples:
  gt quota scan              # Report rate-limited sessions
  gt quota scan --update     # Report and update quota state
  gt quota scan --json       # JSON output`,
	RunE: runQuotaScan,
}

func runQuotaScan(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil {
		return fmt.Errorf("finding town root: %w", err)
	}

	// Load accounts config
	accountsPath := constants.MayorAccountsPath(townRoot)
	acctCfg, loadErr := config.LoadAccountsConfig(accountsPath)
	// acctCfg can be nil if no accounts configured — scan still works

	// Create scanner
	t := ttmux.NewTmux()
	scanner, err := quota.NewScanner(t, nil, acctCfg)
	if err != nil {
		return fmt.Errorf("creating scanner: %w", err)
	}

	results, err := scanner.ScanAll()
	if err != nil {
		return fmt.Errorf("scanning sessions: %w", err)
	}

	// Optionally update quota state
	if scanUpdate && loadErr == nil && acctCfg != nil {
		if err := updateQuotaState(townRoot, results, acctCfg); err != nil {
			return fmt.Errorf("updating quota state: %w", err)
		}
		mgr := quota.NewManager(townRoot)
		if st, loadStateErr := mgr.Load(); loadStateErr == nil {
			emitScanEvents(results, len(mgr.AvailableAccounts(st)))
		}
	}

	if quotaJSON {
		return printScanJSON(results)
	}
	return printScanText(results)
}

func updateQuotaState(townRoot string, results []quota.ScanResult, acctCfg *config.AccountsConfig) error {
	mgr := quota.NewManager(townRoot)
	return mgr.WithLock(func() error {
		state, err := mgr.Load()
		if err != nil {
			return err
		}
		mgr.EnsureAccountsTracked(state, acctCfg.Accounts)

		now := time.Now().UTC().Format(time.RFC3339)
		for _, r := range results {
			if r.RateLimited && r.AccountHandle != "" {
				existing := state.Accounts[r.AccountHandle]
				existing.Status = config.QuotaStatusLimited
				existing.LimitedAt = now
				existing.ResetsAt = r.ResetsAt
				state.Accounts[r.AccountHandle] = existing
			}
		}
		recordScanSnapshot(state, results, now)

		return mgr.SaveUnlocked(state)
	})
}

// recordScanSnapshot overwrites the LimitedSessions map from a scan pass.
// Sessions that no longer match a rate-limit or near-limit signal are dropped
// so the dashboard never shows stale "waiting" entries.
func recordScanSnapshot(state *config.QuotaState, results []quota.ScanResult, now string) {
	snap := make(map[string]config.LimitedSessionState)
	for _, r := range results {
		if !r.RateLimited && !r.NearLimit {
			continue
		}
		snap[r.Session] = config.LimitedSessionState{
			Account:     r.AccountHandle,
			ConfigDir:   r.ConfigDir,
			RateLimited: r.RateLimited,
			NearLimit:   r.NearLimit,
			ResetsAt:    r.ResetsAt,
			MatchedLine: r.MatchedLine,
			DetectedAt:  now,
		}
	}
	quota.RecordLimitedSessions(state, snap)
}

// emitScanEvents fires per-session limited/near-limit events plus a single
// summary event for a scan pass. Best-effort; logging failures are dropped.
func emitScanEvents(results []quota.ScanResult, availableCount int) {
	limited := 0
	near := 0
	for _, r := range results {
		if r.RateLimited {
			limited++
			_ = events.LogFeed(events.TypeQuotaLimited, "quota",
				events.QuotaLimitedPayload(r.Session, r.AccountHandle, r.ResetsAt))
		} else if r.NearLimit {
			near++
			_ = events.LogAudit(events.TypeQuotaNearLimit, "quota",
				events.QuotaNearLimitPayload(r.Session, r.AccountHandle, r.MatchedLine))
		}
	}
	_ = events.LogAudit(events.TypeQuotaScanned, "quota",
		events.QuotaScannedPayload(len(results), limited, near, availableCount))
}

// persistPlanArtifacts writes the LastPlan snapshot plus token expiries from
// a planning pass under a single quota-lock acquisition.
func persistPlanArtifacts(mgr *quota.Manager, plan *quota.RotatePlan) error {
	if plan == nil {
		return nil
	}
	return mgr.WithLock(func() error {
		state, err := mgr.Load()
		if err != nil {
			return err
		}
		quota.ApplyTokenExpiries(state, plan.TokenExpiries)
		quota.RecordLastPlan(state, planSnapshotFrom(plan))
		return mgr.SaveUnlocked(state)
	})
}

// planSnapshotFrom freezes a plan into the persistence schema.
func planSnapshotFrom(plan *quota.RotatePlan) *config.RotationPlanSnapshot {
	limited := make([]string, 0, len(plan.LimitedSessions))
	for _, r := range plan.LimitedSessions {
		limited = append(limited, r.Session)
	}
	sort.Strings(limited)

	unassignable := 0
	for _, r := range plan.LimitedSessions {
		if _, ok := plan.Assignments[r.Session]; !ok {
			unassignable++
		}
	}

	ts := plan.PlannedAt
	if ts == "" {
		ts = time.Now().UTC().Format(time.RFC3339)
	}
	return &config.RotationPlanSnapshot{
		Timestamp:         ts,
		Assignments:       cloneStringMap(plan.Assignments),
		ConfigDirSwaps:    cloneStringMap(plan.ConfigDirSwaps),
		AvailablePool:     append([]string(nil), plan.AvailableAccounts...),
		SkippedAccounts:   cloneStringMap(plan.SkippedAccounts),
		LimitedSessions:   limited,
		UnassignableCount: unassignable,
	}
}

// emitPlanEvents fires quota_assigned plus a quota_token_expired event for
// every account that was skipped due to an expired token. Best-effort; emit
// failures are dropped.
func emitPlanEvents(plan *quota.RotatePlan) {
	if plan == nil {
		return
	}
	limited := 0
	near := 0
	for _, r := range plan.LimitedSessions {
		if r.RateLimited {
			limited++
		} else if r.NearLimit {
			near++
		}
	}
	for _, r := range plan.NearLimitSessions {
		if r.NearLimit {
			near++
		}
	}
	_ = events.LogAudit(events.TypeQuotaScanned, "quota",
		events.QuotaScannedPayload(
			len(plan.LimitedSessions)+len(plan.NearLimitSessions),
			limited, near, len(plan.AvailableAccounts),
		))

	if len(plan.Assignments) > 0 {
		_ = events.LogFeed(events.TypeQuotaAssigned, "quota",
			events.QuotaAssignedPayload(plan.Assignments, plan.AvailableAccounts))
	}

	for handle, reason := range plan.SkippedAccounts {
		expiresAt := plan.TokenExpiries[handle]
		_ = events.LogFeed(events.TypeQuotaTokenExpired, "quota",
			events.QuotaTokenExpiredPayload(handle, expiresAt, reason))
	}
}

func cloneStringMap(in map[string]string) map[string]string {
	if len(in) == 0 {
		return nil
	}
	out := make(map[string]string, len(in))
	for k, v := range in {
		out[k] = v
	}
	return out
}

func printScanJSON(results []quota.ScanResult) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(results)
}

func printScanText(results []quota.ScanResult) error {
	limited := 0
	nearLimit := 0

	for _, r := range results {
		if r.RateLimited {
			limited++
			account := r.AccountHandle
			if account == "" {
				account = "(unknown)"
			}
			resets := ""
			if r.ResetsAt != "" {
				resets = style.Dim.Render(" resets " + r.ResetsAt)
			}
			fmt.Printf(" %s %-25s %s %s%s\n",
				style.Error.Render("!"),
				r.Session,
				style.Dim.Render("account:"),
				account,
				resets,
			)
		} else if r.NearLimit {
			nearLimit++
			account := r.AccountHandle
			if account == "" {
				account = "(unknown)"
			}
			detail := ""
			if r.MatchedLine != "" {
				detail = style.Dim.Render(fmt.Sprintf(" (%s)", r.MatchedLine))
			}
			fmt.Printf(" %s %-25s %s %s%s\n",
				style.Warning.Render("~"),
				r.Session,
				style.Dim.Render("account:"),
				account,
				detail,
			)
		}
	}

	if limited == 0 && nearLimit == 0 {
		fmt.Printf(" %s No rate-limited sessions detected (%d scanned)\n",
			style.SuccessPrefix, len(results))
	} else {
		fmt.Println()
		parts := []string{}
		if limited > 0 {
			parts = append(parts, fmt.Sprintf("%d limited", limited))
		}
		if nearLimit > 0 {
			parts = append(parts, fmt.Sprintf("%d near-limit", nearLimit))
		}
		fmt.Printf(" %s %s of %d sessions\n",
			style.Warning.Render("Summary:"), strings.Join(parts, ", "), len(results))
	}

	return nil
}

// Rotate command flags
var (
	rotateDryRun bool
	rotateFrom   string
	rotateIdle   bool
)

var quotaRotateCmd = &cobra.Command{
	Use:   "rotate",
	Short: "Swap blocked sessions to available accounts",
	Long: `Rotate rate-limited sessions to available accounts.

Scans all sessions for rate limits, plans account assignments using
least-recently-used ordering, and restarts blocked sessions with fresh accounts.

Use --from to preemptively rotate sessions using a specific account before
it hits its rate limit. This is useful for switching idle sessions while
it's not disruptive.

The rotation process:
  1. Scans all Gas Town sessions for rate-limit indicators
  2. Selects available accounts (LRU order)
  3. Swaps macOS Keychain credentials (same config dir preserved)
  4. Restarts blocked sessions via respawn-pane
  5. Sends /resume to recover conversation context

Examples:
  gt quota rotate                    # Rotate all blocked sessions
  gt quota rotate --from work        # Preemptively rotate sessions on 'work' account
  gt quota rotate --from work --idle # Only rotate idle sessions on 'work' account
  gt quota rotate --dry-run          # Show plan without executing
  gt quota rotate --json             # JSON output`,
	RunE: runQuotaRotate,
}

func runQuotaRotate(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil {
		return fmt.Errorf("finding town root: %w", err)
	}

	// Load accounts config (required for rotation)
	accountsPath := constants.MayorAccountsPath(townRoot)
	acctCfg, err := config.LoadAccountsConfig(accountsPath)
	if err != nil {
		return fmt.Errorf("no accounts configured (run 'gt account add' first): %w", err)
	}
	if len(acctCfg.Accounts) < 2 {
		return fmt.Errorf("need at least 2 accounts for rotation (have %d)", len(acctCfg.Accounts))
	}

	// Validate --from account if specified
	if rotateFrom != "" {
		if _, ok := acctCfg.Accounts[rotateFrom]; !ok {
			return fmt.Errorf("account %q not found (available: %s)",
				rotateFrom, strings.Join(accountHandles(acctCfg), ", "))
		}
	}

	// Create scanner and plan rotation
	t := ttmux.NewTmux()
	scanner, err := quota.NewScanner(t, nil, acctCfg)
	if err != nil {
		return fmt.Errorf("creating scanner: %w", err)
	}

	mgr := quota.NewManager(townRoot)
	plan, err := quota.PlanRotation(scanner, mgr, acctCfg, quota.PlanOpts{FromAccount: rotateFrom})
	if err != nil {
		return fmt.Errorf("planning rotation: %w", err)
	}

	// NOTE: We intentionally do NOT persist scan-detected rate limits to the
	// per-account Status field here (stale sessions would poison the pool).
	// The artifacts below are pure observability: LastPlan/LimitedSessions
	// snapshots for the dashboard, plus per-account token expiry parsed at
	// plan time. Status mutation still only happens after successful execute.
	if err := persistPlanArtifacts(mgr, plan); err != nil {
		style.PrintWarning("could not persist plan artifacts: %v", err)
	}
	emitPlanEvents(plan)

	if len(plan.LimitedSessions) == 0 {
		if quotaJSON {
			return json.NewEncoder(os.Stdout).Encode([]quota.RotateResult{})
		}
		if rotateFrom != "" {
			fmt.Printf(" %s No sessions found using account %q\n", style.SuccessPrefix, rotateFrom)
		} else {
			fmt.Printf(" %s No rate-limited sessions detected\n", style.SuccessPrefix)
		}
		return nil
	}

	if len(plan.Assignments) == 0 {
		// Rotation-blocked: sessions are rate-limited but every candidate
		// account was skipped (typically expired tokens). Emit a throttled
		// escalation so operators see it on the dashboard even when nobody
		// is tailing `gt quota rotate`. Also fires when invoked by the
		// quota_dog daemon patrol or the long-running quota watch loop.
		if len(plan.LimitedSessions) > 0 {
			maybeEmitRotationBlockedAlert(mgr, plan)
		}

		if quotaJSON {
			return json.NewEncoder(os.Stdout).Encode([]quota.RotateResult{})
		}
		if rotateFrom != "" {
			fmt.Printf(" %s %d session(s) on %q but no available accounts to rotate to\n",
				style.WarningPrefix, len(plan.LimitedSessions), rotateFrom)
		} else {
			fmt.Printf(" %s %d sessions rate-limited but no available accounts to rotate to\n",
				style.WarningPrefix, len(plan.LimitedSessions))
		}
		if len(plan.SkippedAccounts) > 0 {
			fmt.Println()
			for handle, reason := range plan.SkippedAccounts {
				fmt.Printf(" %s Skipped %s — %s\n", style.WarningPrefix, handle, reason)
			}
		}
		return nil
	}

	// Count unassigned sessions by reason, before idle filtering changes the assignment count.
	// Three reasons a session may not be assigned:
	//   1. No config dir — session has no CLAUDE_CONFIG_DIR and no known account
	//   2. No available accounts — all accounts are limited or consumed
	noConfigDir := 0
	for _, r := range plan.LimitedSessions {
		if _, assigned := plan.Assignments[r.Session]; !assigned {
			if r.AccountHandle == "" && r.ConfigDir == "" {
				noConfigDir++
			}
		}
	}
	unassignable := len(plan.LimitedSessions) - len(plan.Assignments) - noConfigDir

	// Filter to idle sessions only when --idle is set.
	// This avoids interrupting agents that are actively working.
	skippedBusy := 0
	if rotateIdle {
		for session := range plan.Assignments {
			if !t.IsIdle(session) {
				if !quotaJSON {
					fmt.Printf(" %s %-25s %s\n",
						style.Dim.Render("-"), session,
						style.Dim.Render("skipped (busy)"))
				}
				delete(plan.Assignments, session)
				skippedBusy++
			}
		}
		if len(plan.Assignments) == 0 {
			if quotaJSON {
				return json.NewEncoder(os.Stdout).Encode([]quota.RotateResult{})
			}
			fmt.Printf("\n %s No idle sessions to rotate\n", style.WarningPrefix)
			return nil
		}
	}

	// Sort sessions for deterministic output
	sortedSessions := slices.Sorted(maps.Keys(plan.Assignments))

	// Show plan (text only — skip for JSON consumers)
	if !quotaJSON {
		fmt.Println(style.Bold.Render("Rotation Plan"))
		fmt.Println()
		for _, session := range sortedSessions {
			newAccount := plan.Assignments[session]
			var oldAccount string
			for _, r := range plan.LimitedSessions {
				if r.Session == session {
					oldAccount = r.AccountHandle
					break
				}
			}
			if oldAccount == "" {
				oldAccount = "(unknown)"
			}
			fmt.Printf(" %s %-25s %s → %s\n",
				style.ArrowPrefix, session,
				style.Dim.Render(oldAccount),
				style.Success.Render(newAccount),
			)
		}
		if noConfigDir > 0 {
			fmt.Printf("\n %s %d session(s) skipped (no CLAUDE_CONFIG_DIR)\n",
				style.WarningPrefix, noConfigDir)
		}
		if unassignable > 0 {
			fmt.Printf(" %s %d session(s) cannot be rotated (not enough available accounts)\n",
				style.WarningPrefix, unassignable)
		}
		if len(plan.SkippedAccounts) > 0 {
			fmt.Println()
			for handle, reason := range plan.SkippedAccounts {
				acct := acctCfg.Accounts[handle]
				fmt.Printf(" %s Skipped %s — %s\n", style.WarningPrefix, handle, reason)
				fmt.Printf("   Run: claude /login  (in CLAUDE_CONFIG_DIR=%s)\n", acct.ConfigDir)
			}
		}
	}

	if rotateDryRun {
		if quotaJSON {
			// Return plan as JSON for machine consumers
			enc := json.NewEncoder(os.Stdout)
			enc.SetIndent("", "  ")
			return enc.Encode(plan)
		}
		fmt.Println()
		fmt.Println(style.Dim.Render(" (dry run — no changes made)"))
		return nil
	}

	// Execute rotation in-process via the shared RotationExecutor. Same
	// code path the daemon's quota_dog uses — keychain swap, respawn, and
	// state persist all run on this goroutine, with audit events emitted
	// per result so the dashboard sees the same signal regardless of who
	// drove the rotation.
	if !quotaJSON {
		fmt.Println()
	}
	executor := handlers.NewRotationExecutor(handlers.RotationExecutorConfig{
		Tmux:     t,
		Manager:  mgr,
		Accounts: acctCfg,
	})
	results := executor.Execute(cmd.Context(), plan)
	audit := handlers.AuditEmitter{}
	for _, result := range results {
		nowUTC := time.Now().UTC()
		if result.Rotated {
			audit.Emit(qe.SessionRespawned{
				Session:      result.Session,
				FromAccount:  result.OldAccount,
				ToAccount:    result.NewAccount,
				Resumed:      result.ResumedSession != "",
				KeychainSwap: result.KeychainSwap,
				At:           nowUTC,
			})
		} else if result.Error != "" {
			audit.Emit(qe.RotationFailed{
				Session:   result.Session,
				ToAccount: result.NewAccount,
				Reason:    result.Error,
				At:        nowUTC,
			})
		}

		if quotaJSON {
			continue
		}
		if result.Rotated {
			suffix := ""
			if result.ResumedSession != "" {
				suffix = style.Dim.Render(" (resumed)")
			}
			if result.KeychainSwap {
				suffix += style.Dim.Render(" [keychain]")
			}
			fmt.Printf(" %s %s → %s%s\n", style.SuccessPrefix, result.Session, result.NewAccount, suffix)
		} else if result.Error != "" {
			fmt.Printf(" %s %s: %s\n", style.ErrorPrefix, result.Session, result.Error)
		}
	}

	if quotaJSON {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(results)
	}

	return nil
}

var quotaClearCmd = &cobra.Command{
	Use:   "clear [handle...]",
	Short: "Mark account(s) as available again",
	Long: `Clear the rate-limited status for one or more accounts, marking them available.

When no handles are specified, all limited accounts are cleared.

Examples:
  gt quota clear              # Clear all limited accounts
  gt quota clear work         # Clear a specific account
  gt quota clear work personal`,
	RunE: runQuotaClear,
}

func runQuotaClear(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil {
		return fmt.Errorf("finding town root: %w", err)
	}

	mgr := quota.NewManager(townRoot)

	if len(args) == 0 {
		// Clear all limited accounts
		state, err := mgr.Load()
		if err != nil {
			return fmt.Errorf("loading quota state: %w", err)
		}
		cleared := 0
		for handle, acctState := range state.Accounts {
			if acctState.Status == config.QuotaStatusLimited || acctState.Status == config.QuotaStatusCooldown {
				if err := mgr.MarkAvailable(handle); err != nil {
					return fmt.Errorf("clearing %s: %w", handle, err)
				}
				fmt.Printf(" %s %s → available\n", style.SuccessPrefix, handle)
				cleared++
			}
		}
		if cleared == 0 {
			fmt.Printf(" %s No limited accounts to clear\n", style.SuccessPrefix)
		}
		return nil
	}

	for _, handle := range args {
		if err := mgr.MarkAvailable(handle); err != nil {
			return fmt.Errorf("clearing %s: %w", handle, err)
		}
		fmt.Printf(" %s %s → available\n", style.SuccessPrefix, handle)
	}
	return nil
}

// Probe command flags
var (
	probeAll   bool
	probeModel string
	probeLead  time.Duration
)

var quotaProbeCmd = &cobra.Command{
	Use:   "probe [handle...]",
	Short: "Actively test whether limited accounts are usable again",
	Long: `Probe rate-limited accounts to validate they are usable again.

The provider's shown reset time is imprecise, so rather than trust it, this
runs a tiny headless 'claude --print' against each limited account's own config
dir and inspects the result. A rate-limit signature means still limited; a clean
completion means the account is back — it is marked available and a
quota_reactivated event is emitted.

By default only accounts that are "due" are probed: those whose shown reset time
is within the lead window (or already past), plus any with no parseable reset
time. Use --all to probe every limited account regardless of timing.

Examples:
  gt quota probe              # Probe all due limited accounts
  gt quota probe work         # Probe a specific account (ignores gating)
  gt quota probe --all        # Probe every limited account now
  gt quota probe --json       # JSON output`,
	RunE: runQuotaProbe,
}

func runQuotaProbe(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil {
		return fmt.Errorf("finding town root: %w", err)
	}

	acctCfg, err := config.LoadAccountsConfig(constants.MayorAccountsPath(townRoot))
	if err != nil {
		return fmt.Errorf("loading accounts config: %w", err)
	}

	claudePath, err := handlers.ResolveClaudeBinary()
	if err != nil {
		return fmt.Errorf("claude binary not found in PATH (required for probing): %w", err)
	}

	mgr := quota.NewManager(townRoot)
	runner := quota.NewClaudeProbeRunner(claudePath, probeModel, quota.DefaultProbePrompt, quota.DefaultProbeTimeout)

	executor := handlers.NewProbeExecutor(handlers.ProbeExecutorConfig{
		Manager:  mgr,
		Accounts: acctCfg,
		Runner:   runner,
		Lead:     probeLead,
		All:      probeAll,
		Emit: func(handle, prevResetsAt string) {
			_ = events.LogFeed(events.TypeQuotaReactivated, "quota",
				events.QuotaReactivatedPayload(handle, prevResetsAt))
		},
	})

	reports, err := executor.Probe(cmd.Context(), args)
	if err != nil {
		return err
	}

	if quotaJSON {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(reports)
	}
	return printProbeText(reports)
}

func printProbeText(reports []handlers.ProbeReport) error {
	reactivated := 0
	probed := 0
	for _, r := range reports {
		switch {
		case r.Reactivated:
			reactivated++
			probed++
			fmt.Printf(" %s %-20s → available\n", style.SuccessPrefix, r.Handle)
		case r.StillLimited:
			probed++
			detail := ""
			if r.Reason != "" {
				detail = style.Dim.Render(" (" + r.Reason + ")")
			}
			fmt.Printf(" %s %-20s still limited%s\n", style.Warning.Render("~"), r.Handle, detail)
		case r.Skipped:
			fmt.Printf(" %s %-20s %s\n", style.Dim.Render("·"), r.Handle, style.Dim.Render(r.Reason))
		}
	}
	if probed == 0 {
		fmt.Printf(" %s No limited accounts due for probing\n", style.SuccessPrefix)
	} else {
		fmt.Println()
		fmt.Printf(" %s probed %d, reactivated %d\n", style.Bold.Render("Summary:"), probed, reactivated)
	}
	return nil
}

// accountHandles returns sorted account handle names for error messages.
func accountHandles(acctCfg *config.AccountsConfig) []string {
	handles := make([]string, 0, len(acctCfg.Accounts))
	for h := range acctCfg.Accounts {
		handles = append(handles, h)
	}
	slices.Sort(handles)
	return handles
}

// Watch command flags
var (
	watchInterval time.Duration
	watchDryRun   bool
)

var quotaWatchCmd = &cobra.Command{
	Use:   "watch",
	Short: "Monitor sessions and rotate proactively before hard 429",
	Long: `Continuously monitor sessions for approaching rate limits and rotate proactively.

Polls all Gas Town sessions on the specified interval, checking for both
hard rate limits and near-limit warning signals via pane pattern matching.

When a session is detected as approaching its limit, rotation is triggered
before the hard 429 hits.

Examples:
  gt quota watch                      # Watch with default 5m interval
  gt quota watch --interval 2m        # Custom interval
  gt quota watch --dry-run            # Show detections without rotating`,
	RunE: runQuotaWatch,
}

func runQuotaWatch(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil {
		return fmt.Errorf("finding town root: %w", err)
	}

	accountsPath := constants.MayorAccountsPath(townRoot)
	acctCfg, err := config.LoadAccountsConfig(accountsPath)
	if err != nil {
		return fmt.Errorf("no accounts configured: %w", err)
	}
	if len(acctCfg.Accounts) < 2 {
		return fmt.Errorf("need at least 2 accounts for rotation (have %d)", len(acctCfg.Accounts))
	}

	fmt.Printf(" %s Watching for near-limit signals (interval: %s)\n",
		style.Info.Render("Watch:"), watchInterval)
	if watchDryRun {
		fmt.Println(style.Dim.Render(" (dry run — detections only, no rotation)"))
	}
	fmt.Println()

	// Handle graceful shutdown on SIGTERM/SIGINT
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGTERM, syscall.SIGINT)

	ticker := time.NewTicker(watchInterval)
	defer ticker.Stop()

	// Run immediately on start, then on each tick
	for {
		runWatchCycle(townRoot, acctCfg)

		select {
		case <-sigCh:
			fmt.Printf("\n %s Shutting down watch\n", style.Info.Render("Watch:"))
			return nil
		case <-ticker.C:
		}
	}
}

func runWatchCycle(townRoot string, acctCfg *config.AccountsConfig) {
	t := ttmux.NewTmux()
	scanner, err := quota.NewScanner(t, nil, acctCfg)
	if err != nil {
		style.PrintWarning("creating scanner: %v", err)
		return
	}

	// Enable near-limit detection via pane patterns
	if err := scanner.WithWarningPatterns(nil); err != nil {
		style.PrintWarning("setting warning patterns: %v", err)
		return
	}

	mgr := quota.NewManager(townRoot)

	// Sync swapped tokens: if a source account re-authenticated since the
	// last rotation, propagate the fresh token to all target keychain entries.
	if state, err := mgr.Load(); err == nil && len(state.ActiveSwaps) > 0 {
		resolved := quota.ResolveSwapSourceDirs(state.ActiveSwaps, acctCfg.Accounts)
		if n := quota.SyncSwappedTokens(resolved); n > 0 {
			now := time.Now().Format("15:04:05")
			fmt.Printf(" [%s] %s synced %d swapped keychain(s)\n",
				style.Dim.Render(now),
				style.Info.Render("Sync:"),
				n)
		}
	}

	plan, err := quota.PlanRotation(scanner, mgr, acctCfg, quota.PlanOpts{IncludeNearLimit: true})
	if err != nil {
		style.PrintWarning("planning rotation: %v", err)
		return
	}

	if err := persistPlanArtifacts(mgr, plan); err != nil {
		style.PrintWarning("could not persist plan artifacts: %v", err)
	}
	emitPlanEvents(plan)

	// Persist a LimitedSessions snapshot from the watch scan so the dashboard
	// can render "waiting for unlock" without re-scanning tmux.
	if err := mgr.WithLock(func() error {
		state, loadErr := mgr.Load()
		if loadErr != nil {
			return loadErr
		}
		all := append([]quota.ScanResult{}, plan.LimitedSessions...)
		all = append(all, plan.NearLimitSessions...)
		recordScanSnapshot(state, all, time.Now().UTC().Format(time.RFC3339))
		return mgr.SaveUnlocked(state)
	}); err != nil {
		style.PrintWarning("could not persist scan snapshot: %v", err)
	}

	// Rotation-blocked detection: limited sessions exist but no plan assignments
	// were produced (typically because every account had an expired token).
	// Emit a throttled escalation so operators see it on the dashboard / via mail.
	if len(plan.LimitedSessions) > 0 && len(plan.Assignments) == 0 {
		maybeEmitRotationBlockedAlert(mgr, plan)
	}

	// Report findings
	now := time.Now().Format("15:04:05")
	totalTargets := len(plan.LimitedSessions) + len(plan.NearLimitSessions)
	if totalTargets == 0 {
		fmt.Printf(" [%s] %s\n", style.Dim.Render(now), style.Dim.Render("all clear"))
		return
	}

	for _, r := range plan.LimitedSessions {
		fmt.Printf(" [%s] %s %-25s %s\n",
			style.Dim.Render(now),
			style.Error.Render("LIMITED"),
			r.Session,
			style.Dim.Render(r.AccountHandle))
	}
	for _, r := range plan.NearLimitSessions {
		detail := ""
		if r.MatchedLine != "" {
			detail = fmt.Sprintf(" (%s)", r.MatchedLine)
		}
		fmt.Printf(" [%s] %s %-25s %s%s\n",
			style.Dim.Render(now),
			style.Warning.Render("NEAR"),
			r.Session,
			style.Dim.Render(r.AccountHandle),
			style.Dim.Render(detail))
	}

	if watchDryRun || len(plan.Assignments) == 0 {
		return
	}

	// Execute rotation via the shared in-process executor.
	executor := handlers.NewRotationExecutor(handlers.RotationExecutorConfig{
		Tmux:     t,
		Manager:  mgr,
		Accounts: acctCfg,
	})
	results := executor.Execute(context.Background(), plan)
	audit := handlers.AuditEmitter{}
	for _, result := range results {
		nowUTC := time.Now().UTC()
		if result.Rotated {
			audit.Emit(qe.SessionRespawned{
				Session:      result.Session,
				FromAccount:  result.OldAccount,
				ToAccount:    result.NewAccount,
				Resumed:      result.ResumedSession != "",
				KeychainSwap: result.KeychainSwap,
				At:           nowUTC,
			})
			fmt.Printf(" [%s] %s %s → %s\n",
				style.Dim.Render(now),
				style.SuccessPrefix,
				result.Session,
				style.Success.Render(result.NewAccount))
		} else if result.Error != "" {
			audit.Emit(qe.RotationFailed{
				Session:   result.Session,
				ToAccount: result.NewAccount,
				Reason:    result.Error,
				At:        nowUTC,
			})
			fmt.Printf(" [%s] %s %s: %s\n",
				style.Dim.Render(now),
				style.ErrorPrefix,
				result.Session,
				result.Error)
		}
	}
}

// maybeEmitRotationBlockedAlert emits a `gt escalate` once per cooldown window
// when account rotation is wedged (limited sessions exist but no available
// accounts to rotate to). Tracks the last emission in quota.json so the
// alert fires once per incident, not once per polling tick.
func maybeEmitRotationBlockedAlert(mgr *quota.Manager, plan *quota.RotatePlan) {
	state, err := mgr.Load()
	if err != nil {
		style.PrintWarning("quota alert: load state: %v", err)
		return
	}

	nowTime := time.Now().UTC()
	if state.LastBlockedAlertAt != "" {
		if last, parseErr := time.Parse(time.RFC3339, state.LastBlockedAlertAt); parseErr == nil {
			if nowTime.Sub(last) < quotaBlockedAlertCooldown {
				return // still inside cooldown window
			}
		}
	}

	// Build escalation reason from the plan
	sessions := make([]string, 0, len(plan.LimitedSessions))
	for _, s := range plan.LimitedSessions {
		sessions = append(sessions, s.Session)
	}
	sort.Strings(sessions)

	_ = events.LogFeed(events.TypeQuotaBlocked, "quota",
		events.QuotaBlockedPayload(sessions, plan.SkippedAccounts))

	skippedHandles := make([]string, 0, len(plan.SkippedAccounts))
	for h := range plan.SkippedAccounts {
		skippedHandles = append(skippedHandles, h)
	}
	sort.Strings(skippedHandles)

	var reason strings.Builder
	fmt.Fprintf(&reason, "Account rotation blocked: %d session(s) rate-limited, 0 available accounts.\n", len(sessions))
	if len(sessions) > 0 {
		fmt.Fprintf(&reason, "Limited sessions: %s\n", strings.Join(sessions, ", "))
	}
	if len(skippedHandles) > 0 {
		fmt.Fprintln(&reason, "Skipped accounts (reason):")
		for _, h := range skippedHandles {
			fmt.Fprintf(&reason, "  - %s: %s\n", h, plan.SkippedAccounts[h])
		}
	}
	fmt.Fprintln(&reason, "")
	fmt.Fprintln(&reason, "Fix: re-authenticate at least one account via `claude /login` in its config dir,")
	fmt.Fprintln(&reason, "then `gt quota rotate` to recover blocked sessions.")

	title := fmt.Sprintf("Quota rotation blocked: %d limited session(s), no available accounts", len(sessions))
	args := []string{
		"escalate", title,
		"--severity", "high",
		"--source", "quota:watch",
		"--stdin",
	}
	cmd := exec.Command("gt", args...) //nolint:gosec // G204: gt is a trusted internal tool
	cmd.Stdin = strings.NewReader(reason.String())
	if out, runErr := cmd.CombinedOutput(); runErr != nil {
		style.PrintWarning("quota alert: emit escalation failed: %v\n%s", runErr, string(out))
		return
	}

	// Stamp the emission so we don't fire again until cooldown elapses
	state.LastBlockedAlertAt = nowTime.Format(time.RFC3339)
	if saveErr := mgr.Save(state); saveErr != nil {
		style.PrintWarning("quota alert: persist timestamp: %v", saveErr)
	}

	stamp := time.Now().Format("15:04:05")
	fmt.Printf(" [%s] %s rotation blocked — emitted escalation\n",
		style.Dim.Render(stamp),
		style.Warning.Render("ALERT"))
}

func init() {
	quotaStatusCmd.Flags().BoolVar(&quotaJSON, "json", false, "Output as JSON")

	quotaScanCmd.Flags().BoolVar(&quotaJSON, "json", false, "Output as JSON")
	quotaScanCmd.Flags().BoolVar(&scanUpdate, "update", false, "Update quota state with detected limits")

	quotaRotateCmd.Flags().BoolVar(&rotateDryRun, "dry-run", false, "Show plan without executing")
	quotaRotateCmd.Flags().BoolVar(&quotaJSON, "json", false, "Output as JSON")
	quotaRotateCmd.Flags().StringVar(&rotateFrom, "from", "", "Preemptively rotate sessions using this account")
	quotaRotateCmd.Flags().BoolVar(&rotateIdle, "idle", false, "Only rotate sessions at the idle prompt (skip busy agents)")

	quotaWatchCmd.Flags().DurationVar(&watchInterval, "interval", 5*time.Minute, "Poll interval")
	quotaWatchCmd.Flags().BoolVar(&watchDryRun, "dry-run", false, "Show detections without executing rotation")

	quotaProbeCmd.Flags().BoolVar(&quotaJSON, "json", false, "Output as JSON")
	quotaProbeCmd.Flags().BoolVar(&probeAll, "all", false, "Probe every limited account, ignoring the reset-window gate")
	quotaProbeCmd.Flags().StringVar(&probeModel, "model", quota.DefaultProbeModel, "Model to use for the probe (empty = account default)")
	quotaProbeCmd.Flags().DurationVar(&probeLead, "lead", quota.DefaultProbeLeadWindow, "Begin probing this long before the shown reset time")

	quotaCmd.AddCommand(quotaStatusCmd)
	quotaCmd.AddCommand(quotaScanCmd)
	quotaCmd.AddCommand(quotaRotateCmd)
	quotaCmd.AddCommand(quotaClearCmd)
	quotaCmd.AddCommand(quotaWatchCmd)
	quotaCmd.AddCommand(quotaProbeCmd)

	rootCmd.AddCommand(quotaCmd)
}
