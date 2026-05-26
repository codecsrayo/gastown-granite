# Target Architecture — Event-Driven State

The end state. Read `01-CURRENT-STATE.md` first.

## 1. One-line summary

State mutations become events. A single materializer daemon applies the
event log to Dolt. Dolt becomes a materialized view; the event log is
the authority.

## 2. Components

```
┌──────────────────────────────────────────────────────────────┐
│                   PRODUCERS (write side)                      │
│  gt CLI │ polecats │ witness │ refinery │ deacon │ reaper    │
│  doctor (fix) │ mayor │ web UI (mutations) │ external sync   │
└────────────────────────┬─────────────────────────────────────┘
                         │  events.AppendAndWait(evt, timeout)
                         │  events.AppendAsync(evt)
                         │  (both O_APPEND atomic for <PIPE_BUF)
                         ▼
┌──────────────────────────────────────────────────────────────┐
│             STATE EVENT LOG (single source of truth)          │
│  <townRoot>/.beads/state-events.jsonl                         │
│   {seq, ts, actor, target_db, type, payload, trace_id,       │
│    schema_version, parent_seq?}                              │
│   - rotated daily → state-events-YYYY-MM-DD.jsonl            │
│   - compacted weekly (kept indefinitely, archived)           │
│   - fsync every N events (configurable, default N=10)        │
│   - O_APPEND atomic (no flock for sub-PIPE_BUF writes)       │
└────────────────────────┬─────────────────────────────────────┘
                         │ tail with epoll/inotify
                         ▼
┌──────────────────────────────────────────────────────────────┐
│         MATERIALIZER DAEMON (single writer per host)          │
│  beads-materializer (systemd service)                         │
│   - tails state-events.jsonl from .beads/materializer.state  │
│   - dispatches event → handler per type                      │
│   - applies via Dolt SQL transaction (1 tx per event)        │
│   - on success: advances checkpoint, publishes "applied:trace"│
│     to UNIX socket /run/beads/materializer.sock              │
│   - on failure: logs, retries with backoff, eventually DLQ   │
│   - crash recovery: idempotent replay from checkpoint        │
│   - metrics: lag, throughput, error rate via OTEL            │
└────────┬─────────────────────┬───────────────────────────────┘
         │                     │
         ▼                     ▼
┌─────────────────┐  ┌──────────────────────────────────────────┐
│   DOLT (view)   │  │       CONSUMERS (read side, unchanged)    │
│   @ :3307       │  │  bd list/show/sql/ready                   │
│   per-rig DBs   │  │  witness, refinery, doctor reads          │
│   materialized  │  │  web UI                                   │
│   rebuildable   │  │  reaper TTL scan                          │
└─────────────────┘  └──────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────────┐
│       SNAPSHOT EXPORTER (background, optional)                │
│   - reads dolt periodically (default 5m)                      │
│   - writes <rig>/.beads/issues.jsonl atomically               │
│   - artifact for git versioning + offline tools               │
│   - NEVER source of truth                                     │
└──────────────────────────────────────────────────────────────┘
```

## 3. Event schema

### 3.1 Envelope

```jsonc
{
  "seq": 142309,                              // monotonic per log file
  "ts": "2026-05-26T03:52:19.123456789Z",     // RFC3339Nano UTC
  "actor": "Brayan@overseer",                 // who triggered (BEADS_ACTOR)
  "target_db": "hq",                          // rig DB this applies to
  "type": "issue.close",                      // see catalog below
  "payload": { ... },                         // type-specific schema
  "trace_id": "01HXYZABC...",                 // ULID for correlation
  "schema_version": 1,                        // event schema version
  "parent_seq": 142308                        // optional, for causal links
}
```

### 3.2 Event types catalog (initial)

Mirrors current `IsMutating` classifier in `internal/beads/beads_mutations.go`.

| Type | Payload | Notes |
|------|---------|-------|
| `issue.create` | `{id, title, type, priority, status, assignee, labels, description, ...}` | id assigned by producer (existing bd ID scheme) |
| `issue.update` | `{id, fields_changed: {field: newval}}` | partial update, idempotent by trace_id |
| `issue.close` | `{id, reason, resolution}` | |
| `issue.reopen` | `{id, reason}` | |
| `issue.edit` | `{id, description, ...}` | full-field edit, distinguishes from update |
| `dep.add` | `{from, to, type}` | |
| `dep.remove` | `{from, to, type}` | |
| `label.add` | `{id, labels: []}` | |
| `label.remove` | `{id, labels: []}` | |
| `memory.remember` | `{key, value, scope}` | |
| `memory.forget` | `{key}` | |
| `comment.add` | `{id, body, author}` | |
| `agent.claim` | `{agent_id, bead_id, lease_until}` | |
| `agent.release` | `{agent_id, bead_id}` | |
| `import.batch` | `{events: [Event]}` | bulk import; materializer applies sub-events as single tx |
| `schema.migrate` | `{from_version, to_version, sql}` | DDL events |
| `tx.begin` | `{tx_id}` | saga pattern for multi-event ops |
| `tx.commit` | `{tx_id}` | |
| `tx.abort` | `{tx_id, reason}` | |

