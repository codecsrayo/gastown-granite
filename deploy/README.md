# `deploy/`

Host-side glue for Gas Town: install, process plumbing, database server admin.

This tree owns the shell-script side of operations that were historically
`gt <verb>` Go subcommands but **do not belong in the orchestrator binary**.
The decision and the full mapping live in
[`apps/api/docs/13-bootstrap-decision.md`](../apps/api/docs/13-bootstrap-decision.md)
(Paso 10 B1/B2/B3).

## Subdirs

| Path | Houses | Replaces (Go `gt …`) |
|---|---|---|
| [`bootstrap/`](bootstrap/) | filesystem install / init / config / theme / hooks / upgrade / uninstall / stale | B1 — `install`, `init`, `git-init`, `config`, `theme`, `status-line`, `upgrade`, `uninstall`, `stale`, `hooks` (+ `rig add` per D2) |
| [`tmux/`](tmux/) | tmux + host-process plumbing | B2 — `cycle`, `peek`, `cleanup`, `orphans` |
| [`dolt/`](dolt/) | Dolt SQL server admin | B3 — `dolt {start, stop, sql}` |

Everything that does mutate domain state — sessions, beads, merge slots,
quotas — stays in the Rust orchestrator (`apps/api`) and is reachable via
`gt-cli`, `gt-mcp`, or `gt-web`.

## State

Only READMEs ship in the bead that created this skeleton (`hq-mc72.12.9`).
The actual scripts land in follow-up beads — one per Go command — sized once
the rest of Paso 10 stabilizes (see the parent epic `hq-mc72.12`).
