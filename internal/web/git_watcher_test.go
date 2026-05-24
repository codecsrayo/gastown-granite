package web

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestGitWatcher_DiffEmitsCommitEvents(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}

	dir := t.TempDir()
	runGit := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %s: %v (%s)", strings.Join(args, " "), err, out)
		}
	}
	runGit("init", "-b", "main")
	runGit("config", "user.email", "test@example.com")
	runGit("config", "user.name", "tester")
	runGit("commit", "--allow-empty", "-m", "initial")

	gw := newGitWatcher(dir, "gt")
	target := gitTarget{path: dir, label: filepath.Base(dir)}

	ctx := context.Background()
	prev := gw.scanRefs(ctx, dir)
	if len(prev.refs) != 1 {
		t.Fatalf("expected 1 ref after init+commit, got %d (%v)", len(prev.refs), prev.refs)
	}

	runGit("commit", "--allow-empty", "-m", "second commit")
	next := gw.scanRefs(ctx, dir)
	if next.refs["refs/heads/main"] == prev.refs["refs/heads/main"] {
		t.Fatalf("expected main sha to change after second commit")
	}

	sub, _, cancel := gw.Subscribe()
	defer cancel()
	gw.diffAndEmit(ctx, target, prev, next)

	select {
	case ev := <-sub:
		if ev.Kind != "commit" {
			t.Errorf("expected commit kind, got %q", ev.Kind)
		}
		if ev.Branch != "main" {
			t.Errorf("expected branch main, got %q", ev.Branch)
		}
		if ev.Subject != "second commit" {
			t.Errorf("expected subject 'second commit', got %q", ev.Subject)
		}
		if ev.RepoLabel != target.label {
			t.Errorf("expected label %q, got %q", target.label, ev.RepoLabel)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("no event received within 2s")
	}
}

func TestGitWatcher_BranchCreateAndDelete(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}

	dir := t.TempDir()
	runGit := func(args ...string) {
		t.Helper()
		cmd := exec.Command("git", append([]string{"-C", dir}, args...)...)
		if out, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("git %s: %v (%s)", strings.Join(args, " "), err, out)
		}
	}
	runGit("init", "-b", "main")
	runGit("config", "user.email", "test@example.com")
	runGit("config", "user.name", "tester")
	runGit("commit", "--allow-empty", "-m", "initial")

	gw := newGitWatcher(dir, "gt")
	target := gitTarget{path: dir, label: filepath.Base(dir)}

	ctx := context.Background()
	prev := gw.scanRefs(ctx, dir)

	runGit("checkout", "-b", "feature/x")
	mid := gw.scanRefs(ctx, dir)

	sub, _, cancel := gw.Subscribe()
	defer cancel()
	gw.diffAndEmit(ctx, target, prev, mid)

	kinds := drainKinds(sub, 1, 2*time.Second)
	if !contains(kinds, "branch_create") {
		t.Errorf("expected branch_create event, got %v", kinds)
	}

	// Drop the branch and verify delete
	runGit("checkout", "main")
	runGit("branch", "-D", "feature/x")
	after := gw.scanRefs(ctx, dir)
	gw.diffAndEmit(ctx, target, mid, after)
	kinds = drainKinds(sub, 1, 2*time.Second)
	if !contains(kinds, "branch_delete") {
		t.Errorf("expected branch_delete event, got %v", kinds)
	}
}

func TestGitWatcher_RingBufferCap(t *testing.T) {
	gw := newGitWatcher(t.TempDir(), "gt")
	gw.bufCap = 5
	for i := 0; i < 20; i++ {
		gw.broadcast(GitEvent{Kind: "commit", SHA: "a"})
	}
	snap := gw.Snapshot()
	if len(snap) != 5 {
		t.Errorf("expected buf capped to 5, got %d", len(snap))
	}
}

