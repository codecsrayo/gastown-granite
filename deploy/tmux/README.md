# `deploy/tmux/`

Shell scripts that replace the Go `gt` **B2** tmux + host-process surface:

| Future script | Replaces Go `gt …` | What it does |
|---|---|---|
| `cycle.sh` | `cycle` | Cycle between sessions in a tmux group (C-b n/p semantics). |
| `peek.sh` | `peek` | `tmux capture-pane` against a polecat/crew session. |
| `cleanup.sh` | `cleanup` | Reap orphaned Claude processes. |
| `orphans.sh` | `orphans` | Report tmux sessions / PIDs with no matching DB row. |

These are pure tmux + process-list operations: shell + `tmux` + `pgrep` is the
right tool, and putting them under `deploy/` keeps the orchestrator binary
free of process-management code.

Skeleton-only in `hq-mc72.12.9`. Implementation: follow-up
`hq-mc72.12.B2.*` beads. See
[`apps/api/docs/13-bootstrap-decision.md`](../../apps/api/docs/13-bootstrap-decision.md).
