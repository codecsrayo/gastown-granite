package cmd

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"
	"github.com/steveyegge/gastown/internal/events"
	"github.com/steveyegge/gastown/internal/style"
	"github.com/steveyegge/gastown/internal/townlog"
	"github.com/steveyegge/gastown/internal/workspace"
)

// Log command flags
var (
	logTail    int
	logType    string
	logAgent   string
	logSince   string
	logFollow  bool
	logAcp     bool
	logOnline  bool
	logBacklog int

	// log crash flags
	crashAgent    string
	crashSession  string
	crashExitCode int
)

var logCmd = &cobra.Command{
	Use:     "log",
	Aliases: []string{"logs"},
	GroupID: GroupDiag,
	Short:   "View town activity log",
	Long: `View the centralized log of Gas Town agent lifecycle events.

Events logged include:
  spawn   - new agent created
  wake    - agent resumed
  nudge   - message injected into agent
  handoff - agent handed off to fresh session
  done    - agent finished work
  crash   - agent exited unexpectedly
  kill    - agent killed intentionally

Examples:
  gt log                     # Show last 20 events
  gt log -n 50               # Show last 50 events
  gt log --type spawn        # Show only spawn events
  gt log --agent greenplace/    # Show events for gastown rig
  gt log --since 1h          # Show events from last hour
  gt log -f                  # Follow log (like tail -f)
  gt log --online            # Live-listen to ALL streams (town, events, daemon, acp)
  gt logs --online           # Same (logs is an alias for log)`,
	RunE: runLog,
}

var logCrashCmd = &cobra.Command{
	Use:   "crash",
	Short: "Record a crash event (called by tmux pane-died hook)",
	Long: `Record a crash event to the town log.

This command is called automatically by tmux when a pane exits unexpectedly.
It's not typically run manually.

The exit code determines if this was a crash or expected exit:
  - Exit code 0: Expected exit (logged as 'done' if no other done was recorded)
  - Exit code non-zero: Crash (logged as 'crash')

Examples:
  gt log crash --agent greenplace/Toast --session gt-greenplace-Toast --exit-code 1`,
	RunE: runLogCrash,
}

func init() {
	logCmd.Flags().IntVarP(&logTail, "tail", "n", 20, "Number of events to show")
	logCmd.Flags().StringVarP(&logType, "type", "t", "", "Filter by event type (spawn,wake,nudge,handoff,done,crash,kill)")
	logCmd.Flags().StringVarP(&logAgent, "agent", "a", "", "Filter by agent prefix (e.g., gastown/, greenplace/crew/max)")
	logCmd.Flags().StringVar(&logSince, "since", "", "Show events since duration (e.g., 1h, 30m, 24h)")
	logCmd.Flags().BoolVarP(&logFollow, "follow", "f", false, "Follow log output (like tail -f)")
	logCmd.Flags().BoolVar(&logAcp, "acp", false, "View ACP debug logs (requires GT_ACP_DEBUG=1)")
	logCmd.Flags().BoolVar(&logOnline, "online", false, "Live-listen to ALL log streams (town, events, daemon, acp) merged with source prefix")
	logCmd.Flags().IntVar(&logBacklog, "backlog", 20, "When --online: lines of recent history per source to print before going live")

	// crash subcommand flags
	logCrashCmd.Flags().StringVar(&crashAgent, "agent", "", "Agent ID (e.g., greenplace/Toast)")
	logCrashCmd.Flags().StringVar(&crashSession, "session", "", "Tmux session name")
	logCrashCmd.Flags().IntVar(&crashExitCode, "exit-code", -1, "Exit code from pane")
	_ = logCrashCmd.MarkFlagRequired("agent")

	logCmd.AddCommand(logCrashCmd)
	rootCmd.AddCommand(logCmd)
}

