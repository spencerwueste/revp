#!/usr/bin/env bash
# Benchmark every core part and print one summary table.
#
#   bench/suite.sh [duration] [connections]
#
# Each scenario starts a fresh origin + revp, runs oha, and records
# rps / p50 / p99 / RSS. If `nginx` is on PATH it is measured as a reference.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DURATION="${1:-10s}"
CONNS="${2:-64}"
OUT="$ROOT/bench/results"; mkdir -p "$OUT" "$ROOT/bench/certs"
REVP="$ROOT/target/release/revp"
ORIGIN="$ROOT/target/release/bench-upstream"
SUMMARY="$OUT/summary.txt"
ulimit -n 65536 2>/dev/null || true

# Self-signed cert for TLS scenarios (named curve: LibreSSL defaults to explicit params).
if [[ ! -f "$ROOT/bench/certs/cert.pem" ]]; then
  openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -pkeyopt ec_param_enc:named_curve \
    -nodes -keyout "$ROOT/bench/certs/key.pem" -out "$ROOT/bench/certs/cert.pem" -days 30 \
    -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" >/dev/null 2>&1
fi

PIDS=()
# ${PIDS[@]+...} keeps bash 3.2 (macOS) happy under set -u when the array is empty.
cleanup() { for p in ${PIDS[@]+"${PIDS[@]}"}; do kill "$p" 2>/dev/null; done; wait 2>/dev/null; }
trap cleanup EXIT
stop_all() { for p in ${PIDS[@]+"${PIDS[@]}"}; do kill "$p" 2>/dev/null; done; wait 2>/dev/null; PIDS=(); }

measure() { # measure <url> <extra oha flags...>  -> "rps p50 p99 success"
  local url="$1"; shift
  oha -z "$DURATION" -c "$CONNS" --no-tui --output-format json "$@" "$url" 2>/dev/null | python3 "$ROOT/bench/measure.py"
}

rss_mb() { ps -o rss= -p "$1" 2>/dev/null | awk '{printf "%.1f", $1/1024}'; }

ROWS=()
row() { # row <name> <rps p50 p99 ok> <rss>
  local rps p50 p99 okp
  read -r rps p50 p99 okp <<< "${2:-? ? ? ?}"
  ROWS+=("$(printf '%-26s %10s %9s %9s %7s%% %8s' "$1" "$rps" "$p50" "$p99" "$okp" "$3")")
  echo "  -> $1: rps=$rps p50=${p50}ms p99=${p99}ms ok=${okp}% rss=${3}MB"
}

start_origin() { "$ORIGIN" -l 127.0.0.1:13000 "$@" >/dev/null 2>&1 & PIDS+=($!); }
start_revp() { # start_revp <config body>
  printf '%s\n' "$1" > "$OUT/current.toml"
  "$REVP" -c "$OUT/current.toml" >"$OUT/current.revp.log" 2>&1 & REVP_PID=$!; PIDS+=($REVP_PID)
  for _ in $(seq 1 40); do curl -fsSk -o /dev/null "${2:-http://127.0.0.1:18080/}" 2>/dev/null && return; sleep 0.1; done
  echo "revp failed to start:"; cat "$OUT/current.revp.log"; exit 1
}

