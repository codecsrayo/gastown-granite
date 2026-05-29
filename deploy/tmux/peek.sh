#!/usr/bin/env bash
# deploy/tmux/peek.sh — read recent output from a Gas Town session (capture-pane wrapper).
# Replaces Go `gt peek`. Read-only.
#
#   deploy/tmux/peek.sh mayor              # town agent shorthand -> hq-mayor
#   deploy/tmux/peek.sh gt-furiosa 400     # explicit session, last 400 lines
#   deploy/tmux/peek.sh hq-deacon --all    # full scrollback
#
# Town-agent shorthands (mayor/deacon/boot/overseer) map to their hq-* session names. Any
# other argument is treated as a literal tmux session name. Full rig/polecat address
# resolution (<rig>/<polecat> -> <prefix>-<polecat> via the prefix registry) is deferred —
# pass the tmux session name directly for those.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

usage() { echo "usage: peek.sh <session|mayor|deacon|boot|overseer> [lines|--all]" >&2; exit 2; }
[ $# -ge 1 ] || usage

address="$1"; shift
lines=200
all=0
while [ $# -gt 0 ]; do
  case "$1" in
    --all) all=1 ;;
    --) ;;
    *) if printf '%s' "$1" | grep -qE '^[0-9]+$'; then lines="$1"; else echo "peek.sh: bad argument '$1'" >&2; usage; fi ;;
  esac
  shift
done

case "$address" in
  mayor)    session="hq-mayor" ;;
  deacon)   session="hq-deacon" ;;
  boot)     session="hq-boot" ;;
  overseer) session="hq-overseer" ;;
  *)        session="$address" ;;
esac

if ! gt_tmux has-session -t "$session" 2>/dev/null; then
  echo "peek.sh: no session '$session' on tmux socket '$TMUX_SOCKET'" >&2
  exit 1
fi

if [ "$all" -eq 1 ]; then
  gt_tmux capture-pane -p -t "$session" -S -
else
  gt_tmux capture-pane -p -t "$session" -S "-$lines"
fi
