#!/usr/bin/env bash
# End-to-end test of a full-featured revp build against two local origins.
# Covers: routing, strip_prefix, forwarded headers, headers module, rate limit,
# compression, cache, static files, request dashboard, metrics/admin, TLS + HTTP/2, L4 TCP proxy, access log,
# hot reload on file change, SIGHUP reload, graceful shutdown.
#
#   cargo build --release -p revp -p bench-upstream && test/integration.sh
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REVP="${REVP:-$ROOT/target/release/revp}"
ORIGIN="${ORIGIN:-$ROOT/target/release/bench-upstream}"
TMP="$(mktemp -d)"
PASS=0; FAIL=0

ok()   { PASS=$((PASS+1)); echo "  ✓ $1"; }
fail() { FAIL=$((FAIL+1)); echo "  ✗ $1"; [[ -n "${2:-}" ]] && echo "      $2"; }
check() { # check <desc> <cmd...>  — passes if the command exits 0
  local desc="$1"; shift
  if out=$("$@" 2>&1); then ok "$desc"; else fail "$desc" "$out"; fi
}
expect_contains() { # expect_contains <desc> <needle> <haystack>
  if [[ "$3" == *"$2"* ]]; then ok "$1"; else fail "$1" "expected to contain: $2
      got: $(echo "$3" | head -c 400)"; fi
}

