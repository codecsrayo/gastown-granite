package web

import (
	"encoding/json"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"sync/atomic"
	"time"

	"github.com/creack/pty"
	"github.com/gorilla/websocket"

	"github.com/steveyegge/gastown/internal/session"
	"github.com/steveyegge/gastown/internal/tmux"
)

// ttyd-compatible single-byte command prefixes.
// Server → Client:
const (
	srvOutput      = '0'
	srvWindowTitle = '1'
	srvPreferences = '2'
)

// Client → Server:
const (
	cliInput  = '0'
	cliResize = '1'
	cliPause  = '2'
	cliResume = '3'
)

// outCapacity bounds how many PTY-output frames may queue ahead of a slow
// WebSocket writer. When full, the PTY reader blocks, which in turn applies
// backpressure to tmux so we never grow memory unbounded.
const outCapacity = 64

var sessionAttachUpgrader = websocket.Upgrader{
	ReadBufferSize:  4096,
	WriteBufferSize: 4096,
	CheckOrigin: func(r *http.Request) bool {
		origin := r.Header.Get("Origin")
		if origin == "" {
			// Non-browser client; allow.
			return true
		}
		u, err := url.Parse(origin)
		if err != nil {
			return false
		}
		return strings.EqualFold(u.Host, r.Host)
	},
}

type resizePayload struct {
	Cols uint16 `json:"cols"`
	Rows uint16 `json:"rows"`
}

