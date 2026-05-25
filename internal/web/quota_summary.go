package web

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"sort"
	"time"

	"github.com/fsnotify/fsnotify"
	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/quota"
	"github.com/steveyegge/gastown/internal/tmux"
	"github.com/steveyegge/gastown/internal/util"
	"github.com/steveyegge/gastown/internal/workspace"
)

// quotaStreamTickInterval governs how often the stream re-aggregates token
// usage. Transcripts grow without touching quota.json, so the periodic tick is
// the only signal that catches in-flight token consumption. Snapshots whose
// content matches the previously sent payload are suppressed by hash dedupe,
// so an idle stream costs one map walk per tick, not one network frame.
const quotaStreamTickInterval = 3 * time.Second

// quotaStreamKeepaliveInterval is the comment-frame heartbeat used to keep
// idle proxies (nginx, ALB) from reaping the SSE connection.
const quotaStreamKeepaliveInterval = 15 * time.Second

// QuotaSummaryAccount is the per-account view returned from /api/quota/summary.
// It folds quota state, the most recent token-inspection result, and live
// token-usage counts into a single record consumed by the dashboard mosaic.
type QuotaSummaryAccount struct {
	Handle    string `json:"handle"`
	Email     string `json:"email,omitempty"`
	IsDefault bool   `json:"is_default,omitempty"`
	Status    string `json:"status"`
	LimitedAt string `json:"limited_at,omitempty"`
	ResetsAt  string `json:"resets_at,omitempty"`
	// UnlocksAt is ResetsAt parsed into an RFC3339 instant (when available).
	// Populated only for limited/cooldown accounts whose ResetsAt parses to a
	// future time — lets the UI render a live countdown instead of the raw
	// "7pm (America/Los_Angeles)" string.
	UnlocksAt        string              `json:"unlocks_at,omitempty"`
	LastUsed         string              `json:"last_used,omitempty"`
	TokenExpiresAt   string              `json:"token_expires_at,omitempty"`
	TokenLastChecked string              `json:"token_last_checked,omitempty"`
	RotationCount    int                 `json:"rotation_count,omitempty"`
	LastRotatedAt    string              `json:"last_rotated_at,omitempty"`
	ActiveSessions   []string            `json:"active_sessions,omitempty"`
	Usage            *quota.AccountUsage `json:"usage,omitempty"`
	// SwappedTo is the source account handle whose token currently occupies
	// this account's config dir via a quota rotation swap. When set, the
	// account is running another identity — TokenExpiresAt reflects this
	// account's own last-known token, not the borrowed one. Lets the dashboard
	// label the card "running as <source>" instead of implying a fresh login.
	SwappedTo string `json:"swapped_to,omitempty"`
}

// QuotaSummaryCounters totals accounts by status so the dashboard's collapsed
// statbar can render `2● 1✕ 1⊘` without iterating Accounts.
type QuotaSummaryCounters struct {
	Available int `json:"available"`
	Limited   int `json:"limited"`
	Expired   int `json:"expired"`
	Cooldown  int `json:"cooldown"`
}

// QuotaUsageCeilings carries the configured (estimated) token budgets the
// dashboard uses to render "remaining until block" bars. These are NOT
// Anthropic's enforced limit — that is opaque and weighted per model — but an
// operator-tunable visual aid sourced from TownSettings.QuotaUsage.
type QuotaUsageCeilings struct {
	SessionTokenCeiling int64 `json:"session_token_ceiling"`
	WeeklyTokenCeiling  int64 `json:"weekly_token_ceiling"`
}

// QuotaSummaryResponse is the body returned from GET /api/quota/summary.
type QuotaSummaryResponse struct {
	GeneratedAt        string                                `json:"generated_at"`
	Counters           QuotaSummaryCounters                  `json:"counters"`
	Accounts           []QuotaSummaryAccount                 `json:"accounts"`
	UsageCeilings      QuotaUsageCeilings                    `json:"usage_ceilings"`
	LimitedSessions    map[string]config.LimitedSessionState `json:"limited_sessions,omitempty"`
	LastPlan           *config.RotationPlanSnapshot          `json:"last_plan,omitempty"`
	LastBlockedAlertAt string                                `json:"last_blocked_alert_at,omitempty"`
	OrphanSessions     []quota.SessionUsage                  `json:"orphan_sessions,omitempty"`
	// UsageError carries the failure reason when usage aggregation
	// couldn't walk transcripts (tmux down, claude config root missing,
	// etc.). The dashboard surfaces this so an empty bar is explained.
	UsageError string `json:"usage_error,omitempty"`
}

