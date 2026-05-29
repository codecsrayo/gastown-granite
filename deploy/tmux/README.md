# `deploy/tmux/`

Shell scripts that replace the Go `gt` **B2** tmux + host-process surface.

## Shipped (hq-mc72.12.19 — read-only)

| Script | Replaces Go `gt …` | What it does |
|---|---|---|
| `lib.sh` | (shared) | Resolve town root + the per-town tmux socket (`<basename>-<sha256(path)[:6]>`, mirrors Go `townSocketName`; `GT_TMUX_SOCKET` overrides). |
| `peek.sh` | `peek` | `tmux capture-pane` against a session. Town-agent shorthand (`mayor`/`deacon`/`boot`/`overseer` → `hq-*`); else a literal session name. `[lines]` (default 200) or `--all`. |

## Pending (interactive / destructive — separate follow-up beads)

| Script | Replaces | Why deferred |
|---|---|---|
| `cycle.sh` | `cycle` | Switches the attached tmux client between group sessions — interactive/stateful, not meaningfully testable headless. |
| `cleanup.sh` | `cleanup` | **Kills** orphaned Claude processes — destructive; needs careful guards + a sandbox. |
| `orphans.sh` | `orphans` (report) / `orphans kill` | The bare report is portable; the `kill` subcommand is destructive. Split when ported. |

`peek.sh` resolves the same per-town tmux socket the orchestrator uses, so it
reaches the live sessions. See
[`apps/api/docs/13-bootstrap-decision.md`](../../apps/api/docs/13-bootstrap-decision.md).
