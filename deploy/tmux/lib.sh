#!/usr/bin/env bash
# deploy/tmux/lib.sh — shared resolution for the deploy/tmux scripts.
#
# Resolves the per-town tmux socket so these scripts talk to the same tmux server the
# orchestrator spawns sessions on. Paso 10 B2 (hq-mc72.12.19).
set -euo pipefail

# Walk up from $1 (default cwd) for the town-root marker `mayor/town.json`; outermost match
# wins (mirrors Go `beads.FindTownRoot`).
gt_find_town_root() {
  local dir candidate=""
  dir="$(cd "${1:-$PWD}" && pwd)"
  while :; do
    [ -f "$dir/mayor/town.json" ] && candidate="$dir"
    local parent
    parent="$(dirname "$dir")"
    [ "$parent" = "$dir" ] && break
    dir="$parent"
  done
  [ -n "$candidate" ] && printf '%s\n' "$candidate"
}

# Sanitize a town basename the way Go `sanitizeTownName` does: lowercase, non-alphanumeric
# → hyphen, trim leading/trailing hyphens.
gt_sanitize_name() {
  printf '%s' "$1" \
    | tr '[:upper:]' '[:lower:]' \
    | sed -e 's/[^a-z0-9]/-/g' -e 's/^-*//' -e 's/-*$//'
}

# Derive the town tmux socket name: "<sanitized-basename>-<sha256(canonical-path)[:6]>"
# (mirrors Go `session.townSocketName`). Verified: /gt → gt-e7bf91. GT_TMUX_SOCKET overrides.
gt_tmux_socket() {
  if [ -n "${GT_TMUX_SOCKET:-}" ]; then
    printf '%s\n' "$GT_TMUX_SOCKET"
    return 0
  fi
  local town canonical base hash
  town="${1:?town root required}"
  canonical="$(realpath "$town" 2>/dev/null || printf '%s' "$town")"
  base="$(gt_sanitize_name "$(basename "$town")")"
  hash="$(printf '%s' "$canonical" | sha256sum | cut -c1-6)"
  printf '%s-%s\n' "$base" "$hash"
}

TOWN_ROOT="${GT_TOWN_ROOT:-$(gt_find_town_root || true)}"
if [ -z "${TOWN_ROOT:-}" ]; then
  echo "deploy/tmux: not in a Gas Town workspace (no mayor/town.json above cwd); set GT_TOWN_ROOT" >&2
  exit 2
fi
TMUX_SOCKET="$(gt_tmux_socket "$TOWN_ROOT")"

# All tmux invocations go through the town socket.
gt_tmux() {
  tmux -L "$TMUX_SOCKET" "$@"
}
