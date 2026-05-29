#!/usr/bin/env bash
# deploy/dolt/lib.sh — shared resolution for the deploy/dolt scripts.
#
# Sourced by status.sh / sql.sh. Resolves the town root, the Dolt data dir, and the
# host/port the orchestrator connects to. Paso 10 B3 (hq-mc72.12.17): replaces the
# connection-config side of the Go `gt dolt` subcommands. The Go binary tracked far more
# (PID/state files, health metrics); these scripts deliberately keep only what an operator
# needs to inspect and query a running server.
set -euo pipefail

# Walk up from $1 (default cwd) for the town-root marker `mayor/town.json`. Returns the
# OUTERMOST match (a rig that was once a standalone town still carries the marker), mirroring
# Go `beads.FindTownRoot`.
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

# GT_DOLT_HOST / GT_DOLT_PORT override the defaults (127.0.0.1:3307). GT_TOWN_ROOT short-
# circuits the marker walk.
DOLT_HOST="${GT_DOLT_HOST:-127.0.0.1}"
DOLT_PORT="${GT_DOLT_PORT:-3307}"

TOWN_ROOT="${GT_TOWN_ROOT:-$(gt_find_town_root || true)}"
if [ -z "${TOWN_ROOT:-}" ]; then
  echo "deploy/dolt: not in a Gas Town workspace (no mayor/town.json above cwd); set GT_TOWN_ROOT" >&2
  exit 2
fi
DATA_DIR="${GT_DOLT_DATA_DIR:-$TOWN_ROOT/.dolt-data}"

# The verified non-interactive query form for this Dolt build: --host/--port/--no-tls are
# GLOBAL args (before the `sql` subcommand), forcing a TCP connection to the live shared
# server rather than embedded auto-discovery from cwd.
dolt_sql() {
  dolt --host "$DOLT_HOST" --port "$DOLT_PORT" --no-tls sql "$@"
}

# Is something listening on the Dolt port? lsof first (with -sTCP:LISTEN so client PIDs are
# not reported), then ss as a fallback. Echoes the listener PID, or nothing.
dolt_listener_pid() {
  if command -v lsof >/dev/null 2>&1; then
    lsof -i ":$DOLT_PORT" -sTCP:LISTEN -t 2>/dev/null | head -n1 && return 0
  fi
  if command -v ss >/dev/null 2>&1; then
    ss -tlnp "sport = :$DOLT_PORT" 2>/dev/null \
      | grep -oE 'pid=[0-9]+' | head -n1 | cut -d= -f2
  fi
}

# Daemon-side paths (mirror Go `doltserver.DefaultConfig`: <town>/daemon/{dolt.log,dolt.pid}).
DAEMON_DIR="${GT_DOLT_DAEMON_DIR:-$TOWN_ROOT/daemon}"
DOLT_LOG="$DAEMON_DIR/dolt.log"
DOLT_PID="$DAEMON_DIR/dolt.pid"

# Is `pid` a live process? (kill -0, no signal sent.)
dolt_pid_alive() {
  [ -n "${1:-}" ] && kill -0 "$1" 2>/dev/null
}

# Write the managed server config.yaml. Faithful to Go `doltserver.writeServerConfig` +
# `DefaultConfig`: warning log level, 127.0.0.1, 1000 conns, 5-minute read/write timeouts,
# event scheduler + dolt stats off, auto-gc disabled. Overwritten on each start (managed file).
dolt_write_config() {
  local path="$1"
  cat > "$path" <<EOF
# Dolt SQL server configuration — managed by Gas Town (deploy/dolt/start.sh)
# Do not edit manually; overwritten on each server start.

log_level: warning

listener:
  port: $DOLT_PORT
  host: $DOLT_HOST
  max_connections: 1000
  read_timeout_millis: 300000
  write_timeout_millis: 300000

data_dir: "$DATA_DIR"

behavior:
  dolt_transaction_commit: false
  event_scheduler: "OFF"
  auto_gc_behavior:
    enable: false
    archive_level: 0

system_variables:
  dolt_stats_enabled: 0
EOF
}

# Extract a flag value (`--flag value` or `--flag=value`) from a NUL-separated cmdline.
# Reads $2's /proc cmdline; echoes the value or nothing. Linux-only (the deploy target).
dolt_proc_flag() {
  local pid="$1" flag="$2" args arg want=0
  [ -r "/proc/$pid/cmdline" ] || return 0
  while IFS= read -r -d '' arg; do
    if [ "$want" = 1 ]; then printf '%s\n' "$arg"; return 0; fi
    case "$arg" in
      "$flag") want=1 ;;
      "$flag="*) printf '%s\n' "${arg#"$flag"=}"; return 0 ;;
    esac
  done < "/proc/$pid/cmdline"
}

# Does the dolt process `pid` belong to THIS town? Mirrors Go `doltProcessMatchesTownPaths`:
# match if its --data-dir, or --config (== <DATA_DIR>/config.yaml), or cwd resolves to our
# data dir. Used to tell our own server apart from a foreign "imposter" on the same port.
dolt_proc_is_ours() {
  local pid="$1" want actual
  want="$(realpath "$DATA_DIR" 2>/dev/null || printf '%s' "$DATA_DIR")"

  actual="$(dolt_proc_flag "$pid" --data-dir)"
  if [ -n "$actual" ]; then
    [ "$(realpath "$actual" 2>/dev/null || printf '%s' "$actual")" = "$want" ]
    return
  fi
  actual="$(dolt_proc_flag "$pid" --config)"
  if [ -n "$actual" ]; then
    [ "$(realpath "$actual" 2>/dev/null || printf '%s' "$actual")" = "$want/config.yaml" ]
    return
  fi
  actual="$(readlink "/proc/$pid/cwd" 2>/dev/null || true)"
  if [ -n "$actual" ]; then
    [ "$actual" = "$want" ] || [ "$actual" = "$(dirname "$want")" ]
    return
  fi
  return 1
}
