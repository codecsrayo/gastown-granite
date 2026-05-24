package daemon

import (
	"testing"
	"time"
)

func TestQuotaProberInterval(t *testing.T) {
	// nil config → default
	if got := quotaProberInterval(nil); got != defaultQuotaProberInterval {
		t.Errorf("expected default interval %v, got %v", defaultQuotaProberInterval, got)
	}

	config := &DaemonPatrolConfig{
		Patrols: &PatrolsConfig{
			QuotaProber: &QuotaProberConfig{
				Enabled:     true,
				IntervalStr: "90s",
			},
		},
	}
	if got := quotaProberInterval(config); got != 90*time.Second {
		t.Errorf("expected 90s, got %v", got)
	}

	// Invalid interval string → default
	config.Patrols.QuotaProber.IntervalStr = "invalid"
	if got := quotaProberInterval(config); got != defaultQuotaProberInterval {
		t.Errorf("expected default interval %v for invalid string, got %v", defaultQuotaProberInterval, got)
	}
}

func TestIsPatrolEnabled_QuotaProber(t *testing.T) {
	// nil config → opt-in patrol disabled
	if IsPatrolEnabled(nil, "quota_prober") {
		t.Error("expected quota_prober to be disabled with nil config")
	}

	config := &DaemonPatrolConfig{Patrols: &PatrolsConfig{}}
	if IsPatrolEnabled(config, "quota_prober") {
		t.Error("expected quota_prober to be disabled by default (nil sub-config)")
	}

	config.Patrols.QuotaProber = &QuotaProberConfig{Enabled: true}
	if !IsPatrolEnabled(config, "quota_prober") {
		t.Error("expected quota_prober to be enabled when configured")
	}

	config.Patrols.QuotaProber = &QuotaProberConfig{Enabled: false}
	if IsPatrolEnabled(config, "quota_prober") {
		t.Error("expected quota_prober to be disabled when explicitly disabled")
	}
}

func TestQuotaProberDefaultConstants(t *testing.T) {
	if defaultQuotaProberInterval != 2*time.Minute {
		t.Errorf("expected default interval 2m, got %v", defaultQuotaProberInterval)
	}
}
