# 10 — Go ↔ Rust feature-parity audit

> Scope of this doc: enumerate every command / daemon behavior the live **Go** `gt`
> exposes and map it to the **Rust** surface (`gt-web` HTTP route, `gt-mcp` MCP tool,
> or `RealEffects` edge in `bins/gt`). Mark each as `COVERED` / `PARTIAL` / `MISSING`
> and flag what is on the **critical path** for the cutover (Paso 8 / `hq-8iur`).
> No code changes — research only. Sources: `internal/cmd/*.go`,
> `internal/session/*.go`, `apps/api/crates/bins/{gt,gt-web,gt-mcp}/`,
> `apps/api/crates/domain/*/`.

Snapshot date: 2026-05-28. Base commit `3f59b37f`. **Refreshed** after the first
audit (`28103fcf`) to absorb three material moves on the Rust side:
`211c5fff` (boot hydration + sessions write-path + **`SessionRole`/crew shipped**,
hq-8iur.1/.7/.2), `525f1d5d` (MCP `agent.add` now emits an event + new
`quota.register` + `scheduling.create_bead`, hq-mc72.10), `64cba819`
(`rig.create`, hq-mc72.11), `135367bb` (`/health` + `/readyz`, hq-8iur.5).
Refresh again when either side moves materially.

---

## Rust surface today (the universe of things to map *into*)

**`gt-web`** (Axum backend, `bins/gt-web/src/lib.rs`) — read-side + 1 write + probes:

| Method | Route | Purpose |
|---|---|---|
| GET | `/api/sessions` | snapshot active sessions (`SessionQueries`); rows now carry `role` + `crew` and accept a `?role=` filter (hq-8iur.7) |
| GET | `/api/beads?status=…` | snapshot beads (`BeadRepository`) |
| GET | `/api/stream` | SSE: `EventRecord` broadcast from root |
| GET | `/metrics` | Prometheus exposition (`gt-telemetry`); outside the IAM middleware |
| GET | `/health` | liveness probe (always 200 once the process is up); outside IAM (hq-8iur.5) |
| GET | `/readyz` | readiness probe — 200 only after boot hydration done **and** Dolt/PG reachable; outside IAM (hq-8iur.5) |
| POST | `/api/nudge` | publish `AgentEvent::Heartbeat` to bus |

**`gt-mcp`** (`bins/gt-mcp/src/service.rs`) — tools by domain (each is `.execute` + `.validate`):

| Domain | Tools |
|---|---|
| agent | `add`, `remove`, `transition` |
| scheduling | `enqueue`, `mark_dispatched`, `create_bead` *(new — hq-mc72.10)* |
| merge | `start`, `submit`, `complete`, `fail` |
| patrol | `register`, `heartbeat`, `tick`, `close` |
| quota | `sample`, `probe`, `rotate`, `register` *(new — hq-mc72.10)* |
| orch | `launch_convoy`, `complete_member`, `fail_member` |

Behaviour notes on the new tools (all bypass their domain `Command` path — they
write directly and emit **no domain event**, only a frontier audit record):

- `scheduling.create_bead` — creates a `pending` bead in the repo so the
  dispatcher can claim it. Covers the *create* facet of Go `gt bead` / `bd create`.
- `quota.register` — registers (or replaces) a quota account with a live window
  so `sample`/`probe`/`rotate` can act on it. Covers the *add* facet of Go
  `gt account`.
- `rig.create` — **RETIRED (Paso 10 D2, hq-mc72.12.1).** It used to shell out to
  `gt rig add` via `RigCreator` (the last Go-binary exec inside gt-mcp). Removed:
  gt-mcp has no rig domain, and rig creation is filesystem bootstrap (bare clone +
  dir scaffold + bead seeding + tmux pattern update) — classified B1 (CLI/deploy),
  not orchestrator state. Rig creation relocates to `gt-cli`/`deploy/` under B1.
- `agent.add.execute` — now publishes `AgentEvent::Spawned` on the edge relay so
  the add reaches the log / SSE / sessions projector (hq-mc72.10). **But it
  hardcodes `role: Polecat, crew: None`** — MCP can only spawn polecats; mayor /
  dog sessions still come only from the supervisor/sling edge.

**`gt-mcp` read-side Resources** (`resource_list` / `read_resource_json`) — JSON snapshots, one per domain:

