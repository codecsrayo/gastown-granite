#!/usr/bin/env bash
# deploy/dolt/start.sh — launch the Gas Town Dolt SQL server. Replaces Go `gt dolt start`.
#
# Writes a managed config.yaml, then starts `dolt sql-server --config …` detached in its own
# process group, logging to <town>/daemon/dolt.log and recording its pid. Refuses to start if
# a FOREIGN dolt already holds the port (imposter) — use a dedicated kill-imposters step for
# that (not bundled here, to keep this script non-destructive).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

# Already running and owned by us?
if [ -f "$DOLT_PID" ]; then
  existing="$(cat "$DOLT_PID" 2>/dev/null || true)"
  if dolt_pid_alive "$existing"; then
    echo "● Dolt server already running (PID $existing) on $DOLT_HOST:$DOLT_PORT"
    exit 0
  fi
  echo "Clearing stale pidfile (PID $existing not alive)" >&2
  rm -f "$DOLT_PID"
fi

# Foreign listener on the port? Refuse rather than fight it.
listener="$(dolt_listener_pid || true)"
if [ -n "$listener" ]; then
  echo "✗ Port $DOLT_PORT already held by PID $listener (not our pidfile)." >&2
  echo "  If it is a stale/foreign Dolt, stop it first; this script will not kill it." >&2
  exit 1
fi

mkdir -p "$DAEMON_DIR" "$DATA_DIR"

# Clean a stale Unix socket from a prior crash (Dolt warns "file already in use" otherwise).
socket="/tmp/mysql.sock"
[ "$DOLT_PORT" != "3306" ] && socket="/tmp/mysql.$DOLT_PORT.sock"
if [ -e "$socket" ]; then
  echo "Removing stale Unix socket: $socket" >&2
  rm -f "$socket" || true
fi

config="$DATA_DIR/config.yaml"
dolt_write_config "$config"

# Launch detached, in its own session/process group, stdio to the log. `setsid` forks before
# exec, so its pid is NOT the dolt pid — we record the real server pid from the live listener
# once it is up (below), not `$!`.
( cd "$DATA_DIR" && setsid dolt sql-server --config "$config" >>"$DOLT_LOG" 2>&1 & )

# Wait (up to ~5s) for the port to come up, then record the actual listener pid.
for _ in $(seq 1 25); do
  pid="$(dolt_listener_pid || true)"
  if [ -n "$pid" ]; then
    printf '%s\n' "$pid" > "$DOLT_PID"
    echo "● Dolt server started (PID $pid) on $DOLT_HOST:$DOLT_PORT"
    echo "  Data dir: $DATA_DIR"
    echo "  Log:      $DOLT_LOG"
    exit 0
  fi
  sleep 0.2
done
echo "✗ Dolt server did not begin listening within 5s — see $DOLT_LOG" >&2
exit 1
