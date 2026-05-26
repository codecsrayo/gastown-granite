# Migration Phases — Plan with Gates

Read `03-GAP-ANALYSIS.md` before this. If any showstopper in §
Recommendation isn't cleared, do not start Phase 0.

## Phase 0 — Foundation (6 weeks)

**Goal:** Build the event log, producer API, materializer daemon, and
testing infrastructure. NO production behavior change. Existing bd path
remains authoritative.

### Deliverables

1. `internal/events/state/` package (G1, G2).
2. `internal/materializer/` package + `cmd/beads-materializer` binary (G3).
3. `trace_id` envelope + dedup (G4).
4. `schema_version` field + handler matrix (G5).
5. `BEADS_USE_EVENTS` feature flag (G13), default OFF.
6. `internal/testutil/DivergenceDetector` (G14).
7. Nightly soak test job in CI (G14).
8. systemd unit + supervision integration (G11).
9. Operational runbook for materializer failures.

### Entry criteria

- All G3, G6, G7, G11 showstopper questions cleared with owners.
- Engineer dedicated (no parallel ownership of unrelated firefighting).
- 2 weeks slack budget on top of estimate.

### Exit criteria

- Materializer runs in dev for 7+ days with synthetic events, zero
  data loss, zero divergence (events → dolt matches direct dolt
  writes).
- Soak test passes nightly.
- Divergence detector integrated in PR CI.
- Runbook reviewed by ops owner.

### Rollback

Trivial — Phase 0 is additive. Disable systemd unit, ignore the new
package, no production impact.

### Risk

🟢 LOW. Additive only.

---

## Phase 1 — CLI mutations via events (3 weeks)

**Goal:** Flip `internal/beads/` mutating methods to use producer.
Migrate `internal/cmd/`, `internal/doctor/` (already partially via
PR 2), `internal/witness/`, `internal/refinery/`, `internal/deacon/`.

### Deliverables

1. `internal/beads/` mutating methods route through producer when
   `BEADS_USE_EVENTS=1`.
2. PR 2 (original plan): migrate 11 direct exec sites in
   `internal/doctor/` to wrapper. Lands FIRST.
3. Migrate `internal/cmd/` 35 direct exec sites to wrapper or
   producer.
4. Migrate `internal/witness/`, `internal/refinery/`, `internal/deacon/`.
5. Subprocess policy test extended to all migrated packages.
6. `gt events list/replay/tail` commands (G9).
7. Snapshot exporter (G10) replaces auto-export.
8. Observability dashboards live (G12).
9. `gt bd` wrapper command + AGENTS.md update (G7).

### Entry criteria