func runLog(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwdOrError()
	if err != nil {
		return fmt.Errorf("not in a Gas Town workspace: %w", err)
	}

	// Online mode = live-listen to ALL streams concurrently.
	if logOnline {
		return runOnlineMode(townRoot, logBacklog)
	}

	// Handle --acp flag to view ACP debug logs
	if logAcp {
		return viewACPLogs(townRoot)
	}

	logPath := fmt.Sprintf("%s/logs/town.log", townRoot)

	// If following, use tail -f
	if logFollow {
		return followLog(logPath)
	}

	// Check if log file exists
	if _, err := os.Stat(logPath); os.IsNotExist(err) {
		fmt.Printf("%s No log file yet (no events recorded)\n", style.Dim.Render("○"))
		return nil
	}

	// Read events
	events, err := townlog.ReadEvents(townRoot)
	if err != nil {
		return fmt.Errorf("reading events: %w", err)
	}

	if len(events) == 0 {
		fmt.Printf("%s No events in log\n", style.Dim.Render("○"))
		return nil
	}

	// Build filter
	filter := townlog.Filter{}

	if logType != "" {
		filter.Type = townlog.EventType(logType)
	}

	if logAgent != "" {
		filter.Agent = logAgent
	}

	if logSince != "" {
		duration, err := time.ParseDuration(logSince)
		if err != nil {
			return fmt.Errorf("invalid --since duration: %w", err)
		}
		filter.Since = time.Now().Add(-duration)
	}

	// Apply filter
	events = townlog.FilterEvents(events, filter)

	// Apply tail limit
	if logTail > 0 && len(events) > logTail {
		events = events[len(events)-logTail:]
	}

	if len(events) == 0 {
		fmt.Printf("%s No events match filter\n", style.Dim.Render("○"))
		return nil
	}

	// Print events
	for _, e := range events {
		printEvent(e)
	}

	return nil
}

// followLog uses tail -f to follow the log file.
func followLog(logPath string) error {
	// Check if log file exists, create empty if not
	if _, err := os.Stat(logPath); os.IsNotExist(err) {
		// Create logs directory and empty file
		if err := os.MkdirAll(fmt.Sprintf("%s", logPath[:len(logPath)-len("town.log")-1]), 0755); err != nil {
			return fmt.Errorf("creating logs directory: %w", err)
		}
		if _, err := os.Create(logPath); err != nil {
			return fmt.Errorf("creating log file: %w", err)
		}
	}

	fmt.Printf("%s Following %s (Ctrl+C to stop)\n\n", style.Dim.Render("○"), logPath)

	tailCmd := exec.Command("tail", "-f", logPath)
	tailCmd.Stdout = os.Stdout
	tailCmd.Stderr = os.Stderr

	return tailCmd.Run()
}

// viewACPLogs displays the ACP debug log file.
func viewACPLogs(townRoot string) error {
	logPath := fmt.Sprintf("%s/logs/acp.log", townRoot)

	// If following, use tail -f
	if logFollow {
		return followLog(logPath)
	}

	// Check if log file exists
	if _, err := os.Stat(logPath); os.IsNotExist(err) {
		fmt.Printf("%s No ACP log file. Set GT_ACP_DEBUG=1 to enable logging.\n", style.Dim.Render("○"))
		return nil
	}

	// Read the log file
	content, err := os.ReadFile(logPath)
	if err != nil {
		return fmt.Errorf("reading ACP log: %w", err)
	}

	lines := strings.Split(string(content), "\n")

	// Apply tail limit
	if logTail > 0 && len(lines) > logTail {
		lines = lines[len(lines)-logTail:]
	}

	// Print lines
	for _, line := range lines {
		if line != "" {
			fmt.Println(line)
		}
	}

	return nil
}

