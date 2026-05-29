# 13 — Operator signals (hq-mysw, Paso 9.F)

Three pieces that turn the audit log / feed into operator-facing signals: a read-side
**activity log**, an **escalation action**, and a **Notifier PORT** with a real bead-backed
mail adapter. Everything stays on the established rails — pure read-side over the log, the
cross-domain action at the composition root, ports & adapters for I/O.

## 1. Activity log (read-side)

Port of the Go `internal/activity` color-coding onto the type-erased feed. Two read-sides,
one threshold source:

- **In-memory** — `gt_feed::activity` (`activity_view(&FeedState, now_secs) -> Vec<ActivityRow>`).
  The calculator (`ActivityInfo::from_age`) is pure and clock-free; the feed curator never reads
  a clock, so `now_secs` is supplied by the edge. Each correlation lifeline is color-coded from
  its `last_ts`.
- **SQL** — `gt_store_pg::PgActivity` over the `activity_projections` table (one row per
  correlation: `last_activity_secs`, `last_kind`). The outbox drain upserts it per event,
  keeping the MAX timestamp (`GREATEST`). `PgActivity` reuses `gt_feed::activity` for the color
  calc, so the SQL read-side and the in-memory view share **one** set of thresholds.

Thresholds (mirror the Go defaults; operators override via `worker_status` config at the edge):

| Color | Age | Meaning |
|-------|-----|---------|
| `green`   | < 5m       | active |
| `yellow`  | 5m–10m     | stale |
| `red`     | ≥ 10m      | **stuck** — the signal the escalation path keys on |
| `unknown` | no timestamp | no activity data |

Gate (`gt-store-pg/tests/activity_contract.rs`, `GT_PG_URL`): the SQL projection color-codes
each correlation identically to `activity_view` over the same log at the same `now`. On the
ordered append-only log they agree; the PG side additionally survives out-of-order redelivery
(`GREATEST`), which the in-memory `last_ts` (last-ingested) does not model.

## 2. Escalation action

The feed already detects semantic gaps (`gt_feed::FeedProblem`: `TimeoutMissed`,
`DeadLetterDrain`, `UnhandledEvent`). `gt_feed::escalation::intents` is the pure decision of
**which** gaps warrant escalation (stays kernel-only — it does not know the Notifier or beads).

The live action is a reaction at the composition root (`bins/gt/src/root.rs::Reactor::escalate`).
For a stuck signal it:

1. Creates a durable **status bead** (`escalation-<subject>`, `BeadStatus::Failed`, P0) so the
   gap survives a restart and surfaces in any operator panel. Failed (not Pending) keeps the
   scheduler from ever treating the alert as claimable work.
2. Routes the signal (see §4) and, when it warrants mail, pushes a `Notification` through the
   injected `Notifier`.

Triggers (wired via the role/domain events — "wired via Witness/Deacon"):

| Event | Signal | Status bead? |
|-------|--------|--------------|
| `witness.escalation_raised` | `WorkerStuck` | yes (`escalation-<worker>`) |
| `merge.failed` | `MergeStuck` | yes (`escalation-merge-<bead>`) |
| `quota.account_limited` / `quota.block_predicted` / `quota.blocked` | `QuotaBlock` | no — rotation already created the corrective action |

## 3. Notifier PORT

`gt-notify` (a domain crate) owns the port and nothing else: `Notification` (+ `Severity`,
`Signal`), the `Notifier` trait, the routing policy, and a `FakeNotifier` for tests. **No mail /
transport crate is a dependency** — that is the point of the port. The port method is sync and
fire-and-forget, mirroring `gt_root::Effects` (`sling`/`rotate`): the single-writer reactor loop
must not block on SMTP/webhook latency, and the escalation's status bead is the durable record
regardless.

Adapters live at the edge (`bins/gt`):

- **Real** — `MailNotifier`: in Gas Town mail *is* beads (the Go `internal/mail` translates
  messages to/from beads), so each notification becomes a mail bead via the `BeadRepository`.
  No SMTP/webhook dependency; the durable bead is the message and `gt mail` reads it.
  Fire-and-forget `tokio::spawn` for the upsert; addresses via `GT_MAIL_FROM` / `GT_MAIL_TO`.
- **Fake** — `gt_notify::FakeNotifier`: captures notifications in order for the gate.
- **Default** — `gt_root::LogNotifier`: stderr only (no mail until the bin wires `MailNotifier`).

Injected through `RootConfig::notifier` (`Arc<dyn Notifier>`), exactly like
`RootConfig::keychain`.

## 4. Which signals warrant mail vs feed-only

The audit log + feed already record **everything**. A notification is the extra step of pushing
a signal at a human. Rule of thumb: notify only when a human must act and the runtime cannot
self-heal. Routine, self-recovering, or purely informational gaps stay feed-only so the
operator's inbox does not become noise.

The policy is `gt_notify::route(&Signal) -> Channel` — defined over the `Signal` enum so adding
a signal forces a compile-time routing decision.

| Signal | Channel | Why |
|--------|---------|-----|
| `WorkerStuck` (escalation) | **Mail** | the runtime cannot un-stick a worker on its own |
| `MergeStuck` | **Mail** | a failed/stuck merge blocks the lane until someone resolves it |
| `QuotaBlock` | **Mail** | rotation self-heals capacity, but a human should know an account went dark |
| feed gaps that self-recover (a failure marker followed by a later success) | feed-only | `gt_feed::detect` already stays quiet on recovery |
| `UnhandledEvent` (published with no subscriber) | feed-only | a wiring smell for the TUI, not an operator page (`escalation::intents` skips it) |
| activity `yellow` (stale) | feed-only | only `red`/stuck escalates |

Gate (`bins/gt/tests/escalation.rs`): a synthetic `*Stuck` — driven through the Witness, the
merge lane, and the quota domain — reaches the `FakeNotifier`, and the stuck escalations leave a
status bead; quota-block is notification-only.
