package web

import (
	"fmt"
	"os/exec"
	"strings"
	"time"

	"github.com/steveyegge/gastown/internal/tmux"
)

// consoleSessionPrefix is the tmux session name prefix used for all
// dashboard-spawned ephemeral consoles. Shared between spawn (api.go) and
// kill (handleSessionKill) so both halves agree on what is eligible for
// reaping via /api/session/kill.
const consoleSessionPrefix = "gt-console-"

// consoleHistoryLimit caps the tmux per-pane scrollback for spawned
// gt-console-* sessions. Kept in sync with the capture-pane `-S` bound in
// session_attach.go and the scrollback option in terminal-attach.js.
const consoleHistoryLimit = "50000"

// gtConsoleExitWrapper wraps `gt <args>` so the tmux session keeps running
// after the gt command exits — the user reads the output, runs follow-up
// commands, and closes the pane when ready (dashboard Close button kills
// the session via /api/session/kill).
func gtConsoleExitWrapper(gtPath string, args []string) string {
	cmd := gtPath
	for _, a := range args {
		cmd += " " + a
	}
	return cmd + `; printf '\n[exited %d — type exit or close to end]\n' "$?"; ` +
		`exec "${SHELL:-/bin/bash}" -l`
}

// gitGraphConsoleWrapper renders an ASCII commit graph (gitgraph-style) in
// the spawned console, then drops to an interactive shell so the user can
// re-run git commands or close the pane. Colour is forced because the pane
// is a real TTY under tmux. Capped at 300 commits to keep the initial paint
// fast on large repos.
func gitGraphConsoleWrapper() string {
	return `git log --graph --oneline --decorate --all --color=always -n 300; ` +
		`printf '\n[git graph above — wheel to scroll, type exit or close to end]\n'; ` +
		`exec "${SHELL:-/bin/bash}" -l`
}

// spawnTmuxConsole creates a detached tmux session with a unique
// gt-console-<unix-nano> name and the bumped history-limit applied
// BEFORE the pane is created (tmux freezes history-limit at pane-creation
// time — setting it afterwards is a no-op for the existing pane).
//
// command is the shell command to run in the new pane. Pass an empty
// string to launch the user's default login shell with no wrapper —
// that's the bare-console variant used by the palette `console` entry.
func spawnTmuxConsole(workDir, command string) (string, error) {
	sessionName := fmt.Sprintf("%s%d", consoleSessionPrefix, time.Now().UnixNano())

	tmuxArgs := []string{}
	if sock := tmux.GetDefaultSocket(); sock != "" {
		tmuxArgs = append(tmuxArgs, "-L", sock)
	}
	// Compound command: bump history-limit BEFORE creating the pane so the
	// new window inherits the larger scrollback. tmux's CLI parser splits
	// commands on `;` when it's a separate argv entry — passing it as a
	// distinct arg via exec.Command keeps shell-escaping out of the picture.
	tmuxArgs = append(tmuxArgs,
		"set-option", "-g", "history-limit", consoleHistoryLimit, ";",
		"new-session", "-d", "-s", sessionName,
	)
	if command != "" {
		tmuxArgs = append(tmuxArgs, command)
	}

	cmd := exec.Command("tmux", tmuxArgs...)
	if workDir != "" {
		cmd.Dir = workDir
	}
	if out, err := cmd.CombinedOutput(); err != nil {
		return "", fmt.Errorf("tmux new-session: %w (%s)", err, strings.TrimSpace(string(out)))
	}
	return sessionName, nil
}
