package quota

import (
	"errors"
	"testing"
	"time"

	"github.com/steveyegge/gastown/internal/config"
)

func TestClassifyProbe(t *testing.T) {
	tests := []struct {
		name        string
		output      string
		runErr      error
		wantEnabled bool
		wantLimited bool
	}{
		{
			name:        "clean completion → enabled",
			output:      "ok",
			runErr:      nil,
			wantEnabled: true,
			wantLimited: false,
		},
		{
			name:        "hard rate-limit message → still limited",
			output:      "You've hit your usage limit",
			runErr:      nil,
			wantEnabled: false,
			wantLimited: true,
		},
		{
			name:        "rate-limit text even with nonzero exit → still limited",
			output:      "API Error: Rate limit reached",
			runErr:      errors.New("exit status 1"),
			wantEnabled: false,
			wantLimited: true,
		},
		{
			name:        "ambiguous failure with no rate-limit text → not enabled, not limited",
			output:      "dial tcp: connection refused",
			runErr:      errors.New("exit status 1"),
			wantEnabled: false,
			wantLimited: false,
		},
		{
			name:        "expired OAuth token → still limited (do not flip available)",
			output:      "OAuth token has expired",
			runErr:      nil,
			wantEnabled: false,
			wantLimited: true,
		},
		{
			name:        "clean output but process error → conservative, not enabled",
			output:      "ok",
			runErr:      errors.New("context deadline exceeded"),
			wantEnabled: false,
			wantLimited: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ClassifyProbe(tt.output, tt.runErr)
			if got.Enabled != tt.wantEnabled {
				t.Errorf("Enabled = %v, want %v", got.Enabled, tt.wantEnabled)
			}
			if got.RateLimited != tt.wantLimited {
				t.Errorf("RateLimited = %v, want %v", got.RateLimited, tt.wantLimited)
			}
		})
	}
}

func TestShouldProbe(t *testing.T) {
	// Reference "now" in a fixed timezone so reset-time parsing is deterministic.
	loc, _ := time.LoadLocation("America/Los_Angeles")
	now := time.Date(2026, 5, 24, 18, 0, 0, 0, loc) // 6:00pm
	lead := 15 * time.Minute

	tests := []struct {
		name  string
		state config.AccountQuotaState
		want  bool
	}{
		{
			name:  "available account is never due",
			state: config.AccountQuotaState{Status: config.QuotaStatusAvailable, ResetsAt: "5pm (America/Los_Angeles)"},
			want:  false,
		},
		{
			name:  "limited with no reset time → always due",
			state: config.AccountQuotaState{Status: config.QuotaStatusLimited},
			want:  true,
		},
		{
			name:  "limited with unparseable reset time → always due",
			state: config.AccountQuotaState{Status: config.QuotaStatusLimited, ResetsAt: "soon-ish"},
			want:  true,
		},
		{
			name:  "reset well in the future → not yet due",
			state: config.AccountQuotaState{Status: config.QuotaStatusLimited, ResetsAt: "8pm (America/Los_Angeles)"},
			want:  false,
		},
		{
			name:  "within lead window before reset → due",
			state: config.AccountQuotaState{Status: config.QuotaStatusLimited, ResetsAt: "6:10pm (America/Los_Angeles)"},
			want:  true,
		},
		{
			name:  "reset already passed → due",
			state: config.AccountQuotaState{Status: config.QuotaStatusLimited, ResetsAt: "5pm (America/Los_Angeles)"},
			want:  true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := ShouldProbe(tt.state, now, lead); got != tt.want {
				t.Errorf("ShouldProbe = %v, want %v", got, tt.want)
			}
		})
	}
}
