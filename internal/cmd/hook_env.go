package cmd

import (
	"os"

	"github.com/steveyegge/gastown/internal/beads"
)

// envHookBeadVar names the session env variable that sling pins on polecat
// spawn (internal/polecat/session_manager.go) so the polecat can identify its
// hooked bead without a bd query. Needed because bd 1.0.4 still opens an
// embedded scratch DB and auto-imports stale jsonl alongside server writes,
// which can flip status=hooked → open between the sling write and the polecat's
// first prime read. See gg-0nb.
const envHookBeadVar = "GT_HOOK_BEAD"

// resolveEnvHookBead returns the bead pinned by the GT_HOOK_BEAD env var, or
// nil if unset / unresolvable.
//
// The returned bead is authoritative even if its status looks "wrong"
// (e.g. open instead of hooked) because the env was set by the trusted sling
// path. Callers should treat a non-nil result as the active hook regardless of
// the bd-reported status — the scratch auto-import in upstream bd 1.0.4 is the
// known offender that flips the status field.
//
// townRoot and workDir are used to resolve the correct beads directory for
// the bead (rig vs. town-level vs. cross-rig hq-* dispatch).
func resolveEnvHookBead(townRoot, workDir string) *beads.Issue {
	beadID := os.Getenv(envHookBeadVar)
	if beadID == "" {
		return nil
	}
	hookDir := beads.ResolveHookDir(townRoot, beadID, workDir)
	if hookDir == "" {
		hookDir = workDir
	}
	b := beads.New(hookDir)
	issue, err := b.Show(beadID)
	if err != nil || issue == nil {
		return nil
	}
	return issue
}
