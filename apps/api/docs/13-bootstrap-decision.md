# 13 — Bootstrap relocation: `gt` CLI → `deploy/` scripts

> Status: **DECIDED** — 2026-05-29, bead `hq-mc72.12.9` (epic `hq-mc72.12`, Paso 10).
>
> Scope: the three groups of Go `gt` subcommands that doc 11 lists as **Phase 3**
> work — they cannot stay as `gt` subcommands once the Go binary is deleted.
> This doc records where each group goes and why.

---

## Decision

**Relocate B1 + B2 + B3 to `deploy/` shell scripts. Do NOT port them into
`gt-cli` (Rust).**

| Group | Group name | Go commands | New home |
|---|---|---|---|
| **B1** | filesystem bootstrap | `gt install` · `gt init` · `gt git-init` · `gt config` · `gt theme` · `gt status-line` · `gt upgrade` · `gt uninstall` · `gt stale` · `gt hooks` | `deploy/bootstrap/` |
| **B2** | tmux + process plumbing | `gt cycle` · `gt peek` · `gt cleanup` · `gt orphans` | `deploy/tmux/` |
| **B3** | Dolt server admin | `gt dolt {start, stop, sql}` | `deploy/dolt/` |

Each new home gets one shell script per Go command (or a small bundle), to be
landed in follow-up beads — this bead ships the **decision + the directory
skeleton**, not the script ports themselves.

The rig-creation surface (the retired `rig.create` MCP tool, Paso 10 D2) is
also B1: `gt rig add` belongs in `deploy/bootstrap/` (or a thin `gt-cli`
wrapper that shells to it). See [`docs/10-go-rust-parity.md`](10-go-rust-parity.md).

---

## Rationale

1. **These are infra concerns, not orchestrator logic.** B1 sets up
   filesystems and edits user config; B2 manages host processes; B3 controls a
   database server. None of them mutate domain state, observe events, or speak
   to the actor topology — they are *environment* operations.

2. **Doc 10 already classifies most of them as "intentional non-orchestrator"**
   (the `(intentional)` rows in §4 and §8). The Go binary carried them for
   convenience; the right cutover move is to honor that classification by
   physically separating the code, not re-import it into Rust.

3. **A Rust port would be anti-leverage.** `gt install` is hundreds of lines
   of `git clone` + `mkdir` + `cp` + JSON edits. Reimplementing it in Rust to
   then shell out to `git`/`mkdir`/`sed` gives nothing back — and adds a
   compile cycle to every change. Shell is the right tool for this.

4. **Deploy/install scripts belong in the repo's deploy surface.** That
   directory exists for exactly this kind of host-side glue, and it is
   independently revisable by ops without touching the orchestrator.

5. **gt-cli stays thin.** Doc 11 §Phase 9.A frames `gt-cli` as a thin wrapper
   over HTTP/MCP. Pulling install/process/db-server code in would invert that.

---

## What ships with this bead

- This document (`apps/api/docs/13-bootstrap-decision.md`).
- The `deploy/` tree skeleton at the repo root:
  - `deploy/README.md` — what `deploy/` is for, what's in each subdir.
  - `deploy/bootstrap/README.md` — B1 landing zone.
  - `deploy/tmux/README.md` — B2 landing zone.
  - `deploy/dolt/README.md` — B3 landing zone.
- Cross-links from `docs/10-go-rust-parity.md` (the "intentional" rows now
  point at this doc) and `docs/11-cutover-roadmap.md`.

## What does NOT ship with this bead

- The actual shell scripts for any of B1 / B2 / B3. Each is its own
  follow-up bead under `hq-mc72.12`, sized once the rest of Paso 10 stabilizes.
- Any change to `gt-cli` or `gt-mcp` — the decision is purely about *where the
  next code lands*.

---

## Follow-up beads (to file when starting the actual port)

- `hq-mc72.12.B1.x` — one bead per `gt install` / `gt init` / etc., each
  landing the script + a smoke test.
- `hq-mc72.12.B2.x` — `cycle` / `peek` / `cleanup` / `orphans` as
  tmux-wrapper scripts.
- `hq-mc72.12.B3.x` — `dolt start|stop|sql` wrapper that respects the existing
  embedded vs server-mode split (see memory `bd embedded vs server mode`).

---

## Open questions deferred to the follow-up beads

- Whether to keep a thin `gt-cli` wrapper that shells to the deploy scripts
  (so users can still type `gt install`), or require operators to invoke the
  scripts directly. Lean: keep the wrapper for muscle-memory, but make it a
  one-liner exec — not a port.
- Hook installation (`gt hooks`) currently writes Claude Code hook JSON.
  Whether that stays in shell or moves to a Rust subcommand of `gt-cli`
  depends on how often the hook schema evolves. Default: shell, until the
  evolution rate justifies otherwise.