// printEvent prints a single event with styling.
func printEvent(e townlog.Event) {
	ts := e.Timestamp.Format("2006-01-02 15:04:05")

	// Color-code event types
	var typeStr string
	switch e.Type {
	case townlog.EventSpawn:
		typeStr = style.Success.Render("[spawn]")
	case townlog.EventWake:
		typeStr = style.Bold.Render("[wake]")
	case townlog.EventNudge:
		typeStr = style.Dim.Render("[nudge]")
	case townlog.EventHandoff:
		typeStr = style.Bold.Render("[handoff]")
	case townlog.EventHandoffNoPersist:
		typeStr = style.Error.Render("[handoff-NOPERSIST]")
	case townlog.EventDone:
		typeStr = style.Success.Render("[done]")
	case townlog.EventCrash:
		typeStr = style.Error.Render("[crash]")
	case townlog.EventKill:
		typeStr = style.Warning.Render("[kill]")
	case townlog.EventCallback:
		typeStr = style.Bold.Render("[callback]")
	case townlog.EventPatrolStarted:
		typeStr = style.Bold.Render("[patrol_started]")
	case townlog.EventPolecatChecked:
		typeStr = style.Dim.Render("[polecat_checked]")
	case townlog.EventPolecatNudged:
		typeStr = style.Warning.Render("[polecat_nudged]")
	case townlog.EventEscalationSent:
		typeStr = style.Error.Render("[escalation_sent]")
	case townlog.EventPatrolComplete:
		typeStr = style.Success.Render("[patrol_complete]")
	default:
		typeStr = fmt.Sprintf("[%s]", e.Type)
	}

	detail := formatEventDetail(e)
	fmt.Printf("%s %s %s %s\n", style.Dim.Render(ts), typeStr, e.Agent, detail)
}

// formatEventDetail returns a human-readable detail string for an event.
func formatEventDetail(e townlog.Event) string {
	switch e.Type {
	case townlog.EventSpawn:
		if e.Context != "" {
			return fmt.Sprintf("spawned for %s", e.Context)
		}
		return "spawned"
	case townlog.EventWake:
		if e.Context != "" {
			return fmt.Sprintf("resumed (%s)", e.Context)
		}
		return "resumed"
	case townlog.EventNudge:
		if e.Context != "" {
			return fmt.Sprintf("nudged with %q", truncateStr(e.Context, 40))
		}
		return "nudged"
	case townlog.EventHandoff:
		if e.Context != "" {
			return fmt.Sprintf("handed off (%s)", e.Context)
		}
		return "handed off"
	case townlog.EventHandoffNoPersist:
		if e.Context != "" {
			return fmt.Sprintf("handoff FAILED (%s)", e.Context)
		}
		return "handoff FAILED (no persist)"
	case townlog.EventDone:
		if e.Context != "" {
			return fmt.Sprintf("completed %s", e.Context)
		}
		return "completed work"
	case townlog.EventCrash:
		if e.Context != "" {
			return fmt.Sprintf("exited unexpectedly (%s)", e.Context)
		}
		return "exited unexpectedly"
	case townlog.EventKill:
		if e.Context != "" {
			return fmt.Sprintf("killed (%s)", e.Context)
		}
		return "killed"
	case townlog.EventCallback:
		if e.Context != "" {
			return fmt.Sprintf("callback: %s", e.Context)
		}
		return "callback processed"
	case townlog.EventPatrolStarted:
		if e.Context != "" {
			return fmt.Sprintf("started patrol (%s)", e.Context)
		}
		return "started patrol"
	case townlog.EventPolecatChecked:
		if e.Context != "" {
			return fmt.Sprintf("checked %s", e.Context)
		}
		return "checked polecat"
	case townlog.EventPolecatNudged:
		if e.Context != "" {
			return fmt.Sprintf("nudged (%s)", e.Context)
		}
		return "nudged polecat"
	case townlog.EventEscalationSent:
		if e.Context != "" {
			return fmt.Sprintf("escalated (%s)", e.Context)
		}
		return "escalated"
	case townlog.EventPatrolComplete:
		if e.Context != "" {
			return fmt.Sprintf("patrol complete (%s)", e.Context)
		}
		return "patrol complete"
	default:
		if e.Context != "" {
			return fmt.Sprintf("%s (%s)", e.Type, e.Context)
		}
		return string(e.Type)
	}
}

func truncateStr(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen-3] + "..."
}

