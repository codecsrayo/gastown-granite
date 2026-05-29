#!/usr/bin/env bash
# deploy/dolt/sql.sh — run SQL against the live Gas Town Dolt server over TCP.
# Replaces Go `gt dolt sql`. All arguments are forwarded verbatim to `dolt … sql`, so the
# usual forms work:
#   deploy/dolt/sql.sh -q "select * from sessions limit 5"
#   deploy/dolt/sql.sh -r csv -q "show databases"
#   deploy/dolt/sql.sh < import.sql
# The --host/--port/--no-tls globals force a TCP connection to the running shared server
# (never embedded mode), so this always queries the same instance the orchestrator uses.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
. "$HERE/lib.sh"

exec dolt --host "$DOLT_HOST" --port "$DOLT_PORT" --no-tls sql "$@"
