// Package quotaevents defines the typed in-process events for the quota
// rotation pipeline. These events flow on the internal bus and drive the
// chain-of-responsibility handlers in quota/handlers.
//
// They mirror the audit events in internal/events but are typed Go values
// rather than untyped JSONL payloads — the JSONL log is now a sink of the
// bus, not the primary contract.
package quotaevents

import (
	"time"

	"github.com/steveyegge/gastown/internal/quota"
)

// Routing keys used by bus subscribers.
const (
	KindSessionLimited     = "quota.session_limited"
	KindSessionNearLimit   = "quota.session_near_limit"
	KindTokenExpired       = "quota.token_expired"
	KindAccountReactivated = "quota.account_reactivated"
	KindScanCompleted      = "quota.scan_completed"
	KindRotationPlanned    = "quota.rotation_planned"
	KindKeychainSwapped    = "quota.keychain_swapped"
	KindSessionRespawned   = "quota.session_respawned"
	KindRotationFailed     = "quota.rotation_failed"
	KindAccountCleared     = "quota.account_cleared"
)

// SessionLimitedDetected fires when the scanner classifies a tmux session
// as hard rate-limited. The account handle may be empty when the session
// uses an unregistered config dir — downstream handlers must tolerate it.
type SessionLimitedDetected struct {
	Session     string
	Account     string
	ConfigDir   string
	MatchedLine string
	ResetsAt    string
	At          time.Time
}

func (SessionLimitedDetected) Kind() string { return KindSessionLimited }

// SessionNearLimitDetected fires for warning-pattern matches. Only acted on
// when preemptive rotation is enabled.
type SessionNearLimitDetected struct {
	Session     string
	Account     string
	ConfigDir   string
	MatchedLine string
	At          time.Time
}

func (SessionNearLimitDetected) Kind() string { return KindSessionNearLimit }

// TokenExpiredDetected fires when token inspection reveals an expired or
// otherwise unusable token for an account.
type TokenExpiredDetected struct {
	Account   string
	ExpiresAt string
	Reason    string
	At        time.Time
}

func (TokenExpiredDetected) Kind() string { return KindTokenExpired }

// AccountReactivated fires when a probe confirms a previously limited
// account is usable again — distinct from AccountCleared, which fires off
// the imprecise reset-time window.
type AccountReactivated struct {
	Account          string
	PreviousResetsAt string
	Via              string // "probe" | "cooldown"
	At               time.Time
}

func (AccountReactivated) Kind() string { return KindAccountReactivated }

// AccountCleared fires when an account's ResetsAt window has elapsed and
// the cooldown handler flipped it back to available.
type AccountCleared struct {
	Account          string
	PreviousResetsAt string
	At               time.Time
}

func (AccountCleared) Kind() string { return KindAccountCleared }

// ScanCompleted summarizes one scanner cycle. Persistence handlers use it
// to update the LimitedSessions snapshot on quota.json.
type ScanCompleted struct {
	Total     int
	Limited   int
	NearLimit int
	Available int
	Results   []quota.ScanResult
	At        time.Time
}

func (ScanCompleted) Kind() string { return KindScanCompleted }

// RotationPlanned carries the composed plan from the planner link. Swap and
// respawn handlers consume it.
type RotationPlanned struct {
	Plan *quota.RotatePlan
	At   time.Time
}

func (RotationPlanned) Kind() string { return KindRotationPlanned }

// KeychainSwapped fires after a single config dir's keychain has been
// overwritten with a source account's token.
type KeychainSwapped struct {
	TargetConfigDir string
	SourceHandle    string
	At              time.Time
}

func (KeychainSwapped) Kind() string { return KindKeychainSwapped }

// SessionRespawned fires after the tmux session was restarted with the
// new account active.
type SessionRespawned struct {
	Session      string
	FromAccount  string
	ToAccount    string
	Resumed      bool
	KeychainSwap bool
	At           time.Time
}

func (SessionRespawned) Kind() string { return KindSessionRespawned }

// RotationFailed fires for any failed rotation attempt — swap error,
// respawn error, or no candidate accounts.
type RotationFailed struct {
	Session   string
	ToAccount string
	Reason    string
	At        time.Time
}

func (RotationFailed) Kind() string { return KindRotationFailed }