// runLogCrash handles the "gt log crash" command from tmux pane-died hooks.
func runLogCrash(cmd *cobra.Command, args []string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil || townRoot == "" {
		// Try to find town root from conventional location
		// This is called from tmux hook which may not have proper cwd
		home := os.Getenv("HOME")
		defaultRoot := home + "/gt"
		if _, statErr := os.Stat(defaultRoot + "/mayor"); statErr == nil {
			townRoot = defaultRoot
		}
		if townRoot == "" {
			return fmt.Errorf("cannot find town root (tried cwd and ~/gt)")
		}
	}

	// Determine event type based on exit code
	var eventType townlog.EventType
	var context string

	if crashExitCode == 0 {
		// Exit code 0 = normal exit
		// Could be handoff, done, or user quit - we log as "done" if no prior done event
		// The Witness can analyze further if needed
		eventType = townlog.EventDone
		context = "exited normally"
	} else if crashExitCode == 130 {
		// Exit code 130 = Ctrl+C (SIGINT)
		// This is typically intentional user interrupt
		eventType = townlog.EventKill
		context = fmt.Sprintf("interrupted (exit %d)", crashExitCode)
	} else {
		// Non-zero exit = crash
		eventType = townlog.EventCrash
		context = fmt.Sprintf("exit code %d", crashExitCode)
		if crashSession != "" {
			context += fmt.Sprintf(" (session: %s)", crashSession)
		}
	}

	// Log the event
	logger := townlog.NewLogger(townRoot)
	if err := logger.Log(eventType, crashAgent, context); err != nil {
		return fmt.Errorf("logging event: %w", err)
	}
	if eventType == townlog.EventCrash {
		logCrashFeedEvent(townRoot, crashAgent, crashSession, crashExitCode)
	}

	return nil
}

func logCrashFeedEvent(townRoot, agent, session string, exitCode int) {
	if townRoot == "" {
		return
	}
	if session == "" {
		session = "unknown"
	}

	origDir, getwdErr := os.Getwd()
	if err := os.Chdir(townRoot); err != nil {
		return
	}
	if getwdErr == nil {
		defer func() { _ = os.Chdir(origDir) }()
	}

	reason := fmt.Sprintf("crashed with exit code %d", exitCode)
	payload := events.SessionDeathPayload(session, agent, reason, "gt log crash")
	payload["exit_code"] = exitCode
	_ = events.LogFeed(events.TypeSessionDeath, agent, payload)
}

// LogEvent is a helper that logs an event from anywhere in the codebase.
// It finds the town root and logs the event.
func LogEvent(eventType townlog.EventType, agent, context string) error {
	townRoot, err := workspace.FindFromCwd()
	if err != nil {
		return err // Silently fail if not in a workspace
	}
	if townRoot == "" {
		return nil
	}

	logger := townlog.NewLogger(townRoot)
	return logger.Log(eventType, agent, context)
}

// LogEventWithRoot logs an event when the town root is already known.
func LogEventWithRoot(townRoot string, eventType townlog.EventType, agent, context string) error {
	logger := townlog.NewLogger(townRoot)
	return logger.Log(eventType, agent, context)
}

// Convenience functions for common events

// LogSpawn logs a spawn event.
func LogSpawn(townRoot, agent, issueID string) error {
	return LogEventWithRoot(townRoot, townlog.EventSpawn, agent, issueID)
}

// LogWake logs a wake event.
func LogWake(townRoot, agent, context string) error {
	return LogEventWithRoot(townRoot, townlog.EventWake, agent, context)
}

// LogNudge logs a nudge event.
func LogNudge(townRoot, agent, message string) error {
	return LogEventWithRoot(townRoot, townlog.EventNudge, agent, strings.TrimSpace(message))
}

// LogHandoff logs a handoff event.
func LogHandoff(townRoot, agent, context string) error {
	return LogEventWithRoot(townRoot, townlog.EventHandoff, agent, context)
}

// LogHandoffNoPersist logs a failed handoff where Dolt persistence failed.
// Creates a distinct marker in town.log so crash recovery can identify
// handoffs that were attempted but never persisted to Dolt.
func LogHandoffNoPersist(townRoot, agent, context string, persistErr error) error {
	msg := context
	if persistErr != nil {
		msg = fmt.Sprintf("%s — error: %v", context, persistErr)
	}
	return LogEventWithRoot(townRoot, townlog.EventHandoffNoPersist, agent, msg)
}

// LogDone logs a done event.
func LogDone(townRoot, agent, issueID string) error {
	return LogEventWithRoot(townRoot, townlog.EventDone, agent, issueID)
}

// LogCrash logs a crash event.
func LogCrash(townRoot, agent, reason string) error {
	return LogEventWithRoot(townRoot, townlog.EventCrash, agent, reason)
}

// LogKill logs a kill event.
func LogKill(townRoot, agent, reason string) error {
	return LogEventWithRoot(townRoot, townlog.EventKill, agent, reason)
}

