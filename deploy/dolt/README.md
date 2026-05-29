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

## Shipped (hq-mc72.12.20 — lifecycle, sandbox-tested)

| Script | Replaces Go `gt dolt …` | What it does |
|---|---|---|
| `start.sh` | `dolt start` | Write a managed `config.yaml` (faithful to `doltserver.writeServerConfig`: warning log, 127.0.0.1, 1000 conns, 5-min timeouts, event-scheduler/stats off, auto-gc off), clean a stale socket, refuse a foreign listener (no kill), launch `dolt sql-server --config …` detached → `daemon/dolt.log`, pid → `daemon/dolt.pid`, wait for the port. |
| `stop.sh` | `dolt stop` | SIGTERM the recorded pid, wait, clear the pidfile. |

`start.sh`/`stop.sh` were tested on a throwaway server (`GT_DOLT_PORT=3399`,
temp `GT_DOLT_DATA_DIR`) — never against the shared live `:3307`.

## Shipped (hq-mc72.12.22 — admin)

| Script | Replaces Go `gt dolt …` | What it does |
|---|---|---|
| `kill-imposters.sh` | `dolt kill` (imposter) | SIGTERM a **foreign** dolt holding our port (identity via `/proc` `--data-dir`/`--config`/cwd vs our data dir — our own server is never touched). `--dry-run` previews. |
| `restart.sh` | `dolt restart` | `stop.sh` then `start.sh`. Non-destructive: refuses + points at `kill-imposters.sh` if a foreign dolt still holds the port. |

Imposter identity mirrors Go `doltProcessMatchesTownPaths`. Sandbox-tested
against a throwaway foreign server on a non-live port.

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
