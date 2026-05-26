# Event-Driven Architecture Migration — Planning Documents

**Status:** Planning. No code yet.
**Goal:** Evaluate gaps before committing to a multi-month rewrite.
**Author:** session 2026-05-26, branch `refactor/bd-jsonl-lock-race`
**Audience:** Mayor, polecats team, anyone owning Gas Town infra

## Why these docs exist

PR 1 (committed on `refactor/bd-jsonl-lock-race`) added a flock around bd
mutations to defeat the jsonl auto-export throttle race. That's a tactical
fix. The strategic question is whether to convert the whole codebase to
event-driven state so the race class disappears by construction — and to
gain audit trail / replay / time-travel debugging in the bargain.

User asked: "puedes crear los documentos que sean necesarios — la idea es
planificar antes de codear para evaluar gaps."

These docs are that plan. They are NOT a green light to start coding. They
are the artifact a future engineer (or this same engineer in next session)
reads to decide:
- Whether the rewrite is worth doing at all
- Which phases are reversible vs. one-way doors
- Where the unknowns are
- What the operational cost actually is

## Reading order

1. **`01-CURRENT-STATE.md`** — what exists today, drawn from the codebase.
   Read this first. The codebase already has 3 event-adjacent systems; the
   target is partially built. Knowing this changes the proposal.
2. **`02-TARGET-ARCHITECTURE.md`** — the end state. Components, schema,
   consistency model, failure handling.
3. **`03-GAP-ANALYSIS.md`** — what's missing between (1) and (2). The
   gating document. If a gap is unfillable, the whole plan stops here.
4. **`04-MODULE-INVENTORY.md`** — per-module change list. The execution
   spec for phases 1-4. ~70 internal packages catalogued.
5. **`05-MIGRATION-PHASES.md`** — phased plan with gates, rollback,
   timeline, headcount.
6. **`06-OPEN-QUESTIONS.md`** — design decisions not yet resolved.
   Need RFC discussion before Phase 0 closes.

## TL;DR for the impatient

- Existing `internal/events/` is audit-only, NOT mutation-authoritative.
- Existing `internal/bus/` is in-process only, unused outside one package.
- Existing `internal/channelevents/` is file-based RPC for inter-agent
  signals — not a state event log.
- To go event-driven for STATE, we need: (a) extend events package with
  mutation event types, (b) write a materializer daemon that applies events
  to dolt, (c) producer API with sync/async semantics, (d) migrate
  ~14 packages that touch bd directly today.
- Estimate: 7-9 engineer-weeks, plus operational supervision overhead for
  the new daemon, plus polecats team coordination for Phase 2.
- Risk: Phase 2 (polecats migration) is the one-way door. Everything
  before is reversible.
- Alternative: PR 2 (flat migration of doctor to existing wrapper) +
  server bus (`bd --server-port`) covers ~95% of the race risk in 1
  sprint, ~5% of the rewrite cost. See `02-TARGET-ARCHITECTURE.md`
  appendix for the alternative.

## Document status

| Doc | Status | Last edited |
|-----|--------|-------------|
| 00-README | DRAFT | 2026-05-26 |
| 01-CURRENT-STATE | DRAFT | 2026-05-26 |
| 02-TARGET-ARCHITECTURE | DRAFT | 2026-05-26 |
| 03-GAP-ANALYSIS | DRAFT | 2026-05-26 |
| 04-MODULE-INVENTORY | DRAFT | 2026-05-26 |
| 05-MIGRATION-PHASES | DRAFT | 2026-05-26 |
| 06-OPEN-QUESTIONS | DRAFT | 2026-05-26 |

All docs are DRAFT — no plan element should be considered approved until
the open questions in `06-OPEN-QUESTIONS.md` are resolved with the
owning teams (mayor for cross-cutting, polecats for Phase 2 specifically).