// --- Online mode: merged live tail across all Gas Town log streams ---

// onlineSource describes one log file we tail in online mode.
type onlineSource struct {
	tag    string                       // short label shown in output
	path   string                       // file path to tail
	style  func(string) string          // optional color wrapper for the tag
	format func(line string) string     // optional per-line formatter (e.g. JSONL decode)
}

// runOnlineMode tails every known Gas Town log stream concurrently
// and merges their output to stdout, prefixed by source tag.
// Exits cleanly on SIGINT / SIGTERM.
func runOnlineMode(townRoot string, backlog int) error {
	sources := []onlineSource{
		{
			tag:   "town",
			path:  filepath.Join(townRoot, "logs", "town.log"),
			style: func(s string) string { return style.Bold.Render(s) },
		},
		{
			tag:    "event",
			path:   filepath.Join(townRoot, events.EventsFile),
			style:  func(s string) string { return style.Success.Render(s) },
			format: formatEventJSONLine,
		},
		{
			tag:   "dolt",
			path:  filepath.Join(townRoot, "daemon", "dolt.log"),
			style: func(s string) string { return style.Dim.Render(s) },
		},
		{
			tag:   "acp",
			path:  filepath.Join(townRoot, "logs", "acp.log"),
			style: func(s string) string { return style.Warning.Render(s) },
		},
	}

	// Announce what we're listening to.
	fmt.Printf("%s Online mode — listening on:\n", style.Bold.Render("●"))
	for _, s := range sources {
		state := "ready"
		if _, err := os.Stat(s.path); os.IsNotExist(err) {
			state = "missing (will appear when created)"
		}
		fmt.Printf("  %s %s  (%s)\n", s.style(fmt.Sprintf("[%s]", s.tag)), s.path, style.Dim.Render(state))
	}
	fmt.Printf("%s\n\n", style.Dim.Render("Ctrl+C to stop"))

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	// Signal handling — cancel context on Ctrl+C / SIGTERM.
	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)
	defer signal.Stop(sigCh)
	go func() {
		<-sigCh
		cancel()
	}()

	// Print recent backlog from each source first, time-ordered when possible.
	if backlog > 0 {
		printOnlineBacklog(sources, backlog)
	}

	// Merge channel — every source writes formatted lines here.
	out := make(chan string, 256)

	for _, s := range sources {
		go tailOnlineSource(ctx, s, out)
	}

	// Drain. Loop until context cancelled AND channel idle.
	for {
		select {
		case <-ctx.Done():
			fmt.Printf("\n%s Online mode stopped.\n", style.Dim.Render("○"))
			return nil
		case line := <-out:
			fmt.Println(line)
		}
	}
}

