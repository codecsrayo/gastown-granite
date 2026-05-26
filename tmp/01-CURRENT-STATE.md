# Current State — What Exists Today

**Read first.** Most plans go wrong because they assume greenfield. Gas Town
already has half of an event-driven system; the migration is an extension,
not a rewrite. This document is the ground truth.

## 1. State storage

### 1.1 Dolt — authoritative state

- Each rig has its own Dolt database (e.g. `hq`, `plane`).
- Two access modes coexist:
  - **Embedded:** `bd` CLI opens `.beads/embeddeddolt/` directly. Each
    `bd` invocation starts fresh, auto-imports from `issues.jsonl`,
    mutates, auto-exports (throttled 1m).
  - **Server:** Dolt TCP server runs on port from `.beads/dolt-server.port`
    (currently 3307). Polecats and some refinery code talk to this.
- The two modes do NOT share state directly. `issues.jsonl` is the bridge.

### 1.2 `issues.jsonl` — bridge artifact

- Lives at `<rig>/.beads/issues.jsonl`.
- Currently treated as authoritative for cold start: every fresh `bd`
  process auto-imports from it.
- Written by bd's auto-export (throttled). Throttle is the source of
  the race PR 1 patched.
- Versioned in git (intentionally — this is how state crosses host
  boundaries).

### 1.3 Other state files in `.beads/`

