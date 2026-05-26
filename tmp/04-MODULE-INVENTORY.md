# Module Inventory — Per-Package Changes

The execution spec for phases 1-4. Read `01-CURRENT-STATE.md` and
`02-TARGET-ARCHITECTURE.md` first.

This is a per-module audit of every package that touches bd, dolt, or
the existing event-adjacent systems. For each: what it does today, what
it must do in the target arch, the migration steps, the risk.

## Counts at a glance

- **Total Go packages in `internal/`:** ~70
- **Packages that import `internal/beads`:** 15
- **Packages with direct `exec.Command("bd", ...)`:** 11
- **Total direct exec sites of bd:** 57
- **Packages that emit `internal/events`:** ~6 (sling, hook, handoff, etc.)
- **Packages that emit `internal/channelevents`:** 3 (witness, sling_helpers, molecule)
- **Packages affected by Phase 2 (polecat producer migration):** 1 + polecat session protocol consumers

## Risk legend

- 🟢 LOW — mechanical migration, tests unchanged, one PR
- 🟡 MED — behavioral change, tests need updating, coordinate with adjacent team
- 🔴 HIGH — protocol or contract change, multi-PR, requires RFC + sign-off

## A. Foundation — must land before anything else

### A1. `internal/events/`

| Aspect | Today | Target |
|--------|-------|--------|
| Purpose | Activity feed audit log | Activity feed + state events root |
| API surface | `Event{}`, payload helpers | + `state.Producer` interface, `state.Event{}` |
| Storage | `~/gt/.events.jsonl` | + `<townRoot>/.beads/state-events.jsonl` |
| Visibility | audit/feed/both | + `state` (mutates dolt) |

**Changes:**
- Add `internal/events/state/` sub-package with `Producer` interface,
  `Event{}` envelope, `EventType` constants, payload structs.
- Extend Visibility enum: add `VisibilityState` for mutation events.
- Add helper to write to `state-events.jsonl` separately from
  `.events.jsonl` (keep audit log clean of voluminous state events).
- New: `internal/events/state/producer/socket.go` — UNIX socket client.
- New: `internal/events/state/producer/spool.go` — fallback when socket
  unavailable.

**Migration steps:**
1. Define event schema (types, payload structs) — gated by RFC in
   `06-OPEN-QUESTIONS.md` Q1.
2. Implement `AppendAsync` (spool only, no materializer yet).
3. Add unit tests + golden files for each event type.
4. Wire telemetry recording.

**Risk:** 🟢 — additive, no behavior change yet.

**Tests:** new package, dedicated suite. Goldens versioned by schema_version.

### A2. New package: `internal/materializer/`

| Aspect | Today | Target |
|--------|-------|--------|
| Existence | Does not exist | New daemon |

**Changes:**
- New `internal/materializer/` package with `Daemon`, `Dispatcher`,
  `Handler`, `Checkpoint` types.
- New `cmd/beads-materializer/main.go` binary entry.
- systemd unit file `systemd/beads-materializer.service`.
- DLQ implementation: `internal/materializer/dlq.go`.
- Metrics: queue depth, lag, throughput, error count, DLQ size via OTEL.

**Migration steps:**
1. Build dispatcher skeleton with no-op handlers.
2. Implement one handler per event type. Each handler:
   - Open dolt tx for `target_db`.
   - Apply mutation.
   - Commit tx.
   - Advance checkpoint.
3. Unit-test each handler with in-memory dolt.
4. Integration test: end-to-end producer → log → materializer → dolt.
5. Crash recovery test: kill mid-batch, restart, verify checkpoint
   resumes correctly.

**Risk:** 🔴 — net-new daemon. Operational debt. systemd supervision
required. Failure mode visibility (alerts, dashboards) must exist
before Phase 1 ships.

**Tests:** dedicated suite + integration test harness with real Dolt
server in CI.

### A3. `internal/beads/` (the existing wrapper)

| Aspect | Today | Target |
|--------|-------|--------|
| Mutation path | `bd <subcmd>` subprocess + jsonl flock (PR 1) | producer.AppendAndWait |
| Read path | `bd list/show/sql` subprocess | unchanged (dolt direct) |
| API surface | `b.Create(...)`, `b.Close(...)`, etc. | unchanged (callers untouched) |

**Changes:**
- Mutating methods (`Create`, `Close`, `Update`, `AddDep`, `AddLabel`,
  `Remember`, `Forget`, etc.) replaced internally:
  - Build event envelope.
  - Call `producer.AppendAndWait(ctx, evt)`.
  - On success: return same shape as today (e.g. `*Issue`).
  - On failure: same error contract.
