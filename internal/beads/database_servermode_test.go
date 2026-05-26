package beads

import (
	"os"
	"path/filepath"
	"testing"
)

// TestDoltTargetEnvForcesServerMode verifies that when metadata.json declares
// dolt_mode=server, the bd invocation env includes BEADS_DOLT_SERVER_MODE=1.
// Without this, bd that finds a local embedded scratch dir falls back to it and
// silently diverges from the Dolt server (gg-0nb / gg-zr4 split-brain).
func TestDoltTargetEnvForcesServerMode(t *testing.T) {
	dir := t.TempDir()
	meta := `{"backend":"dolt","dolt_database":"hq","dolt_mode":"server","dolt_server_host":"127.0.0.1","dolt_server_port":3307}`
	if err := os.WriteFile(filepath.Join(dir, "metadata.json"), []byte(meta), 0o600); err != nil {
		t.Fatalf("write metadata.json: %v", err)
	}

	env := doltTargetEnvFromBeadsDir(dir, true)

	if !hasEnv(env, "BEADS_DOLT_SERVER_MODE=1") {
		t.Errorf("server-mode metadata did not yield BEADS_DOLT_SERVER_MODE=1; env=%v", env)
	}
	if !hasEnv(env, "BEADS_DOLT_SERVER_DATABASE=hq") {
		t.Errorf("missing database env; env=%v", env)
	}
	if !hasEnv(env, "BEADS_DOLT_SERVER_HOST=127.0.0.1") {
		t.Errorf("missing host env; env=%v", env)
	}
}

// TestDoltTargetEnvEmbeddedNoServerMode verifies embedded mode (or absent
// dolt_mode) does NOT force server mode.
func TestDoltTargetEnvEmbeddedNoServerMode(t *testing.T) {
	dir := t.TempDir()
	meta := `{"backend":"dolt","dolt_database":"hq","dolt_mode":"embedded"}`
	if err := os.WriteFile(filepath.Join(dir, "metadata.json"), []byte(meta), 0o600); err != nil {
		t.Fatalf("write metadata.json: %v", err)
	}

	env := doltTargetEnvFromBeadsDir(dir, true)

	if hasEnv(env, "BEADS_DOLT_SERVER_MODE=1") {
		t.Errorf("embedded metadata should not force server mode; env=%v", env)
	}
}

func hasEnv(env []string, want string) bool {
	for _, e := range env {
		if e == want {
			return true
		}
	}
	return false
}
