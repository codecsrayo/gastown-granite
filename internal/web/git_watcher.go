package web

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/steveyegge/gastown/internal/config"
)

// GitEvent is a single observed change in one of the watched repositories.
// Kinds: "commit", "branch_create", "branch_delete", "head_change".
type GitEvent struct {
	Ts        time.Time `json:"ts"`
	Repo      string    `json:"repo"`            // absolute path
	RepoLabel string    `json:"repo_label"`      // short display label
	Kind      string    `json:"kind"`
	Ref       string    `json:"ref,omitempty"`   // e.g. refs/heads/main
	Branch    string    `json:"branch,omitempty"`
	SHA       string    `json:"sha,omitempty"`
	ShortSHA  string    `json:"short_sha,omitempty"`
	Subject   string    `json:"subject,omitempty"`
	Author    string    `json:"author,omitempty"`
}

// gitTarget describes a single repository to watch and how to label it
// in the feed (e.g. "gastown" for a rig, "gastown/polecat-max" for a worker).
type gitTarget struct {
	path  string
	label string
}

type refSnapshot struct {
	refs map[string]string // ref -> sha
	head string            // current HEAD (branch name or detached SHA)
}

// gitWatcher polls a set of repositories for ref changes and broadcasts
// each observed change as a GitEvent to all subscribers via SSE.
type gitWatcher struct {
	workDir string
	gtPath  string

	mu          sync.Mutex
	subscribers map[chan GitEvent]struct{}
	buf         []GitEvent // ring buffer of recent events for backfill
	bufCap      int

	started   bool
	startOnce sync.Once
}

func newGitWatcher(workDir, gtPath string) *gitWatcher {
	return &gitWatcher{
		workDir:     workDir,
		gtPath:      gtPath,
		subscribers: make(map[chan GitEvent]struct{}),
		bufCap:      200,
	}
}

// Start launches discovery and poll loops. Safe to call multiple times.
func (gw *gitWatcher) Start(ctx context.Context) {
	gw.startOnce.Do(func() {
		gw.started = true
		go gw.run(ctx)
	})
}

// Subscribe returns a channel that receives future GitEvents along with
// a snapshot of the most recent events for initial render. The returned
// cancel function must be called to detach the subscriber.
func (gw *gitWatcher) Subscribe() (<-chan GitEvent, []GitEvent, func()) {
	ch := make(chan GitEvent, 64)
	gw.mu.Lock()
	gw.subscribers[ch] = struct{}{}
	backfill := append([]GitEvent(nil), gw.buf...)
	gw.mu.Unlock()
	cancel := func() {
		gw.mu.Lock()
		if _, ok := gw.subscribers[ch]; ok {
			delete(gw.subscribers, ch)
			close(ch)
		}
		gw.mu.Unlock()
	}
	return ch, backfill, cancel
}

// Snapshot returns a copy of the recent-events ring buffer.
func (gw *gitWatcher) Snapshot() []GitEvent {
	gw.mu.Lock()
	defer gw.mu.Unlock()
	return append([]GitEvent(nil), gw.buf...)
}

func (gw *gitWatcher) broadcast(ev GitEvent) {
	gw.mu.Lock()
	gw.buf = append(gw.buf, ev)
	if len(gw.buf) > gw.bufCap {
		gw.buf = gw.buf[len(gw.buf)-gw.bufCap:]
	}
	for ch := range gw.subscribers {
		select {
		case ch <- ev:
		default:
			// drop for slow subscriber; events also survive in the buf
		}
	}
	gw.mu.Unlock()
}

func (gw *gitWatcher) run(ctx context.Context) {
	const (
		discoverInterval = 30 * time.Second
		pollInterval     = 2500 * time.Millisecond
	)

	targets := gw.discover(ctx)
	snapshots := make(map[string]refSnapshot, len(targets))
	for _, t := range targets {
		snapshots[t.path] = gw.scanRefs(ctx, t.path)
	}

	discoverTick := time.NewTicker(discoverInterval)
	defer discoverTick.Stop()
	pollTick := time.NewTicker(pollInterval)
	defer pollTick.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-discoverTick.C:
			newTargets := gw.discover(ctx)
			byPath := make(map[string]gitTarget, len(newTargets))
			for _, t := range newTargets {
				byPath[t.path] = t
				if _, ok := snapshots[t.path]; !ok {
					snapshots[t.path] = gw.scanRefs(ctx, t.path)
				}
			}
			for path := range snapshots {
				if _, ok := byPath[path]; !ok {
					delete(snapshots, path)
				}
			}
			targets = newTargets
		case <-pollTick.C:
			for _, t := range targets {
				next := gw.scanRefs(ctx, t.path)
				prev, hadPrev := snapshots[t.path]
				snapshots[t.path] = next
				if !hadPrev {
					continue
				}
				gw.diffAndEmit(ctx, t, prev, next)
			}
		}
	}
}