### 3.3 Schema versioning

- Every event has `schema_version` field.
- Materializer dispatcher selects handler by `(type, schema_version)`.
- Adding a new optional field = same version, handler treats absent as nil.
- Removing/renaming a field = new version, both handlers maintained for
  replay of old events.
- Test suite includes a fixture of every historical event version;
  materializer must replay all of them to current state without diff.

## 4. Producer API

### 4.1 Surface

```go
// internal/events/state/producer.go (new sub-package)

type Producer interface {
    // Append + wait for materializer ACK. Blocks ≤timeout.
    AppendAndWait(ctx context.Context, evt Event) error

    // Append, return immediately. Caller can later wait by trace_id.
    AppendAsync(ctx context.Context, evt Event) (trace_id string, err error)

    // Wait for a previously-async event to apply.
    WaitFor(ctx context.Context, traceID string) error

    // Wait until materializer has caught up to or past the given seq.
    WaitCaughtUp(ctx context.Context, seq int64) error
}
```

### 4.2 Two modes, when to use

| Mode | Use case |
|------|----------|
| `AppendAndWait` (default) | Interactive CLI commands. User typed `gt close X`, expects next read to see it. |
| `AppendAsync` | Bulk operations (reaper close 1000 wisps). Throughput >> latency. |

### 4.3 Backpressure

- `AppendAndWait` timeout default 30s. Returns error on timeout.
- Producer should propagate the error — DO NOT silently fall through to
  direct dolt write. That would defeat the design.
- Materializer publishes "queue depth" metric. CLI can warn user when
  depth >100.

### 4.4 Internal flow (AppendAndWait)

```
1. Producer fills envelope (seq=0, materializer assigns).
2. Producer connects to UNIX socket /run/beads/producer.sock.
3. Producer sends event over socket; materializer:
   a. Allocates monotonic seq.
   b. Appends to state-events.jsonl (O_APPEND atomic).
   c. fsync per policy.
   d. Applies to dolt within transaction.
   e. Commits dolt tx.
   f. Advances checkpoint.
   g. Sends ACK with (seq, applied_at) back to producer.
4. Producer returns nil to caller.
```

Fallback path if socket unavailable: producer writes to a "spool"
directory; materializer picks up spool on next start. Surfaces a warning.

## 5. Materializer daemon

### 5.1 Process model

- One per host.
- systemd unit `beads-materializer.service`.
- Restarts on crash (systemd Restart=always).
- Reads from `<townRoot>/.beads/state-events.jsonl` (single file across
  all rigs; routes by `target_db` field).
- Writes to per-rig Dolt DBs via the TCP server.

### 5.2 Checkpoint

- File: `<townRoot>/.beads/materializer.state` (JSON).
- Schema: `{last_applied_seq, last_applied_ts, version}`.
- Written after each event applies (not batched). Fsync.
- On startup: read checkpoint, seek log file to first event after seq.
- Idempotency check: dispatcher refuses to re-apply events ≤ checkpoint.

### 5.3 Dispatch

```go
type Handler func(ctx context.Context, tx *sql.Tx, evt Event) error

var handlers = map[string]Handler{
    "issue.create":   handleIssueCreate,
    "issue.update":   handleIssueUpdate,
    // ... one per event type
}
```

### 5.4 Failure handling

| Failure | Behavior |
|---------|----------|
| Handler returns error | Log, retry with exponential backoff (1s, 2s, 4s, ... 60s max). After 5 retries, write to dead-letter queue `.beads/materializer.dlq`. Do NOT advance checkpoint. Halt until ops clears DLQ. |
| Dolt connection lost | Reconnect with backoff. Producer AppendAndWait times out — surface to user. |
| Disk full on log append | Producer write fails. Materializer keeps trying to read; no progress. Alert. |
| Corrupted event line | Skip + log. Advance checkpoint past it. Operator follow-up via DLQ entry. |
| schema_version unknown | Halt. New deploy needed. Refuse to silently skip. |

### 5.5 Rebuild

- Command: `beads-materializer rebuild --from=<seq> [--db=<rig>]`.
- Drops rig DB tables, replays log from `--from`.
- Used after dolt corruption or schema migration validation.
- Replaces `bd init --reinit-local`.

## 6. Read consistency model

- **Default:** Producer's AppendAndWait blocks until applied → next read
  on this host sees the write.
- **Read-your-writes guarantee:** local to host. Cross-host reads have
  same eventual consistency as today (git push/pull of the log).
- **Monotonic reads:** dolt MVCC inside the server ensures within a
  session.
