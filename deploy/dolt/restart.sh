#!/usr/bin/env bash
# deploy/dolt/restart.sh — stop (if running) then start the Gas Town Dolt server.
# Replaces Go `gt dolt restart`. Composes the tested stop.sh + start.sh.
#
# Intentionally NON-destructive: if a FOREIGN dolt holds the port after stopping our own,
# restart refuses and points at kill-imposters.sh rather than killing it automatically.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

"$HERE/stop.sh"

# If something still holds the port and it is not ours, do not fight it here.
pid="$(dolt_listener_pid || true)"
if [ -n "$pid" ] && ! dolt_proc_is_ours "$pid"; then
  echo "✗ Port $DOLT_PORT still held by a foreign dolt (PID $pid)." >&2
  echo "  Run: deploy/dolt/kill-imposters.sh   then retry restart." >&2
  exit 1
fi

exec "$HERE/start.sh"
