#!/usr/bin/env bash
# deploy/dolt/status.sh — is the Gas Town Dolt server up, and which databases does it serve?
# Replaces the read-only core of Go `gt dolt status`. Exit 0 if reachable, 1 if not.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

pid="$(dolt_listener_pid || true)"
if [ -z "$pid" ]; then
  echo "○ Dolt server not listening on $DOLT_HOST:$DOLT_PORT"
  echo "  Data dir: $DATA_DIR"
  echo "  Start it with: deploy/dolt/start.sh   (pending — see deploy/dolt/README.md)"
  exit 1
fi

echo "● Dolt server listening on $DOLT_HOST:$DOLT_PORT (PID $pid)"
echo "  Data dir: $DATA_DIR"
if dbs="$(dolt_sql -r csv -q 'show databases' 2>/dev/null | tail -n +2)"; then
  echo "  Databases:"
  printf '%s\n' "$dbs" | sed 's/^/    - /'
else
  echo "  (port is open but a test query failed — server may still be loading)" >&2
  exit 1
fi
