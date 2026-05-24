package web

import (
	"encoding/json"
	"log"
	"net/http"
	"sort"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/tmux"
	"github.com/steveyegge/gastown/internal/workspace"
)

// QuotaSummaryAccount is the per-account view returned from /api/quota/summary.
// It folds quota state, the most recent token-inspection result, and live
// token-usage counts into a single record consumed by the dashboard mosaic.
type QuotaSummaryAccount struct {
	Handle           string  `json:"handle"`
	Email            string  `json:"email,omitempty"`
	IsDefault        bool    `json:"is_default,omitempty"`
	Status           string  `json:"status"`
	LimitedAt        string  `json:"limited_at,omitempty"`
	ResetsAt         string  `json:"resets_at,omitempty"`
	// UnlocksAt is ResetsAt parsed into an RFC3339 instant (when available).
	// Populated only for limited/cooldown accounts whose ResetsAt parses to a
	// future time — lets the UI render a live countdown instead of the raw
	// "7pm (America/Los_Angeles)" string.
	UnlocksAt        string  `json:"unlocks_at,omitempty"`
	LastUsed         string  `json:"last_used,omitempty"`
	TokenExpiresAt   string  `json:"token_expires_at,omitempty"`
	TokenLastChecked string  `json:"token_last_checked,omitempty"`
	RotationCount    int     `json:"rotation_count,omitempty"`
	LastRotatedAt    string  `json:"last_rotated_at,omitempty"`
	ActiveSessions   []string `json:"active_sessions,omitempty"`
	Usage            *quota.AccountUsage `json:"usage,omitempty"`
}

// QuotaSummaryCounters totals accounts by status so the dashboard's collapsed
// statbar can render `2● 1✕ 1⊘` without iterating Accounts.
type QuotaSummaryCounters struct {
	Available int `json:"available"`
	Limited   int `json:"limited"`
	Expired   int `json:"expired"`
	Cooldown  int `json:"cooldown"`
}

// QuotaSummaryResponse is the body returned from GET /api/quota/summary.
type QuotaSummaryResponse struct {
	GeneratedAt        string                                 `json:"generated_at"`
	Counters           QuotaSummaryCounters                   `json:"counters"`
	Accounts           []QuotaSummaryAccount                  `json:"accounts"`
	LimitedSessions    map[string]config.LimitedSessionState `json:"limited_sessions,omitempty"`
	LastPlan           *config.RotationPlanSnapshot          `json:"last_plan,omitempty"`
	LastBlockedAlertAt string                                 `json:"last_blocked_alert_at,omitempty"`
	OrphanSessions     []quota.SessionUsage                   `json:"orphan_sessions,omitempty"`
	// UsageError carries the failure reason when usage aggregation
	// couldn't walk transcripts (tmux down, claude config root missing,
	// etc.). The dashboard surfaces this so an empty bar is explained.
	UsageError         string                                 `json:"usage_error,omitempty"`
}

// handleQuotaSummary returns a single JSON snapshot covering every dimension
// the mosaic panel needs: per-account status + token expiry + rotation
// counters + live token usage, plus the current LimitedSessions snapshot and
// the most recent rotation plan.
func (h *APIHandler) handleQuotaSummary(w http.ResponseWriter, r *http.Request) {
	townRoot, err := workspace.FindFromCwdOrError()
	if err != nil {
		h.sendError(w, "town root not found: "+err.Error(), http.StatusInternalServerError)
		return
	}

	acctCfg, err := config.LoadAccountsConfig(constants.MayorAccountsPath(townRoot))
	if err != nil {
		acctCfg = &config.AccountsConfig{Accounts: map[string]config.Account{}}
	}

	mgr := quota.NewManager(townRoot)
	state, err := mgr.Load()
	if err != nil {
		h.sendError(w, "loading quota state: "+err.Error(), http.StatusInternalServerError)
		return
	}
	mgr.EnsureAccountsTracked(state, acctCfg.Accounts)

	// Re-inspect each account's on-disk credentials so a fresh `gt account
	// login` reflects in the next summary fetch without waiting for a
	// scan/rotate cycle to update TokenExpiresAt. Persist only when
	// something changed so we don't write quota.json on every poll.
	if quota.RefreshTokenExpiries(state, acctCfg.Accounts) {
		if perr := mgr.Save(state); perr != nil {
			log.Printf("quota summary: persist refreshed token expiries: %v", perr)
		}
	}

	// Best-effort usage aggregation; failures degrade to "no usage data"
	// rather than failing the whole response — the dashboard should still
	// render status/expiry even if transcript walking trips. The error is
	// surfaced to the client (as resp.UsageError) and logged so an operator
	// looking at an empty bar can see why.
	var usageReport *quota.UsageReport
	var usageErr string
	t := tmux.NewTmux()
	if report, uerr := quota.AggregateUsage(t, state, acctCfg, "", time.Now()); uerr == nil {
		usageReport = report
	} else {
		usageErr = uerr.Error()
		log.Printf("quota summary: usage aggregation failed: %v", uerr)
	}

	resp := BuildQuotaSummary(state, acctCfg, usageReport, time.Now())
	resp.UsageError = usageErr
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(resp); err != nil {
		// Header is already sent; just log.
		// (sendError can't downgrade after WriteHeader.)
		return
	}
}