func TestResolveRigGitDir(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}

	town := t.TempDir()
	// mayor/rigs.json marks the town root for findRigsConfigPath.
	if err := writeFile(t, filepath.Join(town, "mayor", "rigs.json"),
		`{"version":1,"rigs":{"plane":{"git_url":"x"}}}`); err != nil {
		t.Fatal(err)
	}
	// Standard layout: <town>/plane/mayor/rig is the canonical clone.
	rigRepo := filepath.Join(town, "plane", "mayor", "rig")
	cmd := exec.Command("git", "init", rigRepo)
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("git init: %v (%s)", err, out)
	}

	got := resolveRigGitDir(town, "plane")
	if got != rigRepo {
		t.Errorf("resolveRigGitDir = %q, want %q", got, rigRepo)
	}

	if got := resolveRigGitDir(town, "nonexistent"); got != "" {
		t.Errorf("expected empty for unknown rig, got %q", got)
	}
	if got := resolveRigGitDir(town, ""); got != "" {
		t.Errorf("expected empty for blank rig, got %q", got)
	}
}

func writeFile(t *testing.T, path, content string) error {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0755); err != nil {
		return err
	}
	return os.WriteFile(path, []byte(content), 0644)
}

func TestGitGraphConsoleWrapper(t *testing.T) {
	w := gitGraphConsoleWrapper()
	if !strings.Contains(w, "git log --graph") {
		t.Errorf("wrapper missing git log --graph: %q", w)
	}
	if !strings.Contains(w, "exec") || !strings.Contains(w, "SHELL") {
		t.Errorf("wrapper should drop to a shell after rendering: %q", w)
	}
}

func TestParseCrewTargets(t *testing.T) {
	in := `[{"name":"max","rig":"gastown","path":"/tmp/crew/max"},{"name":"toast","rig":"gastown","path":""}]`
	got := parseCrewTargets(in)
	if len(got) != 1 {
		t.Fatalf("expected 1 target (toast has empty path), got %d", len(got))
	}
	if got[0].label != "gastown/max" {
		t.Errorf("expected label gastown/max, got %q", got[0].label)
	}

	// Non-JSON "no workspaces" output must not panic and must return nil.
	if got := parseCrewTargets("No crew workspaces found."); got != nil {
		t.Errorf("expected nil for non-JSON input, got %v", got)
	}
}

func TestGitWatcher_RemoteUpdateEvent(t *testing.T) {
	if _, err := exec.LookPath("git"); err != nil {
		t.Skip("git not available")
	}

	gw := newGitWatcher(t.TempDir(), "gt")
	target := gitTarget{path: "/fake", label: "fake"}

	prev := refSnapshot{refs: map[string]string{
		"refs/remotes/origin/main": "aaaaaaaa",
	}}
	next := refSnapshot{refs: map[string]string{
		"refs/remotes/origin/main": "bbbbbbbb",
	}}

	sub, _, cancel := gw.Subscribe()
	defer cancel()
	gw.diffAndEmit(context.Background(), target, prev, next)

	select {
	case ev := <-sub:
		if ev.Kind != "remote_update" {
			t.Errorf("expected remote_update, got %q", ev.Kind)
		}
		if ev.Branch != "origin/main" {
			t.Errorf("expected branch 'origin/main', got %q", ev.Branch)
		}
	case <-time.After(1 * time.Second):
		t.Fatal("no event received")
	}
}

func TestParsePolecatTargets(t *testing.T) {
	in := `[{"name":"alpha","rig":"gastown","clone_path":"/tmp/alpha"},{"name":"beta","rig":"gastown","clone_path":""}]`
	got := parsePolecatTargets(in)
	if len(got) != 1 {
		t.Fatalf("expected 1 target (beta has empty path), got %d", len(got))
	}
	if got[0].label != "gastown/alpha" {
		t.Errorf("expected label gastown/alpha, got %q", got[0].label)
	}
}

func drainKinds(ch <-chan GitEvent, want int, max time.Duration) []string {
	var kinds []string
	deadline := time.After(max)
	for len(kinds) < want {
		select {
		case ev := <-ch:
			kinds = append(kinds, ev.Kind)
		case <-deadline:
			return kinds
		}
	}
	// Drain anything immediately available too so callers can assert.
	for {
		select {
		case ev := <-ch:
			kinds = append(kinds, ev.Kind)
		default:
			return kinds
		}
	}
}

func contains(haystack []string, needle string) bool {
	for _, h := range haystack {
		if h == needle {
			return true
		}
	}
	return false
}