| File | Purpose | Authoritative? |
|------|---------|---------------|
| `events.jsonl` (bd's, not ours) | bd audit trail when `events-export: true` | currently OFF |
| `routes.jsonl` | Prefix → DB routing rules | yes, hand-edited |
| `metadata.json` | Schema version, init markers | yes |
| `embeddeddolt/` | Embedded Dolt files | yes (when embedded mode) |
| `.locks/agent-*.lock` | Agent claim leases (24+ stale today) | yes |
| `locks/*.flock` | Per-issue flocks | partial — only used for some claim paths |
| `dolt-server.lock`, `.port` | TCP server liveness + port | yes |
| `interactions.jsonl` | Per-actor command history | yes |
| `last-touched` | mtime hint for staleness checks | derived |
| `backups-pre-reinit/` | Snapshots before destructive ops | yes (forensic) |

## 2. Existing event-adjacent infrastructure

Three packages already implement event-like patterns. None of them is the
authoritative state store. Knowing what they DO and DON'T cover is the
critical input to gap analysis.

### 2.1 `internal/events/`

- **Purpose:** Activity feed audit log.
- **Backing store:** `~/gt/.events.jsonl` (one append-only file).
- **Schema:** `{ts, source, type, actor, payload, visibility}` —
  visibility ∈ {audit, feed, both}.
- **Writers:** sling, hook, unhook, handoff commands. No bd mutations
  emit here today.
- **Readers:** `internal/feed/` daemon (curates → `~/gt/.feed.jsonl`);
  `internal/tui/feed/` (TUI display).
- **Concurrency:** uses `gofrs/flock` on the events file.
- **Verdict:** USEFUL FOUNDATION. Schema is close to what we need.
  Visibility tag could be extended with a "state" level. The flock
  pattern is already proven here.

### 2.2 `internal/bus/`

- **Purpose:** In-process typed event bus.
- **Semantics:** Synchronous fan-out. Publish blocks until every
  Subscribe handler runs. Errors joined, not aborted.
- **Used by:** `internal/quota/orchestrator/` ONLY. Other packages
  don't subscribe.
- **Verdict:** WRONG SHAPE for cross-process state mutations. Stays
  useful for in-process subsystem coordination (quota, schedulers).
  Don't try to extend it.

### 2.3 `internal/channelevents/`

- **Purpose:** File-based event emission for named inter-agent channels.
- **Backing store:** `~/gt/events/<channel>/*.event` files (one per event).
- **Writers:** witness handlers, sling helpers, molecule emit.
- **Readers:** `bd await-event` subscribers (refinery watching for
  MERGE_READY, mayor watching for SLOT_OPEN/SLOT_BLOCKED, witness
  watching for POLECAT_DONE).
- **Channels in active use:** `refinery`, `mayor`, `witness`.
- **Verdict:** RPC-ISH. Closer to "messages between known parties" than
  "event log of state changes." Filesystem-per-event has its own
  scalability concerns (inode pressure at scale).

### 2.4 `internal/feed/`

- Curator daemon. Reads `~/gt/.events.jsonl`, filters by visibility,
  deduplicates, aggregates, writes `~/gt/.feed.jsonl`.
- Already implements the tail-and-process pattern. Materializer (which
  we'd need to build) is structurally similar.
- **Verdict:** REUSABLE TEMPLATE for the materializer.

### 2.5 `internal/checkpoint/`

- Session crash recovery for polecats. Not event-related.
- Mentioned here only to disambiguate naming — "checkpoint" in the target
  architecture means something different (materializer high-water mark).

### 2.6 `internal/townlog/`

- Town-level structured logger. Distinct from events.
- Not in scope for migration.

## 3. Current mutation path (`bd` → dolt)

This is the path PR 1 patched. Understanding it precisely is required
before designing the replacement.

```
Caller (any package)
   │
   ▼
internal/beads.Beads.run() / runWithStdin() / runWithRouting()
   │  ├─ acquires jsonl flock (NEW in PR 1, for mutating commands)
   │  ├─ builds env (BEADS_DIR, OTEL)
   │  ├─ exec.Command("bd", args...)
   │  │     │
   │  │     ▼
   │  │  bd binary (external, upstream `steveyegge/beads@v1.0.4`)
   │  │     ├─ auto-import jsonl → embedded dolt (DESTRUCTIVE WIPE)
   │  │     ├─ apply mutation
   │  │     └─ auto-export embedded dolt → jsonl (THROTTLED 1m)
   │  │
   │  └─ if mutating: force-export jsonl (NEW in PR 1, bypasses throttle)
   │  └─ release flock
   ▼
returns to caller
```

Ad-hoc bypass: ~14 packages (mostly `internal/doctor/`, plus
`internal/cli/`, etc.) call `exec.Command("bd", ...)` directly without
going through `internal/beads/`. These bypass PR 1's flock entirely.

A subprocess policy test (`bd_subprocess_policy_test.go`) prohibits this
in `internal/deacon`, `internal/plugin`, `internal/refinery`,
`internal/witness`. The other packages are not yet gated.

## 4. Concurrency model today

- **Within a single bd process:** Dolt's embedded lock + bd's own SQL
  transactions serialize writes.
- **Between bd processes on the same host:** PR 1 flock on
  `<beadsDir>/issues.jsonl.lock` serializes mutating commands per beads
  dir. Read-only commands run unlocked.
- **Between bd processes on different hosts:** No coordination. Git
  versioning of `issues.jsonl` provides last-writer-wins reconciliation.
- **Polecats via TCP server:** Dolt server serializes internally (MVCC).
  But polecats and CLI use different DBs (embedded vs. server) so
  conflicts surface as jsonl import-time merges.
- **Stale agent locks:** 24 of 26 `.locks/agent-*.lock` files are >48h
  old. Nothing reaps them. Indicates the claim-via-file pattern is
  un-self-cleaning.

## 5. Audit/observability today

- **bd-level audit:** `interactions.jsonl` (per-actor command history).
  Not structured for replay.
- **bd event export:** `events-export: false` by default. Could be turned
  on but no consumer exists.
- **gt activity audit:** `~/gt/.events.jsonl` via `internal/events/`.
  Covers sling/hook/handoff — NOT bd mutations.
- **telemetry:** `internal/telemetry/RecordBDCall` records every bd
  subprocess call with duration, args, stderr. Goes to OTEL backend.
  Not replayable to state.
- **Conclusion:** No single timeline of "every state-changing event
  with enough payload to replay." That gap is the strongest argument
  for event-driven.

## 6. What works well today (don't break)

- `internal/beads/` typed API (Beads.Create, Close, Update, etc.) —
  callers shouldn't have to know events vs. dolt. Whatever migration
  does, the public API should stay stable.
- `internal/channelevents/` for inter-agent signals (MERGE_READY etc.) —
  decoupled from state, working fine. Don't touch.
- `internal/feed/` curation pattern — proven, reusable as materializer
  shape.
- Dolt MVCC inside the TCP server — already serializes correctly when
  used. The problem is that CLI mostly doesn't use it.
- Per-rig DB isolation — rigs scale independently. Don't centralize.

## 7. What's broken today (the deltas migration must fix)

| ID | Issue | Source |
|----|-------|--------|
| D1 | jsonl auto-export throttle race (mutating commands lose writes) | observed in hq-i7q/hq-wt8 close sequence; PR 1 patches |
| D2 | Auto-import wipes embedded dolt every CLI invocation | observed in `bd close` logs; amplifies D1 |
| D3 | Embedded vs TCP server split (no sync) | CLAUDE.md memory `project_bd_embedded_vs_server` |
| D4 | 24/26 agent locks stale >48h, no reaper | observed in `/gt/.beads/.locks/` |
| D5 | events.jsonl (bd's) writes have no lock when `events-export: true` | inspection of bd code |
| D6 | `bd init --reinit-local` silently destroys data | CLAUDE.md memory `feedback_bd_reinit_destructive`; pre-existing `backups-pre-reinit/` dir |
| D7 | Doctor doesn't catch 0-byte jsonl | observed in hq-i7q forensic; doctor only catches "bloat" not "empty" |
| D8 | 217 historical hq issues lost in past reinit (forensic) | memory `project_hq_bak_pre_reinit` |
| D9 | No single replayable state event timeline (no audit-from-replay) | architectural |
| D10 | bd is upstream dependency; we can't unilaterally change its lock semantics | architectural |

D1, D4, D7 = surface symptoms (defensive fixes possible).
D2, D3, D5, D6 = structural (need redesign or fork).
D8 = historical (forensic only, no fix).
D9, D10 = architectural (event-driven is the only structural answer).

## 8. Out of scope for this migration

- Replacing Dolt with another store (not happening).
- Replacing bd CLI surface for end users (users still type `bd close X`).
- Polecat session protocol changes beyond the producer API switch.
- Cross-host coordination (still git-based for now).
- iCloud backup format changes.
- Web UI read path (stays Dolt-direct).
- Multi-region replication.

If a proposed change touches any of the above, it's a separate RFC.