// tailOnlineSource follows a single file from end-of-file, emitting
// each new line on out. Handles missing files (waits for creation) and
// truncation (re-opens from start). Stops when ctx is cancelled.
//
// Uses raw os.File reads (not bufio.Reader) because bufio caches the
// io.EOF result and won't re-read the underlying file when new bytes
// arrive — which is exactly the wrong behavior for a live tail.
func tailOnlineSource(ctx context.Context, src onlineSource, out chan<- string) {
	prefix := src.style(fmt.Sprintf("[%s]", src.tag))

	var (
		f      *os.File
		offset int64
		pend   []byte // partial-line buffer (bytes seen since last '\n')
	)

	openAtEnd := func() bool {
		fh, err := os.Open(src.path) //nolint:gosec // operator-controlled path
		if err != nil {
			return false
		}
		end, err := fh.Seek(0, io.SeekEnd)
		if err != nil {
			_ = fh.Close()
			return false
		}
		f = fh
		offset = end
		pend = pend[:0]
		return true
	}

	closeF := func() {
		if f != nil {
			_ = f.Close()
			f = nil
		}
		pend = pend[:0]
	}
	defer closeF()

	emit := func(line string) bool {
		if src.format != nil {
			line = src.format(line)
		}
		if line == "" {
			return true
		}
		select {
		case out <- fmt.Sprintf("%s %s", prefix, line):
			return true
		case <-ctx.Done():
			return false
		}
	}

	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()

	buf := make([]byte, 64*1024)

	// Open immediately so we capture the seek-to-end offset before any
	// new appends arrive — otherwise the first tick would lose any lines
	// written between goroutine start and the first 250ms ticker fire.
	openAtEnd()

	for {
		if f == nil {
			if !openAtEnd() {
				// Wait one tick before retrying (file may not exist yet).
				select {
				case <-ctx.Done():
					return
				case <-ticker.C:
					continue
				}
			}
		}

		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}

		// Detect truncation / rotation: file shrank.
		info, err := os.Stat(src.path)
		if err != nil {
			if os.IsNotExist(err) {
				closeF()
			}
			continue
		}
		if info.Size() < offset {
			closeF()
			fh, oerr := os.Open(src.path) //nolint:gosec // operator-controlled path
			if oerr != nil {
				continue
			}
			f = fh
			offset = 0
		}

		// Read everything new from offset onwards.
		for {
			n, rerr := f.Read(buf)
			if n > 0 {
				offset += int64(n)
				chunk := buf[:n]
				for {
					i := bytes.IndexByte(chunk, '\n')
					if i < 0 {
						pend = append(pend, chunk...)
						break
					}
					pend = append(pend, chunk[:i]...)
					if !emit(string(pend)) {
						return
					}
					pend = pend[:0]
					chunk = chunk[i+1:]
				}
			}
			if rerr != nil {
				// EOF or transient — stop reading; will retry next tick.
				break
			}
			if n == 0 {
				break
			}
		}
	}
}

// formatEventJSONLine decodes one JSONL event from .events.jsonl into
// a compact human-readable line. Falls back to the raw line on parse error.
func formatEventJSONLine(line string) string {
	line = strings.TrimSpace(line)
	if line == "" {
		return ""
	}
	var ev events.Event
	if err := json.Unmarshal([]byte(line), &ev); err != nil {
		return line
	}
	ts := ev.Timestamp
	if t, err := time.Parse(time.RFC3339, ev.Timestamp); err == nil {
		ts = t.Local().Format("2006-01-02 15:04:05")
	}
	parts := []string{
		style.Dim.Render(ts),
		fmt.Sprintf("%s", ev.Type),
	}
	if ev.Actor != "" {
		parts = append(parts, ev.Actor)
	}
	if len(ev.Payload) > 0 {
		parts = append(parts, style.Dim.Render(compactPayload(ev.Payload)))
	}
	return strings.Join(parts, " ")
}

// compactPayload renders a payload map as `k=v k=v` for inline display.
// Keys are sorted for deterministic output.
func compactPayload(p map[string]interface{}) string {
	keys := make([]string, 0, len(p))
	for k := range p {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	var b strings.Builder
	for i, k := range keys {
		if i > 0 {
			b.WriteByte(' ')
		}
		fmt.Fprintf(&b, "%s=%v", k, p[k])
	}
	return b.String()
}

// printOnlineBacklog prints up to `n` recent lines from each source,
// each prefixed with its tag, so users see context before live tail begins.
// Best-effort; missing files are skipped.
func printOnlineBacklog(sources []onlineSource, n int) {
	for _, s := range sources {
		lines, err := tailFileLines(s.path, n)
		if err != nil || len(lines) == 0 {
			continue
		}
		prefix := s.style(fmt.Sprintf("[%s]", s.tag))
		fmt.Printf("%s %s\n", style.Dim.Render("---"), style.Dim.Render(fmt.Sprintf("backlog: %s", s.tag)))
		for _, ln := range lines {
			out := ln
			if s.format != nil {
				out = s.format(ln)
			}
			if out == "" {
				continue
			}
			fmt.Printf("%s %s\n", prefix, out)
		}
	}
	fmt.Printf("%s %s\n\n", style.Dim.Render("---"), style.Dim.Render("live"))
}

// tailFileLines reads the last n lines of a file. Returns nil if missing.
func tailFileLines(path string, n int) ([]string, error) {
	data, err := os.ReadFile(path) //nolint:gosec // operator-controlled path
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	lines := strings.Split(strings.TrimRight(string(data), "\n"), "\n")
	if len(lines) > n {
		lines = lines[len(lines)-n:]
	}
	return lines, nil
}
