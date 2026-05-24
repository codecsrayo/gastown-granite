package web

import (
	"encoding/json"
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

	// Best-effort usage aggregation; failures degrade to "no usage data"
	// rather than failing the whole response — the dashboard should still
	// render status/expiry even if transcript walking trips.
	var usageReport *quota.UsageReport
	t := tmux.NewTmux()
	if report, uerr := quota.AggregateUsage(t, state, acctCfg, "", time.Now()); uerr == nil {
		usageReport = report
	}

	resp := buildQuotaSummary(state, acctCfg, usageReport, time.Now())
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(resp); err != nil {
		// Header is already sent; just log.
		// (sendError can't downgrade after WriteHeader.)
		return
	}
}

// buildQuotaSummary is the pure assembly step, split out for unit testing.
func buildQuotaSummary(
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