- Read methods unchanged.
- Remove `forceExportJSONL`, `jsonlLock` from PR 1 — no longer needed.
- Remove `runWithStdin` mutation branch — events do the writing.
- Keep `runWithStdin` for read commands (sql SELECT etc.).
- Update `IsMutating` classifier to also gate the new dispatch path.

**Migration steps:**
1. Phase 0: add dual-write — mutations call producer AND continue to
   call bd. Compare results. Build confidence.
2. Phase 1: flip mutation methods to producer-only. Keep bd subprocess
   for reads.
3. Phase 4: remove dual-write, remove PR 1 flock code.

**Risk:** 🟡 — public API stays stable but behavior changes (latency,
failure modes). Existing tests must keep passing.

**Tests:** existing `beads_*_test.go` should pass unchanged. Add new
dual-write divergence detector test that fails CI if a mutation
produces different state via the two paths.

## B. CLI surface — high contact

### B1. `internal/cmd/` (84 files, 35 direct bd exec sites)

**Today:**
- Implements every `gt` subcommand.
- ~35 places call `exec.Command("bd", ...)` directly.
- ~49 places call `internal/beads/` wrapper methods.

**Target:**
- All bd subprocess calls eliminated. CLI commands either:
  - Call `internal/beads/` typed methods (for state).
  - Call `internal/beads/` query helpers for read SQL (to be added).
  - For non-state bd operations (e.g. `bd config get`), use a thin
    config helper or shell out (unchanged for those).

**Migration steps:**
1. Audit each of 35 direct exec sites. Categorize:
   - Mutation (`bd close`, `bd update`, ...) → migrate to `b.X()` method.
   - Read (`bd sql`, `bd show --json`) → migrate to read helper.
   - Config (`bd config get/set`) → use config helper.
   - Init (`bd init`) → keep as subprocess (rare, ops-time).
2. Expand `bd_subprocess_policy_test.go` to cover `internal/cmd/`.
3. Mechanical refactor PRs grouped by subcommand family.

**Risk:** 🟡 — sheer volume; high chance of subtle behavior drift.

**Tests:** existing CLI integration tests must keep passing. Add a
"forbidden exec" lint rule.

### B2. `internal/cli/`

Smaller package. Same pattern as B1, fewer files.

**Risk:** 🟢

## C. Agents — Phase 2 critical path

### C1. `internal/polecat/`

**Today:**
- Uses TCP server mode (test harness passes `--server-port`).
- Imports `internal/beads/` (uses `m.beads.Show()` etc.).
- Has 1 direct `exec.Command("bd", "show", id, "--json")` in
  `session_manager.go:891`.
- Has 1 direct `exec.Command("bd", "update", id, "--status=hooked",
  "--assignee="+agentID)` in `session_manager.go:989`.
- Manages polecat lifecycle, target_clean, namepool, heartbeats.

**Target:**
- All mutations via `Producer.AppendAndWait` or `AppendAsync`.
- Reads via `internal/beads/` wrapper (which uses dolt direct).
- `bd update --status=hooked` becomes `issue.update` event with
  `fields_changed: {status: hooked, assignee: X}`.
- `bd show --json` becomes typed `b.Show()` (no behavior change).

**Migration steps:**
1. Inventory every mutation polecat issues (lifecycle events: claim,
   release, hook, status change). Each becomes an event type or
   reuses existing.
2. Replace the 2 direct exec sites with wrapper calls.
3. For bulk operations (target_clean), use `AppendAsync` + `WaitCaughtUp`.
4. Update polecat tests to assert events emitted, not just dolt state.
5. **Coordinate with polecat team** — they own this; we propose, they
   review and execute.

**Risk:** 🔴 — Phase 2 one-way door. Once polecats are on producer API,
rollback is hard (dual-write must remain enabled during entire phase,
then deleted).

**Tests:** polecat integration tests must keep passing. New: event
sequence assertions (claim → hook → unhook → release expected order).

### C2. `internal/witness/`

**Today:**
- 1 file imports beads.
- Emits `channelevents` for MERGE_READY, SLOT_OPEN, SLOT_BLOCKED.
- Does patrol checks (mostly reads).

**Target:**
- Patrol reads unchanged.
- Patrol-triggered mutations (closing stale beads, clearing locks) go
  through producer.
