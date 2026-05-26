# Open Questions — Unresolved Design Decisions

These need answers before Phase 0 closes. Some need RFCs of their own.
Owners should be assigned and dated.

## Q1. Event schema — single envelope vs. type-specific?

**Question:** Should the event envelope use `payload: map[string]any`
(flexible, like current `internal/events/`) or typed payload structs
per type (more boilerplate, but compile-time safety)?

**Trade-off:**
- map[string]any: easier to evolve, no codegen, but runtime errors
  if handler misreads field.
- Typed structs: compile-time safety, easier to refactor, but every
  new type = new struct + serialization code.

**Recommendation:** Typed structs. Boilerplate is acceptable cost for
preventing the kind of subtle schema bugs that would corrupt the log.

**Owner:** TBD. Needs to be resolved before any handler is written.

## Q2. Where does the state event log live?

**Question:**
- Option A: `<townRoot>/.beads/state-events.jsonl` (single file across
  all rigs, routed by `target_db` field).
- Option B: `<rig>/.beads/state-events.jsonl` (per-rig log,
  materializer reads N files).

**Trade-off:**
- A: single-writer simplicity, easy cross-rig transactions, but the
  file becomes a contention point under high write load.
- B: per-rig isolation (rig deletion = log deletion), but cross-rig
  events (sling, slot_open) need multi-target handling.

**Recommendation:** A. Cross-rig events are first-class in the
business logic; the log should reflect that.

**Owner:** TBD.

## Q3. Materializer: one per host, or one per rig?

**Question:** Same as Q2 but for the daemon.

**Trade-off:**
- One per host: simpler ops, single supervision point.
- One per rig: rig isolation, scales horizontally, but N daemons to
  supervise.

**Recommendation:** One per host. Per-rig is over-engineering at
current scale (~21 rigs in the only town today).

**Owner:** TBD.

## Q4. Idempotency window — how long to keep `seen_trace_ids`?

**Question:** Producer crashes between Append and Wait can cause
duplicate sends. Materializer dedups by trace_id. How long to remember?

**Options:**
- 1h — catches retries from same caller.
- 24h — covers manual replay after a bad day.
- 7d — covers extended outage scenarios.

**Trade-off:** Memory cost is bounded (each trace_id ~32 bytes;
7d at 100 events/s = ~60M entries ≈ 2GB). Use sqlite-backed cache to
avoid RAM pressure.

**Recommendation:** 24h. Most retries happen within minutes; 24h
covers operator-initiated replay.

**Owner:** TBD.

## Q5. How to handle bd subcommands we DON'T migrate to events?

**Question:** `bd config get/set`, `bd init`, `bd sql SELECT ...`, etc.
are not state mutations in the event-log sense. They stay as bd
subprocess calls. But:
- `bd config set` mutates bd's config file, not dolt state. Is that an
  event? No, config is bd's domain.
- `bd init` creates the database. Is that an event? Probably no —
  bootstrap predates the log.
- `bd sql UPDATE ...` is a backdoor mutation. Is that an event? It
  SHOULD be, but it bypasses our wrapper entirely.

**Recommendation:**
- Config: stays subprocess.
- Init: stays subprocess, but emit a synthetic `schema.init` event so
  the log has a starting marker.
- Raw `bd sql UPDATE/INSERT/DELETE`: lint rule forbids. Only `SELECT`
  allowed.

**Owner:** TBD.

## Q6. UNIX socket protocol — wire format?

**Question:** JSON line-delimited? Protobuf? Length-prefixed?

**Trade-off:**
- JSON: easy debugging (`nc -U socket | jq`), but parse overhead.
- Protobuf: efficient, schema versioned, but more setup.
- Length-prefixed binary: minimal, but opaque to humans.

**Recommendation:** JSON line-delimited. Volume is low enough that
parse overhead doesn't matter; debuggability is high value.

**Owner:** TBD.

## Q7. Cross-host conflict resolution — what does git merge look like?

**Question:** When two hosts edit the same issue and push to git, how
does the merge work?

