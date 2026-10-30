#!/usr/bin/env bash
# Live ACME test against Pebble (Let's Encrypt's test CA) in Docker, once per
# challenge type: HTTP-01, then TLS-ALPN-01 with no port-80 listener at all.
#
# Pebble validates by connecting to <domain>:httpPort or <domain>:tlsPort. A
# challtestsrv DNS container answers every name with this host's IP (as seen
# from Docker), and Pebble's ports are set to revp's listeners. revp must get a
# cert, store it, install it in the TLS resolver, and serve HTTPS with it.
# MODES="tls-alpn-01" runs one case only.
#
#   cargo build --release -p revp -p bench-upstream && test/acme-pebble.sh
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REVP="${REVP:-$ROOT/target/release/revp}"
ORIGIN="${ORIGIN:-$ROOT/target/release/bench-upstream}"
# Under the project dir (not /var/folders): Docker Desktop can bind-mount it, and we
# mount a directory rather than a single file, which fails on macOS.
TMP="$ROOT/target/acme-test"; rm -rf "$TMP"; mkdir -p "$TMP/pebble-cfg"
NET=revp-acme-net
DOMAIN=revp.test
PASS=0; FAIL=0
ok()   { PASS=$((PASS+1)); echo "  ✓ $1"; }
fail() { FAIL=$((FAIL+1)); echo "  ✗ $1"; [[ -n "${2:-}" ]] && echo "      $2"; }

cleanup() {
  [[ -n "${REVP_PID:-}" ]] && kill "$REVP_PID" 2>/dev/null
  [[ -n "${O1:-}" ]] && kill "$O1" 2>/dev/null
  docker rm -f pebble pebble-dns >/dev/null 2>&1
  docker network rm $NET >/dev/null 2>&1
  wait 2>/dev/null; rm -rf "$TMP"
}
trap cleanup EXIT

command -v docker >/dev/null || { echo "docker not found"; exit 1; }
docker info >/dev/null 2>&1 || { echo "docker daemon not running"; exit 1; }

echo "pulling pebble images..."
docker pull -q ghcr.io/letsencrypt/pebble:latest >/dev/null
docker pull -q ghcr.io/letsencrypt/pebble-challtestsrv:latest >/dev/null

HOST_IP=$(docker run --rm busybox sh -c 'nslookup host.docker.internal 2>/dev/null | awk "/^Address/ && !/#53/ {print \$2}" | tail -1')
[[ -z "$HOST_IP" ]] && HOST_IP=$(docker run --rm busybox getent hosts host.docker.internal | awk '{print $1}')
echo "host as seen from docker: $HOST_IP"

docker network create $NET >/dev/null 2>&1
docker run -d --name pebble-dns --network $NET ghcr.io/letsencrypt/pebble-challtestsrv:latest \
  -defaultIPv4 "$HOST_IP" -defaultIPv6 "" -dnsserver ":8053" -doh "" -http01 "" -https01 "" -tlsalpn01 "" -management ":8055" >/dev/null
sleep 1
if [[ "$(docker inspect -f '{{.State.Running}}' pebble-dns 2>/dev/null)" != "true" ]]; then
  echo "challtestsrv failed to start:"; docker logs pebble-dns 2>&1 | tail -20; exit 1
fi
# Pebble wants an ip:port for -dnsserver, not a container name.
DNS_IP=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' pebble-dns)
echo "challtestsrv at $DNS_IP:8053 answering A records with $HOST_IP"

cat > "$TMP/pebble-cfg/pebble.json" <<EOF
{ "pebble": {
    "listenAddress": "0.0.0.0:14000",
    "managementListenAddress": "0.0.0.0:15000",
    "certificate": "test/certs/localhost/cert.pem",
    "privateKey": "test/certs/localhost/key.pem",
    "httpPort": 18080,
    "tlsPort": 18443,
    "ocspResponderURL": "",
    "externalAccountBindingRequired": false
} }
EOF
docker run -d --name pebble --network $NET -p 14000:14000 -p 15000:15000 \
  -v "$TMP/pebble-cfg:/cfg:ro" \
  -e PEBBLE_VA_NOSLEEP=1 -e PEBBLE_VA_ALWAYS_VALID=0 \
  ghcr.io/letsencrypt/pebble:latest -config /cfg/pebble.json -dnsserver "$DNS_IP:8053" >/dev/null
docker cp pebble:/test/certs/pebble.minica.pem "$TMP/pebble-root.pem" >/dev/null
for _ in $(seq 1 50); do curl -fsSk https://localhost:14000/dir >/dev/null 2>&1 && break; sleep 0.2; done
curl -fsSk https://localhost:14000/dir >/dev/null || { echo "pebble not up"; docker logs pebble | tail; exit 1; }
echo "pebble directory up"

