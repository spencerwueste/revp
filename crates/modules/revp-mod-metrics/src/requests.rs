//! Recent-request log behind the admin dashboard.
//!
//! The newest N requests, kept in memory in a ring shared by every route and
//! config generation. Recording is opt-in per route (`metrics = { requests = N }`);
//! the ring's capacity is the largest N any route has asked for since startup.
//!
//! Queries copy the ring's `Arc`s under the lock and filter outside it, so a
//! dashboard poll holds the lock for a pointer copy, not a scan.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, LazyLock};

use base64::Engine as _;
use parking_lot::Mutex;
use serde::Serialize;

/// Longest query string kept per request; the rest is cut.
const MAX_QUERY: usize = 1024;
/// Longest JWT payload decoded for the user name.
const MAX_JWT_PAYLOAD: usize = 8192;
/// Points in a time series.
const SERIES_POINTS: u64 = 60;
/// Most rows one page may ask for.
const MAX_LIMIT: usize = 500;

#[derive(Debug, Serialize)]
pub struct Rec {
    pub id: u64,
    /// Unix milliseconds at request start.
    pub ts: u64,
    pub method: String,
    pub host: Option<String>,
    pub path: String,
    pub query: Option<String>,
    pub status: u16,
    pub duration_ms: f64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub route: String,
    /// Upstream name when proxied; otherwise the module that answered (`static`, `ratelimit`).
    pub service: String,
    pub upstream: Option<String>,
    pub client: Option<String>,
    pub user: Option<String>,
    /// `http`, `websocket`, `grpc` or `static`.
    pub kind: &'static str,
    pub by: Option<&'static str>,
    pub error: Option<String>,
    pub user_agent: Option<String>,
    pub referer: Option<String>,
    pub tls: bool,
    pub proto: &'static str,
    pub cache: Option<&'static str>,
}

struct Ring {
    cap: AtomicUsize,
    seq: AtomicU64,
    buf: Mutex<VecDeque<Arc<Rec>>>,
}

static RING: LazyLock<Ring> = LazyLock::new(|| Ring {
    cap: AtomicUsize::new(0),
    seq: AtomicU64::new(0),
    buf: Mutex::new(VecDeque::new()),
});

/// Grow the ring to at least `n` entries. Never shrinks: a reload that asks for
/// less keeps what is already held.
pub fn reserve(n: usize) {
    RING.cap.fetch_max(n, Relaxed);
}

pub fn next_id() -> u64 {
    RING.seq.fetch_add(1, Relaxed) + 1
}

pub fn push(rec: Rec) {
    let cap = RING.cap.load(Relaxed);
    if cap == 0 {
        return;
    }
    let rec = Arc::new(rec);
    // Freeing the evicted record's strings happens after the lock is released.
    let evicted = {
        let mut buf = RING.buf.lock();
        let evicted = if buf.len() >= cap { buf.pop_front() } else { None };
        while buf.len() >= cap {
            buf.pop_front();
        }
        buf.push_back(rec);
        evicted
    };
    drop(evicted);
}

fn snapshot() -> Vec<Arc<Rec>> {
    RING.buf.lock().iter().cloned().collect()
}

pub fn cut_query(q: &str) -> String {
    if q.len() <= MAX_QUERY {
        return q.to_string();
    }
    let mut end = MAX_QUERY;
    while !q.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &q[..end])
}

/// The first non-empty string claim of a `Bearer` JWT, in `claims` order.
///
/// Display only: the signature is NOT checked, so this names whoever the token
/// claims to be. The backend that verifies the token is what enforces access.
pub fn jwt_user(authorization: &[u8], claims: &[String]) -> Option<String> {
    let token = authorization
        .strip_prefix(b"Bearer ")
        .or_else(|| authorization.strip_prefix(b"bearer "))?;
    let mut parts = token.split(|b| *b == b'.');
    let (_header, payload, _sig) = (parts.next()?, parts.next()?, parts.next()?);
    if payload.len() > MAX_JWT_PAYLOAD || parts.next().is_some() {
        return None;
    }
    let payload = payload.strip_suffix(b"==").or_else(|| payload.strip_suffix(b"=")).unwrap_or(payload);
    let json = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&json).ok()?;
    claims.iter().find_map(|c| {
        v.get(c)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(200).collect())
    })
}

