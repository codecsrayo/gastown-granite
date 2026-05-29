# `deploy/dolt/`

Shell scripts that replace the Go `gt` **B3** Dolt SQL-server admin surface:

| Future script | Replaces Go `gt dolt …` | What it does |
|---|---|---|
| `start.sh` | `dolt start` | Launch `dolt sql-server` in `/gt/.dolt-data` (no `--user` — see memory `Dolt server restart`). |
| `stop.sh` | `dolt stop` | Graceful shutdown of the Dolt server. |
| `sql.sh` | `dolt sql` | Wrapper for ad-hoc SQL against the live server. |

The orchestrator (`apps/api/crates/kernel/gt-store-dolt`) connects to Dolt as
a **client** (MySQL wire on `:3307`); it has no business starting the server.
Doc 10 already classifies `gt dolt` as `M (intentional)` for that reason —
this just makes the home explicit.

Skeleton-only in `hq-mc72.12.9`. Implementation: follow-up
`hq-mc72.12.B3.*` beads. See
[`apps/api/docs/13-bootstrap-decision.md`](../../apps/api/docs/13-bootstrap-decision.md).

Important constraints carried over from current ops (see memory
`bd embedded vs server mode`, `Dolt server restart`):
- `bd` defaults to embedded Dolt; the orchestrator uses the TCP server.
  The `.jsonl` export bridges the two — `start.sh` must not disable
  `export.auto`.
- `bd init --reinit-local` silently wipes Dolt data; deploy scripts that touch
  init should always export `jsonl` first and confirm.