- Phase 0 exit criteria met.
- Materializer has been running in dev for 14+ days.
- Soak passes 7 consecutive nights.
- Polecats team has signed off on Phase 2 scope (so Phase 1 doesn't
  build toward something we won't be allowed to complete).

### Exit criteria

- All CLI commands work end-to-end via events when flag ON.
- Divergence detector reports zero diffs over 14 days.
- Snapshot exporter producing valid jsonl every 5m.
- Doctor reports materializer healthy.
- Flag flipped to ON by default for a single dev rig for 7 days
  without incident.

### Rollback procedure

1. Set `BEADS_USE_EVENTS=0` env var.
2. Restart agents.
3. All mutations resume direct dolt writes via PR 1 path.
4. Events in flight at flip time: materializer continues to apply
   them; events already applied stay applied. New events stop being
   generated. No data loss.

### Risk

🟡 MEDIUM. Behavior changes for many code paths. Mitigation:
divergence detector + feature flag + dual-write window.

---

## Phase 2 — Polecats (3 weeks + coordination)

**Goal:** Migrate polecat lifecycle mutations to producer.

**This is the one-way door.** Once polecats write via producer, the
direct-write code in polecat session_manager is removed. Rollback
requires re-implementing it.

### Deliverables

1. Polecat session lifecycle events: `agent.claim`, `agent.release`,
   `agent.hook`, `agent.unhook`, status changes.
2. `internal/polecat/session_manager.go` mutations route through
   producer.
3. Bulk ops (target_clean) use `AppendAsync` + `WaitCaughtUp`.
4. Polecat heartbeats stay as today (NOT events — too high frequency).
5. Polecat integration tests assert event sequence.
6. Migration scripts for in-flight polecats at flip time.

### Entry criteria

- Phase 1 exit criteria met.
- Phase 1 live in production for 14+ days at one rig (e.g., gastown
  itself), then 7+ days at all rigs.
- Polecats team RFC signed off.
- Rollback procedure rehearsed (table-top exercise).

### Exit criteria

- All polecat mutations visible in event log.
- Polecat heartbeat latency P99 < 100ms (proxy for "polecats not
  destabilized").
- Witness handlers see no behavior drift.
- Materializer DLQ empty for 7 consecutive days.

### Rollback procedure

**Hard.** Step by step:
1. Stop all polecats gracefully.
2. Re-deploy polecat binary with `BEADS_USE_EVENTS=0` and
   reverted code that writes direct.
3. Reconcile: any events in log but not yet applied to dolt:
   materializer must drain before re-deploy, OR they're lost.
4. Restart polecats.
5. Expect 30-60min of suspended polecat work during transition.

Coordinate with polecat team for rollback exercise BEFORE Phase 2
goes live.

### Risk

🔴 HIGH. One-way door. Polecat team coordination required.
Heartbeats not affected, but every other lifecycle event is.

---

## Phase 3 — External integrations (2 weeks)

**Goal:** Wire GitHub mirror, Slack notifications, iCloud backup,
git versioning to consume from event log.

### Deliverables

1. `internal/github/` subscribes to materializer's applied-event
   stream. Pushes `issue.create`/`close` to GH near-real-time.
2. `internal/bitbucket/` similar pattern.
3. Slack notifications via materializer subscriber.
4. iCloud backup uses snapshot exporter's consistent checkpoints.
5. Git commit hook reads snapshot exporter output (no more flaky
   jsonl).

### Entry criteria

- Phase 2 exit criteria met + 14 days production stable.

### Exit criteria

- GH mirror lag P99 < 60s (vs. current minutes via polling).
- No silent push failures (alerts wired).
- iCloud backups verified consistent (restore test passes).

### Rollback

Each integration independently disable-able. Fall back to current
poll-based mirroring.

### Risk

🟢 LOW. Additive, each integration isolated.

---

## Phase 4 — Cleanup (2 weeks)

**Goal:** Remove dead code from prior phases. Make event-driven the
ONLY path.

### Deliverables

1. Remove PR 1 flock + `forceExportJSONL` from `internal/beads/`.
2. Remove `runWithStdin` mutation branch.
3. Remove embedded dolt mode entirely (bd-side; document as required
   bd flag).
4. Remove `--allow-stale` plumbing (no longer needed).
5. Remove stale agent locks reaper (claims are events now).
6. Remove `bd init --reinit-local` (replaced by `materializer
   rebuild`).
7. Remove dual-write code paths.
8. Remove `BEADS_USE_EVENTS` flag (always on).
9. Update AGENTS.md, CLAUDE.md, all docs.
10. Archive `01-CURRENT-STATE.md` as historical artifact.

### Entry criteria

- Phase 3 stable 30+ days production.
- All rigs migrated.
- No outstanding bugs in materializer DLQ for 14+ days.

### Exit criteria

- `grep -r 'exec.Command.*"bd".*update\|close\|create' internal/` returns
  zero hits.
- `grep -r 'BEADS_USE_EVENTS' internal/` returns zero hits.
- All deuda items D1-D7 closed.

### Rollback

Not feasible. By Phase 4, dual-write infrastructure has been deleted.

### Risk

🟢 LOW. Cleanup, no new behavior. Catches any lingering bugs by
forcing the new path everywhere.

---

## Timeline summary

| Phase | Effort (eng-weeks) | Elapsed (with reviews) | Cumulative |
|-------|--------------------|------------------------|------------|
| 0 | 6 | 8-10 wk | 2-2.5 mo |
| 1 | 3 | 5-6 wk | 3.5-4 mo |
| 2 | 3 + coord | 6-8 wk | 5-6 mo |
| 3 | 2 | 3-4 wk | 6-7 mo |
| 4 | 2 | 3-4 wk | 7-8 mo |

**Total elapsed:** 7-8 months for one engineer with normal interruption
rate.

**Faster path** (two engineers, dedicated): 4-5 months elapsed.

## Decision points (gates)

| Gate | When | Decision | If NO |
|------|------|----------|-------|
| G-0a | Before Phase 0 starts | Operational owner accepts new daemon? | Drop plan, use lighter alternative |
| G-0b | Before Phase 0 starts | Polecats team accepts Phase 2 in principle? | Drop plan (Phase 1 alone is wasted work) |
| G-1a | Before Phase 1 starts | Materializer stable 14d in dev? | Extend Phase 0 |
| G-1b | Before Phase 1 default-on | Divergence zero for 14d? | Extend bake time |
| G-2a | Before Phase 2 starts | Phase 1 stable in prod 14d at gastown rig? | Extend Phase 1 |
| G-2b | Before Phase 2 starts | Polecats RFC signed off? | Halt at Phase 1 indefinitely |
| G-3a | Before Phase 3 starts | Phase 2 stable 14d all rigs? | Extend Phase 2 stabilization |
| G-4a | Before Phase 4 starts | All phases stable 30d? | Defer cleanup |

A NO at G-0a or G-0b is a STOP. Don't bend the plan to fit; the cost
of incomplete migration (two write paths forever) is worse than no
migration.

## Resource requirements

- **Engineering:** 1 senior eng minimum, dedicated. Two parallel
  helps Phase 0 + Phase 2.
- **Ops:** On-call coverage for materializer alerts from Phase 0
  go-live onward.
- **Coordination:** Polecats team (Phase 2), platform team (Phase 0
  for systemd integration).
- **Compute:** Materializer is single-threaded per host; modest CPU
  (<5% under normal load). Memory ~100MB. Disk: event log grows
  ~50KB/day per active rig under current load.

## Communication plan

- Pre-Phase 0: RFC published in `tmp/` folder (these docs).
  Distributed to mayor, polecats, ops. 1-week comment window.
- Phase 0 mid-point: status update + risk reassessment.
- Phase 1 default-on: pre-announcement 48h prior; rollback contact.
- Phase 2 start: explicit go/no-go meeting with polecats team.
- Each phase exit: post-mortem on any incidents, retro on estimates.

## What happens if migration stalls mid-plan

Mid-plan stall = lived-with dual-write or partial-migration state.
- Mid-Phase 0: trivial — back out, no impact.
- Mid-Phase 1: keep `BEADS_USE_EVENTS=0`. CLI still works direct.
  Materializer idle.
- Mid-Phase 2: WORST CASE. Some polecats on producer, others direct.
  Event log incomplete. Must either finish Phase 2 or fully roll
  back.
- Mid-Phase 3: external integrations partially wired. Each
  independently disable-able.
- Mid-Phase 4: cleanup not done. Dead code lingers. Eng cost only.

The risk shape says: complete through Phase 1 or back out before
Phase 2. Don't enter Phase 2 unless committed to finishing it.
