//! Metrics module.
//!
//! Counts requests, status classes, methods, latency histogram, bytes and
//! in-flight requests per route using plain atomics (no locks on the hot path).
//! Rendered in Prometheus text format on the admin listener at `/metrics`.
//! Counters live in a process-wide registry so they survive config reloads.
//!
//! Optionally it also keeps the most recent requests in memory and serves a
//! dashboard over them on the admin listener at `/dashboard`:
//!
//! ```toml
//! [defaults.modules.metrics]
//! requests = 50000                         # keep the newest 50k; 0 (default) = off
//! user_claims = ["email", "name", "sub"]   # JWT claims tried, in order, for the user
//! ```

pub mod requests;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use parking_lot::RwLock;
use revp_core::http::Version;
use revp_core::{
    async_trait, parse_module_config, AdminEndpoint, BuildCtx, Ctx, Error, Flow, Module,
    ModuleFactory, Phases, Result, Session,
};
use serde::Deserialize;

/// The dashboard, built from `dashboard/` (`npm run build`) into one file.
static DASHBOARD_HTML: &[u8] = include_bytes!("../dashboard/dist/index.html");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Cfg {
    /// Recent requests to keep for the dashboard. 0 = don't record this route.
    requests: usize,
    /// JWT claims tried in order to name the user of a request.
    user_claims: Vec<String>,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            requests: 0,
            user_claims: vec!["email".into(), "preferred_username".into(), "name".into(), "sub".into()],
        }
    }
}

/// Histogram bucket upper bounds in microseconds (0.5ms .. 10s).
const BUCKETS_US: [u64; 14] = [
    500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000, 250_000, 500_000, 1_000_000,
    2_500_000, 5_000_000, 10_000_000,
];
const METHODS: [&str; 8] = ["GET", "HEAD", "POST", "PUT", "DELETE", "PATCH", "OPTIONS", "other"];
const CACHE_STATES: [&str; 7] = ["hit", "miss", "stale", "expired", "revalidated", "bypass", "other"];

#[derive(Default)]
pub struct RouteMetrics {
    /// index 0 = unknown/0, 1..=5 = 1xx..5xx
    by_class: [AtomicU64; 6],
    by_method: [AtomicU64; 8],
    hist: [AtomicU64; 14],
    hist_sum_us: AtomicU64,
    hist_count: AtomicU64,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    inflight: AtomicI64,
    upstream_errors: AtomicU64,
    short_circuited: AtomicU64,
    cache: [AtomicU64; 7],
}

impl RouteMetrics {
    #[inline]
    fn observe(&self, status: u16, method: &str, us: u64, bytes_in: u64, bytes_out: u64) {
        let class = (status / 100) as usize;
        self.by_class[if class <= 5 { class } else { 0 }].fetch_add(1, Relaxed);
        let mi = METHODS.iter().position(|m| *m == method).unwrap_or(7);
        self.by_method[mi].fetch_add(1, Relaxed);
        for (i, b) in BUCKETS_US.iter().enumerate() {
            if us <= *b {
                self.hist[i].fetch_add(1, Relaxed);
                break;
            }
        }
        self.hist_sum_us.fetch_add(us, Relaxed);
        self.hist_count.fetch_add(1, Relaxed);
        self.bytes_in.fetch_add(bytes_in, Relaxed);
        self.bytes_out.fetch_add(bytes_out, Relaxed);
    }
}

static REGISTRY: LazyLock<RwLock<BTreeMap<String, Arc<RouteMetrics>>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));
static STARTED: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Get (or create) the counters for a route name.
pub fn route_metrics(name: &str) -> Arc<RouteMetrics> {
    if let Some(m) = REGISTRY.read().get(name) {
        return m.clone();
    }
    REGISTRY
        .write()
        .entry(name.to_string())
        .or_insert_with(|| Arc::new(RouteMetrics::default()))
        .clone()
}

