# 10 — Go ↔ Rust feature-parity audit

> Scope of this doc: enumerate every command / daemon behavior the live **Go** `gt`
> exposes and map it to the **Rust** surface (`gt-web` HTTP route, `gt-mcp` MCP tool,
> or `RealEffects` edge in `bins/gt`). Mark each as `COVERED` / `PARTIAL` / `MISSING`
> and flag what is on the **critical path** for the cutover (Paso 8 / `hq-8iur`).
> No code changes — research only. Sources: `internal/cmd/*.go`,
> `internal/session/*.go`, `apps/api/crates/bins/{gt,gt-web,gt-mcp}/`,
> `apps/api/crates/domain/*/`.

Snapshot date: 2026-05-28. Refresh when either side moves materially.

---

## Rust surface today (the universe of things to map *into*)

**`gt-web`** (Axum backend, `bins/gt-web/src/lib.rs`) — read-side + 1 write:

| Method | Route | Purpose |
|---|---|---|
| GET | `/api/sessions` | snapshot active sessions (`SessionQueries`) |
| GET | `/api/beads?status=…` | snapshot beads (`BeadRepository`) |
| GET | `/api/stream` | SSE: `EventRecord` broadcast from root |
| GET | `/metrics` | Prometheus exposition (`gt-telemetry`) |
| POST | `/api/nudge` | publish `AgentEvent::Heartbeat` to bus |

**`gt-mcp`** (`bins/gt-mcp/src/service.rs`) — tools by domain (each is `.execute` + `.validate`):

| Domain | Tools |
|---|---|
| agent | `add`, `remove`, `transition` |
| scheduling | `enqueue`, `mark_dispatched` |
| merge | `start`, `submit`, `complete`, `fail` |
| patrol | `register`, `heartbeat`, `tick`, `close` |
| quota | `sample`, `probe`, `rotate` |
| orch | `launch_convoy`, `complete_member`, `fail_member` |

**`RealEffects`** (`bins/gt/src/effects_real.rs`) — production-only edges out of the core:

| Edge | Behavior |
|---|---|
| `sling(convoy, member)` | spawns `gt sling <member>` subprocess (host Go binary) |
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
| `gt sling` ★ | Unified work-dispatch (bead/formula → agent) | `RealEffects::sling` shells out to Go `gt sling` (so Rust still depends on Go binary); MCP `scheduling.enqueue` covers the enqueue facet only | **P★** |
| `gt unsling` ★ | Reverse a sling (release a claim) | MCP `scheduling.mark_dispatched` cannot undo; no Rust path | **M★** |
| `gt hook` | Show or attach work on a hook | none (hook protocol lives in Go) | **M** |
| `gt claim` | (subcommand under several) atomic claim of an issue | partial: `BeadRepository::cas_release` exists in Dolt adapter, but no public claim/release route | **P** |
| `gt release` | Release stuck in_progress issues → pending | none | **M** |
| `gt redispatch` | Re-dispatch a bead | none | **M** |
| `gt scheduler` | Manage dispatch scheduler (subcmds: clear, etc.) | partial: `gt-scheduling` domain owns the actor; no admin surface | **P★** |

### 2 · Bead lifecycle (issue tracker, parallel to `bd`)

| Go cmd | Purpose | Rust | Status |
|---|---|---|---|
| `gt bead` | Bead management (move between repos, etc.) | none | **M** |
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
| `gt agents` ★ | List all Gas Town agent sessions | `GET /api/sessions` (read-side ports cover it once `hq-8iur.7` lands role/crew); today the snapshot has `{id, rig, state}` but no role/crew distinction | **P★** |
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
| `gt daemon start` / `stop` | (the gastown background daemon — convoy watcher, bd sync, etc.) | none. Rust has no `gt daemon` analogue; the Go daemon owns convoy-event observation, polecat reconciliation, bd→Dolt sync. | **M★** |
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
| `gt health` / `gt health-check` | Liveness | partial: `/metrics` covers most of it for Prometheus | **P** |
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
| `gt quota status` | Show account quota status | none (read-side route missing; `gt-quota` domain has the state in actor) | **M★** |
| `gt quota rotate` ★ | Rotate account (manual) | MCP `quota.rotate.execute`; `RealEffects::rotate` is the production edge | **C★** |
| `gt quota probe` | Probe account headers | MCP `quota.probe.execute`; gap: real `anthropic-ratelimit-*` parser still PLANEADO per `04-persistence.md`/`features/token-tracking-prediction.md` | **P★** |
| `gt account` | Manage Claude Code accounts | none. Account CRUD (add, list, set-default, retire) is Go-only. | **M★** |
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
| `gt rig` | Manage rigs | none | **M★** |
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
| `sling(convoy, member)` | `tokio::process::Command` spawning host `gt sling` | **Bootstrap dependency — Rust currently *needs* the Go `gt` binary on PATH to actually dispatch work.** Critical-path to remove for true cutover. |
| `rotate(account)` | hits `QuotaCommand::Rotate` chain via `QuotaSlot` | covers the predictive + manual rotation path; account CRUD is still Go-only |

Anything not in this table (witness escalation actions, mayor delegations, mail
delivery, audit-log writes triggered from outside the bus) currently runs in
**Go** even when called via Rust — the Rust core publishes the event, the Go
side-effect machinery reacts in production.

---

## Critical-path summary for the flip (hq-8iur)

Items marked **★** above. Coarse-grained dependency order:

1. **`gt prime` parity** (★, §3). Without this, agents cannot reorient after
   compaction → Rust can't be the only point of truth for "who am I in this
   cwd".
2. **`gt agents` + role/crew on Session** (★, §3, depends on `hq-8iur.7`).
   Read-side of agent inventory.
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
8. **`gt quota status` + `gt account` CRUD** (★, §6). Quota actor state is
   read-only via reflection today; needed for operator UI.
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

> Bead `hq-8iur.7` asks for `Dog(DogKind: Witness|Refinery|Deacon|Sheriff|...)`.
> **The Go source treats `Dog` differently from that assumption — it is its
> own `Role`, not a parent category over the watchdogs.** Confirm with agent A
> before closing the Rust `SessionRole` enum.

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

Identity is reconstructed from the **tmux session name** (`ParseSessionName`
in `internal/session/identity.go`). There is no per-role env or per-rig path —
the session name is the discriminator. Formats (from `internal/session/names.go`):

| Role | tmux session name pattern | Scope | Helper |
|---|---|---|---|
| `mayor` | `hq-mayor` | town (1/machine) | `MayorSessionName()` |
| `deacon` | `hq-deacon` | town (1/machine) | `DeaconSessionName()` |
| `overseer` | `hq-overseer` | town (the human; no agent session in practice) | `OverseerSessionName()` |
| `boot` *(actually parsed as `RoleDeacon` Name="boot")* | `hq-boot` | town (deacon watchdog) | `BootSessionName()` |
| `dog` | `hq-dog-<name>` | town (named) | `DogSessionName(name)` |
| `witness` | `<rigPrefix>-witness` | rig (1/rig) | `WitnessSessionName(prefix)` |
| `refinery` | `<rigPrefix>-refinery` | rig (1/rig) | `RefinerySessionName(prefix)` |
| `crew` | `<rigPrefix>-crew-<name>` | rig (N/rig) | `CrewSessionName(prefix, name)` |
| `polecat` | `<rigPrefix>-<name>` | rig (N/rig, ephemeral) | `PolecatSessionName(prefix, name)` |

`<rigPrefix>` = beads prefix from `PrefixRegistry` (e.g. `gt` for gastown, `bd`
for beads, `hq` when configured as a rig prefix). When `hq-` is the prefix
suffix is matched first against known town-role names (`mayor`, `deacon`,
`overseer`, `boot`); unknown suffixes fall through to rig-level parsing so
`hq-<polecat>` resolves correctly when `hq` is a registered rig prefix
(see `ParseSessionNameWithRegistry`).

### Implication for the Rust `SessionRole` enum

The bead description in `hq-8iur.7` sketches
`SessionRole::{Mayor, Dog(DogKind: Witness|Refinery|Deacon|Sheriff|…), Polecat}`.
**That parent/child shape does not match the Go source.** Concretely:

- `Witness` and `Refinery` are **rig-level** roles, sibling of `Polecat` and
  `Crew` — they are not children of `Dog`.
- `Deacon` is **town-level**, sibling of `Mayor`. Not a `Dog`.
- `Dog` is its own role (`hq-dog-<name>`) — a town-level named worker, distinct
  from any of the above. There is no `Sheriff` role today (the term appears
  only in `info.go` describing a deacon plugin, not as a constant).

Recommended canonical Rust enum (mirrors the Go source):

```rust
pub enum SessionRole {
    Mayor,                    // town
    Deacon,                   // town
    Overseer,                 // town (human — usually absent)
    Boot,                     // town (deacon watchdog; Go parses as Deacon+Name="boot")
    Dog { name: String },     // town, named (hq-dog-<name>)
    Witness { rig: String },  // rig
    Refinery { rig: String }, // rig
    Crew { rig: String, name: String },     // rig
    Polecat { rig: String, name: String },  // rig (default ephemeral worker)
}
```

`crew` and `polecat` carry `name`; `crew` is **not** a `Session` per the
description in `hq-8iur.7` — it is an attribute on the polecat session. That
shape works: `Polecat` is the session, `crew: Option<String>` is the attribute
of which claude agent is running inside it right now. The `Crew` variant above
exists only to identify a *human crew worker* tmux session
(`<prefix>-crew-<name>`), which Go does treat as a distinct session.

If agent A wants `Dog` as a parent for the watchdogs, that's an explicit
divergence from Go — flag it before merging the enum so the projector
(write-side, `hq-8iur.2`) doesn't misclassify Go-spawned sessions on cutover.

---

## Refresh checklist

When updating this doc:

1. `grep -rEhn "rootCmd\.AddCommand\(" internal/cmd/*.go | grep -v _test` —
   verify the top-level count and add new rows.
2. `grep -rEh 'name\s*=\s*"' apps/api/crates/bins/gt-mcp/src/service.rs | grep '#\[tool'` —
   refresh the MCP tool list.
3. `grep -rEhn '\.route\(' apps/api/crates/bins/gt-web/src/*.rs` — refresh the
   HTTP route table.
4. `grep -rEn 'fn (sling|rotate|<new>)' apps/api/crates/bins/gt/src/effects_real.rs` —
   refresh the RealEffects edges.
5. `grep -rEn 'Role[A-Z][a-zA-Z]* Role =' internal/session/identity.go` —
   refresh the role taxonomy.

Anything that drifts between this doc and the source is a parity regression —
re-run `hq-8iur.3` or open a sub-bead before the cutover step that depends on
the assumption.
