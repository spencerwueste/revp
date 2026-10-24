#!/usr/bin/env bash
# Load-test revp against a local origin and report numbers.
#
#   bench/run.sh [config.toml] [duration] [connections]
#
# Runs oha against the origin directly, then through revp, so the proxy's own
# overhead is visible. Results land in bench/results/<config-name>.txt.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONFIG="${1:-$ROOT/bench/bare.toml}"
DURATION="${2:-15s}"
CONNS="${3:-64}"
NAME="$(basename "$CONFIG" .toml)"
OUT="$ROOT/bench/results"
mkdir -p "$OUT"

REVP="$ROOT/target/release/revp"
ORIGIN="$ROOT/target/release/bench-upstream"
PROXY_URL="${PROXY_URL:-http://127.0.0.1:18080/}"
ORIGIN_URL="${ORIGIN_URL:-http://127.0.0.1:13000/}"
ORIGIN_ARGS="${ORIGIN_ARGS:--l 127.0.0.1:13000}"
# Extra oha flags, e.g. OHA_EXTRA='-H "Accept-Encoding: gzip"'
OHA_EXTRA="${OHA_EXTRA:-}"

cleanup() {
  [[ -n "${REVP_PID:-}" ]] && kill "$REVP_PID" 2>/dev/null || true
  [[ -n "${ORIGIN_PID:-}" ]] && kill "$ORIGIN_PID" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT

ulimit -n 65536 2>/dev/null || true

# shellcheck disable=SC2086
"$ORIGIN" $ORIGIN_ARGS >"$OUT/$NAME.origin.log" 2>&1 &
ORIGIN_PID=$!
"$REVP" -c "$CONFIG" >"$OUT/$NAME.revp.log" 2>&1 &
REVP_PID=$!
sleep 1

if ! curl -fsS -o /dev/null "$PROXY_URL"; then
  echo "revp not answering on $PROXY_URL; log:"; cat "$OUT/$NAME.revp.log"; exit 1
fi

run_oha() {
  local label="$1" url="$2"
  echo "== $label ($url) — $DURATION, $CONNS conns =="
  # shellcheck disable=SC2086
  oha -z "$DURATION" -c "$CONNS" --no-tui --output-format json $OHA_EXTRA "$url" | python3 "$ROOT/bench/summarize.py"
}

{
  echo "revp bench — $(date) — config: $NAME — $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)"
  echo
  DIRECT=$(run_oha "origin direct" "$ORIGIN_URL" | tee /dev/stderr | grep '^RPS=' | cut -d= -f2)
  echo
  PROXY=$(run_oha "through revp" "$PROXY_URL" | tee /dev/stderr | grep '^RPS=' | cut -d= -f2)
  echo
  python3 - "$DIRECT" "$PROXY" <<'EOF'
import sys
d,p=float(sys.argv[1]),float(sys.argv[2])
print(f"proxy throughput = {p/d*100:.1f}% of origin-direct ({p:,.0f} vs {d:,.0f} rps)")
EOF
  echo
  echo "revp RSS after run: $(ps -o rss= -p "$REVP_PID" | awk '{printf "%.1f MB", $1/1024}')"
} 2>&1 | grep -v '^RPS=' | tee "$OUT/$NAME.txt"