- `channelevents` stay as-is (they're inter-agent signals, not state).

**Migration steps:**
1. Audit witness handlers for any mutation paths.
2. Replace with producer calls.
3. Keep channelevents emission separate (different layer).

**Risk:** 🟡 — fewer touch points than polecat, but witness is on the
critical path for sling correctness.

**Tests:** witness handler tests + integration with materializer.

### C3. `internal/refinery/`

**Today:**
- 2 files import beads.
- Refinery is the canonical clone manager; handles merge queue.
- Subscribes to channelevents (MERGE_READY).

**Target:**
- Mutations on merge complete / fail go through producer.
- Subscription pattern unchanged.

**Migration steps:**
1. Audit refinery mutation sites.
2. Replace.

**Risk:** 🟡

### C4. `internal/deacon/`

**Today:**
- Manages death warrants for stuck agents.
- Imports beads (mutations to close stuck issues).
- Already gated by `bd_subprocess_policy_test.go`.

**Target:**
- Death warrant operations → producer events.
- New event types possibly: `agent.warrant.issued`, `agent.warrant.executed`.

**Risk:** 🟡

### C5. `internal/reaper/` (skill, not Go package)

The reaper is a skill (`backup`, `reaper`, etc. per /skills list), not a
Go internal package. It calls bd via shell. After migration:
- Skill scripts call `gt` commands (which use producer).
- Bulk close uses `AppendAsync` for throughput.

**Risk:** 🟢 — skills are user-edit-able, low blast radius.

## D. Doctor & diagnostics

### D1. `internal/doctor/` (14 files use beads, 11 direct exec sites)

**Today:**
- 72 check files.
- 11 directly call `exec.Command("bd", ...)`.
- Fixes call `bd update`, `bd close`, `bd label`, `bd config set`.
- Reads call `bd sql --csv`, `bd show --json`.

**Target (immediate, PR 2 of original plan):**
- Migrate 11 direct sites to `internal/beads/` wrapper. Closes D1 race.

**Target (event-driven):**
- All fix paths emit events.
- Read paths unchanged.

**Migration steps:**
1. PR 2 (covered in original plan): mechanical wrapper migration.
2. Add `QuerySQL/QueryJSON/ExecSQL` helpers to wrapper.
3. Doctor checks adopt helpers; ad-hoc CSV parsing deleted.
4. Phase 2: Fix() methods route through producer.
5. New doctor check: `materializer-health-check.go` — verifies daemon
   alive + lag < threshold.
6. New doctor check: `event-log-integrity-check.go` — validates
   `state-events.jsonl` parseable.

**Risk:** 🟡 (PR 2 alone 🟢)

### D2. `internal/health/`

Aggregates check results. Add materializer + event log health
indicators.

**Risk:** 🟢

## E. Storage & infra

### E1. `internal/doltserver/`

**Today:**
- Manages Dolt TCP server lifecycle.
- Has 2 files referencing beads.

**Target:**
- Becomes critical infrastructure (the only write target).
- Add hooks: notify materializer on server start/stop.
- Health endpoint that materializer can poll.

**Migration steps:**
1. Add server start/stop hooks.
2. Add admin SQL endpoint for materializer's `BEGIN/COMMIT`.
3. Document supervision (systemd ordering: dolt server → materializer).

**Risk:** 🟡

### E2. `internal/feed/`

**Today:**
- Curator daemon for `.events.jsonl` → `.feed.jsonl`.
- Already a tail-and-process daemon.

**Target:**
- Reusable template for materializer design (same pattern).
- Continues curating audit-only events.
- Could later also tail state-events.jsonl for "state activity" feed
  (e.g. notify on issue.create) — separate feature.

**Risk:** 🟢

### E3. New: `internal/snapshot/`

**Today:**
- Auto-export via bd (throttled, racy).

**Target:**
- New package with `Exporter` that reads dolt, writes
  `<rig>/.beads/issues.jsonl` atomically (tempfile + rename).
- Runs every 5m (configurable). Triggered on demand for git commit hooks.
- Coordinates with materializer to snapshot at consistent seq.

**Risk:** 🟢

## F. External integrations

### F1. `internal/github/`

**Today:**
- GitHub mirror — push issues to GH.
- Currently queries dolt periodically.

**Target:**
- Subscribe to materializer's applied-event stream.
- Push `issue.create`/`close` events to GH.
- Fallback: dolt scan if subscriber falls behind.

**Risk:** 🟡 — GH API rate limits already a concern.

### F2. `internal/web/`

**Today:**
- Web UI / API server.
- 1 direct bd exec.
- Mostly read.

**Target:**
- Read path unchanged (dolt direct).
- Mutation endpoints (assign, close from UI) use `Producer.AppendAndWait`.

**Risk:** 🟢

### F3. `internal/wasteland/`

Federation across rigs. Not in scope for initial migration but should
be evaluated separately — event log per host complicates federation.

**Risk:** unknown — out of scope, deferred.

### F4. `internal/bitbucket/`

Bitbucket integration. Same pattern as F1.

**Risk:** 🟡

## G. Lifecycle & supervision

### G1. `internal/daemon/`

**Today:**
- Generic daemon scaffolding.
- 1 file references beads.

**Target:**
- Materializer reuses this scaffolding.
- Add daemon-coordination primitives (event log lag awareness).

**Risk:** 🟢

### G2. `internal/boot/`

**Today:**
- Town boot sequence.

**Target:**
- Add materializer to boot order (after dolt server, before agents).
- Add lag check before declaring boot complete.

**Risk:** 🟡

### G3. `internal/scheduler/`

Cron scheduling for routines. Producer for scheduled mutations
(reaper-style closes).

**Risk:** 🟢

## H. Tools & TUI

### H1. `internal/tui/feed/`

**Today:**
- TUI feed display, tails `.events.jsonl`.
- 3 direct bd exec sites.

**Target:**
- Tail `state-events.jsonl` too for "state activity" view (optional).
- Replace 3 direct exec with wrapper.

**Risk:** 🟢

### H2. `internal/tui/convoy/`

Convoy tracking TUI. Same pattern.

**Risk:** 🟢

### H3. `cmd/gt-proxy-client`, `cmd/gt-proxy-server`

Proxy binaries. Don't appear to touch bd directly. Out of scope unless
audit reveals otherwise.

**Risk:** unknown — needs audit before Phase 0.

## I. Test infrastructure

### I1. `internal/testutil/`

**Today:**
- Test harness for beads.
- 1 file references beads.

**Target:**
- Add `MaterializerHarness` for integration tests.
- Add `EventCapture` recorder that subscribes to events for test
  assertions.

**Risk:** 🟢

### I2. `internal/beads/bd_subprocess_policy_test.go`

**Today:**
- Forbids `exec.Command("bd", ...)` in deacon/plugin/refinery/witness.

**Target:**
- Extend to all packages by end of Phase 1.
- Eventually flips to "no direct mutations via bd" — only reads
  allowed.

**Risk:** 🟢

## J. Memory & user-facing tools

### J1. `bd remember` / `bd forget` (memory subsystem)

`Remember` and `Forget` events emit `memory.*` events. `bd recall` is a
read, unchanged.

Already in event type catalog.

**Risk:** 🟢

### J2. `gt remember` / `gt forget`

CLI wrappers. Just call the same backend. No special handling.

**Risk:** 🟢

## K. Skills (shell-script-based)

The following skills (in `~/.claude/skills/` or rig-local) call bd
directly via shell:

- `backup` — bd export + sync. Read-only relative to state. ☑️ no change.
- `reaper` — bd close stale beads. Migrate to `gt reaper` command that
  uses producer.
- `patrol` — runs patrol checks. Already calls into internal/witness.
- `pr-list`, `ghi-list` — GH read tools. No state mutations.
- `crew-commit` — git commit + push. Not state-related.
- `caveman:*` — formatting skills. Not state-related.

**Risk:** 🟢 — skills are loose coupling; update independently.

## Summary table

| Module | Files touching bd | Direct exec | Phase | Risk |
|--------|------------------:|------------:|-------|------|
| internal/cmd | 84 | 35 | 1 | 🟡 |
| internal/doctor | 14 | 11 | 1 (PR 2) | 🟢→🟡 |
| internal/beads | 4 | 0 (wraps) | 0+1+4 | 🟡 |
| internal/polecat | 2 | 1 | 2 | 🔴 |
| internal/witness | 1 | 0 | 1 | 🟡 |
| internal/refinery | 2 | 0 | 1 | 🟡 |
| internal/deacon | (gated) | 0 | 1 | 🟡 |
| internal/doltserver | 2 | 2 | E | 🟡 |
| internal/web | 1 | 1 | 1 | 🟢 |
| internal/mail | 3 | 1 | 1 | 🟢 |
| internal/tui/feed | — | 3 | 1 | 🟢 |
| internal/tui/convoy | — | 1 | 1 | 🟢 |
| internal/convoy | 1 | 1 | 1 | 🟢 |
| internal/rig | 2 | 1 | 1 | 🟢 |
| internal/github | — | 0 | 3 | 🟡 |
| internal/bitbucket | — | 0 | 3 | 🟡 |
| NEW: materializer | — | — | 0 | 🔴 |
| NEW: events/state | — | — | 0 | 🟢 |
| NEW: snapshot | — | — | 1 | 🟢 |

## Sequencing rule

A package CANNOT migrate to producer-only writes until:
1. Materializer (A2) is live + supervised.
2. Producer API (A1) is stable.
3. Dual-write divergence detector test passes for that package's
   mutations in CI for 7+ days.

This rule is what makes Phase 0 mandatory and non-skippable.
