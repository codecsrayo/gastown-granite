#!/usr/bin/env bash
# deploy/dolt/kill-imposters.sh — kill a FOREIGN dolt sql-server holding this town's port.
# Replaces Go `gt dolt kill` (imposter path). DESTRUCTIVE: sends SIGTERM to another process.
#
# An "imposter" is a dolt listening on our port whose --data-dir / --config / cwd points at a
# DIFFERENT data dir than ours. Our own server is never touched. Use --dry-run to preview.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

dry=0
[ "${1:-}" = "--dry-run" ] && dry=1

pid="$(dolt_listener_pid || true)"
if [ -z "$pid" ]; then
  echo "✓ No process on $DOLT_HOST:$DOLT_PORT — nothing to kill"
  exit 0
fi
if dolt_proc_is_ours "$pid"; then
  echo "✓ Listener on :$DOLT_PORT (PID $pid) is THIS town's server (data dir $DATA_DIR) — not an imposter"
  exit 0
fi

owner="$(dolt_proc_flag "$pid" --data-dir)"
[ -z "$owner" ] && owner="$(dolt_proc_flag "$pid" --config)"
[ -z "$owner" ] && owner="$(readlink "/proc/$pid/cwd" 2>/dev/null || echo '?')"
echo "Found imposter dolt on :$DOLT_PORT"
echo "  PID:      $pid"
echo "  Owner:    $owner"
echo "  Expected: $DATA_DIR"

if [ "$dry" = 1 ]; then
  echo "~ --dry-run: not killing"
  exit 0
fi

kill -TERM "$pid" 2>/dev/null || true
for _ in $(seq 1 25); do dolt_pid_alive "$pid" || break; sleep 0.2; done
if dolt_pid_alive "$pid"; then
  echo "✗ Imposter (PID $pid) survived SIGTERM" >&2
  exit 1
fi
echo "✓ Imposter killed (was PID $pid)"
