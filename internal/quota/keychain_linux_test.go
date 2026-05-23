//go:build linux

package quota

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func writeFile(t *testing.T, path, content string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(path), err)
	}
	if err := os.WriteFile(path, []byte(content), 0600); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func readFile(t *testing.T, path string) string {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read %s: %v", path, err)
	}
	return string(b)
}

func TestLinuxReadWriteKeychainToken(t *testing.T) {
	dir := t.TempDir()
	want := `{"claudeAiOauth":{"accessToken":"abc"}}`
	if err := WriteKeychainToken(dir, "claude-code", want); err != nil {
		t.Fatalf("write: %v", err)
	}
	got, err := ReadKeychainToken(dir)
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	if got != want {
		t.Fatalf("token mismatch: got %q want %q", got, want)
	}
	// File mode must be 0600 so the credential isn't world-readable.
	info, err := os.Stat(filepath.Join(dir, ".credentials.json"))
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0600 {
		t.Fatalf("unexpected file mode: %o", perm)
	}
}

func TestLinuxSwapAndRestoreKeychainCredential(t *testing.T) {
	targetDir := t.TempDir()
	sourceDir := t.TempDir()

	targetToken := `{"claudeAiOauth":{"accessToken":"target-old"}}`
	sourceToken := `{"claudeAiOauth":{"accessToken":"source-fresh"}}`
	writeFile(t, filepath.Join(targetDir, ".credentials.json"), targetToken)
	writeFile(t, filepath.Join(sourceDir, ".credentials.json"), sourceToken)

	backup, err := SwapKeychainCredential(targetDir, sourceDir)
	if err != nil {
		t.Fatalf("swap: %v", err)
	}
	if backup == nil || backup.Token != targetToken {
		t.Fatalf("backup did not capture original target token: %+v", backup)
	}

	got := readFile(t, filepath.Join(targetDir, ".credentials.json"))
	if got != sourceToken {
		t.Fatalf("target not overwritten with source: got %q", got)
	}
	// Source must be untouched — the swap copies one way.
	if src := readFile(t, filepath.Join(sourceDir, ".credentials.json")); src != sourceToken {
		t.Fatalf("source mutated unexpectedly: %q", src)
	}

	if err := RestoreKeychainToken(backup); err != nil {
		t.Fatalf("restore: %v", err)
	}
	got = readFile(t, filepath.Join(targetDir, ".credentials.json"))
	if got != targetToken {
		t.Fatalf("restore did not revert: %q", got)
	}
}

func TestLinuxSwapOAuthAccountRoundTrip(t *testing.T) {
	targetDir := t.TempDir()
	sourceDir := t.TempDir()

	targetDoc := map[string]any{
		"oauthAccount": map[string]string{"accountUuid": "target-uuid"},
		"other":        "preserve-me",
	}
	sourceDoc := map[string]any{
		"oauthAccount": map[string]string{"accountUuid": "source-uuid"},
	}
	tb, _ := json.Marshal(targetDoc)
	sb, _ := json.Marshal(sourceDoc)
	writeFile(t, filepath.Join(targetDir, ".claude.json"), string(tb))
	writeFile(t, filepath.Join(sourceDir, ".claude.json"), string(sb))

	backup, err := SwapOAuthAccount(targetDir, sourceDir)
	if err != nil {
		t.Fatalf("swap oauth: %v", err)
	}
	if backup == nil {
		t.Fatalf("backup is nil")
	}

	// Target should now carry source's oauthAccount but keep "other".
	postSwap := readFile(t, filepath.Join(targetDir, ".claude.json"))
	if !strings.Contains(postSwap, "source-uuid") {
		t.Fatalf("post-swap missing source uuid: %s", postSwap)
	}
	if !strings.Contains(postSwap, "preserve-me") {
		t.Fatalf("post-swap dropped unrelated field: %s", postSwap)
	}

	if err := RestoreOAuthAccount(targetDir, backup); err != nil {
		t.Fatalf("restore: %v", err)
	}
	postRestore := readFile(t, filepath.Join(targetDir, ".claude.json"))
	if !strings.Contains(postRestore, "target-uuid") {
		t.Fatalf("post-restore missing original uuid: %s", postRestore)
	}
}

func TestLinuxValidateKeychainTokenExpiry(t *testing.T) {
	dir := t.TempDir()

	expired := time.Now().Add(-1*time.Hour).UnixNano() / int64(time.Millisecond)
	fresh := time.Now().Add(1*time.Hour).UnixNano() / int64(time.Millisecond)

	cases := []struct {
		name     string
		token    string
		wantErr  bool
	}{
		{
			name:    "fresh-claudeAiOauth",
			token:   `{"claudeAiOauth":{"expiresAt":` + itoa(fresh) + `}}`,
			wantErr: false,
		},
		{
			name:    "expired-claudeAiOauth",
			token:   `{"claudeAiOauth":{"expiresAt":` + itoa(expired) + `}}`,
			wantErr: true,
		},
		{
			name:    "opaque-token-treated-as-valid",
			token:   `not-json-not-jwt`,
			wantErr: false,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			writeFile(t, filepath.Join(dir, ".credentials.json"), tc.token)
			err := ValidateKeychainToken(dir)
			if tc.wantErr && err == nil {
				t.Fatalf("expected expiry error, got nil")
			}
			if !tc.wantErr && err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
		})
	}
}

func TestLinuxSyncSwappedTokens(t *testing.T) {
	targetDir := t.TempDir()
	sourceDir := t.TempDir()

	writeFile(t, filepath.Join(targetDir, ".credentials.json"), `old-target`)
	writeFile(t, filepath.Join(sourceDir, ".credentials.json"), `fresh-source`)

	updated := SyncSwappedTokens(map[string]string{targetDir: sourceDir})
	if updated != 1 {
		t.Fatalf("expected 1 update, got %d", updated)
	}
	if got := readFile(t, filepath.Join(targetDir, ".credentials.json")); got != `fresh-source` {
		t.Fatalf("sync did not propagate: %q", got)
	}

	// Second call: tokens already match, should not update.
	updated = SyncSwappedTokens(map[string]string{targetDir: sourceDir})
	if updated != 0 {
		t.Fatalf("expected 0 updates on second call, got %d", updated)
	}
}

func itoa(n int64) string {
	const digits = "0123456789"
	if n == 0 {
		return "0"
	}
	neg := n < 0
	if neg {
		n = -n
	}
	var buf [20]byte
	i := len(buf)
	for n > 0 {
		i--
		buf[i] = digits[n%10]
		n /= 10
	}
	if neg {
		i--
		buf[i] = '-'
	}
	return string(buf[i:])
}