// resolveRigGitDir returns the canonical git clone for a rig so commands
// like `git graph <rig>` operate on the rig's repo, not the HQ root.
// Preference: explicit LocalRepo → mayor clone → refinery clone → rig dir.
// Returns "" when no git repo can be located.
func resolveRigGitDir(workDir, rigName string) string {
	if rigName == "" {
		return ""
	}
	rigsPath, err := findRigsConfigPath(workDir)
	if err != nil {
		return ""
	}
	townRoot := filepath.Dir(filepath.Dir(rigsPath))

	if rigs, err := config.LoadRigsConfig(rigsPath); err == nil {
		if entry, ok := rigs.Rigs[rigName]; ok && entry.LocalRepo != "" {
			if abs, err := filepath.Abs(entry.LocalRepo); err == nil && isGitRepo(abs) {
				return abs
			}
		}
	}

	rigDir := filepath.Join(townRoot, rigName)
	for _, role := range []string{"mayor", "refinery"} {
		p := filepath.Join(rigDir, role, "rig")
		if isGitRepo(p) {
			return p
		}
	}
	if isGitRepo(rigDir) {
		return rigDir
	}
	return ""
}

func (gw *gitWatcher) discover(ctx context.Context) []gitTarget {
	seen := make(map[string]gitTarget)
	add := func(t gitTarget) {
		if t.path == "" || !isGitRepo(t.path) {
			return
		}
		if _, dup := seen[t.path]; dup {
			return
		}
		seen[t.path] = t
	}

	rigsPath, err := findRigsConfigPath(gw.workDir)
	if err == nil {
		townRoot := filepath.Dir(filepath.Dir(rigsPath)) // strip /mayor/rigs.json
		if rigs, err := config.LoadRigsConfig(rigsPath); err == nil {
			for name, entry := range rigs.Rigs {
				// Explicit LocalRepo wins when the rig config has it.
				if entry.LocalRepo != "" {
					if abs, err := filepath.Abs(entry.LocalRepo); err == nil {
						add(gitTarget{path: abs, label: name})
					}
				}
				// Standard layout: <townRoot>/<rigName>/<role>/rig/
				// covers mayor + refinery + witness clones.
				rigDir := filepath.Join(townRoot, name)
				for _, role := range []string{"mayor", "refinery", "witness"} {
					p := filepath.Join(rigDir, role, "rig")
					if isGitRepo(p) {
						add(gitTarget{path: p, label: name + "/" + role})
					}
				}
				// Polecats: <rigDir>/polecats/<polecatName>/<rigName>/
				if entries, err := os.ReadDir(filepath.Join(rigDir, "polecats")); err == nil {
					for _, e := range entries {
						if !e.IsDir() || strings.HasPrefix(e.Name(), ".") {
							continue
						}
						p := filepath.Join(rigDir, "polecats", e.Name(), name)
						add(gitTarget{path: p, label: name + "/polecat-" + e.Name()})
					}
				}
				// Crew: <rigDir>/crew/<crewName>/<rigName>/
				if entries, err := os.ReadDir(filepath.Join(rigDir, "crew")); err == nil {
					for _, e := range entries {
						if !e.IsDir() || strings.HasPrefix(e.Name(), ".") {
							continue
						}
						p := filepath.Join(rigDir, "crew", e.Name(), name)
						add(gitTarget{path: p, label: name + "/crew-" + e.Name()})
					}
				}
			}
		}
	}

	// gt CLI fallbacks: useful when filesystem layout drifts from convention
	// or when running in a unit-test workspace where clones live elsewhere.
	if out, err := gw.runGt(ctx, 3*time.Second, []string{"polecat", "list", "--all", "--json"}); err == nil {
		for _, p := range parsePolecatTargets(out) {
			add(p)
		}
	}
	if out, err := gw.runGt(ctx, 3*time.Second, []string{"crew", "list", "--all", "--json"}); err == nil {
		for _, p := range parseCrewTargets(out) {
			add(p)
		}
	}

	// Workdir fallback when no rigs are configured (e.g. running gt
	// dashboard inside a plain git repo for local development).
	if len(seen) == 0 && gw.workDir != "" {
		if abs, err := filepath.Abs(gw.workDir); err == nil && isGitRepo(abs) {
			seen[abs] = gitTarget{path: abs, label: filepath.Base(abs)}
		}
	}

	targets := make([]gitTarget, 0, len(seen))
	for _, t := range seen {
		targets = append(targets, t)
	}
	sort.Slice(targets, func(i, j int) bool { return targets[i].label < targets[j].label })
	return targets
}