"$ORIGIN" -l 127.0.0.1:13000 >/dev/null 2>&1 & O1=$!

# One revp run per challenge type, each with fresh storage so a new cert is ordered.
#   http-01:     plain listener on Pebble's httpPort + TLS listener
#   tls-alpn-01: TLS listener on Pebble's tlsPort only; nothing on port 80
run_case() {
  local mode=$1 dir="$TMP/$1"
  mkdir -p "$dir"
  local plain_listener='[[listeners]]
addr = "0.0.0.0:18080"'
  [[ "$mode" == "tls-alpn-01" ]] && plain_listener=""
  echo
  echo "== $mode =="
  cat > "$dir/revp.toml" <<EOF
[server]
log_level = "info"
pid_file = "$dir/revp.pid"
upgrade_sock = "$dir/upgrade.sock"
$plain_listener
[[listeners]]
addr = "0.0.0.0:18443"
tls = true
[acme]
email = "test@$DOMAIN"
domains = ["$DOMAIN"]
directory = "https://localhost:14000/dir"
ca_root = "$TMP/pebble-root.pem"
storage = "$dir/acme"
challenge = "$mode"
[upstreams.o]
servers = ["127.0.0.1:13000"]
[[routes]]
upstream = "o"
EOF

  "$REVP" -c "$dir/revp.toml" >"$dir/revp.log" 2>&1 & REVP_PID=$!

  echo "waiting for certificate..."
  for _ in $(seq 1 150); do [[ -f "$dir/acme/$DOMAIN/fullchain.pem" ]] && break; sleep 0.2; done
  if [[ -f "$dir/acme/$DOMAIN/fullchain.pem" ]]; then ok "certificate obtained and stored"; else fail "certificate obtained" "$(tail -20 "$dir/revp.log")"; echo "--- pebble ---"; docker logs pebble 2>&1 | tail -20; kill "$REVP_PID" 2>/dev/null; REVP_PID=""; return; fi
  [[ -f "$dir/acme/account.json" ]] && ok "account credentials saved" || fail "account credentials saved"
  perm=$(stat -f %Lp "$dir/acme/$DOMAIN/privkey.pem" 2>/dev/null || stat -c %a "$dir/acme/$DOMAIN/privkey.pem")
  [[ "$perm" == "600" ]] && ok "private key is 0600" || fail "private key is 0600" "$perm"
  grep -q "acme: $DOMAIN: certificate installed" "$dir/revp.log" && ok "logged install" || fail "logged install"
  grep -q "ordering certificate ($mode)" "$dir/revp.log" && ok "ordered with $mode" || fail "ordered with $mode" "$(grep ordering "$dir/revp.log")"

  sleep 0.3
  served=$(echo | openssl s_client -connect 127.0.0.1:18443 -servername $DOMAIN 2>/dev/null | openssl x509 -noout -text 2>/dev/null | grep -E "Issuer:|DNS:")
  # ACME certs identify the host in the SAN; the subject is usually empty.
  [[ "$served" == *"DNS:$DOMAIN"* ]] && ok "TLS serves the ACME cert for $DOMAIN (SAN)" || fail "TLS serves the ACME cert" "$served"
  [[ "$served" == *"Pebble"* ]] && ok "issued by Pebble" || fail "issued by Pebble" "$served"
  # Pebble's intermediate is not in minica root chain for curl verification; accept -k but verify the name.
  body=$(curl -sSk --resolve "$DOMAIN:18443:127.0.0.1" "https://$DOMAIN:18443/")
  [[ "$body" == "Hello, world!" ]] && ok "HTTPS request proxied over the new cert" || fail "HTTPS request proxied" "$body"
  if [[ "$mode" == "http-01" ]]; then
    chal=$(curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:18080/.well-known/acme-challenge/nope)
    [[ "$chal" == "200" ]] && ok "unknown challenge token falls through to route" || fail "challenge fall-through" "$chal"
  else
    grep -q "answered tls-alpn-01 challenge" "$dir/revp.log" && ok "answered the challenge inside the TLS handshake" || fail "answered tls-alpn-01" "$(tail -20 "$dir/revp.log")"
    if lsof -nP -iTCP:18080 -sTCP:LISTEN >/dev/null 2>&1; then fail "nothing listening on the HTTP port"; else ok "nothing listening on the HTTP port"; fi
  fi

  kill "$REVP_PID" 2>/dev/null; wait "$REVP_PID" 2>/dev/null; REVP_PID=""
  [[ $FAIL -eq 0 ]] || tail -30 "$dir/revp.log"
}

for mode in ${MODES:-http-01 tls-alpn-01}; do run_case "$mode"; done

echo
echo "passed: $PASS  failed: $FAIL"
[[ $FAIL -eq 0 ]] || exit 1