BASE='[server]
threads = 0
log_level = "warn"
pid_file = "/tmp/revp-bench.pid"
upgrade_sock = "/tmp/revp-bench-upgrade.sock"
watch_config = false
[admin]
listen = "127.0.0.1:19090"
[upstreams.origin]
servers = ["127.0.0.1:13000"]
'
LISTEN_HTTP='[[listeners]]
addr = "127.0.0.1:18080"
'
LISTEN_TLS="[[listeners]]
addr = \"127.0.0.1:18443\"
tls = true
[tls]
certs = [{ cert = \"$ROOT/bench/certs/cert.pem\", key = \"$ROOT/bench/certs/key.pem\" }]
"

echo "revp benchmark suite — $(date) — $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), $(sysctl -n hw.ncpu 2>/dev/null || nproc) cores — $DURATION x $CONNS conns"
echo

echo "[1/10] origin direct (13 B body)"
start_origin
row "origin direct" "$(measure http://127.0.0.1:13000/)" "-"
stop_all

echo "[2/10] bare proxy, no modules"
start_origin
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"'
row "revp bare" "$(measure http://127.0.0.1:18080/)" "$(rss_mb $REVP_PID)"
stop_all

echo "[3/10] metrics + accesslog"
start_origin
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"
[routes.modules.metrics]
[routes.modules.accesslog]
output = "'"$OUT"'/bench-access.log"'
row "revp metrics+accesslog" "$(measure http://127.0.0.1:18080/)" "$(rss_mb $REVP_PID)"
stop_all; rm -f "$OUT/bench-access.log"

echo "[4/10] ratelimit (limit not reached)"
start_origin
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"
[routes.modules.ratelimit]
rps = 10000000'
row "revp ratelimit" "$(measure http://127.0.0.1:18080/)" "$(rss_mb $REVP_PID)"
stop_all

echo "[5/10] headers (set 2, remove 1)"
start_origin
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"
[routes.modules.headers]
request.set = { x-a = "1" }
response.set = { x-b = "2" }
response.remove = ["server"]'
row "revp headers" "$(measure http://127.0.0.1:18080/)" "$(rss_mb $REVP_PID)"
stop_all

echo "[6/10] cache (all hits after first)"
start_origin --max-age 300
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"
[routes.modules.cache]
ttl = "300s"'
row "revp cache hit" "$(measure http://127.0.0.1:18080/)" "$(rss_mb $REVP_PID)"
stop_all

echo "[7/10] compress gzip (20 KB body)"
start_origin --body-size 20000
row "origin direct 20KB" "$(measure http://127.0.0.1:13000/)" "-"
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"'
row "revp bare 20KB" "$(measure http://127.0.0.1:18080/)" "$(rss_mb $REVP_PID)"
stop_all
start_origin --body-size 20000
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"
[routes.modules.compress]
level = 6'
row "revp compress gzip 20KB" "$(measure http://127.0.0.1:18080/ -H 'Accept-Encoding: gzip')" "$(rss_mb $REVP_PID)"
stop_all

echo "[8/10] TLS 1.3 (HTTP/1.1 and HTTP/2)"
start_origin
start_revp "$BASE$LISTEN_TLS"'[[routes]]
name = "r"
upstream = "origin"' https://127.0.0.1:18443/
row "revp https h1" "$(measure https://127.0.0.1:18443/ --insecure)" "$(rss_mb $REVP_PID)"
row "revp https h2" "$(measure https://127.0.0.1:18443/ --insecure --http2)" "$(rss_mb $REVP_PID)"
stop_all

echo "[9/10] L4 TCP splice"
start_origin
start_revp "$BASE"'[[l4]]
listen = "127.0.0.1:15432"
upstream = "origin"' http://127.0.0.1:15432/
row "revp l4 tcp" "$(measure http://127.0.0.1:15432/)" "$(rss_mb $REVP_PID)"
stop_all

echo "[10/10] everything on one route"
start_origin
start_revp "$BASE$LISTEN_HTTP"'[[routes]]
name = "r"
upstream = "origin"
[routes.modules.metrics]
[routes.modules.accesslog]
output = "'"$OUT"'/bench-access.log"
[routes.modules.ratelimit]
rps = 10000000
[routes.modules.headers]
response.set = { x-b = "2" }
[routes.modules.compress]
level = 6'
row "revp all modules" "$(measure http://127.0.0.1:18080/)" "$(rss_mb $REVP_PID)"
stop_all; rm -f "$OUT/bench-access.log"

if command -v nginx >/dev/null 2>&1; then
  echo "[ref] nginx reverse proxy (keepalive upstream)"
  mkdir -p "$OUT/nginx"
  cat > "$OUT/nginx/nginx.conf" <<EOF
worker_processes auto;
error_log $OUT/nginx/error.log warn;
pid $OUT/nginx/nginx.pid;
events { worker_connections 16384; }
http {
  access_log off;
  upstream origin { server 127.0.0.1:13000; keepalive 256; }
  server {
    listen 127.0.0.1:18090;
    location / {
      proxy_pass http://origin;
      proxy_http_version 1.1;
      proxy_set_header Connection "";
      proxy_set_header X-Forwarded-For \$remote_addr;
    }
  }
}
EOF
  start_origin
  nginx -c "$OUT/nginx/nginx.conf" -p "$OUT/nginx" & PIDS+=($!); sleep 0.5
  NGX_PID=$(cat "$OUT/nginx/nginx.pid" 2>/dev/null)
  row "nginx bare (reference)" "$(measure http://127.0.0.1:18090/)" "$(ps -o rss= -p $(pgrep -P "$NGX_PID" | head -1) 2>/dev/null | awk '{printf "%.1f", $1/1024}')"
  nginx -c "$OUT/nginx/nginx.conf" -p "$OUT/nginx" -s quit 2>/dev/null
  stop_all
fi

{
  echo
  printf '%-26s %10s %9s %9s %8s %8s\n' "scenario" "req/s" "p50 ms" "p99 ms" "200s" "RSS MB"
  printf '%-26s %10s %9s %9s %8s %8s\n' "--------" "-----" "------" "------" "----" "------"
  for r in "${ROWS[@]}"; do echo "$r"; done
} | tee "$SUMMARY"
echo
echo "saved to $SUMMARY"