cleanup() {
  for p in "${REVP_PID:-}" "${O1:-}" "${O2:-}"; do [[ -n "$p" ]] && kill "$p" 2>/dev/null; done
  wait 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT

# ---- self-signed cert for the TLS listener ----
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -pkeyopt ec_param_enc:named_curve -nodes \
  -keyout "$TMP/key.pem" -out "$TMP/cert.pem" -days 1 -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,DNS:cached.test" >/dev/null 2>&1 || { echo "openssl failed"; exit 1; }
# LibreSSL emits SEC1 EC keys; rustls needs PKCS#8.
openssl pkcs8 -topk8 -nocrypt -in "$TMP/key.pem" -out "$TMP/key8.pem" 2>/dev/null && mv "$TMP/key8.pem" "$TMP/key.pem"

# ---- files for the static module ----
mkdir -p "$TMP/www/assets" "$TMP/www/sub"
echo '<html>shell</html>' > "$TMP/www/index.html"
head -c 30000 /dev/zero | tr '\0' 'a' > "$TMP/www/assets/app.js"
echo 'sub index' > "$TMP/www/sub/index.html"
echo 'SECRET' > "$TMP/www/.env"
echo 'outside' > "$TMP/outside.txt"; ln -s "$TMP/outside.txt" "$TMP/www/escape.txt"

cat > "$TMP/revp.toml" <<EOF
[server]
log_level = "info"
pid_file = "$TMP/revp.pid"
upgrade_sock = "$TMP/upgrade.sock"
grace_period = "1s"
graceful_shutdown_timeout = "2s"

[[listeners]]
addr = "127.0.0.1:18080"
[[listeners]]
addr = "127.0.0.1:18081"
redirect_https = true
[[listeners]]
addr = "127.0.0.1:18443"
tls = true

[tls]
certs = [{ hosts = ["localhost", "cached.test"], cert = "$TMP/cert.pem", key = "$TMP/key.pem" }]

[admin]
listen = "127.0.0.1:19090"

[defaults.modules]
metrics = { requests = 1000 }
accesslog = { output = "$TMP/access.log" }

[upstreams.small]
servers = ["127.0.0.1:13000"]
health_check = { kind = "http", path = "/", interval = "1s" }

[upstreams.big]
servers = ["127.0.0.1:13001"]
health_check = { kind = "tcp", interval = "1s" }

[upstreams.dead]
servers = ["127.0.0.1:13999"]
health_check = { kind = "tcp", interval = "1s", consecutive_failure = 1 }

[[routes]]
name = "api"
paths = ["/api/*"]
upstream = "small"
strip_prefix = "/api"
host_header = "upstream"

[[routes]]
name = "limited"
hosts = ["limited.test"]
upstream = "small"
[routes.modules.ratelimit]
rps = 1
burst = 2

[[routes]]
name = "cached"
hosts = ["cached.test"]
upstream = "big"
[routes.modules.cache]
ttl = "30s"
[routes.modules.headers]
response.set = { x-test = "yes" }
response.remove = ["server"]

[[routes]]
name = "zipped"
hosts = ["zipped.test"]
upstream = "big"
[routes.modules.compress]
level = 6

[[routes]]
name = "files"
hosts = ["files.test"]
[routes.modules.static]
root = "$TMP/www"
fallback = "/index.html"
[[routes.modules.static.headers]]
paths = ["/assets/*"]
set = { cache-control = "immutable" }
[routes.modules.headers]
response.set = { x-test = "files" }
[routes.modules.compress]
level = 6

[[routes]]
name = "files-then-proxy"
hosts = ["mixed.test"]
upstream = "small"
[routes.modules.static]
root = "$TMP/www"

[[routes]]
name = "dead"
hosts = ["dead.test"]
upstream = "dead"

[[routes]]
name = "default"
upstream = "small"

[[l4]]
listen = "127.0.0.1:15432"
upstream = "big"
EOF

"$ORIGIN" -l 127.0.0.1:13000 >/dev/null 2>&1 & O1=$!
"$ORIGIN" -l 127.0.0.1:13001 --body-size 20000 --max-age 30 >/dev/null 2>&1 & O2=$!
"$REVP" -c "$TMP/revp.toml" >"$TMP/revp.log" 2>&1 & REVP_PID=$!
for _ in $(seq 1 30); do curl -fsS -o /dev/null http://127.0.0.1:19090/healthz 2>/dev/null && break; sleep 0.2; done

H=http://127.0.0.1:18080
A=http://127.0.0.1:19090

echo "routing"
expect_contains "default route proxies" "Hello, world!" "$(curl -sS $H/)"
echo_out=$(curl -sS $H/api/echo/x?y=1)
expect_contains "strip_prefix rewrites path" "GET /echo/x?y=1 HTTP/1.1" "$echo_out"
expect_contains "host_header=upstream" "host: 127.0.0.1:13000" "$(echo "$echo_out" | tr 'A-Z' 'a-z')"
expect_contains "x-forwarded-for added" "x-forwarded-for: 127.0.0.1" "$(echo "$echo_out" | tr 'A-Z' 'a-z')"
expect_contains "x-forwarded-proto http" "x-forwarded-proto: http" "$(echo "$echo_out" | tr 'A-Z' 'a-z')"
expect_contains "host route picks big origin" "20000" "$(curl -sS -o /dev/null -w '%{size_download}' -H 'Host: cached.test' $H/)"
expect_contains "unhealthy upstream -> 503" "503" "$(curl -sS -o /dev/null -w '%{http_code}' -H 'Host: dead.test' $H/)"

echo "headers module"
hdrs=$(curl -sS -D - -o /dev/null -H 'Host: cached.test' $H/ | tr 'A-Z' 'a-z')
expect_contains "response.set" "x-test: yes" "$hdrs"
if [[ "$hdrs" != *"server:"* ]]; then ok "response.remove server"; else fail "response.remove server" "$hdrs"; fi

echo "ratelimit module"
codes=$(for i in 1 2 3 4; do curl -sS -o /dev/null -w '%{http_code} ' -H 'Host: limited.test' $H/; done)
expect_contains "burst of 2 then 429" "200 200 429 429" "$codes"
expect_contains "retry-after on 429" "retry-after:" "$(curl -sS -D - -o /dev/null -H 'Host: limited.test' $H/ | tr 'A-Z' 'a-z')"

echo "compress module"
zh=$(curl -sS -D - -o "$TMP/z.bin" -H 'Host: zipped.test' -H 'Accept-Encoding: gzip' $H/ | tr 'A-Z' 'a-z')
expect_contains "gzip negotiated" "content-encoding: gzip" "$zh"
zsize=$(stat -f %z "$TMP/z.bin" 2>/dev/null || stat -c %s "$TMP/z.bin")
if [[ "$zsize" -lt 2000 ]]; then ok "body compressed (20000 -> $zsize bytes)"; else fail "body compressed" "size $zsize"; fi
expect_contains "no compression without accept-encoding" "20000" "$(curl -sS -o /dev/null -w '%{size_download}' -H 'Host: zipped.test' $H/)"

echo "static module"
F=(-H 'Host: files.test')
expect_contains "serves a file" "200 text/javascript; charset=utf-8 30000" "$(curl -sS -o /dev/null -w '%{http_code} %{content_type} %{size_download}' "${F[@]}" $H/assets/app.js)"
expect_contains "cmp body" "same" "$(curl -sS "${F[@]}" $H/assets/app.js | cmp -s - "$TMP/www/assets/app.js" && echo same)"
fh=$(curl -sS -D - -o /dev/null "${F[@]}" $H/assets/app.js | tr 'A-Z' 'a-z')
expect_contains "per-path header rule" "cache-control: immutable" "$fh"
expect_contains "route headers module applies" "x-test: files" "$fh"
expect_contains "etag sent" "etag: \"" "$fh"
etag=$(echo "$fh" | sed -n 's/^etag: //p' | tr -d '\r')
expect_contains "if-none-match -> 304" "304" "$(curl -sS -o /dev/null -w '%{http_code}' "${F[@]}" -H "If-None-Match: $etag" $H/assets/app.js)"
expect_contains "compress applies to files" "content-encoding: gzip" "$(curl -sS -D - -o /dev/null "${F[@]}" -H 'Accept-Encoding: gzip' $H/assets/app.js | tr 'A-Z' 'a-z')"
expect_contains "HEAD has no body" "200 0" "$(curl -sS -I -o /dev/null -w '%{http_code} %{size_download}' "${F[@]}" $H/assets/app.js)"
expect_contains "spa fallback" "shell" "$(curl -sS "${F[@]}" $H/some/client/route)"
expect_contains "dir without slash -> 308" "308 /sub/" "$(curl -sS -o /dev/null -w '%{http_code} %{redirect_url}' "${F[@]}" $H/sub | sed 's#http://[^/]*##')"
expect_contains "dir index" "sub index" "$(curl -sS "${F[@]}" $H/sub/)"
expect_contains "dotfile hidden" "shell" "$(curl -sS "${F[@]}" $H/.env)"
expect_contains "symlink out of root refused" "shell" "$(curl -sS "${F[@]}" $H/escape.txt)"
expect_contains "encoded .. -> 400" "400" "$(curl -sS -o /dev/null -w '%{http_code}' "${F[@]}" --path-as-is $H/%2e%2e/outside.txt)"
expect_contains "POST without upstream -> 405" "405" "$(curl -sS -o /dev/null -w '%{http_code}' -X POST "${F[@]}" $H/)"
expect_contains "file hit on mixed route" "30000" "$(curl -sS -o /dev/null -w '%{size_download}' -H 'Host: mixed.test' $H/assets/app.js)"
expect_contains "miss on mixed route proxies" "Hello, world!" "$(curl -sS -H 'Host: mixed.test' $H/nope)"
printf '[[listeners]]\naddr = "127.0.0.1:1"\n[[routes]]\npaths = ["/*"]\n' > "$TMP/noup.toml"
expect_contains "route without upstream or static rejected" "no upstream" "$("$REVP" --check -c "$TMP/noup.toml" 2>&1)"

echo "cache module"
curl -sS -o /dev/null -H 'Host: cached.test' $H/c1; curl -sS -o /dev/null -H 'Host: cached.test' $H/c1; curl -sS -o /dev/null -H 'Host: cached.test' $H/c1
m=$(curl -sS $A/metrics)
expect_contains "cache miss counted" 'revp_cache_total{route="cached",state="miss"} ' "$m"
hits=$(echo "$m" | sed -n 's/^revp_cache_total{route="cached",state="hit"} \([0-9]*\)$/\1/p')
if [[ "${hits:-0}" -ge 2 ]]; then ok "cache hits counted ($hits)"; else fail "cache hits counted" "hits=${hits:-0}"; fi
expect_contains "cached response carries age" "age:" "$(curl -sS -D - -o /dev/null -H 'Host: cached.test' $H/c1 | tr 'A-Z' 'a-z')"

echo "request dashboard"
JWT="eyJhbGciOiJub25lIn0.$(printf '{"sub":"u1","email":"tester@example.com"}' | base64 | tr '+/' '-_' | tr -d '=\n').x"
curl -sS -o /dev/null -H "Authorization: Bearer $JWT" "$H/api/echo/dash?k=v"
expect_contains "dashboard page served" "200 text/html" "$(curl -sS -o /dev/null -w '%{http_code} %{content_type}' $A/dashboard)"
dj=$(curl -sS "$A/dashboard/api/requests?q=echo/dash&limit=5")
expect_contains "request recorded" '"path":"/api/echo/dash"' "$dj"
expect_contains "query kept" '"query":"k=v"' "$dj"
expect_contains "user read from the JWT" '"user":"tester@example.com"' "$dj"
expect_contains "service is the upstream name" '"service":"small"' "$dj"
expect_contains "status filter" '"matched":0' "$(curl -sS "$A/dashboard/api/requests?q=echo/dash&status=5xx")"
expect_contains "user profile" '"requests":1' "$(curl -sS "$A/dashboard/api/users?user=tester%40example.com")"
expect_contains "static requests typed" '"kind":"static"' "$(curl -sS "$A/dashboard/api/requests?route=files&limit=1")"

echo "admin + metrics"
expect_contains "/healthz" "ok" "$(curl -sS $A/healthz)"
expect_contains "/upstreams shows healthy" "127.0.0.1:13000	weight=1	healthy" "$(curl -sS $A/upstreams)"
expect_contains "/upstreams shows unhealthy" "127.0.0.1:13999	weight=1	unhealthy" "$(curl -sS $A/upstreams)"
expect_contains "requests counted per route" 'revp_requests_total{route="default",class="2xx"}' "$m"
expect_contains "429s counted" 'revp_requests_total{route="limited",class="4xx"} 3' "$m"
expect_contains "latency histogram" 'revp_request_duration_seconds_bucket{route="default",le="+Inf"}' "$m"
expect_contains "short circuits counted" 'revp_short_circuited_total{route="limited"} 3' "$m"

echo "tls"
expect_contains "https + http/2" "HTTP/2 200" "$(curl -sSk --http2 -D - -o /dev/null https://127.0.0.1:18443/ 2>&1)"
expect_contains "x-forwarded-proto https" "x-forwarded-proto: https" "$(curl -sSk https://localhost:18443/api/echo | tr 'A-Z' 'a-z')"
expect_contains "sni picks cert" "CN=localhost" "$(echo | openssl s_client -connect 127.0.0.1:18443 -servername cached.test 2>/dev/null | grep subject=)"

echo "https redirect"
rh=$(curl -sS -D - -o /dev/null 'http://localhost:18081/api/echo?x=1' | tr 'A-Z' 'a-z')
expect_contains "plain listener answers 308" "308" "$(echo "$rh" | head -1)"
expect_contains "location keeps host, path, query" "location: https://localhost:18443/api/echo?x=1" "$rh"
expect_contains "redirect followed to https" "x-forwarded-proto: https" "$(curl -sSkL http://localhost:18081/api/echo | tr 'A-Z' 'a-z')"
expect_contains "other plain listener still proxies" "200" "$(curl -sS -o /dev/null -w '%{http_code}' $H/)"

echo "l4"
expect_contains "tcp proxy splices bytes" "20000" "$(curl -sS -o /dev/null -w '%{size_download}' http://127.0.0.1:15432/)"

echo "access log"
sleep 0.3
expect_contains "json line written" '"route":"default"' "$(grep '"uri":"/"' "$TMP/access.log" | head -1)"
expect_contains "429 logged with module" '"by":"ratelimit"' "$(grep '"status":429' "$TMP/access.log" | head -1)"

echo "hot reload"
gen0=$(curl -sS $A/config/generation)
cat >> "$TMP/revp.toml" <<EOF

[[routes]]
name = "reloaded"
hosts = ["reload.test"]
upstream = "big"
EOF
for _ in $(seq 1 30); do [[ "$(curl -sS $A/config/generation)" != "$gen0" ]] && break; sleep 0.2; done
expect_contains "generation bumped on file change" "$((gen0+1))" "$(curl -sS $A/config/generation)"
expect_contains "new route live" "20000" "$(curl -sS -o /dev/null -w '%{size_download}' -H 'Host: reload.test' $H/)"
kill -HUP "$REVP_PID"; sleep 0.5
expect_contains "SIGHUP reloads" "$((gen0+2))" "$(curl -sS $A/config/generation)"
printf '\n[[routes]]\nupstream = "nope"\n' >> "$TMP/revp.toml"; sleep 0.8
expect_contains "bad config rejected, old kept" "$((gen0+2))" "$(curl -sS $A/config/generation)"
expect_contains "error logged" "reload failed, keeping previous config" "$(grep 'reload failed' "$TMP/revp.log" | head -1)"
expect_contains "still serving after bad reload" "Hello" "$(curl -sS $H/)"

echo "graceful shutdown"
curl -sS -o /dev/null $H/
kill -TERM "$REVP_PID"
for _ in $(seq 1 50); do kill -0 "$REVP_PID" 2>/dev/null || break; sleep 0.1; done
if ! kill -0 "$REVP_PID" 2>/dev/null; then ok "exits on SIGTERM"; else fail "exits on SIGTERM"; fi
REVP_PID=""

echo
echo "passed: $PASS  failed: $FAIL"
[[ $FAIL -eq 0 ]] || { echo "--- revp.log ---"; tail -40 "$TMP/revp.log"; exit 1; }
