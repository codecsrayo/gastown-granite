#!/usr/bin/env bash
# deploy/dolt/stop.sh — stop the Gas Town Dolt SQL server. Replaces Go `gt dolt stop`.
# SIGTERM the recorded pid and wait for it to exit; clears the pidfile.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

if [ ! -f "$DOLT_PID" ]; then
  echo "○ Dolt server not running (no pidfile at $DOLT_PID)"
  exit 0
fi
pid="$(cat "$DOLT_PID" 2>/dev/null || true)"
if ! dolt_pid_alive "$pid"; then
  echo "○ Dolt server not running (stale pidfile, PID $pid) — cleared"
  rm -f "$DOLT_PID"
  exit 0
fi

kill -TERM "$pid" 2>/dev/null || true
for _ in $(seq 1 50); do
  dolt_pid_alive "$pid" || break
  sleep 0.2
done
if dolt_pid_alive "$pid"; then
  echo "✗ Dolt server (PID $pid) did not stop after SIGTERM + 10s" >&2
  exit 1
fi
rm -f "$DOLT_PID"
echo "○ Dolt server stopped (was PID $pid)"