/// Render everything in Prometheus text exposition format.
pub fn render() -> Vec<u8> {
    let reg = REGISTRY.read();
    let mut o = String::with_capacity(4096 + reg.len() * 2048);

    let _ = writeln!(o, "# HELP revp_build_info Build information.\n# TYPE revp_build_info gauge");
    let _ = writeln!(o, "revp_build_info{{version=\"{}\"}} 1", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(o, "# HELP revp_uptime_seconds Seconds since the metrics module first ran.\n# TYPE revp_uptime_seconds gauge");
    let _ = writeln!(o, "revp_uptime_seconds {}", STARTED.elapsed().as_secs());

    let _ = writeln!(o, "# HELP revp_requests_total Requests by status class.\n# TYPE revp_requests_total counter");
    for (name, m) in reg.iter() {
        for (i, c) in m.by_class.iter().enumerate() {
            let v = c.load(Relaxed);
            if v > 0 || i == 2 {
                let class = if i == 0 { "0xx".to_string() } else { format!("{i}xx") };
                let _ = writeln!(o, "revp_requests_total{{route=\"{name}\",class=\"{class}\"}} {v}");
            }
        }
    }

    let _ = writeln!(o, "# HELP revp_requests_by_method_total Requests by method.\n# TYPE revp_requests_by_method_total counter");
    for (name, m) in reg.iter() {
        for (i, c) in m.by_method.iter().enumerate() {
            let v = c.load(Relaxed);
            if v > 0 {
                let _ = writeln!(o, "revp_requests_by_method_total{{route=\"{name}\",method=\"{}\"}} {v}", METHODS[i]);
            }
        }
    }

    let _ = writeln!(o, "# HELP revp_request_duration_seconds Time from request start to response end.\n# TYPE revp_request_duration_seconds histogram");
    for (name, m) in reg.iter() {
        let mut cum = 0u64;
        for (i, b) in BUCKETS_US.iter().enumerate() {
            cum += m.hist[i].load(Relaxed);
            let _ = writeln!(o, "revp_request_duration_seconds_bucket{{route=\"{name}\",le=\"{}\"}} {cum}", *b as f64 / 1e6);
        }
        let count = m.hist_count.load(Relaxed);
        let _ = writeln!(o, "revp_request_duration_seconds_bucket{{route=\"{name}\",le=\"+Inf\"}} {count}");
        let _ = writeln!(o, "revp_request_duration_seconds_sum{{route=\"{name}\"}} {}", m.hist_sum_us.load(Relaxed) as f64 / 1e6);
        let _ = writeln!(o, "revp_request_duration_seconds_count{{route=\"{name}\"}} {count}");
    }

    let simple = [
        ("revp_inflight_requests", "Requests currently being processed.", "gauge"),
        ("revp_request_bytes_total", "Request body bytes received from clients.", "counter"),
        ("revp_response_bytes_total", "Response body bytes sent to clients.", "counter"),
        ("revp_upstream_errors_total", "Requests that failed with a proxy/upstream error.", "counter"),
        ("revp_short_circuited_total", "Requests answered by a module without reaching upstream.", "counter"),
    ];
    for (metric, help, ty) in simple {
        let _ = writeln!(o, "# HELP {metric} {help}\n# TYPE {metric} {ty}");
        for (name, m) in reg.iter() {
            let v: i64 = match metric {
                "revp_inflight_requests" => m.inflight.load(Relaxed),
                "revp_request_bytes_total" => m.bytes_in.load(Relaxed) as i64,
                "revp_response_bytes_total" => m.bytes_out.load(Relaxed) as i64,
                "revp_upstream_errors_total" => m.upstream_errors.load(Relaxed) as i64,
                _ => m.short_circuited.load(Relaxed) as i64,
            };
            let _ = writeln!(o, "{metric}{{route=\"{name}\"}} {v}");
        }
    }

    let _ = writeln!(o, "# HELP revp_cache_total Cache outcome for requests on routes with caching.\n# TYPE revp_cache_total counter");
    for (name, m) in reg.iter() {
        for (i, c) in m.cache.iter().enumerate() {
            let v = c.load(Relaxed);
            if v > 0 {
                let _ = writeln!(o, "revp_cache_total{{route=\"{name}\",state=\"{}\"}} {v}", CACHE_STATES[i]);
            }
        }
    }
    o.into_bytes()
}

pub struct MetricsModule {
    m: Arc<RouteMetrics>,
    record: bool,
    user_claims: Vec<String>,
}

fn proto(v: Version) -> &'static str {
    match v {
        Version::HTTP_09 => "HTTP/0.9",
        Version::HTTP_10 => "HTTP/1.0",
        Version::HTTP_11 => "HTTP/1.1",
        Version::HTTP_2 => "HTTP/2",
        Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/?",
    }
}

impl MetricsModule {
    fn record(&self, s: &Session, e: Option<&Error>, cx: &Ctx, us: u64) {
        let req = s.req_header();
        let header = |n: &str| {
            req.headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.chars().take(300).collect::<String>())
        };
        let upgrade = req
            .headers
            .get("upgrade")
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
        let grpc = req
            .headers
            .get("content-type")
            .is_some_and(|v| v.as_bytes().starts_with(b"application/grpc"));
        let kind = if upgrade {
            "websocket"
        } else if grpc {
            "grpc"
        } else if cx.short_circuit == Some("static") {
            "static"
        } else {
            "http"
        };
        let upstream_name = cx.route.as_ref().map(|r| r.upstream.as_str()).unwrap_or("");
        let service = match cx.short_circuit {
            Some(by) => by.to_string(),
            None if !upstream_name.is_empty() => upstream_name.to_string(),
            None => "-".to_string(),
        };
        let ts = std::time::SystemTime::now()
            .checked_sub(cx.elapsed())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        requests::push(requests::Rec {
            id: requests::next_id(),
            ts,
            method: req.method.as_str().to_string(),
            host: revp_core::router::request_host(req).map(String::from),
            path: req.uri.path().to_string(),
            query: req.uri.query().map(requests::cut_query),
            status: cx.status,
            duration_ms: us as f64 / 1000.0,
            bytes_in: s.body_bytes_read() as u64,
            bytes_out: s.body_bytes_sent() as u64,
            route: cx.route_name().to_string(),
            service,
            upstream: cx.upstream_addr.map(|a| a.to_string()),
            client: s.client_addr().and_then(|a| a.as_inet()).map(|a| a.ip().to_string()),
            user: req
                .headers
                .get("authorization")
                .and_then(|v| requests::jwt_user(v.as_bytes(), &self.user_claims)),
            kind,
            by: cx.short_circuit,
            error: e.map(|e| e.to_string()),
            user_agent: header("user-agent"),
            referer: header("referer"),
            tls: s.digest().and_then(|d| d.ssl_digest.as_ref()).is_some(),
            proto: proto(req.version),
            cache: s.cache.enabled().then(|| s.cache.phase().as_str()),
        });
    }
}