// handleSessionAttach upgrades the request to a WebSocket and streams a
// bidirectional tmux attach session using the ttyd wire protocol (single
// command byte prefix). Three goroutines coordinate per connection:
//
//	pty-reader  — copies tmux output into a bounded channel.
//	ws-writer   — drains that channel, owns the only WriteMessage call.
//	ws-reader   — drives WriteMessage-free reads and forwards to PTY.
//
// Centralizing writes is required by gorilla/websocket (one concurrent
// writer max). WriteControl pings remain safe in parallel per their docs.
func (h *APIHandler) handleSessionAttach(w http.ResponseWriter, r *http.Request) {
	sessionName := r.URL.Query().Get("session")
	if sessionName == "" {
		http.Error(w, "missing session", http.StatusBadRequest)
		return
	}
	if !session.HasKnownPrefix(sessionName) {
		http.Error(w, "invalid session name: must start with a known rig prefix", http.StatusBadRequest)
		return
	}
	for _, c := range sessionName {
		if !((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '_') {
			http.Error(w, "invalid session name: contains invalid characters", http.StatusBadRequest)
			return
		}
	}

	conn, err := sessionAttachUpgrader.Upgrade(w, r, nil)
	if err != nil {
		log.Printf("session attach: ws upgrade failed: %v", err)
		return
	}
	defer conn.Close()

	socketArgs := []string{}
	if sock := tmux.GetDefaultSocket(); sock != "" {
		socketArgs = []string{"-L", sock}
	}

	// Dump scrollback history first so the user can wheel-scroll up to
	// see what happened before this attach started. `-e` preserves ANSI
	// colors; `-S -10000` grabs up to 10k lines of history (matches the
	// client-side xterm scrollback cap so nothing is wasted). Done as a
	// one-shot exec and written directly to the WS — no goroutines yet,
	// so this WriteMessage is safe.
	capArgs := append([]string{}, socketArgs...)
	capArgs = append(capArgs, "capture-pane", "-p", "-e", "-S", "-10000", "-t", sessionName)
	if capOut, capErr := exec.Command("tmux", capArgs...).Output(); capErr == nil && len(capOut) > 0 {
		frame := make([]byte, len(capOut)+1)
		frame[0] = srvOutput
		copy(frame[1:], capOut)
		if werr := conn.WriteMessage(websocket.BinaryMessage, frame); werr != nil {
			return
		}
	}

	// `-u` forces UTF-8. `attach -d` detaches any other clients so tmux
	// honors this client's window size (otherwise it picks MIN across all
	// attached clients, leaving the browser pane stuck at 80x24).
	attachArgs := append([]string{}, socketArgs...)
	attachArgs = append(attachArgs, "-u", "attach", "-d", "-t", sessionName)

	cmd := exec.Command("tmux", attachArgs...)
	cmd.Env = append(os.Environ(), "TERM=xterm-256color")

	ptmx, err := pty.Start(cmd)
	if err != nil {
		_ = conn.WriteMessage(websocket.TextMessage, []byte("tmux start failed: "+err.Error()))
		return
	}

	outCh := make(chan []byte, outCapacity)
	done := make(chan struct{})
	var paused atomic.Bool

	// Cleanup deferred so it runs even on panic. Order matters: kill the
	// PTY first so the producer goroutine's ptmx.Read returns an error and
	// the producer exits BEFORE we close outCh. Sending on a closed
	// channel panics and would crash the whole process.
	defer func() {
		_ = ptmx.Close()
		if cmd.Process != nil {
			_ = cmd.Process.Kill()
		}
		_, _ = cmd.Process.Wait()
	}()

	// ws-writer: only goroutine allowed to call WriteMessage.
	writerDone := make(chan struct{})
	go func() {
		defer close(writerDone)
		for frame := range outCh {
			if err := conn.WriteMessage(websocket.BinaryMessage, frame); err != nil {
				// Drain remaining frames so producer doesn't deadlock,
				// then exit. close(done) below signals reader to stop.
				go func() {
					for range outCh {
					}
				}()
				return
			}
		}
	}()

	// pty-reader: copies PTY output → channel with ttyd prefix.
	go func() {
		defer close(done)
		buf := make([]byte, 8192)
		for {
			n, err := ptmx.Read(buf)
			if n > 0 {
				// Spin-wait while paused. PAUSE is rare and short-lived
				// (client flow control), so polling is fine here.
				for paused.Load() {
					select {
					case <-time.After(50 * time.Millisecond):
					case <-writerDone:
						return
					}
				}
				frame := make([]byte, n+1)
				frame[0] = srvOutput
				copy(frame[1:], buf[:n])
				select {
				case outCh <- frame:
				case <-writerDone:
					return
				}
			}
			if err != nil {
				return
			}
		}
	}()

	// Periodic ping. WriteControl is documented safe alongside WriteMessage.
	pingTicker := time.NewTicker(20 * time.Second)
	defer pingTicker.Stop()
	pingDone := make(chan struct{})
	go func() {
		defer close(pingDone)
		for {
			select {
			case <-pingTicker.C:
				_ = conn.WriteControl(websocket.PingMessage, nil, time.Now().Add(5*time.Second))
			case <-done:
				return
			}
		}
	}()

	closeReason := "session ended"
	// ws-reader: blocks until client disconnects or PTY closes.
readLoop:
	for {
		select {
		case <-done:
			break readLoop
		default:
		}
		_, data, err := conn.ReadMessage()
		if err != nil {
			closeReason = "client disconnected"
			break readLoop
		}
		if len(data) == 0 {
			continue
		}
		switch data[0] {
		case cliInput:
			if _, werr := ptmx.Write(data[1:]); werr != nil {
				closeReason = "pty write failed"
				break readLoop
			}
		case cliResize:
			var rs resizePayload
			if jerr := json.Unmarshal(data[1:], &rs); jerr != nil {
				continue
			}
			if rs.Cols == 0 || rs.Rows == 0 {
				continue
			}
			_ = pty.Setsize(ptmx, &pty.Winsize{Cols: rs.Cols, Rows: rs.Rows})
		case cliPause:
			paused.Store(true)
		case cliResume:
			paused.Store(false)
		}
	}

	// Wind down. Order matters to avoid send-on-closed-channel panic:
	//   1. Close PTY → producer's ptmx.Read errors → producer exits.
	//   2. Wait for producer (<-done) to confirm no more sends to outCh.
	//   3. Close outCh → writer drains pending frames then exits.
	_ = ptmx.Close()
	<-done
	close(outCh)
	<-writerDone

	exitCode := -1
	if cmd.ProcessState != nil {
		exitCode = cmd.ProcessState.ExitCode()
	}
	reason := fmt.Sprintf("%s (exit %d)", closeReason, exitCode)
	_ = conn.WriteControl(
		websocket.CloseMessage,
		websocket.FormatCloseMessage(websocket.CloseNormalClosure, reason),
		time.Now().Add(time.Second),
	)
}
