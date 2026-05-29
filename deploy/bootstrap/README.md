# `deploy/bootstrap/`

Shell scripts that replace the Go `gt` **B1** bootstrap surface:

| Future script | Replaces Go `gt …` | What it does |
|---|---|---|
| `install.sh` | `install` | Create an HQ workspace from scratch (town root, settings, hooks). |
| `init.sh` | `init` | Initialize the cwd as a rig (bead prefix, dirs, scaffolding). |
| `git-init.sh` | `git-init` | Init git in a rig. |
| `rig-add.sh` | `rig add` (was `rig.create` MCP, retired Paso 10 D2) | Clone a repo + scaffold `refinery/rig`, `mayor/rig`, `witness/`, `.repo.git`, `.beads`. |
| `config.sh` | `config` | Manage configuration files. |
| `theme.sh` | `theme` | UI theme switcher (mostly status-line + TUI colors). |
| `status-line.sh` | `status-line` | tmux status-line generator. |
| `upgrade.sh` | `upgrade` | Post-install migration. |
| `uninstall.sh` | `uninstall` | Tear down. |
| `stale.sh` | `stale` | Binary version freshness check. |
| `hooks-install.sh` | `hooks` | Install Claude Code hook JSON. |

None of the above ship in the skeleton bead (`hq-mc72.12.9`). Each is a
follow-up `hq-mc72.12.B1.*` to be filed when the actual port begins. The
decision rationale lives in
[`apps/api/docs/13-bootstrap-decision.md`](../../apps/api/docs/13-bootstrap-decision.md).