**Today:** `issues.jsonl` merge — last-writer-wins per line. Often
produces conflicts requiring manual resolution.

**After migration:**
- Per-host event logs both contain edits.
- On `git pull`, both logs exist. Materializer must apply both.
- Last-write-wins per-field could be derived from event timestamps,
  but clocks may skew.

**Options:**
- Hybrid logical clock (HLC) in events.
- CRDT-style per-field merge.
- Manual conflict resolution UI.

**Recommendation:** Defer to a separate RFC. For initial migration,
keep per-host logs and accept that cross-host conflicts may produce
duplicate events that the dedup layer filters.

**Owner:** TBD. Probably mayor + wasteland team if federation matters.

## Q8. Polecat heartbeats — events or not?

**Question:** Polecats emit heartbeats every ~30s. That's 21 polecats
* 2/min = ~40 events/min just for liveness. Are these events?

**Options:**
- Yes, full events with full handling — log grows fast.
- Yes, but compacted by materializer (only retain latest per polecat).
- No, stays as separate `.heartbeat` files (current behavior).

**Recommendation:** No. Heartbeats are ephemeral liveness signals, not
state mutations. Keep separate.

**Owner:** TBD with polecats team.

## Q9. Schema migration semantics — replay-safe?

**Question:** When schema changes (e.g., add a column to dolt issues
table), how do we replay events from before the change?

**Options:**
- Migrations are events themselves (`schema.migrate`). Replay order
  preserves them.
- Migrations are out-of-band (manual SQL). Replay assumes current
  schema.

**Recommendation:** Schema migrations are events. Replay order
matters; materializer fails if a `schema.migrate` is missing for an
event that needs it.

**Owner:** TBD. Affects long-term replay-from-zero capability.

## Q10. Authorization — who can emit which events?

**Question:** Today, anyone with shell access can `bd close X`. Post
migration, anyone can emit `issue.close` events. Is that enough?

**Considerations:**
- Polecats should only emit events about themselves and their
  assigned issues.
- Witness should be able to close stuck issues.
- Mayor can do anything.

**Options:**
- No authorization. Trust local users (current state).
- Authorization in materializer (reject events from unauthorized
  actor for unauthorized actions).
- Authorization in producer (refuse to send).

**Recommendation:** No authorization initially. Add later if needed.
This matches current bd behavior. Adding auth post-hoc is feasible.

**Owner:** TBD.

## Q11. Backward compatibility — bd standalone usage