// ---- queries ----------------------------------------------------------------

fn params(query: &str) -> Vec<(String, String)> {
    let dec = |s: &str| {
        percent_encoding::percent_decode_str(&s.replace('+', " "))
            .decode_utf8_lossy()
            .into_owned()
    };
    query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (dec(k), dec(v))
        })
        .collect()
}

fn list(v: &str) -> Vec<String> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

enum StatusSel {
    Class(u16),
    Code(u16),
}

#[derive(Default)]
struct Filter {
    q: Option<String>,
    methods: Vec<String>,
    status: Vec<StatusSel>,
    routes: Vec<String>,
    services: Vec<String>,
    users: Vec<String>,
    kinds: Vec<String>,
    hosts: Vec<String>,
    since: Option<u64>,
    until: Option<u64>,
    min_ms: Option<f64>,
    errors: bool,
}

/// `-` in `user` selects requests with no user.
const ANON: &str = "-";

impl Filter {
    fn from(p: &[(String, String)]) -> Filter {
        let mut f = Filter::default();
        for (k, v) in p {
            match k.as_str() {
                "q" if !v.trim().is_empty() => f.q = Some(v.trim().to_lowercase()),
                "method" => f.methods = list(v).into_iter().map(|m| m.to_uppercase()).collect(),
                "status" => {
                    f.status = list(v)
                        .iter()
                        .filter_map(|s| {
                            let s = s.to_ascii_lowercase();
                            match s.strip_suffix("xx") {
                                Some(c) => c.parse().ok().map(StatusSel::Class),
                                None => s.parse().ok().map(StatusSel::Code),
                            }
                        })
                        .collect()
                }
                "route" => f.routes = list(v),
                "service" => f.services = list(v),
                "user" => f.users = list(v),
                "kind" => f.kinds = list(v),
                "host" => f.hosts = list(v),
                "since" => f.since = v.parse().ok(),
                "until" => f.until = v.parse().ok(),
                "min_ms" => f.min_ms = v.parse().ok(),
                "errors" => f.errors = v == "1" || v == "true",
                _ => {}
            }
        }
        f
    }

    fn matches(&self, r: &Rec) -> bool {
        let any = |set: &[String], v: &str| set.is_empty() || set.iter().any(|s| s == v);
        if !any(&self.methods, &r.method)
            || !any(&self.routes, &r.route)
            || !any(&self.services, &r.service)
            || !any(&self.kinds, r.kind)
            || !any(&self.hosts, r.host.as_deref().unwrap_or(""))
            || !any(&self.users, r.user.as_deref().unwrap_or(ANON))
        {
            return false;
        }
        if !self.status.is_empty()
            && !self.status.iter().any(|s| match s {
                StatusSel::Class(c) => r.status / 100 == *c,
                StatusSel::Code(c) => r.status == *c,
            })
        {
            return false;
        }
        if self.errors && r.status < 400 && r.error.is_none() {
            return false;
        }
        if self.since.is_some_and(|t| r.ts < t) || self.until.is_some_and(|t| r.ts > t) {
            return false;
        }
        if self.min_ms.is_some_and(|m| r.duration_ms < m) {
            return false;
        }
        if let Some(q) = &self.q {
            let hay = [
                Some(r.path.as_str()),
                r.query.as_deref(),
                r.host.as_deref(),
                r.user.as_deref(),
                Some(r.route.as_str()),
                Some(r.service.as_str()),
                r.client.as_deref(),
                r.error.as_deref(),
                r.user_agent.as_deref(),
            ];
            if !hay.iter().flatten().any(|h| contains_ci(h, q)) {
                return false;
            }
        }
        true
    }
}

