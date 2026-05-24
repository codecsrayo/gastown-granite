package daemon

import (
	"context"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/steveyegge/gastown/internal/events"
)

const (
	defaultLoginWatchInterval = 30 * time.Second
	loginWatchScanLines       = 40
	loginWatchCacheTTL        = 30 * time.Minute
)

// LoginWatchConfig holds configuration for the login_watch patrol.
//
// The patrol scans tmux session pane content for Claude / Anthropic OAuth
// login URLs and emits a `login_required` event when one is seen. The
// dashboard subscribes to that event and surfaces a copy-able toast — users
// can't easily select text from the embedded tmux preview, so reaching the
// URL through the terminal is painful.
type LoginWatchConfig struct {
	Enabled     bool   `json:"enabled"`
	IntervalStr string `json:"interval,omitempty"`
}

// loginURLPatterns matches OAuth / login flow URLs that appear in Claude
// Code sessions when an account needs re-auth. Order is irrelevant —
// the first regex that matches wins.
var loginURLPatterns = []*regexp.Regexp{
	regexp.MustCompile(`https?://[^\s]*console\.anthropic\.com/[^\s]+`),
	regexp.MustCompile(`https?://[^\s]*claude\.ai/auth[^\s]*`),
	regexp.MustCompile(`https?://[^\s]*claude\.ai/login[^\s]*`),
	regexp.MustCompile(`https?://[^\s]*oauth[^\s]*`),
}

// loginPromptPatterns matches phrases that often accompany an OAuth URL.
// Used as a confirmation signal — a bare https URL alone isn't enough,
// we want to be sure it's actually a login prompt.
var loginPromptPatterns = []*regexp.Regexp{
	regexp.MustCompile(`(?i)open this URL`),
	regexp.MustCompile(`(?i)paste this code`),
	regexp.MustCompile(`(?i)claude /login`),
	regexp.MustCompile(`(?i)please visit`),
	regexp.MustCompile(`(?i)complete (the )?login`),
	regexp.MustCompile(`(?i)authenticate.*browser`),
}

// loginSeenCache de-dupes emissions per (session, url). Entries expire
// after loginWatchCacheTTL so the toast can re-appear if the same URL
// surfaces again after a long gap (e.g. user dismissed the toast hours ago
// and the same login is still pending).
type loginSeenCache struct {
	mu      sync.Mutex
	entries map[string]time.Time
}

func newLoginSeenCache() *loginSeenCache {
	return &loginSeenCache{entries: make(map[string]time.Time)}
}

// shouldEmit reports whether (session, url) is fresh enough to emit again.
// Inserts the current timestamp on first call. Returns false if the same
// key was emitted within loginWatchCacheTTL.
func (c *loginSeenCache) shouldEmit(session, url string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	key := session + "\x00" + url
	now := time.Now()
	if last, ok := c.entries[key]; ok {
		if now.Sub(last) < loginWatchCacheTTL {
			return false
		}
	}
	c.entries[key] = now
	// Opportunistic GC: drop stale entries when the map gets large.
	if len(c.entries) > 256 {
		for k, t := range c.entries {
			if now.Sub(t) >= loginWatchCacheTTL {
				delete(c.entries, k)
			}
		}
	}
	return true
}

func loginWatchInterval(cfg *DaemonPatrolConfig) time.Duration {
	if cfg != nil && cfg.Patrols != nil && cfg.Patrols.LoginWatch != nil {
		if cfg.Patrols.LoginWatch.IntervalStr != "" {
			if d, err := time.ParseDuration(cfg.Patrols.LoginWatch.IntervalStr); err == nil && d > 0 {
				return d
			}
		}
	}
	return defaultLoginWatchInterval
}

// runLoginWatch examines every tmux session's pane for OAuth URLs and
// publishes a `login_required` event per (session, url) pair, dedup'd
// through d.loginSeen so we don't spam the dashboard.
func (d *Daemon) runLoginWatch() {
	if !d.isPatrolActive("login_watch") {
		return
	}
	if d.tmux == nil {
		return
	}
	if d.loginSeen == nil {
		d.loginSeen = newLoginSeenCache()
	}

	_, cancel := context.WithTimeout(d.ctx, 30*time.Second)
	defer cancel()

	sessions, err := d.tmux.ListSessions()
	if err != nil {
		d.logger.Printf("login_watch: list sessions: %v", err)
		return
	}

	for _, sess := range sessions {
		pane, err := d.tmux.CapturePane(sess, loginWatchScanLines)
		if err != nil {
			continue
		}
		url, line := findLoginURL(pane)
		if url == "" {
			continue
		}
		if !d.loginSeen.shouldEmit(sess, url) {
			continue
		}
		payload := map[string]interface{}{
			"session":  sess,
			"url":      url,
			"context":  line,
			"detected": time.Now().UTC().Format(time.RFC3339),
		}
		if err := events.LogFeed(events.TypeLoginRequired, "daemon", payload); err != nil {
			d.logger.Printf("login_watch: emit event: %v", err)
			continue
		}
		d.logger.Printf("login_watch: %s needs login — %s", sess, url)
	}
}

// findLoginURL scans pane text for an OAuth URL accompanied by a login
// prompt phrase. Returns the URL and the line it was found on, or ("", "")
// if no convincing match exists.
//
// The two-signal requirement (URL + prompt context) avoids false positives
// from agents pasting random https links into their session output.
func findLoginURL(pane string) (string, string) {
	lines := strings.Split(pane, "\n")
	// Search the recent tail first — login prompts sit at the bottom.
	hasPromptContext := false
	for _, line := range lines {
		for _, re := range loginPromptPatterns {
			if re.MatchString(line) {
				hasPromptContext = true
				break
			}
		}
		if hasPromptContext {
			break
		}
	}
	if !hasPromptContext {
		return "", ""
	}
	for _, line := range lines {
		for _, re := range loginURLPatterns {
			if m := re.FindString(line); m != "" {
				return m, strings.TrimSpace(line)
			}
		}
	}
	return "", ""
}