- **Stale read OK paths:** explicit opt-in via `bd --allow-stale` (already
  supported); doctor and patrol checks should use this.

## 7. External integrations

### 7.1 GitHub mirror (existing)

- Today: cron job runs `gt gh sync` periodically; queries dolt, pushes
  diffs to GH.
- Target: subscribe to materializer's UNIX socket (pub stream of applied
  events); push selected types (issue.create, issue.close) to GH in
  near-real-time.
- Failure: if mirror lags, dolt is still authoritative. GH catches up.

### 7.2 Slack notifications

- Today: ad-hoc Webhook calls from various places.
- Target: subscribe to materializer stream; filter by type +
  configurable rules.

### 7.3 iCloud backup (existing)

- Today: backup skill snapshots Dolt DBs.
- Target: snapshot exporter coordinates with materializer to checkpoint
  the log + DB at the same seq. Backup is "consistent at seq N".

### 7.4 git versioning of issues.jsonl

- Today: jsonl committed periodically.
- Target: snapshot exporter writes jsonl every 5m (or on demand); commit
  hook reads dolt and writes consistent snapshot. jsonl is no longer
  the source of truth — git history preserves audit of WHAT state
  existed at each push.

## 8. Failure modes summary table

| Failure | Recovery | User-visible |
|---------|----------|--------------|
| Producer crash mid-Append | O_APPEND atomic if <PIPE_BUF; otherwise partial line discarded on read | None |
| Producer crash post-Append, pre-Wait | Event still applied. Caller may retry — idempotent by trace_id | Possible duplicate report; dedup at handler |
| Materializer crash mid-handler | Tx rollback in dolt; replay on restart | Brief latency spike |
| Materializer crash post-apply, pre-checkpoint | Re-apply on restart; handler MUST be idempotent | None |
| Materializer dies and won't restart | All AppendAndWait time out; AppendAsync queues to spool | All writes stop; pager alert |
| Dolt server crash | Materializer pauses; producers stall; reads stall | Critical alert |
| Disk full | Append fails → producer error | User sees error |
| Log file corruption | Validate-on-read; skip corrupt line + log; continue | Operator runs `materializer audit` |
| Network partition (multi-host eventually) | Per-host materializer authoritative locally; cross-host = git reconciliation | Cross-host stale reads |
| Schema version mismatch | Materializer halts on unknown version | Deploy version mismatch error |
| Polecat writes bypass producer (Phase 2 incomplete) | Log gap — events.jsonl misses those mutations | Visible drift between log and dolt; doctor check flags |

## 9. What this solves (mapped to D1-D10 from `01-CURRENT-STATE.md`)

| Debt | Solved by |
|------|-----------|
| D1 race | jsonl is artifact; no race to lose |
| D2 wipe | no auto-import; dolt is materialized incrementally |
| D3 embedded/server split | embedded mode deleted; server only |
| D4 stale agent locks | claims are events with TTL; reaper scans log |
| D5 events.jsonl lock | O_APPEND atomic by design |
| D6 reinit-local destructive | replaced by `materializer rebuild` (requires explicit confirm) |
| D7 doctor 0-byte | jsonl is artifact; 0-byte = snapshot exporter bug, not state |
| D8 historical loss | unchanged — can't recover what wasn't logged. From now on, log is authoritative + git-versioned |
| D9 no replayable audit | event log IS the audit |
| D10 bd upstream divergence | bd CLI subprocess becomes thin shim that converts subcommands to events (we wrap, not fork) |

## 10. What this does NOT solve

- Cross-host conflicts (still git-based).
- Performance under extreme write burst (>10k events/s) — would need
  log sharding by rig.
- Schema migration of historical events (open question, see `06-OPEN-QUESTIONS.md`).
- Polecat session UX changes if they're used to direct dolt access.

## Appendix A: Lighter alternative

If the 7-9 week cost or polecat coordination risk is unacceptable:

**A1: PR 2 (1 day) + Server bus (1 sprint)**

- PR 2: migrate 14 packages to `internal/beads/` wrapper (closes D1
  across all callers).
- Server bus: CLI defaults to `--server-port` when `dolt-server.port`
  exists. Embedded is fallback. Dolt MVCC eliminates D3 race-class.
- Covers ~95% of race risk for ~5% of the rewrite cost.
- Does NOT give: D9 replayable audit, D4 stale lock structural fix,
  D6 reinit redesign, time-travel debugging.

**A2: bd-events flag enabled + thin shim**

- Turn on bd's existing `events-export: true`.
- Write a small daemon that tails bd's events.jsonl and emits to our
  log.
- Read-only view of bd events; can't drive mutations.
- Cheap audit win, no architectural improvement.

These alternatives are documented for contrast. Pick this full
event-driven path only if the audit/replay/structural-correctness
benefits justify the cost.