| URI | Snapshot |
|---|---|
| `gt://agent/sessions` | active sessions incl. `role` + `crew` (read-side of `gt agents`) |
| `gt://scheduling/queue` | `{queued, in_flight}` |
| `gt://patrol/leases` | `{live_leases, expired_emitted}` |
| `gt://merge/slots` | merge slots `[{bead, branch, state}]` |
| `gt://orch/convoys` | convoys + per-member progress |
| `gt://quota/accounts` | `{accounts, predictions_emitted}` (partial read-side of `gt quota status`) |

**`RealEffects`** (`bins/gt/src/effects_real.rs`) — production-only edges out of the core:

| Edge | Behavior |
|---|---|
| `sling(convoy, member)` | spawns a Rust-managed `tmux` polecat via `gt_polecat::PolecatLifecycle` — **no Go binary** (hq-mc72.12 D1, was `gt sling <member>` subprocess). `member` pinned as `GT_HOOK_BEAD` |
| `rotate(account)` | flips quota active-account pointer via `gt-quota::Rotate` chain |

Everything else the Rust API exposes is internal (bus, dolt/pg adapters, audit, replay).

---

## Parity table — top-level Go gt commands (111 cmds)

`rootCmd.AddCommand` registrations in `internal/cmd/*.go`, grouped by purpose.
Status legend: **C** = covered by Rust surface · **P** = partial (some sub or
proxy exists) · **M** = missing. `★` = critical path for flip (Rust must own
this before traffic moves).

### 1 · Work dispatch (the unified "send work to an agent" surface)

| Go cmd | Purpose (from Short:) | Rust equivalent | Status |
|---|---|---|---|
| `gt sling` ★ | Unified work-dispatch (bead/formula → agent) | `RealEffects::sling` now spawns a Rust-managed `tmux` polecat via `gt_polecat::PolecatLifecycle` — **no Go dependency** (hq-mc72.12 D1). MCP `scheduling.enqueue` covers the enqueue facet; the `gt sling` CLI verb + namepool/formula resolution remain (B-track) | **P★** |
| `gt unsling` ★ | Reverse a sling (release a claim) | MCP `scheduling.mark_dispatched` cannot undo; no Rust path | **M★** |
| `gt hook` | Show or attach work on a hook | none (hook protocol lives in Go) | **M** |
| `gt claim` | (subcommand under several) atomic claim of an issue | partial: `BeadRepository::cas_release` exists in Dolt adapter, but no public claim/release route | **P** |
| `gt release` | Release stuck in_progress issues → pending | none | **M** |
| `gt redispatch` | Re-dispatch a bead | none | **M** |
| `gt scheduler` | Manage dispatch scheduler (subcmds: clear, etc.) | partial: `gt-scheduling` domain owns the actor; no admin surface | **P★** |

### 2 · Bead lifecycle (issue tracker, parallel to `bd`)

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt bead` | Bead management (create, move between repos, etc.) | partial: MCP `scheduling.create_bead` covers the *create* facet (pending bead the dispatcher can claim); move/repo ops have no Rust path | **P** |
| `gt close` | Close issues | none in API (`bd close` is the actual interface; gt-mcp does not wrap it) | **M** |
| `gt assign` | Assign issue | none | **M** |
| `gt ready` | Show ready beads | partial: `GET /api/beads?status=ready` exists | **P** |
| `gt blocked` | Show blocked beads | partial: `GET /api/beads?status=blocked` exists | **P** |
| `gt done` ★ | Signal work ready for merge queue (writes MERGE_READY channel event) | `gt-merge` domain owns `MergeEvent::Ready`, `MergeBoard`. MCP `merge.start` accepts a Ready slot, but the **producer side** (refinery polls `gt-channel` for the `.event` file) is wired in `bins/gt` via the channel watcher, NOT exposed as a CLI verb a polecat can call. The Go `gt done` writes the channel event; in Rust the polecat would have to call MCP `merge.start` directly. | **P★** |
| `gt cat` | Display bead content | none | **M** |
| `gt peek` | View recent output from polecat/crew session | none (would need tmux capture-pane wrapper) | **M** |
| `gt show` | Show issue details | partial: covered by `bd show` (separate binary) | **C** (via bd) |
| `gt edit` / `gt repair` / `gt forget` / `gt prune-branches` | Various bead-state surgical ops | none | **M** |
| `gt issue` | Generic issue subcommands | none | **M** |

### 3 · Identity / orientation (what am I, what's my context)

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt prime` ★ | Output role context for current directory (canonical post-compaction reorient) | none. The Rust API has no concept of "what role am I in this cwd"; this lives entirely in the Go CLI + `GT_ROLE` env | **M★** |
| `gt whoami` | Current identity for mail | none | **M** |
| `gt agents` ★ | List all Gas Town agent sessions | `GET /api/sessions` + MCP `gt://agent/sessions`; **hq-8iur.7 landed** (`211c5fff`) so rows now carry `role` + `crew` with a `?role=` filter. Read-side is COVERED; the remaining gap is the **write-side** (only `gt sling` Go / the supervisor edge populate non-polecat rows — see §4 daemons) | **C** (read) / **P★** (write) |
| `gt session` | Manage polecat sessions (subcmds) | none | **M** |
| `gt polecat` | Manage polecats (persistent identity, ephemeral sessions) | none | **M** |
| `gt agent` | Per-agent admin | partial: MCP `agent.add/remove/transition` are the core mutations | **P** |
| `gt agent-log` | Tail agent log | partial: `/api/stream` SSE delivers the EventRecord live stream | **P** |
| `gt costs` | Show costs for running Claude sessions | none. Plumbed into `gt-quota` events (`TokensSampled`), but no rollup route or MCP tool | **M★** |
| `gt audit` | Query work history by actor | none in HTTP (`gt-audit` log exists; no `/api/audit` route) | **M★** |

