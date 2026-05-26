# Recommendations — Pragmatic Path

**Status:** Counter-proposal to the full event-driven migration in
`02-TARGET-ARCHITECTURE.md`.
**Author:** review session 2026-05-26.
**Verdict on full migration:** Defer. ROI is weak (D9 admits this in
`06-OPEN-QUESTIONS.md` Q20). The plan below kills ~95% of the pain in
~5% of the time.

## TL;DR

Skip the materializer daemon, the producer API, and the new state-event
log. Use what already exists better. Three changes tomorrow resolve
most of D1–D7. Twelve weeks of work resolve the rest. No SPOF added.

## The three changes for tomorrow

| # | Change | Cost | Closes |
|---|--------|------|--------|
| 1 | Reaper of stale `agent-*.lock` files | 1 day | D4 |
| 2 | PR 2 for `internal/doctor/` (25 sites) | 1–2 days | D1 in doctor surface |
| 3 | Server bus by default in CLI | 1 day | D3 |

After these three, the bulk of the race class is gone without any new
daemon, socket, or schema.

---

## Tier 1 — Quick wins (days, this week)

### Q1. Stale-lock reaper (D4)

**State today:** 27 of 28 `<townRoot>/.beads/.locks/agent-*.lock` files
are >48h stale (26 in `gastown-sandbox`, 2 in `gastown`). Nothing reaps
them. Claim-via-file is un-self-cleaning.

**Fix:** cron handler in `internal/witness/` (patrol pattern already
exists): `lstat` each lock, delete if `mtime < now - 24h`. Zero risk —
an active polecat recreates its lock on next claim.

**Cost:** 1 day.

### Q2. Doctor check: zero-byte jsonl (D7)

**State today:** `internal/doctor/jsonl_bloat_check.go` only catches
"bloat". hq-i7q burned 217 issues silently when jsonl went to 0 bytes.

**Fix:** `os.Stat()` before the bloat block; fail with
`severity=critical` if size == 0. Single file edit.

**Cost:** 1 hour.

### Q3. `bd init --reinit-local` explicit confirm (D6)

**State today:** Destroys data with no prompt. The
`backups-pre-reinit/` directory exists because of this.

**Fix:** wrap in `internal/cmd/` with mandatory
`--yes-i-have-a-backup` flag. Refuse otherwise.

**Cost:** 1 hour.

### Q4. Land PR 1 (jsonl flock)

**State today:** Branch `refactor/bd-jsonl-lock-race` referenced in the
migration docs but not on `main`. `internal/beads/exec.go` has no
flock around mutating subprocess calls. Pattern is already proven in
`internal/beads/beads_agent.go:13` (`gofrs/flock`).

**Fix:** lift the agent flock pattern around the mutation path in
`exec.go`. Read-only commands stay unlocked.

**Cost:** done if the branch exists; ~1 day if it must be rewritten.

---

## Tier 2 — PR 2: delete the bypass (sprint 1)

Migrate the 153 direct `exec.Command("bd", ...)` sites outside
`internal/beads/` to the wrapper. After this, PR 1's flock covers
everything.

### Order (highest leverage first)

| Package | Sites | Files | Approach |
|---------|------:|------:|----------|
| `internal/cmd/` | 93 | 35 | Batch by subcommand family (hook, sling, close, …). One small PR each. |
| `internal/doctor/` | 25 | 11 | Add `QuerySQL`/`QueryJSON`/`ExecSQL` helpers to wrapper first, then mechanical refactor. |
| `internal/doltserver/` | 4 | — | One PR. |
| `internal/rig/` | 12 | 1 | All in `manager.go` (mostly around `bd init --reinit-local` path L635). One PR. |
| `internal/tui/feed/` | 5 | 3 | `convoy.go:2`, `convoy_issues.go:2`, `events.go:1`. One PR. |
| `internal/doltserver/` | 4 | — | One PR. |
| `internal/testutil/` | 3 | — | Test helpers — convert to wrapper if migration must be policy-pure; otherwise leave. |
| `internal/tui/convoy/` | 3 | 1 | `model.go`. One PR. |
| `internal/mail/` | 2 | — | One PR. |
| `internal/web/` | 2 | — | One PR. |
| `internal/polecat/` | 2 | 1 | `session_manager.go` `validateIssue` (`bd show`) + `hookIssue` (`bd update --status=hooked`). Mechanical. |
| `internal/convoy/`, `internal/deps/` | 1 each | — | Roll into adjacent PRs. |

**Total bypass: 153** (matches grep across `exec.Command("bd"` +
`exec.CommandContext(_, "bd"` outside `internal/beads/`).

### Supporting work

- Extend `internal/beads/bd_subprocess_policy_test.go` to gate each
  package as it migrates.
- Add a `forbidigo` lint rule: `exec.Command.*"bd"` is forbidden
  outside `internal/beads/`.
- Each PR runs a divergence sanity check (compare wrapper output vs
  raw `bd` for the migrated subcommand on a fixture corpus).

**Cost:** 2–3 weeks elapsed for one engineer working in PR-sized
chunks.

---

## Tier 3 — Lean on what exists (sprint 2)

### T1. Server bus by default

**State today:** Embedded mode and TCP server coexist; clients
arbitrarily pick one. D3 race surfaces here.

**Fix:** `internal/beads/exec.go` checks `<beadsDir>/dolt-server.port`.
If present, pass `--server-port` to the bd subprocess. Embedded becomes
explicit fallback. Dolt MVCC inside the server handles serialization.

**Cost:** 1 day implementation + 1 sprint of soak under real load.

