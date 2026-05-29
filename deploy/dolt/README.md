# `deploy/dolt/`

Shell scripts that replace the Go `gt` **B3** Dolt SQL-server admin surface.

## Shipped (hq-mc72.12.17 — read-only ops)

| Script | Replaces Go `gt dolt …` | What it does |
|---|---|---|
| `lib.sh` | (shared) | Resolve town root (`mayor/town.json`), data dir, host/port (`GT_DOLT_HOST`/`GT_DOLT_PORT`, default `127.0.0.1:3307`). |
| `status.sh` | `dolt status` (core) | Detect a listener on the Dolt port and list served databases. Exit 0 up / 1 down. |
| `sql.sh` | `dolt sql` | Forward args verbatim to `dolt --host H --port P --no-tls sql …` — always TCP to the live shared server, never embedded mode. |

Verified invocation form for this Dolt build: `--host`/`--port`/`--no-tls` are
**global** args placed *before* the `sql` subcommand.

## Pending (data-critical — separate follow-up bead)

| Script | Replaces | Why deferred |
|---|---|---|
| `start.sh` | `dolt start` | Needs the `config.yaml` (`writeServerConfig`: timeouts, port) + stale-socket cleanup + port-conflict/imposter detection ported from `internal/doltserver`, and a sandbox to verify — it cannot be tested against the shared live `:3307` without risking every other agent (split-brain). |
| `stop.sh` / `restart.sh` / `kill-imposters.sh` | `dolt stop` / `restart` / kill | Same: process-lifecycle on the shared data store. |

The orchestrator (`apps/api/crates/kernel/gt-store-dolt`) connects to Dolt as
a **client** (MySQL wire on `:3307`); it has no business starting the server.
See [`apps/api/docs/13-bootstrap-decision.md`](../../apps/api/docs/13-bootstrap-decision.md).

Important constraints carried over from current ops (see memory
`bd embedded vs server mode`, `Dolt server restart`):
- `bd` defaults to embedded Dolt; the orchestrator uses the TCP server.
  The `.jsonl` export bridges the two — `start.sh` must not disable
  `export.auto`.
- `bd init --reinit-local` silently wipes Dolt data; deploy scripts that touch
  init should always export `jsonl` first and confirm.
