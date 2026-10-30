# revp

A lean, modular reverse proxy built on [Pingora](https://github.com/cloudflare/pingora).

The core does one thing: accept HTTP/1.1, HTTP/2, WebSocket, gRPC and raw TCP,
pick a backend, and forward. Everything else — metrics, access logs, rate
limiting, header rewriting, caching, compression, static files, ACME — is a **module**:
compiled in with a Cargo feature, switched on per route from config, and absent
from the hot path when a route doesn't use it.

```
                 ┌──────────────── revp ────────────────────────┐
clients ──TLS──► │ listeners ─► router ─► per-route pipeline ─► │ ──► upstream pools
                 │  (rustls)    host+path   [modules…]          │     (LB, health checks)
                 │                                              │
   SIGHUP/file ► │ hot reload: compile new generation, swap Arc │
      :9090    ► │ admin: /healthz /upstreams /metrics /reload  │
                 └──────────────────────────────────────────────┘
```

## Status

| Milestone | What | State |
|---|---|---|
| M0 | Pass-through proxy + benchmark harness | ✅ |
| M1 | TOML config, host/path routing, upstream pools (round-robin / random / consistent-hash, weights), TCP+HTTP health checks, TLS termination with SNI, timeouts | ✅ |
| M2 | Module API (phases, priorities, per-route pipelines), `metrics`, `accesslog`, `headers` | ✅ |
| M3 | Hot reload (file watch + SIGHUP + `POST /reload`), graceful shutdown, zero-downtime upgrade (Linux), systemd unit, Containerfile, Quadlet | ✅ |
| M4 | `ratelimit`, `cache`, `compress` | ✅ |
| M5 | ACME HTTP-01 (Let's Encrypt), L4 TCP proxy | ✅ |
| M6 | Hardening + benchmarks | ✅ benchmarks below; TLS-backend comparison deferred (see *Not done*) |

Tests: 14 unit tests (`cargo test --workspace`), 37 end-to-end checks
(`test/integration.sh`), and a live ACME issuance against Pebble in Docker
(`test/acme-pebble.sh`).

## Quick start

```bash
# needs: Rust 1.85+, cmake (for Pingora's zlib-ng)
cargo build --release                       # all modules
cargo build --release --no-default-features # bare proxy, ~8 MB
cargo build --release --no-default-features --features metrics,accesslog

./target/release/revp -c examples/revp.toml --check   # validate (compiles routes + module config)
./target/release/revp -c examples/revp.toml
./target/release/revp --modules                        # list compiled-in modules
```

Minimal config:

```toml
[[listeners]]
addr = "0.0.0.0:80"

[upstreams.app]
servers = ["10.0.0.11:8080", { addr = "10.0.0.12:8080", weight = 2 }]
health_check = { kind = "http", path = "/healthz", interval = "5s" }

[[routes]]
hosts = ["example.com"]
upstream = "app"
[routes.modules.metrics]
[routes.modules.ratelimit]
rps = 100
```

See [`examples/revp.toml`](examples/revp.toml) for every option with its default.

## Configuration model

- **`[[listeners]]`** — fixed at startup. Plain or `tls = true` (HTTP/2 via ALPN by default).
  Changing listeners needs a restart or `--upgrade`. `redirect_https = true` on a plain
  listener answers every routed request with a 308 to the same URL on the TLS listener
  (ACME challenges are still served there).
- **`[tls].certs`** — PEM cert/key pairs with the SNI names they serve
  (`*.example.com` wildcards supported). Reloaded live.
- **`[acme]`** — HTTP-01 against Let's Encrypt (or any directory URL). Certs land in
  `storage/<domain>/`, renew `renew_before_days` before expiry, and are installed into
  the TLS resolver without a restart. Needs a plain listener on :80.
- **`[upstreams.<name>]`** — static `ip:port` list with weights, LB algorithm, TLS to backend,
  `http_version = "2"` for gRPC/h2c, timeouts, and an optional health check.
- **`[[routes]]`** — matched by host (exact → `*.wildcard` → any), then longest path
  (`/api/*`, `/users/{id}`). `strip_prefix`, `host_header`, forwarded headers, and a
  `modules` table. `[defaults.modules]` applies to every route.
- **`[[l4]]`** — raw TCP listener → upstream, bytes spliced both ways.
- **`[admin]`** — `/healthz`, `/upstreams` (health per backend), `/config/generation`,
  `POST /reload`, plus module endpoints (`/metrics`). Bind it privately.

Hot reload: edit the file (or `kill -HUP`, or `POST /reload`). The new config is fully
compiled first; if anything is wrong the old one keeps serving and the error is logged.
In-flight requests finish on the generation they started with.

## Modules

| Module | Phase(s) | What it does |
|---|---|---|
| `metrics` | request, log | Per-route counters/histograms in atomics; Prometheus text on `/metrics` |
| `accesslog` | log | One JSON object per request, written by a dedicated thread via a bounded queue (drops, never blocks) |
| `ratelimit` | request | Token bucket per client IP or header value; sharded, ~lock-free; `429` + `Retry-After` |
| `headers` | upstream-request, response | Static set/remove, pre-parsed at load |
| `cache` | cache | In-memory LRU store (pingora-cache), honors `Cache-Control`, per-route TTL/key, request collapsing |
| `compress` | early-request | gzip / brotli / zstd via Pingora's streaming encoder, per-algorithm levels |
| `static` | request | Serves files from a directory: index files, SPA fallback, ETag/`304`, per-path headers, traversal and symlink guards. A route using it may omit `upstream`; with one, misses are proxied |

Writing your own: [`docs/MODULES.md`](docs/MODULES.md).

## Benchmarks

`bench/suite.sh 10s 64` — Apple M4 Max (14 cores), macOS, release build (`lto = "fat"`),
rustls, oha with 64 keep-alive connections for 10 s per scenario. Origin is
`tools/bench-upstream` (13-byte body unless noted). Load generator, origin and proxy all
share the same machine, so "origin direct" is the ceiling for the box, not a proxy number.

| scenario | req/s | p50 ms | p99 ms | RSS MB | note |
|---|---:|---:|---:|---:|---|
| origin direct | 120,111 | 0.528 | 0.615 | – | ceiling |
| **revp bare** | **61,176** | **1.040** | **1.329** | **28.5** | no modules |
| revp metrics + accesslog | 61,171 | 1.044 | 1.324 | 28.7 | ±0% |
| revp ratelimit | 61,276 | 1.026 | 1.759 | 28.9 | ±0% |
| revp headers | 62,200 | 1.016 | 1.691 | 28.5 | ±0% |
| revp **all modules** (metrics, accesslog, ratelimit, headers, compress) | 61,861 | 1.021 | 1.619 | 29.0 | ±0% |
| revp cache hit | 119,525 | 0.531 | 0.914 | 25.3 | = origin ceiling; origin untouched |
| revp https HTTP/1.1 | 61,540 | 1.028 | 1.550 | 30.3 | TLS 1.3, keep-alive |
| revp https HTTP/2 | 62,289 | 1.023 | 1.354 | 37.8 | |
| revp L4 TCP splice | 63,277 | 1.000 | 1.195 | 20.9 | |
| origin direct, 20 KB body | 105,321 | 0.599 | 0.748 | – | |
| revp bare, 20 KB body | 46,583 | 1.369 | 1.672 | 29.0 | |
| revp compress gzip, 20 KB body | 43,971 | 1.458 | 1.972 | 41.2 | −5.6% rps vs bare for gzip-6 |
| nginx 1.31 `proxy_pass` + keepalive (reference) | 56,991 | 1.092 | 1.544 | 9.1 | `access_log off`, `worker_processes auto` |

Reading it:

- **The module system costs nothing measurable.** Bare vs. five modules on the same route
  is within run-to-run noise. Phases are pre-split per route, so a route pays only for
  hooks it uses, and those hooks are atomics and a bounded channel push.
- **Cache hits run at the machine's ceiling** and never touch the origin.
- **TLS at steady state is free** with keep-alive. Handshakes are the real cost: with a
  new connection per request (`oha --disable-keepalive`, 64 conns, 5 s) revp does
  **10,623 req/s over plain TCP vs 6,123 req/s with a full TLS 1.3 handshake each**
  (ECDSA P-256, rustls/ring, load generator on the same cores). That number is what a
  BoringSSL build would have to beat to be worth its build complexity.
- **Compression is the one module with a real price** (CPU for gzip, ~12 MB more RSS for
  encoder buffers). It's per route for that reason.
- **vs nginx:** ~7% more req/s and lower p50/p99 at the cost of ~3× RSS (Pingora
  worker runtimes vs nginx's event loop). Your results will vary by kernel; Linux with
  `reuseport = true` is the intended target.

Re-run yourself: `cargo build --release -p revp -p bench-upstream && bench/suite.sh`.

## Deploying

- **systemd:** [`deploy/revp.service`](deploy/revp.service) (`systemctl reload revp` → SIGHUP).
- **Zero-downtime upgrade (Linux):** [`deploy/upgrade.sh`](deploy/upgrade.sh) — the new
  binary takes the listening sockets over a unix socket; the old one drains and exits.
- **Containers:** [`Containerfile`](Containerfile) (docker/podman, `--build-arg FEATURES=…`),
  [`deploy/revp.container`](deploy/revp.container) (Quadlet).

## Not done / deliberately deferred

- **TLS backend comparison.** Everything here is rustls (pure Rust, static-friendly).
  Pingora also supports BoringSSL; the cert resolver in `revp-core/src/tls.rs` is
  rustls-specific, so a BoringSSL build is a real piece of work, not a feature flag.
  Decide after measuring TLS handshake load in your environment.
- Rate limits are per instance (a shared/Redis backend fits behind `Limiter`).
- DNS-01 / wildcard ACME, DNS-based or Docker-label backend discovery, HTTP/3,
  WASM plugins — all out of scope by design.
- `bench-upstream` is a deliberately dumb origin; real backends will be the bottleneck
  long before the proxy is.