// computeQuotaSnapshot bundles the work that turns the on-disk quota state
// plus a fresh tmux/transcript walk into a single QuotaSummaryResponse. Split
// out from the SSE handler so the streamer can call it on a ticker without
// duplicating the load/refresh/aggregate plumbing.
func computeQuotaSnapshot(townRoot string, now time.Time) (QuotaSummaryResponse, error) {
	acctCfg, err := config.LoadAccountsConfig(constants.MayorAccountsPath(townRoot))
	if err != nil {
		acctCfg = &config.AccountsConfig{Accounts: map[string]config.Account{}}
	}

	mgr := quota.NewManager(townRoot)
	state, err := mgr.Load()
	if err != nil {
		return QuotaSummaryResponse{}, fmt.Errorf("loading quota state: %w", err)
	}
	mgr.EnsureAccountsTracked(state, acctCfg.Accounts)

	if quota.RefreshTokenExpiries(state, acctCfg.Accounts) {
		if perr := mgr.Save(state); perr != nil {
			log.Printf("quota stream: persist refreshed token expiries: %v", perr)
		}
	}

	var usageReport *quota.UsageReport
	var usageErr string
	t := tmux.NewTmux()
	if report, uerr := quota.AggregateUsage(t, state, acctCfg, "", now); uerr == nil {
		usageReport = report
	} else {
		usageErr = uerr.Error()
		log.Printf("quota stream: usage aggregation failed: %v", uerr)
	}

	// Overlay per-config-dir walk for authoritative totals. AggregateUsage is
	// tmux-driven and misses transcripts whose owning session isn't currently
	// attached, so its totals can read 0 even when the on-disk transcripts
	// (what `claude /status` reports) carry significant usage. The walker
	// scans every account's config dir directly; we keep the tmux walker's
	// Sessions[] + OrphanSessions but override Counts/WeekCounts.
	if walkReport, werr := quota.WalkAccountUsage(acctCfg, state, now); werr == nil && walkReport != nil {
		if usageReport == nil {
			usageReport = walkReport
		} else {
			for handle, walkEntry := range walkReport.Accounts {
				existing, ok := usageReport.Accounts[handle]
				if !ok {
					usageReport.Accounts[handle] = walkEntry
					continue
				}
				existing.Counts = walkEntry.Counts
				existing.WeekCounts = walkEntry.WeekCounts
				existing.WindowStart = walkEntry.WindowStart
				existing.WeekWindowStart = walkEntry.WeekWindowStart
				usageReport.Accounts[handle] = existing
			}
		}
	} else if werr != nil {
		log.Printf("quota stream: per-account walk failed: %v", werr)
	}

	resp := BuildQuotaSummary(state, acctCfg, usageReport, ResolveUsageCeilings(townRoot), now)
	resp.UsageError = usageErr
	return resp, nil
}

// snapshotHash returns a content fingerprint of the snapshot that ignores
// GeneratedAt — that field ticks every call and would defeat the dedupe.
func snapshotHash(resp QuotaSummaryResponse) (string, []byte, error) {
	resp.GeneratedAt = ""
	buf, err := json.Marshal(resp)
	if err != nil {
		return "", nil, err
	}
	sum := sha256.Sum256(buf)
	return hex.EncodeToString(sum[:]), buf, nil
}

