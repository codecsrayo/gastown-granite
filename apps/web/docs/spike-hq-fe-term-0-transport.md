# Spike `hq-fe-term.0` — Terminal transport

**Status:** decided · 2026-05-30
**Owner:** sheriff (session `hq-sheriff-host`)
**Epic:** `hq-fe-term` (Dock terminal, feature §11 in [frontend-features.md](frontend-features.md))
**Downstream:** `hq-fe-term.1` (PTY adapter), `hq-fe-term.2` (transport route), `hq-fe-view.11` (xterm dock)

## Question

The dashboard needs a dock terminal that attaches to an existing tmux session
(polecat / crew member) and supports both directions: tmux pane bytes streamed
to xterm, and user keystrokes sent back. Three transport substrates were on the
table:

- **A — WebSocket on gt-api (gt-web)**: new `GET /api/sessions/:id/term` upgrade,
  binary frames carry tmux pane bytes + keystrokes.
- **B — MCP tool with streaming**: a `terminal.attach` MCP tool on gt-mcp that
  emits chunks via the MCP transport (rmcp `StreamableHttpService` already
  mounted at `/mcp`).
- **C — separate binary** (`gt-term`): its own axum process, its own port,
  proxied by Traefik next to `gt-api` / `gt-mcp`.

## Decision

**Option A — WebSocket on gt-api (gt-web).**

Endpoint: `GET /api/sessions/:id/term` with `Upgrade: websocket`. Binary
frames; bytes from tmux pane flow to client, client text frames flow back as
`tmux send-keys` (or PTY write — see §Implementation).

## Why this and not the other two

### Why not B — MCP tool with streaming

- MCP tools are **request/response with streamed chunks**, not duplex. A
  pty attach is duplex: the client must send keystrokes mid-stream. Modelling
  that as one tool call ("attach"), then *another tool call per keystroke*
  multiplies tool-invocation envelopes (audit log + scope check + actor
  resolution every key) — see `record_envelope` plumbing in
  [gt-mcp/src/http.rs:6](../../api/crates/bins/gt-mcp/src/http.rs#L6). Wrong
  granularity.
- MCP is the **agent-to-orchestrator** plane (canon in memory
  `feedback_mcp_canonical_for_agents` / `feedback_dolt_sql_for_hq_beads`).
  The dashboard is a *human-to-orchestrator* surface; piping a human PTY
  through it conflates audiences and breaks the principle that MCP envelopes
  are agent invocations.
- rmcp's session manager is `LocalSessionManager` (in-process clones of
  `McpService`) — fine for short tool calls, but each long-lived session
  pins one `McpService` clone for the attach lifetime; the actor handles
  inside are `Arc`-cheap, but the *session table* is not built for terminal
  durations.

### Why not C — separate binary `gt-term`