// parseCrewTargets parses `gt crew list --all --json` output. The crew CLI
// emits a non-JSON message when there are no workspaces, so unmarshal errors
// are silently treated as empty.
func parseCrewTargets(jsonStr string) []gitTarget {
	var items []struct {
		Name string `json:"name"`
		Rig  string `json:"rig"`
		Path string `json:"path"`
	}
	if err := json.Unmarshal([]byte(jsonStr), &items); err != nil {
		return nil
	}
	var out []gitTarget
	for _, it := range items {
		if it.Path == "" {
			continue
		}
		abs, err := filepath.Abs(it.Path)
		if err != nil {
			continue
		}
		label := it.Name
		if it.Rig != "" && it.Name != "" {
			label = it.Rig + "/" + it.Name
		} else if label == "" {
			label = filepath.Base(abs)
		}
		out = append(out, gitTarget{path: abs, label: label})
	}
	return out
}

func parsePolecatTargets(jsonStr string) []gitTarget {
	var items []map[string]interface{}
	if err := json.Unmarshal([]byte(jsonStr), &items); err != nil {
		var wrapper map[string][]map[string]interface{}
		if err := json.Unmarshal([]byte(jsonStr), &wrapper); err != nil {
			return nil
		}
		for _, v := range wrapper {
			items = v
			break
		}
	}

	var out []gitTarget
	for _, item := range items {
		path, _ := item["clone_path"].(string)
		if path == "" {
			continue
		}
		abs, err := filepath.Abs(path)
		if err != nil {
			continue
		}
		rig, _ := item["rig"].(string)
		name, _ := item["name"].(string)
		label := name
		if rig != "" && name != "" {
			label = rig + "/" + name
		} else if label == "" {
			label = filepath.Base(abs)
		}
		out = append(out, gitTarget{path: abs, label: label})
	}
	return out
}

func isGitRepo(path string) bool {
	if path == "" {
		return false
	}
	info, err := os.Stat(filepath.Join(path, ".git"))
	if err != nil {
		return false
	}
	// .git can be a directory (regular repo) or a file (worktree, submodule).
	return info != nil
}

// scanRefs returns the current local-branch refs and HEAD for the repo.
// On error returns an empty snapshot — diffing against empty just looks
// like the repo "vanished" until the next poll succeeds, which is fine.
func (gw *gitWatcher) scanRefs(ctx context.Context, repo string) refSnapshot {
	snap := refSnapshot{refs: map[string]string{}}
	ctx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, "git", "-C", repo,
		"for-each-ref", "--format=%(refname) %(objectname)", "refs/heads", "refs/remotes")
	cmd.Stdin = nil
	out, err := cmd.Output()
	if err != nil {
		return snap
	}
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		parts := strings.SplitN(line, " ", 2)
		if len(parts) != 2 {
			continue
		}
		snap.refs[parts[0]] = parts[1]
	}

	// HEAD: branch name when on a branch, sha when detached.
	headCmd := exec.CommandContext(ctx, "git", "-C", repo, "symbolic-ref", "--quiet", "--short", "HEAD")
	headCmd.Stdin = nil
	if b, err := headCmd.Output(); err == nil {
		snap.head = strings.TrimSpace(string(b))
	} else {
		shaCmd := exec.CommandContext(ctx, "git", "-C", repo, "rev-parse", "HEAD")
		shaCmd.Stdin = nil
		if b, err := shaCmd.Output(); err == nil {
			snap.head = "detached@" + strings.TrimSpace(string(b))
		}
	}
	return snap
}

