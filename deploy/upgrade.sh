#!/usr/bin/env bash
# Zero-downtime binary upgrade (Linux only).
#
# Pingora hands the listening sockets from the running process to the new one
# over a unix socket, so no connection is refused during the switch.
#
#   deploy/upgrade.sh /usr/local/bin/revp.new /etc/revp/revp.toml
set -euo pipefail
NEW_BIN="${1:?new binary}"
CONFIG="${2:-/etc/revp/revp.toml}"
PID_FILE="$(grep -E '^\s*pid_file' "$CONFIG" | sed -E 's/.*=\s*"(.*)".*/\1/' || true)"
PID_FILE="${PID_FILE:-/tmp/revp.pid}"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "socket hand-off needs Linux (SCM_RIGHTS over the upgrade socket)"; exit 1
fi
if [[ ! -f "$PID_FILE" ]]; then
  echo "no running revp (pid file $PID_FILE missing)"; exit 1
fi
OLD_PID="$(cat "$PID_FILE")"

echo "validating new binary/config..."
"$NEW_BIN" -c "$CONFIG" --check

echo "starting new revp with --upgrade (takes over sockets from pid $OLD_PID)..."
"$NEW_BIN" -c "$CONFIG" --upgrade --daemon

sleep 1
echo "asking old revp (pid $OLD_PID) to drain and exit..."
kill -QUIT "$OLD_PID"
echo "done. new pid: $(cat "$PID_FILE")"