- Three new costs and zero new capability:
  1. Another compose service + container image + Traefik route.
  2. Another scope-config consumer (`mcp-scope.toml`'s `[actors.*]` already
     scopes both gt-mcp and gt-web — splitting again means three files to
     keep in sync, see header of
     [mcp-scope.toml](../../api/deploy/mcp-scope.toml)).
  3. Another listener to monitor and to put behind the JWT auth middleware
     in gt-web (`AuthConfig::Jwt`, [gt-web RBAC chain](frontend-api-surface.md#auditoría-y-observabilidad-de-la-frontier)).
- The only thing a separate bin buys is process isolation. PTY handling on
  the same axum runtime is well-understood; the security blast radius is the
  *tmux session itself*, not the host — process separation does not change
  that.
- Pop-out window (follow-up `hq-fe-term.*`) still loads the same `/api/sessions/:id/term`;
  it does not need its own bin.

### Why A wins

- gt-web is already the human/JWT surface (`bins/gt-web`, scope guards
  from `hq-fe-rbac.3`). Adding `terminal.attach` as a scope means **one
  config diff** in `mcp-scope.toml` and a per-method `ScopeGuard` on the
  upgrade route — same pattern that landed `06a4e990` (split per-method
  scope guards on dual-method routes).
- The Pty substrate already exists in-tree: `gt-login::pty` ships a `Pty`
  port + `PortablePty` (real, via the `portable-pty` crate) + `FakePty`
  (tests). The PTY adapter `hq-fe-term.1` can be the same port, cloned into
  `gt-terminal` (or directly used) — see
  [gt-login/src/pty.rs:38](../../api/crates/domain/orchestration/gt-login/src/pty.rs#L38).
- tmux substrate also exists: `gt-polecat::tmux::Tmux` port with `TmuxCli`
  + `FakeTmux`, including `send_keys` for chord delivery
  ([gt-polecat/src/tmux.rs:54](../../api/crates/domain/lifecycle/gt-polecat/src/tmux.rs#L54)).
- Axum WS upgrade is a single extractor; gt-web already depends on axum
  and broadcasts via `tokio::sync::broadcast` (see
  [gt-web/src/stream.rs](../../api/crates/bins/gt-web/src/stream.rs)), so
  fan-out semantics are familiar.
- xterm.js on the SvelteKit side speaks raw bytes — a WS with binary frames
  is its native transport; no extra protocol layer needed.

## Implementation outline (for `hq-fe-term.1` / `.2`)

1. **Crate `gt-terminal`** (new, under `domain/platform/`): ports
   `TerminalPty` (re-export `gt_login::pty::Pty`?) + `TerminalTmux`
   (re-export `gt_polecat::tmux::Tmux`?). Domain holds the
   pane-attach state machine (start, stream-bytes, send-keys, detach,
   error-detach).
2. **`hq-fe-term.1` — adapter pick**: use `tmux pipe-pane -O '...'` for
   read-side (continuous stream of pane bytes to a fifo / stdout) and
   `tmux send-keys` for write-side. Avoids dragging a fresh PTY for every
   attach — tmux is already the multiplexer. `portable-pty` is the
   fallback if pipe-pane proves too coarse (no scroll-back hydration).
3. **`hq-fe-term.2` — WS route in gt-web**:
   `GET /api/sessions/:id/term` returning `axum::extract::ws::WebSocketUpgrade`.
   - `route_layer` is wrong here (see memory `feedback_axum_method_router_layer`);
     attach the `ScopeGuard` directly to the WS route via the
     split-per-method-router pattern from `06a4e990` if any other method
     ever shares the path.
   - Binary frames in both directions; text frames reserved for control
     (e.g., resize: `{"cols":80,"rows":24}`).
4. **Scope**: add `terminal.attach` to `[roles.operator]` in
   `mcp-scope.toml`; deny by default for `reader`/`sheriff`.
5. **SSE coexistence**: leave the broadcast SSE bus alone. Terminal does
   not piggy-back on `events.jsonl` — pane bytes are not a domain event,
   they are I/O. `quota.rotated` chips for the dock still ride normal SSE.

## Out-of-scope (deferred to follow-ups)

- **Pop-out window** — same WS route, different chrome. Tracked under
  `hq-fe-term.*` follow-up; no transport change.
- **Multi-pane / split layout in xterm** — UI-only; transport unchanged.
- **Recording / replay** — write the stream to disk (rotate by session) when
  audit requires it. Out of scope for transport spike.
- **Cross-host attach** — current target is in-process: gt-web inside
  `gastown-sandbox` shares the tmux server socket with the polecats it
  spawns. Multi-host needs a different design (and a different bead).

## Risks accepted

- **WS through Traefik / TLS termination**: confirmed compose already
  handles the upgrade for `gastown.codecsrayo.com` (cutover `hq-fe-cut.1`
  serves SvelteKit static via fallback; WS lives under `/api/*` and rides
  the same router).
- **Backpressure**: tmux pane writes are blocking-friendly; xterm consumes
  bytes fast. If a slow client stalls, drop frames + reset the pane
  (`tmux pipe-pane -O` is idempotent). Worst case: user sees a flicker, not
  a wedged server.
- **Auth lifetime**: JWT carries scope at upgrade; if it expires mid-attach,
  server closes the WS. Client reconnects with a fresh token — same
  contract as SSE reconnect.

## Acceptance criteria for the spike

- [x] Three options surveyed (A / B / C above) with concrete pointers to
      existing substrate.
- [x] Decision recorded with the substrate the decision rides on (`gt-login::pty`,
      `gt-polecat::tmux`, gt-web routing + scope guards).
- [x] Next-bead actions written (`hq-fe-term.1` adapter, `hq-fe-term.2` route).
- [x] Frontend doc cross-refs reconciled (link from
      [frontend-features.md §11](frontend-features.md#11--dock-terminal-xterm)
      and [frontend-api-surface.md](frontend-api-surface.md#terminal--interactive-gaps)
      stop saying "TBD post-spike").

## Cross-refs

- Feature card: [frontend-features.md §11](frontend-features.md#11--dock-terminal-xterm)
- API gap table: [frontend-api-surface.md "Terminal / interactive gaps"](frontend-api-surface.md#terminal--interactive-gaps)
- Epic plan: [frontend-migration-sveltekit.md row hq-fe-term](frontend-migration-sveltekit.md)
- Pty substrate: [apps/api/crates/domain/orchestration/gt-login/src/pty.rs](../../api/crates/domain/orchestration/gt-login/src/pty.rs)
- Tmux substrate: [apps/api/crates/domain/lifecycle/gt-polecat/src/tmux.rs](../../api/crates/domain/lifecycle/gt-polecat/src/tmux.rs)
- gt-mcp HTTP transport (not the one we picked, kept for reference): [apps/api/crates/bins/gt-mcp/src/http.rs](../../api/crates/bins/gt-mcp/src/http.rs)
