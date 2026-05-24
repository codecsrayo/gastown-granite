package web

import (
	"encoding/json"
	"log"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"strings"
	"time"

	"github.com/creack/pty"
	"github.com/gorilla/websocket"

	"github.com/steveyegge/gastown/internal/session"
	"github.com/steveyegge/gastown/internal/tmux"
)

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

type sessionAttachClientMsg struct {
	Type string `json:"type"`
	Cols uint16 `json:"cols"`
	Rows uint16 `json:"rows"`
	Data string `json:"data"`
}

// handleSessionAttach upgrades the request to a WebSocket and streams a
// bidirectional tmux attach session. Client sends keystrokes (binary frames
// or text frames with `{type:"input",data:"..."}`) and the server streams
// terminal output as binary frames. Resize via `{type:"resize",cols,rows}`.
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

	args := []string{}
	if sock := tmux.GetDefaultSocket(); sock != "" {
		args = append(args, "-L", sock)
	}
	// `-u` forces UTF-8. `attach -d` detaches any other clients so tmux
	// honors this client's window size (otherwise it picks MIN across all
	// attached clients, leaving the browser pane stuck at 80x24).
	args = append(args, "-u", "attach", "-d", "-t", sessionName)

	cmd := exec.Command("tmux", args...)
	cmd.Env = append(os.Environ(), "TERM=xterm-256color")

	ptmx, err := pty.Start(cmd)
	if err != nil {
		_ = conn.WriteMessage(websocket.TextMessage, []byte("tmux start failed: "+err.Error()))
		return
	}

	cleanup := func() {
		_ = ptmx.Close()
		if cmd.Process != nil {
			_ = cmd.Process.Kill()
		}
		_, _ = cmd.Process.Wait()
	}
	defer cleanup()

	// PTY -> WS
	ptyDone := make(chan struct{})
	go func() {
		defer close(ptyDone)
		buf := make([]byte, 8192)
		for {
			n, err := ptmx.Read(buf)
			if n > 0 {
				if werr := conn.WriteMessage(websocket.BinaryMessage, buf[:n]); werr != nil {
					return
				}
			}
			if err != nil {
				return
			}
		}
	}()

	// Periodic ping to keep proxies from closing the connection.
	pingTicker := time.NewTicker(20 * time.Second)
	defer pingTicker.Stop()
	go func() {
		for range pingTicker.C {
			_ = conn.WriteControl(websocket.PingMessage, nil, time.Now().Add(5*time.Second))
		}
	}()

	// WS -> PTY (blocks until client disconnects or pty closes)
	for {
		select {
		case <-ptyDone:
			return
		default:
		}
		mt, data, err := conn.ReadMessage()
		if err != nil {
			return
		}
		switch mt {
		case websocket.BinaryMessage:
			if _, werr := ptmx.Write(data); werr != nil {
				return
			}
		case websocket.TextMessage:
			var msg sessionAttachClientMsg
			if jerr := json.Unmarshal(data, &msg); jerr != nil {
				// Treat as raw input.
				if _, werr := ptmx.Write(data); werr != nil {
					return
				}
				continue
			}
			switch msg.Type {
			case "resize":
				if msg.Cols == 0 || msg.Rows == 0 {
					continue
				}
				_ = pty.Setsize(ptmx, &pty.Winsize{Cols: msg.Cols, Rows: msg.Rows})
			case "input":
				if _, werr := ptmx.Write([]byte(msg.Data)); werr != nil {
					return
				}
			}
		}
	}
}
