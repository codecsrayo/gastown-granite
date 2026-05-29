# 12 — Event log portability (Go → Rust cutover)

> Scope: settle the question raised by `hq-8iur.8` — how does the Rust API treat
> the pre-cutover Go event log (`/gt/.events.jsonl`)? Companion to
> [11-cutover-roadmap.md](11-cutover-roadmap.md). Decision deliverable, not a
> design proposal.

Snapshot date: 2026-05-28. Status: **decided — approach (b)**.

---

## TL;DR

At the cutover moment, the Rust API starts writing a fresh `EventRecord`-format
log. The pre-cutover Go log is **frozen in place** as a forensic cold-storage
artifact — it is **not** replayed through the Rust domain reducers, and Rust
does **not** ship a runtime Go-log translator. Pre-cutover state lives in
`bd` / Dolt (beads), the Postgres audit projection, and the frozen Go log;
post-cutover state is event-sourced from the Rust log.

Consequence: the `hq-8iur.8` gate (*"fixture Go replay-ea limpio por el path
Rust"*) is **retired** rather than satisfied. `hq-8iur.4` (shadow harness)
re-scopes accordingly — see §6.

---

## 1. The two candidate approaches

The forking question on `hq-8iur.8` was:

- **(a) Translator** — ship a Go-log → `EventRecord` edge adapter that maps Go
  vocabulary (`session_start`, `quota_scanned`, …) onto Rust domain events
  (`agent.spawned`, `quota.usage_probed`, …), synthesizes the missing
  `event_id` / `correlation_id` / `causation_id`, and feeds the result through
  `replay_gt`. The golden-log test would prove a real slice of
  `/gt/.events.jsonl` replays clean.

- **(b) Clean cutover** — Rust writes its own log from the cutover moment; the
  Go log is not replayed at all. Forensic continuity falls to (i) `bd` / Dolt
  for canonical bead state, (ii) the Postgres audit table, and (iii) the frozen
  Go JSONL retained on disk.

We picked **(b)**.

---

## 2. Why (b) — the evidence

### 2.1 Format mismatch is not cosmetic

The Go event log has a different schema **and** a different vocabulary from
`gt-audit::EventRecord`:

| Field | Go line (`internal/townlog/logger.go` etc.) | Rust `EventRecord` (`gt-audit/src/record.rs`) |
|---|---|---|
| identity | _none_ | `event_id` (UUID), `correlation_id`, `causation_id` |
| time | `ts: "2026-05-23T21:09:02Z"` (RFC3339 string) | `ts: <RFC3339 string>` — same wire, but domain payloads carry `now_secs: u64` (epoch) |
| routing | `type: "session_start"` (verb only) | `type: "agent.spawned"` (domain-prefixed; `replay_gt` errors on missing prefix) |
| origin | `source`, `actor`, `visibility` (envelope-style metadata) | _none_ at the record level — encoded inside the typed payload |
| body | `payload: {...}` (per-package shapes; `internal/quota/quotaevents`, `internal/townlog`, `internal/agentlog`, …) | `payload: <serde JSON of the typed domain enum variant>` |

Replay routes by domain prefix and **errors** on records whose `kind` lacks one
(see `apps/api/crates/bins/gt/src/event.rs::GtEvent::from_record`). Every Go
line would therefore need both a vocabulary rewrite *and* a payload reshape to
match the exact serde shape of the target Rust enum variant — not a 1:1 rename.

### 2.2 Vocabulary divergence is ~10% unmappable

Counts from the real `/gt/.events.jsonl` in `gastown-sandbox` (4,390 lines,
snapshot date above):

| Class | Lines | Examples | Notes |
|---|---:|---|---|
| Mappable to a Rust domain | ~2,900 (65%) | `quota_limited` → `quota.account_limited`; `quota_rotated` → `quota.rotated`; `session_start`/`spawn` → `agent.spawned`; `session_death` → `agent.session_end`; `nudge` → `agent.heartbeat` (semantic) | Each needs per-type payload reshape and ID synthesis |
| Feed-only (no Rust reducer) | ~680 (15%) | `quota_scanned` | Scanner aggregate; `gt-quota` has no reducer state for it |
| Meta / boot (skip) | ~390 (10%) | `boot`, `test` | Would route to `META_PREFIXES`-style skip set |
| **Unmappable** (no Rust domain at all) | ~420 (10%) | `mail`, `escalation_sent` / `escalation_acked` / `escalation_closed` / `escalation_reassigned`, `hook` / `unhook`, `quota_token_expired`, `quota_assigned`, `quota_reactivated`, `quota_swap_failed`, `quota_spawn_denied`, `handoff`, `halt`, `done` | The corresponding Rust domains are **MISSING** in the parity audit ([10-go-rust-parity.md](10-go-rust-parity.md) §3, §7, §9) — mail, escalation, hook protocol, account CRUD. No reducer to fold these into. |

A faithful translator could classify but not actually map the unmappable
tail, so reconstructed Rust state from a Go log would be a strict subset of
Go state. The "fixture replays clean" gate is therefore satisfiable only in a
weak sense — *clean* would have to mean *clean for the mapped subset; the rest
explicitly classified as drop-with-reason*.

### 2.3 Synthesized IDs are lossy

Go records carry no `event_id`, `correlation_id`, or `causation_id`. A
translator would synthesize them — typically `event_id = UUIDv5(hash of line)`,
`correlation_id = per-actor`, `causation_id = None`. That suffices to make
`replay_gt` not error, but it is *not* the causal graph the Rust event model
relies on for traceability (`docs/06-observability.md`). The Postgres audit
projection serves causal queries far better than synthesized chains.

### 2.4 Ongoing-maintenance cost is high

A production translator (option a1) couples Rust to the Go log format
permanently: every new Go event type the Go binary emits has to land a Rust
mapping or the shadow harness / replay breaks in prod. The cutover's goal is
to **drop the Go runtime**, not to acquire a fresh Go→Rust coupling at the
edge.

A bounded "tests-only" translator (option a2) is the strongest case for (a),
but its only consumer is the golden-log test itself plus `hq-8iur.4`'s shadow
harness — both of which can be reframed (see §6).

### 2.5 Bead-state continuity does not depend on log replay

Bead identity, status, parents, and assignment live in `bd` / Dolt (the
embedded + server-mode store, with the `issues.jsonl` mirror in
`/gt/.beads/`). That is the canonical durable state — Rust simply continues
reading and writing it via `gt-store-dolt`. Replaying the Go event log
would not reconstruct any bead row that is not already in Dolt; the log is
side-effect history, not the truth of the bead set.

---

## 3. The cutover protocol

The flip moment is the single point in time at which the Rust process becomes
the producer of `/gt/.events.jsonl`. Mixing two record formats in one file is
disallowed.

### 3.1 Pre-cutover (today)

- Go binary `gt` writes the Go-format records to `/gt/.events.jsonl`.
- Rust `gt` / `gt-web` / `gt-mcp` default `GT_EVENT_LOG=/tmp/gt.events.jsonl`
  (see `apps/api/crates/bins/gt/src/main.rs:57`,
  `apps/api/crates/bins/gt-web/src/main.rs:66`,
  `apps/api/crates/bins/gt-mcp/src/main.rs`). The two logs are file-disjoint
  during shadow.
- The Postgres `audit_events` table (`gt-store-pg`) mirrors every Rust
  `EventRecord` independently of the JSONL spill (see
  [04-persistence.md](04-persistence.md)).

### 3.2 Flip

In order:

1. Stop the Go `gt daemon` and any Go-spawned watchdog tmux sessions (mayor,
   deacon, witness/refinery per rig). The producer of the Go log is now
   silent.
2. Atomically rename the Go log to a frozen forensic file:
   ```bash
   mv /gt/.events.jsonl /gt/.events.jsonl.go.frozen.<UTC-epoch>
   ```
   The frozen file is read-only from this point — operators inspect it with
   `jq`, `rg`, or load it into the parity-audit Postgres for queries.
3. Start the Rust composition root (`bins/gt`) and `gt-web` with
   `GT_EVENT_LOG=/gt/.events.jsonl`. The first line written is a fresh
   `EventRecord` and the file is now the Rust log.
4. `gt-mcp` (in-process MCP sub-binary) reads the same `GT_EVENT_LOG` for its
   audit sink — confirmed in `bins/gt-mcp/src/main.rs:111`
   (`JsonlAudit::new(Arc::new(JsonlWriter::new(&log_path)))`).

### 3.3 Post-cutover

- Replay (`gt_audit::read_all` → `replay_gt`) only ever sees `EventRecord`
  records — the format reducer in `gt-audit::reader` does not have to handle a
  Go-format prefix. `is_meta(rec)` continues to skip frontier-audit prefixes
  (`mcp.*`).
- `load_state` in `bins/gt/src/event.rs` rebuilds `GtState` from byte 0 of the
  new file. There is no "fold the Go remnant first" branch.
- The frozen Go log stays on disk indefinitely as cold-storage evidence; it is
  never re-opened by the Rust process.

### 3.4 What happens to pre-flip Rust shadow logs

The `/tmp/gt.events.jsonl` shadow spill is **not** carried over. It exists
only to keep the Rust replay/determinism tests honest during shadow. The flip
starts a fresh Rust log at the production path.

---

## 4. Forensic continuity (what replaces in-process replay)

Per-question answer for "I need to know what happened before the flip":

| Question | Pre-cutover source | Post-cutover access path |
|---|---|---|
| What did bead `hq-foo` go through? | `bd show hq-foo`; the events table in Dolt | unchanged — `bd` and Dolt persist across flip |
| Which account rotated when? | `quota_rotated` lines in `/gt/.events.jsonl.go.frozen.*`; or `audit_events` rows whose `type` matched the Go quota types | `audit_events` row scan (the Postgres mirror — survives the flip); cold log for verbatim |
| Who escalated to whom in the past? | `escalation_*` lines in frozen Go log; `gt mail` archive | frozen Go log only — Rust has no mail/escalation domain (parity doc §3 + §7, MISSING) |
| What did a polecat session do between spawn and death? | `session_start` / `session_death` / `spawn` lines + agent logs (`internal/agentlog`) | frozen Go log + claude conversation JSONL under `~/.claude/projects/` (out of scope for this doc) |

The frozen Go log is **not** machine-loaded by any Rust component. Treat it
like an archived rotated log — `grep`/`jq` access only.

---

## 5. Risks and explicit divergences

### 5.1 Risk: a forensic-replay requirement re-emerges

If a future incident requires reconstructing Rust-domain state from
pre-cutover history (e.g. a regression discovered post-flip whose root cause
predates the flip), the path is:

1. Build a one-shot translator as a fresh-scoped bead (the option-a2 from the
   `hq-8iur.8` design fork). Scope it as **tests / offline only** — never
   wire it into the runtime path.
2. Run it against the frozen Go log to produce a synthesized `EventRecord`
   slice that replays through `replay_gt`. Document the unmappable tail
   exactly as enumerated in §2.2.

This is explicitly **not** blocked or precluded by this decision; we are
choosing not to pay for it pre-emptively.

### 5.2 Divergence: bead `hq-8iur.8` gate is retired

The bead's original gate text — *"fixture Go replay-ea limpio por el path
Rust; divergencias explicadas, no silenciosas"* — assumed approach (a). It is
not technically satisfied. The bead closes with **this document** as the
deliverable instead. The closing record should reference this file.

### 5.3 Divergence: no continuous correlation between pre- and post-flip events

Causation graphs (`correlation_id` / `causation_id`) do not span the flip.
A chain that began with a Go-spawned actor and continues with a Rust-folded
event cannot be queried as one in-process causal trace; it has to be joined
across the frozen Go log and the new Rust log by domain identifiers
(`session`, `account`, `bead`).

This is acceptable because the cutover is a hard reboot of the producer
identity — every long-running actor (mayor, deacon, witness, refinery) is
restarted under Rust ownership, so cross-flip causal threads are vanishingly
rare in practice. The few exceptions (in-flight quota rotations, in-flight
convoys) belong in the cutover runbook (`hq-8iur.6`).

---

## 6. Implication for `hq-8iur.4` (shadow / parallel-run harness)

`hq-8iur.4` had naturally read as *"Rust reads the live Go log and diffs the
folded state against what Go reports."* That assumed a translator. With
approach (b), the shadow harness re-scopes:

- Rust and Go run **side by side**, each writing its own log to its own path.
  Rust's path is `/tmp/gt.events.jsonl` (or another shadow path); Go keeps
  `/gt/.events.jsonl`.
- The harness compares **externally observable side-effects** at sync points,
  not log-replay state:
  - bead status transitions in Dolt (single source of truth — both producers
    write here);
  - quota rotation outcomes (which account is active);
  - active-session inventory (`/api/sessions` vs `gt agents`);
  - convoy state transitions (`orch.*` events seen by Rust vs convoy channel
    events seen by Go).
- Divergences are reported as *side-effect drift*, classified as `expected`
  (the Go runtime is doing something Rust intentionally won't, e.g. mail) or
  `unexpected` (a domain Rust owns disagrees with Go).

This sidesteps the unmappable-tail problem (mail / escalation / hook produce
no side-effect Rust ever has to match) and aligns the harness with what the
cutover gate actually cares about: *does Rust drive the town to the same
state Go did?*

Concrete deliverables for `hq-8iur.4` (out of scope for this doc):

- A diff job that polls both `/api/sessions` (Rust) and `gt agents` JSON (Go)
  on a tick and reports drift.
- A diff job that compares the active quota account at each minute boundary
  (Rust quota state vs Go `gt quota status`).
- A bead-status diff (Dolt is shared, so this catches any reducer that wrote
  state through a different path on each side).

---

## 7. Open questions deferred to other beads

- **Rust account CRUD** (`hq-8iur.5+` or follow-on): without it, `quota_*`
  domain events are emitted but the operator surface for `gt account
  add/list/retire` stays Go-only (parity §6). Not a portability blocker, but
  the cutover runbook (`hq-8iur.6`) has to declare which side owns account
  state at the flip moment.
- **Mail / escalation / hook in Rust** (out of scope of Paso 8): if these
  ever land as Rust domains, this doc's unmappable tail shrinks. The
  `Unmappable` row in §2.2 doubles as the requirements list for them.
- **Log retention policy** for `/gt/.events.jsonl.go.frozen.*`: the frozen
  file is unbounded by this doc. Operations should decide whether to
  compress / archive after N days.

---

## 8. Refresh checklist

When updating this doc:

1. `wc -l /gt/.events.jsonl` and the type histogram (the snippet in §2.2) —
   if material churn, refresh the counts so future readers see current
   evidence.
2. `grep -rEn 'enum [A-Z]\w*Event' apps/api/crates/domain/*/src/events.rs` —
   if a new Rust domain enum lands (e.g. mail), update the "Unmappable" row
   and §7.
3. The cutover-runbook reference in §3 — keep `hq-8iur.6` and this doc in
   sync about what happens to the Go log file at the flip moment.