### 4 · Daemons / supervisors (the long-running services)

Spawn signal = how the daemon's tmux session is identified (see §Roles below).

| Go cmd / daemon | Tmux session name | Rust | Status |
|---|---|---|---|
| `gt daemon start` / `stop` | (the gastown background daemon — convoy watcher, bd sync, etc.) | none as a `gt daemon` analogue, but **boot hydration landed** (`211c5fff`, hq-8iur.1): the Rust process now replays the event log into the actors at boot before serving, and `135367bb` adds a systemd unit + graceful drain (hq-8iur.5). The Go daemon still owns convoy-event observation, polecat reconciliation, bd→Dolt sync. | **P★** |
| `gt mayor` (start) ★ | `hq-mayor` | `gt-orchestration::mayor` is implemented as a productor (event borde) inside the actor; no `mayor start` CLI / route — the Mayor process IS the Go binary today | **P★** |
| `gt deacon` (start) ★ | `hq-deacon` | `gt-orchestration::deacon` exists as productor; same gap — the Deacon process today is the Go binary | **P★** |
| `gt boot status` | `hq-boot` | none (deacon watchdog is Go-only) | **M** |
| `gt witness start <rig>` ★ | `<prefix>-witness` | `gt-patrol::witness` is the producer of `PolecatStale` / lease-expired; the actor is wired in `bins/gt` root. **The witness daemon process itself is still the Go binary**; the Rust API has the domain logic but no `witness start` CLI parity. | **P★** |
| `gt refinery start <rig>` ★ | `<prefix>-refinery` | `gt-merge::refinery` is implemented as a producer that awaits `MERGE_READY` via `gt-channel`. Same situation: domain logic in Rust, **but the refinery process spawned in a tmux pane is the Go binary**. | **P★** |
| `gt dolt` | (Dolt SQL server admin: start/stop/sql) | none. `gt-store-dolt` connects as a client (MySQL wire 3307); managing the server is a deploy concern. | **M** (intentional) |
| `gt scheduler` | (dispatch scheduler admin) | partial: dispatcher actor in `gt-scheduling`; no admin surface | **P** |
| `gt nudge-poller` | (background poller for nudge mail) | none | **M** |
| `gt reaper` ★ | (wisp + issue GC) | none. Today the `/reaper` skill shells out; no Rust route. | **M★** |
| `gt patrol` | (patrol digest cmd, NOT the witness — this is the daily rollup) | none | **M** |
| `gt mol` / `gt mol-patrol` | (molecule patrol — cross-rig health) | none | **M** |
| `gt heartbeat` | Update agent heartbeat state | partial: `POST /api/nudge` emits `AgentEvent::Heartbeat`; full lifecycle (register / close) covered by MCP `patrol.heartbeat` | **C** |
| `gt vitals` | Health vitals (panel) | none | **M** |
| `gt doctor` ★ | Run health checks on the workspace | none. `gt doctor` does dolt-vs-jsonl reconciliation, polecat orphan scan, env audit — none of that is in Rust. | **M★** |
| `gt health` / `gt health-check` | Liveness | `/health` (liveness) + `/readyz` (readiness, gated on hydration + Dolt/PG reachable) landed in `135367bb` (hq-8iur.5); `/metrics` adds Prometheus | **C** |
| `gt cleanup` | Clean up orphaned Claude processes | none (process-side, Go is the right place) | **M** |
| `gt warrant file` | File a death warrant for a stuck agent | none. Patrol domain has `LeaseExpired` events; the human-initiated warrant path is Go-only. | **M★** |
| `gt estop` | Emergency stop | none | **M★** |
| `gt shutdown` | Shut down town | none | **M** |
| `gt orphans` | Find orphan tmux/processes | none | **M** |
| `gt stale` | Check if gt binary is stale | none (it's a self-check of the binary) | **M** (intentional) |

### 5 · Convoy / orchestration (Paso 6.d in Rust)

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt convoy create` ★ | Create a new convoy | MCP `orch.launch_convoy.execute` | **C★** |
| `gt convoy launch` ★ | Launch a staged convoy | MCP `orch.launch_convoy.execute` (same surface, staging is implicit) | **C★** |
| `gt convoy stage` | Stage members before launch | none in API; `ConvoyBoard` accepts them in `launch_convoy` payload | **P** |
| `gt convoy watch` | Watch a convoy progress | partial: `/api/stream` SSE carries `orch.*` events | **P** |
| `gt convoy handoff` | Handoff from one member to next | covered by `OrchEvent::MemberCompleted → MemberDispatched` reaction in root | **C★** |
| `gt convoy <misc>` (40+ subcmds incl. resolve-beadsdir, stranded, transitions, property tests, etc.) | various edge ops | mostly none | **P** |

### 6 · Quota / account rotation (Paso 6.c + 7.b/c)

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt quota status` | Show account quota status | partial: MCP resource `gt://quota/accounts` returns `{accounts, predictions_emitted}` (count + prediction tally), but no per-account window/usage breakdown route yet | **P★** |
| `gt quota rotate` ★ | Rotate account (manual) | MCP `quota.rotate.execute`; `RealEffects::rotate` is the production edge | **C★** |
| `gt quota probe` | Probe account headers | MCP `quota.probe.execute`; gap: real `anthropic-ratelimit-*` parser still PLANEADO per `04-persistence.md`/`features/token-tracking-prediction.md` | **P★** |
| `gt account` | Manage Claude Code accounts | partial: MCP `quota.register` covers the *add/replace* facet (register an account + live window); list / set-default / retire are still Go-only | **P★** |
| `gt costs` | Show costs | none (see §3) | **M★** |
| `gt cost-tier` | Show / set cost tier | none | **M** |
| `gt namepool` | Polecat name pool | none | **M** |

### 7 · Messaging / coordination (mail, nudge, channels)

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt nudge` ★ | Send a synchronous message to any worker | `POST /api/nudge` (heartbeat-shaped only — does NOT cover arbitrary mail / payload) | **P★** |
| `gt broadcast` | Broadcast nudge to all workers | none | **M** |
| `gt mail` | Agent messaging system (inbox / send / reply) | none | **M** |
| `gt signal` | Claude Code hook signal handlers | none (this is the receiving side of `claude` hook stdin protocol) | **M** |
| `gt callbacks` | Handle agent callbacks | none | **M** |
| `gt thanks` | Send a thank-you signal | none | **M** |
| `gt await-event` / `gt await-signal` | Block until a channel event arrives | partial: `gt-channel` crate implements the channel watcher; no CLI / route to await externally | **P★** |
| `gt emit-event` | Emit a channel event | partial: `gt-channel::emit` is library-only; no CLI / route | **P★** |
| `gt feed` | Show real-time activity feed | `/api/stream` SSE (machine-readable); no TUI equivalent | **C** |
| `gt activity` | Activity rollup | none | **M** |

### 8 · Workspace / config / install

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt install` | Create a new HQ workspace | none (CLI bootstrap is a Go responsibility) | **M** (intentional) |
| `gt init` | Initialize cwd as a rig | none | **M** (intentional) |
| `gt town` | Town-level subcmds | none | **M** |
| `gt rig` | Manage rigs | MCP `rig.create` **RETIRED** (Paso 10 D2, hq-mc72.12.1 — removed the Go-exec). Rig creation is filesystem bootstrap (B1): relocates to `gt-cli`/`deploy/`, not orchestrator state. list / remove / park have no Rust path | **M** (B1) |
| `gt worktree` | Create worktree in another rig | none | **M** |
| `gt config` | Manage configuration | none | **M** (intentional — config is filesystem) |
| `gt hooks` / `gt hook` | Install / manage hooks | none (claude-side hooks live in JSON) | **M** |
| `gt upgrade` | Post-install migration | none (binary-side) | **M** (intentional) |
| `gt uninstall` | Uninstall hooks | none | **M** (intentional) |
| `gt version` | Print version | none in API (would be `/api/version`) | **M** |
| `gt git-init` | Init git in current rig | none | **M** (intentional) |

### 9 · Crew (human workers + cycle/dock/peek)

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt crew` | Manage crew workers | partial: covered structurally by `SessionRole::Crew` once `hq-8iur.7` lands. Today: invisible. | **P★** |
| `gt cycle` | Cycle between sessions in a group | none (tmux operation) | **M** |
| `gt mail` (crew subcommands) | crew-targeted mail | none | **M** |
| `gt escalate` | Escalate to deacon/mayor | none. Patrol/orch produce escalation events internally, but the human-initiated `gt escalate` has no route. | **M★** |
| `gt seance` | Crew → polecat connection ritual | none | **M** |

### 10 · Self / misc / utilities

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt status` ★ | Show overall town status | partial: `/api/sessions` + `/api/beads` cover ~30% of the info | **P★** |
| `gt status-line` | Statusline for tmux | none | **M** |
| `gt theme` | UI themes | none | **M** (intentional) |
| `gt log` | View town activity log | partial: SSE covers live; no historical query route | **P** |
| `gt metrics` | Show command usage statistics | partial: `/metrics` is Prometheus exposition (different shape) | **P** |
| `gt dashboard` | Start convoy tracking dashboard | partial: `gt-web` is the API substrate. The browser UI moved out of scope ([07-frontend.md](07-frontend.md), SvelteKit pista separada). | **C** (via gt-web) |
| `gt audit` | Query work history | (see §3) | **M★** |
| `gt audit` subcmds | various | none | **M** |
| `gt dog` ★ | Manage `Dog` role agents (see §Roles below) | none. `Dog` is a session role but lacks a dedicated control surface in Rust. | **M★** |
| `gt memories` | List / search stored memories | none | **M** |
| `gt info` | Project info | none | **M** |
| `gt directive` | Inject directive into agent | none | **M** |
| `gt plugin` | Plugin management | partial: `gt-plugin` crate is the trait container, no admin surface | **P** |
| `gt formula` | Formula management (sling formulas) | none | **M** |
| `gt krc` | (internal cmd) | none | **M** |
| `gt synthesis` | (internal cmd) | none | **M** |
| `gt tap` | (internal cmd) | none | **M** |
| `gt trail` | (internal cmd) | none | **M** |
| `gt witness` (top-level cmd, distinct from rig witness daemon) | Manage witness daemon — see §4 | partial: domain logic only | **P★** |
| `gt up` / `gt down` / `gt resume` / `gt thaw` | Power / freeze controls | none | **M** |
| `gt notify` | Send a notify | none | **M** |
| `gt pruneBranches` | GC remote branches | none | **M** |
| `gt release` / `gt rememberCmd` / `gt mountain` / `gt mq` / `gt peek` / `gt cat` | misc | none | **M** |
| `gt commit` | Commit (the gastown-aware commit) | none | **M** |
| `gt changelog` | Changelog | none | **M** |
| `gt compact` | Compaction (context window) | none | **M** |
| `gt checkpoint` | Checkpoint state | none | **M** |
| `gt cleanup` (rerun) | (see §4) | none | **M** |
| `gt orphans` (rerun) | (see §4) | none | **M** |
| `gt show` (rerun) | (see §2) | none | **C** (via bd) |
| `gt dnd` | Do-not-disturb mode | none | **M** |
| `gt enable` / `gt disable` | Toggle features | none | **M** |
| `gt maintain` | Maintenance tasks | none | **M** |

---

## RealEffects coverage today

The composition root (`bins/gt`) drives all production side-effects through one
trait (`Effects`). Implementations:

| Method | Production (`RealEffects`) | Notes |
|---|---|---|
| `sling(convoy, member)` | `gt_polecat::PolecatLifecycle` → detached `tmux` session running the agent | **Self-hosted (hq-mc72.12 D1)** — the Go-binary dependency is removed; the orchestrator spawns its own polecats. Spawn template (`GT_RIG`/`GT_RIG_PATH`/`GT_POLECAT_CMD`/…) comes from env. |
| `rotate(account)` | hits `QuotaCommand::Rotate` chain via `QuotaSlot` | covers the predictive + manual rotation path; account CRUD is still Go-only |

Anything not in this table (witness escalation actions, mayor delegations, mail
delivery, audit-log writes triggered from outside the bus) currently runs in
**Go** even when called via Rust — the Rust core publishes the event, the Go
side-effect machinery reacts in production.

---

## Critical-path summary for the flip (hq-8iur)

Items marked **★** above. Coarse-grained dependency order:

0. **DONE since the first audit** — keep off the open list: `hq-8iur.7`
   (SessionRole + crew + `?role=` filter), `hq-8iur.1` (boot hydration), and
   `hq-8iur.5` (`/health` + `/readyz` + graceful shutdown + systemd) have all
   landed (`211c5fff` / `135367bb`). The `gt agents` **read-side** and the
   liveness surface are now covered.
1. **`gt prime` parity** (★, §3). Without this, agents cannot reorient after
   compaction → Rust can't be the only point of truth for "who am I in this
   cwd".
2. **SessionRole projector fidelity** (★, §Roles D1/D3/D4/D5, depends on
   `hq-8iur.2`). The schema shipped, but the shipped `Dog(DogKind)` shape
   mislabels boot/crew/named-dog sessions vs Go. The *write-side* of `gt agents`
   (populating non-polecat rows) is not safe until these are handled.
3. **`gt sling` self-hosted** (★, §1). Today `RealEffects::sling` shells out to
   the Go binary. Cutover requires either (a) the Rust composition root
   spawning a Rust-managed tmux session, or (b) the Go `gt sling` becoming a
   thin shim that delegates back to MCP `scheduling.enqueue` + a Rust-owned
   spawner. Bead `hq-8iur.6` (RealEffects::sling self-host) tracks this.
4. **Witness + Refinery daemon parity** (★, §4). The domain logic is in Rust,
   but the long-running process invoked by tmux is still the Go binary.
   Producer-side wiring of `gt witness start` / `gt refinery start` as Rust
   binaries (or a single multiplexed daemon) is the actual flip.
5. **Mayor + Deacon parity** (★, §4). Same gap shape as witness/refinery.
6. **`gt doctor`** (★, §4). The reconciliation surface (dolt vs jsonl, orphan
   detection) — needed to verify a Rust-only town is healthy.
7. **`gt audit` + `gt costs`** (★, §3 + §6). Required by ops/finance review;
   the data exists (Postgres `JSONB` audit + `token_usage` projection), only
   the HTTP routes are missing.
8. **`gt quota status` + `gt account` CRUD** (★, §6). Partially advanced:
   `gt://quota/accounts` gives a count + prediction tally and MCP `quota.register`
   covers account *add/replace*. Still missing: per-account window/usage
   breakdown and list / set-default / retire.
9. **`gt nudge` arbitrary payload** (★, §7). Current `/api/nudge` is
   heartbeat-shaped only.
10. **`gt await-event` / `gt emit-event`** (★, §7). Cross-process channel
    events are the substrate for `gt done` and rig-to-rig handoff; today
    `gt-channel` is library-only.
11. **`gt warrant file` + `gt estop` + `gt escalate`** (★, §4, §9). Operator
    safety surface. Should not flip without these.
12. **`gt dog` control surface** (★, §10). With the role taxonomy below as
    blocker.

Items NOT critical-path (intentionally Go-only):

- Filesystem bootstrap: `gt install`, `gt init`, `gt git-init`, `gt config`,
  `gt theme`, `gt status-line`, `gt upgrade`, `gt uninstall`, `gt stale`,
  `gt hooks` install.
- tmux/process plumbing: `gt cycle`, `gt peek` (capture-pane), `gt cleanup`,
  `gt orphans`. These belong on the OS side; the Rust API can publish events,
  not execute tmux.
- Dolt server admin: `gt dolt` (deploy concern, not API concern).
- UI themes / TUI: covered by SvelteKit migration ([07-frontend.md](07-frontend.md)).

---

## Roles / Dog kinds (feeds hq-8iur.7)

> **Status: hq-8iur.7 has SHIPPED** (`211c5fff`). Agent A adopted the
> `Dog(DogKind)` parent/child shape — the exact shape the first audit warned was
> a divergence from Go. That is now a *known, merged* divergence to manage, not
> an open design question. This section documents (a) the Go taxonomy, (b) what
> actually shipped in Rust, and (c) the concrete classification bugs the
> write-side projector (`hq-8iur.2`) must handle so a Go-spawned session is not
> mislabelled at cutover. The `DogKind::Sheriff` doc-comment in
> `gt-agent/src/state.rs` explicitly defers the Sheriff/Dog taxonomy decision to
> **this bead** — resolved below.

### Canonical Role constants

Source of truth: `internal/session/identity.go`:
```go
const (
    RoleMayor    Role = "mayor"
    RoleDeacon   Role = "deacon"
    RoleOverseer Role = "overseer"
    RoleWitness  Role = "witness"
    RoleRefinery Role = "refinery"
    RoleCrew     Role = "crew"
    RolePolecat  Role = "polecat"
    RoleDog      Role = "dog"
)
```

The string-constant duplicate lives in `internal/constants/constants.go`
(`RoleMayor = "mayor"`, etc.) and covers six of these (Mayor, Deacon, Witness,
Refinery, Polecat, Crew) — Overseer and Dog are session-only.

### Spawn identification — how each role is recognized at runtime

There are **two complementary signals**, not one. The projector must understand
both because Go-spawned sessions carry the env, while orphan/reconnect paths
fall back to the name.

**(1) tmux session name** — the *persistent* discriminator, parsed by
`ParseSessionNameWithRegistry` (`internal/session/identity.go`) into an
`AgentIdentity{Role, Rig, Name, Prefix}`. Formats (from `internal/session/names.go`):

| Role | tmux session name pattern | Scope | Helper |
|---|---|---|---|
| `mayor` | `hq-mayor` | town (1/machine) | `MayorSessionName()` |
| `deacon` | `hq-deacon` | town (1/machine) | `DeaconSessionName()` |
| `overseer` | `hq-overseer` | town (the human; no agent session in practice) | `OverseerSessionName()` |
| `boot` *(parsed as `RoleDeacon` Name="boot"; `GTRole()` returns `"boot"`)* | `hq-boot` | town (deacon watchdog) | `BootSessionName()` |
| `dog` | `hq-dog-<name>` | town (named) | `DogSessionName(name)` |
| `witness` | `<rigPrefix>-witness` | rig (1/rig) | `WitnessSessionName(prefix)` |
| `refinery` | `<rigPrefix>-refinery` | rig (1/rig) | `RefinerySessionName(prefix)` |
| `crew` | `<rigPrefix>-crew-<name>` | rig (N/rig) | `CrewSessionName(prefix, name)` |
| `polecat` | `<rigPrefix>-<name>` | rig (N/rig, ephemeral) | `PolecatSessionName(prefix, name)` |

`<rigPrefix>` = beads prefix from `PrefixRegistry` (e.g. `gt` for gastown, `bd`
for beads, `hq` when configured as a rig prefix). When `hq-` is the prefix the
suffix is matched first against known town-role names (`mayor`, `deacon`,
`overseer`, `boot`); unknown suffixes fall through to rig-level parsing so
`hq-<polecat>` resolves correctly when `hq` is a registered rig prefix
(see `ParseSessionNameWithRegistry`).

**(2) Environment** — the *runtime* "what am I", injected into the session env at
spawn and read all over the Go CLI (`root.go:216`, `prime.go`, `sling.go`,
`costs.go`, `whoami.go`, `telemetry/subprocess.go`):

| Env var | Value | Source |
|---|---|---|
| `GT_ROLE` | flat role string = `AgentIdentity.GTRole()` (= `Address()`, **except boot → `"boot"`**) | set at spawn; `gt prime` seeds it into the session env if missing and warns on cwd mismatch (`warnRoleMismatch`) |
| `GT_RIG` | rig name | spawn / prime |
| `GT_RIG_PATH` | rig filesystem path | `setuphooks.go`, `polecat/manager.go` |
| `GT_HOOK_BEAD` | bead injected at polecat spawn (deferred-spawn hook) | `polecat/session_manager.go` |

`gt prime` is the reconciler: it reads `GT_ROLE`, compares against the
cwd-inferred identity, and prints a prominent warning if they disagree — so the
two signals are kept consistent, with the env winning at runtime and the name
being the durable fallback.

### What actually shipped in Rust (`211c5fff`, `gt-agent/src/state.rs`)

```rust
pub enum DogKind { Witness /*default*/, Refinery, Deacon, Overseer, Sheriff, Dog }

pub enum SessionRole {
    Mayor,
    Dog(DogKind),   // ← parent over the supervisors
    Polecat,        // ← Default (legacy events + gt sling rows)
}

pub struct Session { id, rig, state, role: SessionRole, crew: Option<String> }
```

- `SessionRole::as_str()` flattens to the Go role strings —
  `mayor | polecat | witness | refinery | deacon | overseer | sheriff | dog` —
  so the wire/DTO value matches Go even though the in-memory shape differs.
- `SessionRole::parse()` is the inverse, accepting those same eight strings.
- `crew: Option<String>` matches the recommendation: crew is the claude agent
  *inside* a polecat, not a session kind. ✅ This part agrees with Go.

### Divergences from Go the projector (`hq-8iur.2`) must handle

These are concrete misclassification bugs, not style nits. Each is on the
**critical path** because the write-side projector turns Go-spawned tmux
sessions / replayed Go events into `Session` rows at cutover.

| # | Divergence | Why it bites | Recommended handling |
|---|---|---|---|
| D1 | `Dog(DogKind)` is a **parent** over witness/refinery/deacon/overseer. In Go these are flat siblings; `is_dog()` in Go is true only for `hq-dog-<name>`. | Rust `SessionRole::is_dog()` returns `true` for witness/refinery/deacon/overseer/sheriff too. Any logic gating on `is_dog()` **over-matches** vs Go. | Audit every `is_dog()` call site; gate on the specific `DogKind` instead, or add `is_supervisor()` vs `is_bare_dog()`. |
| D2 | `DogKind::Sheriff` has **no Go role constant** and **no tmux name pattern** (`<x>-sheriff` does not exist in `names.go`). It is the GitHub-sheriff plugin/agent (Paso 9.B/9.D), not a session kind. | A `role=sheriff` row can never come from parsing a Go session → only from a future Rust-native spawn. Expecting Go to emit it is a bug. | **Decision (this bead):** keep `Sheriff` as a forward-looking, Rust-only kind. Document that the projector never produces it from Go input. Do **not** add it to `names.go` expectations. |
| D3 | No `name` on `Dog`, no `Boot` kind. Go carries `Name` (`hq-dog-<name>`) and parses `hq-boot` as `Deacon Name="boot"` with `GTRole()=="boot"`. | `hq-dog-alice` vs `hq-dog-bob` collapse to one identity; `hq-boot` collapses into `deacon`. Round-trip is lossy. | If named dogs / the boot watchdog must be distinguished, add `DogKind::Dog{name}` (or a `name` field on `Session` for dogs) and a `Boot` kind. Otherwise document the loss explicitly. |
| D4 | `GT_ROLE`/`GTRole()` emits `"boot"` and Go has a `crew` session (`<prefix>-crew-<name>`); `SessionRole::parse()` accepts **neither**. | `parse("boot")` and `parse("crew-…")` → `None` → projector falls back to `Default = Polecat`. The boot watchdog and human crew sessions get **silently mislabelled as polecats**. | Map `"boot" → Dog(Deacon)` (or a `Boot` kind) in `parse()`; decide whether a human-crew tmux session needs its own representation (today it has none — see D5). |
| D5 | Go `RoleCrew` (`<prefix>-crew-<name>`) is a **distinct human-crew session**; Rust has no variant for it (crew is only an attribute). | A Go crew session has no faithful Rust `SessionRole`. | Confirm with ops whether human-crew tmux sessions are in cutover scope. If yes, add a `Crew` variant; if no, document that they are out of scope and the projector skips them. |

D2 is **resolved by this bead** (keep Sheriff, Rust-only). D1/D3/D4/D5 remain
open and should be tracked against `hq-8iur.2` before the flip.

---

## Refresh checklist

When updating this doc:

1. `grep -rEhn "rootCmd\.AddCommand\(" internal/cmd/*.go | grep -v _test` —
   verify the top-level count and add new rows. (111 at this snapshot.)
2. `grep -rEn 'name = "' apps/api/crates/bins/gt-mcp/src/service.rs` —
   refresh the MCP tool list (`<domain>.<verb>.{validate,execute}`).
3. `grep -rEn 'fn resource_list' -A 12 apps/api/crates/bins/gt-mcp/src/service.rs` —
   refresh the read-side `gt://` Resource catalog.
4. `grep -rEhn '\.route\(' apps/api/crates/bins/gt-web/src/*.rs` — refresh the
   HTTP route table (incl. `/health`, `/readyz`).
5. `grep -rEn 'fn (sling|rotate|<new>)' apps/api/crates/bins/gt/src/effects_real.rs` —
   refresh the RealEffects edges.
6. `grep -rEn 'Role[A-Z][a-zA-Z]* Role =' internal/session/identity.go` —
   refresh the Go role taxonomy; cross-check the shipped Rust enum at
   `apps/api/crates/domain/lifecycle/gt-agent/src/state.rs` (`SessionRole` / `DogKind`)
   and re-validate the D1–D5 divergence table.

Anything that drifts between this doc and the source is a parity regression —
re-run `hq-8iur.3` or open a sub-bead before the cutover step that depends on
the assumption.