**Question:** Users today type `bd close X` directly. Post migration:
- Option A: `bd close X` still works, does direct dolt write (but
  doesn't emit event). Log becomes incomplete.
- Option B: `bd close X` is intercepted by PATH wrapper that emits
  event. Maintains log completeness.
- Option C: Document `bd close` as deprecated; users must use `gt bd
  close` or `gt close`.

**Recommendation:** B + C. PATH wrapper preserves UX; deprecation
in docs accelerates organic migration.

**Risk of B:** PATH wrapper has its own race risk (swap mid-call).
Needs careful design — symlink atomic swap, not rename of executable.

**Owner:** TBD.

## Q12. Performance budget — what's acceptable producer latency?

**Question:** AppendAndWait adds latency vs. direct write. What's
the budget?

**Today:** `bd close X` takes ~200-500ms (dominated by bd subprocess
spawn + dolt connection).

**Target post-migration:**
- Network/socket roundtrip: <5ms.
- Materializer apply: <50ms typical.
- Total budget: <100ms P50, <500ms P99.

**Concern:** If materializer queue grows, P99 spikes. Must alert
when queue depth >100.

**Owner:** TBD.

## Q13. What is the "current state" view served to clients?

**Question:** Materializer applies events into dolt. Clients (bd
list/show) read dolt. But materializer might lag. Is that visible
to clients?

**Options:**
- Lag is acceptable. Clients see whatever dolt has now.
- "Strict mode" flag for clients: refuse to read if lag > N.
- Synchronous read-after-write: read uses producer's
  WaitCaughtUp.

**Recommendation:** Default lag is acceptable (no behavior change vs.
today, which had jsonl staleness too). Add `--fresh` flag for
read commands that wants WaitCaughtUp before query.

**Owner:** TBD.

## Q14. Naming — `state-events.jsonl` vs. something else?

**Question:** Existing `.events.jsonl` is audit. New log is state.
Naming:
- `state-events.jsonl` — descriptive, parallel to existing.
- `mutations.jsonl` — clearer purpose.
- `log.jsonl` — terse.
- `wal.jsonl` — write-ahead-log semantics implied.

**Recommendation:** `state-events.jsonl`. Parallel to existing
naming wins clarity.

**Owner:** TBD (bikeshed).

## Q15. Compaction strategy — when and how?

**Question:** Event log grows forever. At some point, replay-from-zero
becomes infeasible.

**Options:**
- Snapshot + truncate: take dolt snapshot at seq N, archive events
  before N, replay only post-N for recovery.
- Compaction: keep latest event per key (issue.close hq-123 supersedes
  prior issue.create / update for hq-123). Lossy.
- Tiered: hot (uncompacted, last 30d) + warm (compacted, beyond).

**Recommendation:** Tiered, deferred to year+1. Initially keep
everything. Plan compaction when log >10GB or replay >1h.

**Owner:** TBD.

## Q16. Multi-rig writes — atomic across rigs?

**Question:** Sling creates work in rig A AND updates state in town
hq. Is that one atomic event or two?

**Options:**
- Single event, `target_db: ["hq", "rig-a"]`. Materializer applies
  both in one logical transaction (BUT dolt doesn't have cross-DB
  transactions natively).
- Two events with `tx.begin` / `tx.commit` wrapping. Saga pattern;
  materializer applies in order, rolls back via `tx.abort` if any
  fails.

**Recommendation:** Saga pattern. Cross-DB atomicity is genuinely
hard; saga gives us best-effort with explicit rollback signaling.

**Owner:** TBD.

## Q17. Should `internal/bus/` be reused?

**Question:** Existing in-process bus is sync, single-package use.
Could it be the producer→materializer transport?

**No.** Bus is in-process; materializer is a separate daemon. Cross-
process requires socket or similar. Bus stays as-is for its current
purpose.

**Owner:** Resolved here.

## Q18. Hooks — do they emit events too?

**Question:** Witness handlers, sling helpers emit `channelevents`
(MERGE_READY, SLOT_OPEN). Do those become state events?

**No.** Channelevents are inter-agent RPC-style signals, not state
mutations. They stay as-is. State events are about "what changed in
the database" — channelevents are about "what should agent X do next."

**Owner:** Resolved here.

## Q19. Event log access from web UI?

**Question:** Should web UI display the event log? Audit / "who
changed this when" UX.

**Recommendation:** Yes, in Phase 3. Powerful debugging tool. Read-
only, no auth concerns.

**Owner:** TBD.

## Q20. Migration cost recovery — when does this pay off?

**Question:** 7-8 months elapsed. What's the break-even?

**Honest answer:** Break-even is qualitative, not quantitative.
- Race incidents (like hq-i7q) avoided: ~1-2 per quarter. Each costs
  ~half-day eng to investigate. Not a strong economic case.
- Audit trail / replay: occasionally useful; rare incidents where
  "what happened?" requires forensic work.
- Polecats race-free: bigger qualitative win (currently they "just
  work" but on fragile foundation).

**Recommendation:** Don't justify this on cost-recovery. Justify it
on strategic correctness if you believe the codebase will grow N x
more state operations. Otherwise, the alternative (PR 1 + PR 2 +
server bus) is correctly priced for current scale.

**Owner:** Strategic decision — mayor.

---

## How to use this doc

1. Assign owner to each Q.
2. Set deadline for resolution (e.g., 2 weeks).
3. Each resolution updates the relevant target architecture section.
4. Phase 0 cannot start until all Q1-Q15 + Q20 are resolved (Q16-Q19
   can resolve during Phase 0).

## Status

All open. No owners assigned yet. Awaiting overseer / mayor
distribution.
