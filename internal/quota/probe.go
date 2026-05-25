package quota

import (
	"bytes"
	"context"
	"os"
	"os/exec"
	"regexp"
	"time"

	"github.com/steveyegge/gastown/internal/config"
	"github.com/steveyegge/gastown/internal/constants"
	"github.com/steveyegge/gastown/internal/util"
)

// Probing actively validates whether a rate-limited account is usable again,
// rather than trusting the human-readable ResetsAt time (which the provider
// reports imprecisely). A probe runs `claude --print` headlessly against the
// account's own config dir and classifies the output: a rate-limit signature
// means still limited, a clean completion means the account is back.

const (
	// DefaultProbeModel is the model alias used for probes. The cheapest tier
	// keeps probe token cost negligible; "haiku" is an alias so it tracks the
	// current Haiku release without pinning a stale model ID.
	DefaultProbeModel = "haiku"

	// DefaultProbePrompt is a trivial single-turn prompt — just enough to force
	// one round-trip to the API so a live rate limit surfaces.
	DefaultProbePrompt = "Reply with the single word: ok"

	// DefaultProbeTimeout bounds a single probe invocation.
	DefaultProbeTimeout = 60 * time.Second

	// DefaultProbeLeadWindow is how long before the shown ResetsAt that probing
	// begins. The provider's reset time is imprecise, so we start a little early
	// and retry each cycle until the account actually comes back.
	DefaultProbeLeadWindow = 15 * time.Minute
)

// rateLimitProbePatterns are the compiled hard-limit signatures, reused from the
// scanner's defaults so probe classification stays consistent with detection.
var rateLimitProbePatterns = compileProbePatterns()

func compileProbePatterns() []*regexp.Regexp {
	out := make([]*regexp.Regexp, 0, len(constants.DefaultRateLimitPatterns))
	for _, p := range constants.DefaultRateLimitPatterns {
		out = append(out, regexp.MustCompile("(?i)"+p))
	}
	return out
}

// matchesRateLimit reports whether probe output carries a rate-limit signature.
func matchesRateLimit(output string) bool {
	if output == "" {
		return false
	}
	for _, re := range rateLimitProbePatterns {
		if re.MatchString(output) {
			return true
		}
	}
	return false
}

// ProbeRunner executes a single account probe against a config dir and returns
// the combined stdout+stderr plus the process error. Swappable for tests.
type ProbeRunner func(ctx context.Context, configDir string) (string, error)

// ProbeOutcome is the classified result of a single probe.
type ProbeOutcome struct {
	// Enabled is true only when the probe completed cleanly with no rate-limit
	// signature — i.e. the account is usable again.
	Enabled bool
	// RateLimited is true when output still carries a rate-limit signature.
	RateLimited bool
	// Output is the combined stdout+stderr from the probe (truncated by caller).
	Output string
	// Err is the process error, if any.
	Err error
}

// ClassifyProbe decides whether an account is usable from a probe's output and
// process error. Classification is conservative: an account is marked enabled
// only when the probe ran without error AND produced no rate-limit signature.
// An ambiguous failure (network blip, claude crash, auth error) leaves the
// account limited so a transient failure never flips a still-limited account to
// available.
func ClassifyProbe(output string, runErr error) ProbeOutcome {
	limited := matchesRateLimit(output)
	return ProbeOutcome{
		Enabled:     runErr == nil && !limited,
		RateLimited: limited,
		Output:      output,
		Err:         runErr,
	}
}

// NewClaudeProbeRunner returns a ProbeRunner that shells out to `claude --print`
// with CLAUDE_CONFIG_DIR pointed at the account being probed. The probe is fully
// isolated from live agent sessions — it reads the account's own credentials and
// never touches the keychain or any running session.
func NewClaudeProbeRunner(claudePath, model, prompt string, timeout time.Duration) ProbeRunner {
	if prompt == "" {
		prompt = DefaultProbePrompt
	}
	if timeout <= 0 {
		timeout = DefaultProbeTimeout
	}
	return func(ctx context.Context, configDir string) (string, error) {
		cctx, cancel := context.WithTimeout(ctx, timeout)
		defer cancel()

		args := []string{"--print"}
		if model != "" {
			args = append(args, "--model", model)
		}
		args = append(args, prompt)

		cmd := exec.CommandContext(cctx, claudePath, args...) //nolint:gosec // G204: claudePath resolved via LookPath by caller
		// Isolate the probe to the target account's config dir. Inherit the rest
		// of the environment so PATH / proxy settings still apply.
		cmd.Env = append(os.Environ(), "CLAUDE_CONFIG_DIR="+util.ExpandHome(configDir))

		var buf bytes.Buffer
		cmd.Stdout = &buf
		cmd.Stderr = &buf
		// Stdin nil → child reads from os.DevNull, so claude stays non-interactive.

		err := cmd.Run()
		return buf.String(), err
	}
}

// ShouldProbe reports whether a limited account is due for a probe given the
// gating window. The shown ResetsAt is imprecise, so we begin probing `lead`
// before it and retry until the account actually returns. Accounts with an
// empty or unparseable reset time are always due (we have nothing to gate on).
func ShouldProbe(acctState config.AccountQuotaState, now time.Time, lead time.Duration) bool {
	if acctState.Status != config.QuotaStatusLimited {
		return false
	}
	resetTime, ok := resolvedUnlock(acctState, now)
	if !ok {
		return true // no parseable reset — nothing to gate on
	}
	return !now.Before(resetTime.Add(-lead))
}