// handleQuotaStream streams QuotaSummaryResponse snapshots to the dashboard
// over Server-Sent Events. It emits one snapshot on connect, then re-emits on
// either of two triggers:
//
//   - mayor/quota.json writes (fsnotify): rotation, status flips, swap
//     bookkeeping — sub-second push.
//   - quotaStreamTickInterval ticker: catches token consumption from
//     transcripts that never touch quota.json.
//
// Snapshots are deduped by content hash so an idle stream stays quiet on the
// wire. The connection ends when the client disconnects (ctx cancel).
func (h *APIHandler) handleQuotaStream(w http.ResponseWriter, r *http.Request) {
	flusher, ok := w.(http.Flusher)
	if !ok {
		http.Error(w, "SSE not supported", http.StatusInternalServerError)
		return
	}

	townRoot, err := workspace.FindFromCwdOrError()
	if err != nil {
		h.sendError(w, "town root not found: "+err.Error(), http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "text/event-stream")
	w.Header().Set("Cache-Control", "no-cache")
	w.Header().Set("Connection", "keep-alive")
	w.Header().Set("X-Accel-Buffering", "no")

	fmt.Fprintf(w, "event: connected\ndata: ok\n\n")
	flusher.Flush()

	ctx := r.Context()
	streamQuotaSnapshots(ctx, w, flusher, townRoot, quotaStreamTickInterval, quotaStreamKeepaliveInterval)
}

// streamQuotaSnapshots is the loop body of handleQuotaStream, split out so
// tests can drive it with a synthetic town root and short intervals.
func streamQuotaSnapshots(
	ctx context.Context,
	w http.ResponseWriter,
	flusher http.Flusher,
	townRoot string,
	tickEvery, keepaliveEvery time.Duration,
) {
	var lastHash string

	emit := func(reason string) {
		resp, err := computeQuotaSnapshot(townRoot, time.Now())
		if err != nil {
			log.Printf("quota stream: snapshot (%s): %v", reason, err)
			return
		}
		hash, buf, herr := snapshotHash(resp)
		if herr != nil {
			log.Printf("quota stream: hash: %v", herr)
			return
		}
		if hash == lastHash {
			return
		}
		lastHash = hash
		// Re-marshal with the live GeneratedAt so the client sees the wall
		// clock when the frame was sent (snapshotHash zeroed it for dedupe).
		resp.GeneratedAt = time.Now().UTC().Format(time.RFC3339)
		framed, ferr := json.Marshal(resp)
		if ferr != nil {
			// Fallback: send the deduped payload (still valid, just stale ts).
			framed = buf
		}
		fmt.Fprintf(w, "event: quota-snapshot\ndata: %s\n\n", framed)
		flusher.Flush()
	}

	emit("initial")

	// fsnotify on quota.json. Optional: if it can't be installed (file missing,
	// kernel limits), fall back to ticker-only — usage tick will still catch
	// rotation writes within tickEvery.
	var watcherEvents <-chan fsnotify.Event
	if watcher, werr := fsnotify.NewWatcher(); werr == nil {
		defer watcher.Close()
		path := constants.MayorQuotaPath(townRoot)
		if addErr := watcher.Add(path); addErr == nil {
			watcherEvents = watcher.Events
		} else {
			log.Printf("quota stream: watch %s: %v — ticker-only mode", path, addErr)
		}
	} else {
		log.Printf("quota stream: fsnotify watcher: %v — ticker-only mode", werr)
	}

	tick := time.NewTicker(tickEvery)
	defer tick.Stop()
	keepalive := time.NewTicker(keepaliveEvery)
	defer keepalive.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-keepalive.C:
			fmt.Fprintf(w, ": keepalive\n\n")
			flusher.Flush()
		case <-tick.C:
			emit("tick")
		case ev, ok := <-watcherEvents:
			if !ok {
				watcherEvents = nil
				continue
			}
			if ev.Op&(fsnotify.Write|fsnotify.Create) != 0 {
				emit("quota.json")
			}
		}
	}
}

// BuildQuotaSummary is the pure assembly step, split out for unit testing and
// reused by the `gt quota status --json` CLI command so both surfaces emit an
// identical observability shape.
func BuildQuotaSummary(
	state *config.QuotaState,
	acctCfg *config.AccountsConfig,
	usageReport *quota.UsageReport,
	ceilings QuotaUsageCeilings,
	now time.Time,
) QuotaSummaryResponse {
	resp := QuotaSummaryResponse{
		GeneratedAt:        now.UTC().Format(time.RFC3339),
		UsageCeilings:      ceilings,
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

		// Flag accounts whose config dir is currently a quota-swap target so
		// the dashboard shows "running as <source>" instead of attributing the
		// borrowed login state to this account. ActiveSwaps keys are expanded
		// config dirs (see quota.RecordSwap callers).
		if len(state.ActiveSwaps) > 0 {
			if src, ok := state.ActiveSwaps[util.ExpandHome(acct.ConfigDir)]; ok && src != handle {
				entry.SwappedTo = src
			}
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

// ResolveUsageCeilings loads the configured token ceilings from the town's
// settings, falling back to compiled-in defaults for any unset (zero) value.
// A missing or unreadable settings file degrades to all-defaults so the
// dashboard always has a denominator for its usage bars.
func ResolveUsageCeilings(townRoot string) QuotaUsageCeilings {
	d := config.DefaultQuotaUsageConfig()
	out := QuotaUsageCeilings{
		SessionTokenCeiling: d.SessionTokenCeiling,
		WeeklyTokenCeiling:  d.WeeklyTokenCeiling,
	}
	ts, err := config.LoadOrCreateTownSettings(config.TownSettingsPath(townRoot))
	if err != nil || ts == nil || ts.QuotaUsage == nil {
		return out
	}
	if ts.QuotaUsage.SessionTokenCeiling > 0 {
		out.SessionTokenCeiling = ts.QuotaUsage.SessionTokenCeiling
	}
	if ts.QuotaUsage.WeeklyTokenCeiling > 0 {
		out.WeeklyTokenCeiling = ts.QuotaUsage.WeeklyTokenCeiling
	}
	return out
}