### T2. Telemetry coverage audit

**State today:** `internal/telemetry/RecordBDCall` exists. Suspected
that only the wrapper records; the 153 bypass sites do not.

**Fix:** after PR 2 lands, all calls flow through the wrapper →
telemetry is automatic. Add P50/P99 latency dashboard per subcommand
+ error rate. This is the observability D12 asks for, free.

**Cost:** dashboard ~3 days. Auto-coverage is a side effect of PR 2.

### T3. Agent-claim semantics with TTL

**State today:** `internal/beads/beads_agent.go` flock + file. File
persists post-crash → D4.

**Two options:**
- A: TTL in the lock file contents (`{pid, started, expires}`); reaper
  compares.
- B: fcntl record locks via `golang.org/x/sys/unix`. Auto-release on
  process exit. Structurally correct.

**Recommendation:** B. Removes the reaper dependency entirely.

**Cost:** 2–3 days.

---

## Tier 4 — Audit/replay without rewriting (month)

### M1. Query existing logs, don't write new ones

**Already on disk:**
- `<townRoot>/.events.jsonl` (~659KB in `gastown-sandbox`, ~25KB in
  `gastown` hq): sling, hook, handoff, etc.
- `<townRoot>/.beads/interactions.jsonl`: per-actor command history
  (~305KB in `gastown-sandbox`, **404 bytes in `gastown` hq — verify
  capture is wired before relying on it**).

**Build:** `gt events query --actor X --since Y --type Z` over those
two files. No new log, no materializer.

**Push `internal/feed/curator.go`** to also curate state-relevant
events from `interactions.jsonl`. The tail-and-process daemon pattern
already exists — reuse it.

**Pre-req:** audit why `gastown/.beads/interactions.jsonl` is near-empty
while sandbox has 305KB. If hq-side bd writes bypass the log, M1's
query coverage is sandbox-only until fixed.

**Cost:** 3–5 days.

### M2. Snapshot exporter (atomic)

**State today:** bd's auto-export is throttled and racy.
`internal/atomicfile/` already exists.

**Fix:** new `internal/snapshot/Exporter`: reads dolt, writes tempfile,
renames. Runs every 5m + on-demand. Coordinates nothing — dolt is
already the only writer if T1 lands.

**Cost:** 2–3 days.

---

## What to NOT do (from the full migration plan)

| Item | Why skip |
|------|----------|
| Materializer daemon | Adds per-host SPOF without on-call coverage (G11 admits this). Operational debt for marginal benefit. |
| Producer API (`AppendAndWait` / `AppendAsync`) | New API surface with no current caller demand. |
| `state-events.jsonl` (new log) | Duplicates `events.jsonl` + `interactions.jsonl`. Use what's there. |
| systemd unit | Gas Town town processes don't use systemd. Breaking that precedent costs more than it saves. |
| `schema_version` + handler matrix | Permanent maintenance tax on every event-schema change. No payback under current scale. |
| `BEADS_USE_EVENTS` feature flag | Only needed if the migration happens. |
| Phase 2 polecat migration coordination | Polecats are ephemeral agents — there is no "polecats team" to RFC with. The 2 exec sites in `session_manager.go` are mechanical (Tier 2). |

---

## Mapping deltas (D1–D10) to this plan

| Debt | Closed by |
|------|-----------|
| D1 jsonl race | Q4 (PR 1 flock) + Tier 2 (PR 2 across all bypass) |
| D2 auto-import wipe | T1 (server bus by default — embedded mode unused) |
| D3 embedded/server split | T1 |
| D4 stale agent locks | Q1 (reaper) + T3 (fcntl) |
| D5 events.jsonl unlocked | Out of scope — bd-internal; surface to upstream as separate issue |
| D6 reinit destructive | Q3 (confirm flag) |
| D7 doctor 0-byte | Q2 |
| D8 historical loss | Not recoverable. Q3 + Q2 prevent recurrence. |
| D9 no replayable audit | M1 (query existing logs) — partial; full replay deferred until justified |
| D10 bd upstream divergence | No change — accept the dependency, wrap don't fork |

D9 is the only delta this plan does NOT fully close. That is the
intentional trade — replayable state requires the full migration; the
audit-query partial covers ~80% of the actual debugging use cases for
~5% of the cost.

---

## Effort summary

| Tier | Elapsed | What's gained |
|------|---------|---------------|
| 1 (Q1–Q4) | ~1 week | D4, D6, D7, D1 partial |
| 2 (PR 2) | 2–3 weeks | D1 fully closed everywhere |
| 3 (T1–T3) | 1–2 weeks | D3 closed; D4 fully closed; observability |
| 4 (M1–M2) | 1–2 weeks | D9 partial; snapshot exporter |
| **Total** | **6–8 weeks** | D1, D3, D4, D6, D7 closed; D9 partial; D8 prevented from recurring |

Compare against full migration: 7–8 **months** for the same delta
coverage plus the (unjustified) full audit-replay capability.

---

## Decision trigger for revisiting the full migration

This plan is correct for current scale. The full event-driven plan
becomes the right call when **any** of the following becomes true:

1. >3 race incidents per month sustained over a quarter (today: ~1–2
   per quarter).
2. An external consumer contractually requires a replayable audit
   trail (today: none).
3. State-write throughput exceeds ~100 events/sec sustained (today:
   well under).
4. Multi-host federation moves from "nice to have" to a committed
   roadmap item (today: deferred per `01-CURRENT-STATE.md` §8).

Until then, keep the planning docs (00–06) as background. Execute
this one.