func (gw *gitWatcher) diffAndEmit(ctx context.Context, t gitTarget, prev, next refSnapshot) {
	for ref, sha := range next.refs {
		isRemote := strings.HasPrefix(ref, "refs/remotes/")
		prevSha, existed := prev.refs[ref]
		if !existed {
			kind := "branch_create"
			if isRemote {
				kind = "remote_create"
			}
			ev := GitEvent{
				Ts:        time.Now().UTC(),
				Repo:      t.path,
				RepoLabel: t.label,
				Kind:      kind,
				Ref:       ref,
				Branch:    shortBranch(ref),
				SHA:       sha,
				ShortSHA:  shortSHA(sha),
			}
			gw.enrichCommit(ctx, t.path, &ev)
			gw.broadcast(ev)
			continue
		}
		if prevSha != sha {
			kind := "commit"
			if isRemote {
				kind = "remote_update"
			}
			ev := GitEvent{
				Ts:        time.Now().UTC(),
				Repo:      t.path,
				RepoLabel: t.label,
				Kind:      kind,
				Ref:       ref,
				Branch:    shortBranch(ref),
				SHA:       sha,
				ShortSHA:  shortSHA(sha),
			}
			gw.enrichCommit(ctx, t.path, &ev)
			gw.broadcast(ev)
		}
	}
	for ref := range prev.refs {
		if _, still := next.refs[ref]; !still {
			kind := "branch_delete"
			if strings.HasPrefix(ref, "refs/remotes/") {
				kind = "remote_delete"
			}
			gw.broadcast(GitEvent{
				Ts:        time.Now().UTC(),
				Repo:      t.path,
				RepoLabel: t.label,
				Kind:      kind,
				Ref:       ref,
				Branch:    shortBranch(ref),
			})
		}
	}
	if prev.head != "" && next.head != "" && prev.head != next.head {
		gw.broadcast(GitEvent{
			Ts:        time.Now().UTC(),
			Repo:      t.path,
			RepoLabel: t.label,
			Kind:      "head_change",
			Branch:    next.head,
		})
	}
}

func (gw *gitWatcher) enrichCommit(ctx context.Context, repo string, ev *GitEvent) {
	if ev.SHA == "" {
		return
	}
	ctx, cancel := context.WithTimeout(ctx, 1500*time.Millisecond)
	defer cancel()
	cmd := exec.CommandContext(ctx, "git", "-C", repo, "show", "-s",
		"--format=%an%x00%s", ev.SHA)
	cmd.Stdin = nil
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return
	}
	line := strings.TrimRight(stdout.String(), "\n")
	parts := strings.SplitN(line, "\x00", 2)
	if len(parts) == 2 {
		ev.Author = parts[0]
		ev.Subject = parts[1]
	}
}

func shortSHA(sha string) string {
	if len(sha) >= 8 {
		return sha[:8]
	}
	return sha
}

func shortBranch(ref string) string {
	if strings.HasPrefix(ref, "refs/heads/") {
		return strings.TrimPrefix(ref, "refs/heads/")
	}
	if strings.HasPrefix(ref, "refs/remotes/") {
		return strings.TrimPrefix(ref, "refs/remotes/")
	}
	return ref
}

// runGt invokes the gt binary the same way APIHandler does, but without the
// concurrent-command semaphore (watcher discovery is single-threaded so it
// can't trigger a process storm on its own).
func (gw *gitWatcher) runGt(ctx context.Context, timeout time.Duration, args []string) (string, error) {
	gtPath := gw.gtPath
	if gtPath == "" {
		gtPath = "gt"
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, gtPath, args...)
	if gw.workDir != "" {
		cmd.Dir = gw.workDir
	}
	cmd.Stdin = nil
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr
	if err := cmd.Run(); err != nil {
		return stdout.String(), fmt.Errorf("gt %s: %w (%s)", strings.Join(args, " "), err, strings.TrimSpace(stderr.String()))
	}
	return stdout.String(), nil
}