// BuildQuotaSummary is the pure assembly step, split out for unit testing and
// reused by the `gt quota status --json` CLI command so both surfaces emit an
// identical observability shape.
func BuildQuotaSummary(
	state *config.QuotaState,
	acctCfg *config.AccountsConfig,
	usageReport *quota.UsageReport,
	now time.Time,
) QuotaSummaryResponse {
	resp := QuotaSummaryResponse{
		GeneratedAt:        now.UTC().Format(time.RFC3339),
		LimitedSessions:    state.LimitedSessions,
		LastPlan:           state.LastPlan,
		LastBlockedAlertAt: state.LastBlockedAlertAt,
	}

	handles := make([]string, 0, len(acctCfg.Accounts))
	for h := range acctCfg.Accounts {
		handles = append(handles, h)
	}
	sort.Strings(handles)

	sessionsByAccount := map[string][]string{}
	if state.LimitedSessions != nil {
		for sess, info := range state.LimitedSessions {
			if info.Account == "" {
				continue
			}
			sessionsByAccount[info.Account] = append(sessionsByAccount[info.Account], sess)
		}
		for _, list := range sessionsByAccount {
			sort.Strings(list)
		}
	}

	for _, handle := range handles {
		acct := acctCfg.Accounts[handle]
		qs := state.Accounts[handle]
		status := string(qs.Status)
		if status == "" {
			status = string(config.QuotaStatusAvailable)
		}

		// Reclassify status when the parsed token expiry has elapsed. Lets
		// the dashboard render "expired" without waiting for the next
		// rotation attempt to flip the account into the skipped pool.
		if qs.TokenExpiresAt != "" {
			if exp, err := time.Parse(time.RFC3339, qs.TokenExpiresAt); err == nil && now.After(exp) {
				status = "expired"
			}
		}

		switch status {
		case string(config.QuotaStatusAvailable):
			resp.Counters.Available++
		case string(config.QuotaStatusLimited):
			resp.Counters.Limited++
		case string(config.QuotaStatusCooldown):
			resp.Counters.Cooldown++
		case "expired":
			resp.Counters.Expired++
		}

		entry := QuotaSummaryAccount{
			Handle:           handle,
			Email:            acct.Email,
			IsDefault:        handle == acctCfg.Default,
			Status:           status,
			LimitedAt:        qs.LimitedAt,
			ResetsAt:         qs.ResetsAt,
			LastUsed:         qs.LastUsed,
			TokenExpiresAt:   qs.TokenExpiresAt,
			TokenLastChecked: qs.TokenLastChecked,
			RotationCount:    qs.RotationCount,
			LastRotatedAt:    qs.LastRotatedAt,
			ActiveSessions:   sessionsByAccount[handle],
		}

		// Parse the human-readable ResetsAt ("7pm", "3:30pm (America/...)") into
		// an absolute instant so the UI can render a live countdown. Only emit
		// when the parsed time is still ahead of `now` — a past parse means the
		// next auto-clear sweep will flip the account to available anyway, and
		// surfacing a negative countdown would confuse the user.
		if qs.ResetsAt != "" && (status == string(config.QuotaStatusLimited) || status == string(config.QuotaStatusCooldown)) {
			if t, err := quota.ParseResetTime(qs.ResetsAt, now); err == nil && t.After(now) {
				entry.UnlocksAt = t.UTC().Format(time.RFC3339)
			}
		}
		if usageReport != nil {
			if u, ok := usageReport.Accounts[handle]; ok {
				cp := u
				entry.Usage = &cp
			}
		}
		resp.Accounts = append(resp.Accounts, entry)
	}

	if usageReport != nil && len(usageReport.OrphanSessions) > 0 {
		resp.OrphanSessions = usageReport.OrphanSessions
	}

	return resp
}