/// `needle` is already lower-case.
fn contains_ci(hay: &str, needle: &str) -> bool {
    if hay.is_ascii() && needle.is_ascii() {
        hay.as_bytes()
            .windows(needle.len().max(1))
            .any(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
    } else {
        hay.to_lowercase().contains(needle)
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

fn sorted_durations<'a>(it: impl Iterator<Item = &'a Arc<Rec>>) -> Vec<f64> {
    let mut d: Vec<f64> = it.map(|r| r.duration_ms).collect();
    d.sort_by(|a, b| a.total_cmp(b));
    d
}

#[derive(Serialize)]
struct Summary {
    requests: usize,
    errors: usize,
    client_errors: usize,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    bytes_out: u64,
    first_ts: Option<u64>,
    last_ts: Option<u64>,
}

fn summary(rs: &[&Arc<Rec>]) -> Summary {
    let d = sorted_durations(rs.iter().copied());
    Summary {
        requests: rs.len(),
        errors: rs.iter().filter(|r| r.status >= 500 || r.error.is_some()).count(),
        client_errors: rs.iter().filter(|r| (400..500).contains(&r.status)).count(),
        p50_ms: percentile(&d, 0.50),
        p95_ms: percentile(&d, 0.95),
        p99_ms: percentile(&d, 0.99),
        bytes_out: rs.iter().map(|r| r.bytes_out).sum(),
        first_ts: rs.iter().map(|r| r.ts).min(),
        last_ts: rs.iter().map(|r| r.ts).max(),
    }
}

#[derive(Serialize, Default, Clone)]
struct Point {
    t: u64,
    #[serde(rename = "2xx")]
    c2: u32,
    #[serde(rename = "3xx")]
    c3: u32,
    #[serde(rename = "4xx")]
    c4: u32,
    #[serde(rename = "5xx")]
    c5: u32,
    other: u32,
    p50_ms: f64,
    p95_ms: f64,
}

/// Requests per status class and latency over time, oldest first.
fn series(rs: &[&Arc<Rec>], now: u64) -> (u64, Vec<Point>) {
    let Some(start) = rs.iter().map(|r| r.ts).min() else {
        return (1000, vec![]);
    };
    let span = now.saturating_sub(start).max(1);
    // Whole seconds, at least one, so a bucket's count reads as "per step".
    let step = span.div_ceil(SERIES_POINTS).div_ceil(1000).max(1) * 1000;
    let first = start / step * step;
    let n = ((now - first) / step + 1) as usize;
    let mut pts: Vec<Point> = (0..n).map(|i| Point { t: first + i as u64 * step, ..Default::default() }).collect();
    let mut durs: Vec<Vec<f64>> = vec![Vec::new(); n];
    for r in rs {
        let i = (((r.ts - first) / step) as usize).min(n - 1);
        let p = &mut pts[i];
        match r.status / 100 {
            2 => p.c2 += 1,
            3 => p.c3 += 1,
            4 => p.c4 += 1,
            5 => p.c5 += 1,
            _ => p.other += 1,
        }
        durs[i].push(r.duration_ms);
    }
    for (p, d) in pts.iter_mut().zip(durs.iter_mut()) {
        d.sort_by(|a, b| a.total_cmp(b));
        p.p50_ms = percentile(d, 0.50);
        p.p95_ms = percentile(d, 0.95);
    }
    (step, pts)
}

/// Counts per value over the whole ring, most frequent first; the dashboard's
/// filter options. Capped so a path-like value cannot flood it.
fn facet<'a>(all: &'a [Arc<Rec>], f: impl Fn(&'a Rec) -> &'a str) -> Vec<(&'a str, usize)> {
    let mut m: HashMap<&str, usize> = HashMap::new();
    for r in all {
        *m.entry(f(r)).or_default() += 1;
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.truncate(200);
    v
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `GET /dashboard/api/requests` — filtered, sorted page plus summary, series
/// and facets.
pub fn query_requests(query: &str) -> Vec<u8> {
    let p = params(query);
    let get = |k: &str| p.iter().find(|(pk, _)| pk == k).map(|(_, v)| v.as_str());
    let filter = Filter::from(&p);
    let all = snapshot();
    let mut hits: Vec<&Arc<Rec>> = all.iter().filter(|r| filter.matches(r)).collect();

    let desc = get("order") != Some("asc");
    match get("sort").unwrap_or("ts") {
        "duration" => hits.sort_by(|a, b| a.duration_ms.total_cmp(&b.duration_ms)),
        "status" => hits.sort_by_key(|r| r.status),
        "bytes" => hits.sort_by_key(|r| r.bytes_out),
        "path" => hits.sort_by(|a, b| a.path.cmp(&b.path)),
        "route" => hits.sort_by(|a, b| a.route.cmp(&b.route)),
        "service" => hits.sort_by(|a, b| a.service.cmp(&b.service)),
        "user" => hits.sort_by(|a, b| a.user.cmp(&b.user)),
        "method" => hits.sort_by(|a, b| a.method.cmp(&b.method)),
        _ => hits.sort_by_key(|r| r.id),
    }
    if desc {
        hits.reverse();
    }

    let limit = get("limit").and_then(|v| v.parse().ok()).unwrap_or(100).min(MAX_LIMIT);
    let offset = get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    let page: Vec<&Rec> = hits.iter().skip(offset).take(limit).map(|r| r.as_ref()).collect();
    let now = now_ms();
    let (step_ms, points) = series(&hits, now);

    let body = serde_json::json!({
        "recording": RING.cap.load(Relaxed) > 0,
        "capacity": RING.cap.load(Relaxed),
        "held": all.len(),
        "now": now,
        "matched": hits.len(),
        "summary": summary(&hits),
        "series": { "step_ms": step_ms, "points": points },
        "facets": {
            "method": facet(&all, |r| &r.method),
            "status": facet(&all, |r| match r.status / 100 { 1 => "1xx", 2 => "2xx", 3 => "3xx", 4 => "4xx", 5 => "5xx", _ => "other" }),
            "route": facet(&all, |r| &r.route),
            "service": facet(&all, |r| &r.service),
            "kind": facet(&all, |r| r.kind),
            "host": facet(&all, |r| r.host.as_deref().unwrap_or("")),
            "user": facet(&all, |r| r.user.as_deref().unwrap_or(ANON)),
        },
        "items": page,
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

#[derive(Serialize)]
struct UserRow<'a> {
    user: &'a str,
    #[serde(flatten)]
    summary: Summary,
    routes: Vec<(&'a str, usize)>,
    services: Vec<(&'a str, usize)>,
    clients: Vec<(&'a str, usize)>,
    user_agents: Vec<(&'a str, usize)>,
}

fn top<'a>(rs: &[&'a Arc<Rec>], f: impl Fn(&'a Rec) -> Option<&'a str>, n: usize) -> Vec<(&'a str, usize)> {
    let mut m: HashMap<&str, usize> = HashMap::new();
    for r in rs {
        if let Some(v) = f(r) {
            *m.entry(v).or_default() += 1;
        }
    }
    let mut v: Vec<_> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    v.truncate(n);
    v
}

/// `GET /dashboard/api/users` — one row per user seen in the ring (requests
/// without one are grouped as `-`). `?user=x` returns that user's row with a
/// time series, for the profile view. Other request filters apply too.
pub fn query_users(query: &str) -> Vec<u8> {
    let p = params(query);
    let filter = Filter::from(&p);
    let all = snapshot();
    let mut by_user: HashMap<&str, Vec<&Arc<Rec>>> = HashMap::new();
    for r in all.iter().filter(|r| filter.matches(r)) {
        by_user.entry(r.user.as_deref().unwrap_or(ANON)).or_default().push(r);
    }
    let mut rows: Vec<UserRow> = by_user
        .iter()
        .map(|(u, rs)| UserRow {
            user: u,
            summary: summary(rs),
            routes: top(rs, |r| Some(&r.route), 8),
            services: top(rs, |r| Some(&r.service), 8),
            clients: top(rs, |r| r.client.as_deref(), 5),
            user_agents: top(rs, |r| r.user_agent.as_deref(), 3),
        })
        .collect();
    rows.sort_by(|a, b| b.summary.last_ts.cmp(&a.summary.last_ts));

    let body = if filter.users.len() == 1 {
        let u = filter.users[0].as_str();
        let rs = by_user.get(u).cloned().unwrap_or_default();
        serde_json::json!({
            "user": rows.into_iter().find(|r| r.user == u),
            "series": series(&rs, now_ms()).1,
        })
    } else {
        serde_json::json!({ "users": rows })
    };
    serde_json::to_vec(&body).unwrap_or_default()
}

/// `GET /dashboard/api/request?id=N`
pub fn query_one(query: &str) -> Vec<u8> {
    let id: Option<u64> = params(query)
        .into_iter()
        .find(|(k, _)| k == "id")
        .and_then(|(_, v)| v.parse().ok());
    let rec = id.and_then(|id| RING.buf.lock().iter().find(|r| r.id == id).cloned());
    serde_json::to_vec(&rec.as_deref()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> Vec<u8> {
        let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("Bearer eyJhbGciOiJIUzI1NiJ9.{b}.sig").into_bytes()
    }

    #[test]
    fn user_from_jwt() {
        let claims = vec!["email".to_string(), "sub".to_string()];
        assert_eq!(jwt_user(&jwt(r#"{"sub":"u1","email":"a@b.c"}"#), &claims).as_deref(), Some("a@b.c"));
        assert_eq!(jwt_user(&jwt(r#"{"sub":"u1"}"#), &claims).as_deref(), Some("u1"));
        assert_eq!(jwt_user(&jwt(r#"{"email":""}"#), &claims), None);
        assert_eq!(jwt_user(b"Basic abc", &claims), None);
        assert_eq!(jwt_user(b"Bearer not-a-jwt", &claims), None);
        assert_eq!(jwt_user(b"Bearer a.!!!.c", &claims), None);
    }

    #[test]
    fn case_insensitive_search() {
        assert!(contains_ci("/API/Claims", "api/claims"));
        assert!(!contains_ci("/x", "y"));
        assert!(contains_ci("Ünïcode", "ünï"));
    }

    fn rec(path: &str, status: u16, user: Option<&str>, ms: f64) -> Rec {
        Rec {
            id: next_id(), ts: now_ms(), method: "GET".into(), host: Some("a.test".into()),
            path: path.into(), query: None, status, duration_ms: ms, bytes_in: 0, bytes_out: 10,
            route: "api".into(), service: "core".into(), upstream: None, client: None,
            user: user.map(String::from), kind: "http", by: None, error: None, user_agent: None,
            referer: None, tls: false, proto: "HTTP/1.1", cache: None,
        }
    }

    // One test touches the process-wide ring, so the assertions stay ordered.
    #[test]
    fn ring_queries() {
        reserve(3);
        push(rec("/old", 200, None, 1.0));
        push(rec("/api/claims", 200, Some("a@b.c"), 5.0));
        push(rec("/api/claims/9", 503, Some("a@b.c"), 50.0));
        push(rec("/api/x", 404, None, 2.0));
        let v: serde_json::Value = serde_json::from_slice(&query_requests("")).unwrap();
        assert_eq!(v["held"], 3, "capacity 3 drops the oldest");
        let v: serde_json::Value = serde_json::from_slice(&query_requests("q=CLAIMS&sort=duration&order=desc")).unwrap();
        assert_eq!(v["matched"], 2);
        assert_eq!(v["items"][0]["path"], "/api/claims/9");
        let v: serde_json::Value = serde_json::from_slice(&query_requests("status=4xx,503")).unwrap();
        assert_eq!(v["matched"], 2);
        let v: serde_json::Value = serde_json::from_slice(&query_requests("user=-")).unwrap();
        assert_eq!(v["matched"], 1);
        let v: serde_json::Value = serde_json::from_slice(&query_users("user=a%40b.c")).unwrap();
        assert_eq!(v["user"]["requests"], 2);
        assert_eq!(v["user"]["errors"], 1);
        let v: serde_json::Value = serde_json::from_slice(&query_users("")).unwrap();
        assert_eq!(v["users"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn query_cut() {
        assert_eq!(cut_query("a=1"), "a=1");
        assert!(cut_query(&"é".repeat(800)).ends_with('…'));
    }
}