#[async_trait]
impl Module for MetricsModule {
    fn name(&self) -> &'static str {
        "metrics"
    }
    fn phases(&self) -> Phases {
        Phases::REQUEST | Phases::LOG
    }

    async fn on_request(&self, _s: &mut Session, _cx: &mut Ctx) -> Result<Flow> {
        self.m.inflight.fetch_add(1, Relaxed);
        Ok(Flow::Continue)
    }

    async fn on_log(&self, s: &mut Session, e: Option<&Error>, cx: &mut Ctx) {
        self.m.inflight.fetch_sub(1, Relaxed);
        let us = cx.elapsed().as_micros() as u64;
        self.m.observe(
            cx.status,
            s.req_header().method.as_str(),
            us,
            s.body_bytes_read() as u64,
            s.body_bytes_sent() as u64,
        );
        if e.is_some() {
            self.m.upstream_errors.fetch_add(1, Relaxed);
        }
        if cx.short_circuit.is_some() {
            self.m.short_circuited.fetch_add(1, Relaxed);
        }
        if s.cache.enabled() {
            use revp_core::pingora_cache::CachePhase::*;
            let i = match s.cache.phase() {
                Hit => 0,
                Miss => 1,
                Stale | StaleUpdating => 2,
                Expired => 3,
                Revalidated | RevalidatedNoCache(_) => 4,
                Bypass => 5,
                _ => 6,
            };
            self.m.cache[i].fetch_add(1, Relaxed);
        }
        if self.record {
            self.record(s, e, cx, us);
        }
    }
}

pub struct MetricsFactory;

impl ModuleFactory for MetricsFactory {
    fn name(&self) -> &'static str {
        "metrics"
    }
    fn priority(&self) -> i32 {
        0
    }
    fn build(
        &self,
        cfg: &revp_core::toml::Table,
        bcx: &BuildCtx<'_>,
    ) -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(cfg)?;
        requests::reserve(c.requests);
        Ok(Arc::new(MetricsModule {
            m: route_metrics(bcx.route_name),
            record: c.requests > 0,
            user_claims: c.user_claims,
        }))
    }
    fn admin_endpoints(&self) -> Vec<AdminEndpoint> {
        let html = || -> Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync> {
            Arc::new(|_| DASHBOARD_HTML.to_vec())
        };
        let json = "application/json";
        vec![
            AdminEndpoint {
                path: "/metrics",
                content_type: "text/plain; version=0.0.4; charset=utf-8",
                handler: Arc::new(|_| render()),
            },
            AdminEndpoint { path: "/dashboard", content_type: "text/html; charset=utf-8", handler: html() },
            AdminEndpoint { path: "/dashboard/", content_type: "text/html; charset=utf-8", handler: html() },
            AdminEndpoint {
                path: "/dashboard/api/requests",
                content_type: json,
                handler: Arc::new(requests::query_requests),
            },
            AdminEndpoint {
                path: "/dashboard/api/users",
                content_type: json,
                handler: Arc::new(requests::query_users),
            },
            AdminEndpoint {
                path: "/dashboard/api/request",
                content_type: json,
                handler: Arc::new(requests::query_one),
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_and_render() {
        let m = route_metrics("t");
        m.observe(200, "GET", 1_200, 10, 100);
        m.observe(503, "POST", 2_000_000, 0, 0);
        let out = String::from_utf8(render()).unwrap();
        assert!(out.contains("revp_requests_total{route=\"t\",class=\"2xx\"} 1"));
        assert!(out.contains("revp_requests_total{route=\"t\",class=\"5xx\"} 1"));
        assert!(out.contains("revp_requests_by_method_total{route=\"t\",method=\"POST\"} 1"));
        assert!(out.contains("revp_request_duration_seconds_bucket{route=\"t\",le=\"0.0025\"} 1"));
        assert!(out.contains("revp_request_duration_seconds_count{route=\"t\"} 2"));
        assert!(out.contains("revp_response_bytes_total{route=\"t\"} 100"));
    }
}
