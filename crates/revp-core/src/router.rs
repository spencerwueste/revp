//! Host + path routing, compiled once per config generation.
//!
//! Lookup is: host (exact → wildcard → any) then path via a radix tree.
//! No allocation on the hot path for already-lowercase hosts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use http::header::HeaderName;
use pingora_http::RequestHeader;

use crate::config::{normalize_path_pattern, Config, RouteCfg};
use crate::module::Pipeline;

/// How the `Host` header is set on the upstream request.
#[derive(Debug, Clone)]
pub enum HostHeader {
    Preserve,
    Upstream,
    Literal(http::HeaderValue),
}

/// A compiled route.
pub struct Route {
    pub name: String,
    pub upstream: String,
    pub strip_prefix: Option<String>,
    pub host_header: HostHeader,
    pub forwarded_headers: bool,
    pub downstream_read_timeout: Option<Duration>,
    pub downstream_write_timeout: Option<Duration>,
    pub pipeline: Pipeline,
}

/// Radix-tree path matcher with prefix fallback.
///
/// matchit alone will not fall back from `/api/{*rest}` to `/{*rest}` for `/api/x/y`
/// when the deeper subtree has no match, which is exactly what a proxy expects.
/// On a miss we walk up one segment at a time to the nearest enclosing catch-all.
#[derive(Default)]
struct PathRouter {
    inner: matchit::Router<Arc<Route>>,
    has_catch_all: bool,
}

impl PathRouter {
    fn insert(&mut self, pattern: &str, route: Arc<Route>) -> Result<(), matchit::InsertError> {
        self.inner.insert(pattern, route.clone())?;
        if let Some(prefix) = pattern.strip_suffix("/{*rest}") {
            self.has_catch_all = true;
            // `/api/*` should also match `/api` and `/api/`; ignore conflicts with
            // explicit routes for those paths.
            if prefix.is_empty() {
                let _ = self.inner.insert("/", route.clone());
            } else {
                let _ = self.inner.insert(prefix, route.clone());
                let _ = self.inner.insert(format!("{prefix}/"), route);
            }
        }
        Ok(())
    }

    #[inline]
    fn at(&self, path: &str) -> Option<Arc<Route>> {
        if let Ok(m) = self.inner.at(path) {
            return Some(m.value.clone());
        }
        if !self.has_catch_all {
            return None;
        }
        // Walk up: /a/b/c -> /a/b/ -> /a/ -> /
        let mut p = path.trim_end_matches('/');
        while let Some(i) = p.rfind('/') {
            if let Ok(m) = self.inner.at(&p[..=i]) {
                return Some(m.value.clone());
            }
            if i == 0 {
                break;
            }
            p = &p[..i];
        }
        None
    }
}

#[derive(Default)]
pub struct Router {
    exact: HashMap<String, PathRouter>,
    /// `("example.com", router)` matches `*.example.com`.
    wildcard: Vec<(String, PathRouter)>,
    any: Option<PathRouter>,
    pub routes: Vec<Arc<Route>>,
}

impl Router {
    pub fn build(
        cfg: &Config,
        mut build_pipeline: impl FnMut(&RouteCfg, &str) -> anyhow::Result<Pipeline>,
    ) -> anyhow::Result<Router> {
        let mut router = Router::default();
        for (i, rc) in cfg.routes.iter().enumerate() {
            let name = rc.name.clone().unwrap_or_else(|| format!("route-{i}"));
            let pipeline = build_pipeline(rc, &name)?;
            let host_header = match rc.host_header.as_str() {
                "preserve" => HostHeader::Preserve,
                "upstream" => HostHeader::Upstream,
                lit => HostHeader::Literal(
                    http::HeaderValue::from_str(lit)
                        .with_context(|| format!("route {name:?}: bad host_header"))?,
                ),
            };
            let route = Arc::new(Route {
                name: name.clone(),
                upstream: rc.upstream.clone(),
                strip_prefix: rc.strip_prefix.clone(),
                host_header,
                forwarded_headers: rc.forwarded_headers,
                downstream_read_timeout: rc.downstream_read_timeout,
                downstream_write_timeout: rc.downstream_write_timeout,
                pipeline,
            });
            router.routes.push(route.clone());

            let insert = |pr: &mut PathRouter| -> anyhow::Result<()> {
                for p in &rc.paths {
                    let pat = normalize_path_pattern(p);
                    pr.insert(&pat, route.clone())
                        .with_context(|| format!("route {name:?}: path {p:?}"))?;
                }
                Ok(())
            };

            if rc.hosts.is_empty() {
                insert(router.any.get_or_insert_with(PathRouter::default))?;
                continue;
            }
            for h in &rc.hosts {
                let h = h.to_ascii_lowercase();
                if let Some(suffix) = h.strip_prefix("*.") {
                    let pr = match router.wildcard.iter_mut().find(|(s, _)| s == suffix) {
                        Some((_, pr)) => pr,
                        None => {
                            router.wildcard.push((suffix.to_string(), PathRouter::default()));
                            &mut router.wildcard.last_mut().unwrap().1
                        }
                    };
                    insert(pr)?;
                } else {
                    insert(router.exact.entry(h).or_default())?;
                }
            }
        }
        // Longer wildcard suffixes first so `*.a.b.com` beats `*.b.com`.
        router
            .wildcard
            .sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        Ok(router)
    }

    /// Resolve the request to a route. `None` → 404.
    pub fn match_request(&self, req: &RequestHeader) -> Option<Arc<Route>> {
        let host = request_host(req);
        let path = req.uri.path();

        if let Some(h) = host {
            if let Some(r) = self.lookup_host(h, path) {
                return Some(r);
            }
        }
        self.any.as_ref().and_then(|pr| pr.at(path))
    }

    fn lookup_host(&self, host: &str, path: &str) -> Option<Arc<Route>> {
        // Fast path: no uppercase → look up without allocating.
        let lowered;
        let host = if host.bytes().any(|b| b.is_ascii_uppercase()) {
            lowered = host.to_ascii_lowercase();
            lowered.as_str()
        } else {
            host
        };
        if let Some(pr) = self.exact.get(host) {
            if let Some(r) = pr.at(path) {
                return Some(r);
            }
        }
        if !self.wildcard.is_empty() {
            if let Some((_, rest)) = host.split_once('.') {
                for (suffix, pr) in &self.wildcard {
                    if rest == suffix {
                        if let Some(r) = pr.at(path) {
                            return Some(r);
                        }
                    }
                }
            }
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }
}

static HOST: HeaderName = http::header::HOST;

/// Host without port, from `:authority` (HTTP/2) or `Host` (HTTP/1.1).
pub fn request_host(req: &RequestHeader) -> Option<&str> {
    let raw = match req.uri.host() {
        Some(h) => Some(h),
        None => req.headers.get(&HOST).and_then(|v| v.to_str().ok()),
    }?;
    Some(strip_port(raw))
}

fn strip_port(h: &str) -> &str {
    if h.starts_with('[') {
        // [v6]:port
        return h.split(']').next().map(|s| &h[..s.len() + 1]).unwrap_or(h);
    }
    match h.rfind(':') {
        Some(i) if h[i + 1..].bytes().all(|b| b.is_ascii_digit()) => &h[..i],
        _ => h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(routes: &str) -> Config {
        Config::parse(&format!(
            r#"
            [[listeners]]
            addr = "0.0.0.0:8080"
            [upstreams.a]
            servers = ["127.0.0.1:1"]
            [upstreams.b]
            servers = ["127.0.0.1:2"]
            {routes}
            "#
        ))
        .unwrap()
    }

    fn req(host: &str, path: &str) -> RequestHeader {
        let mut r = RequestHeader::build("GET", path.as_bytes(), None).unwrap();
        r.insert_header("host", host).unwrap();
        r
    }

    fn build(c: &Config) -> Router {
        Router::build(c, |_, _| Ok(Pipeline::default())).unwrap()
    }

    #[test]
    fn exact_then_wildcard_then_any() {
        let c = cfg(r#"
            [[routes]]
            name = "exact"
            hosts = ["api.example.com"]
            upstream = "a"
            [[routes]]
            name = "wild"
            hosts = ["*.example.com"]
            upstream = "b"
            [[routes]]
            name = "any"
            upstream = "a"
        "#);
        let r = build(&c);
        assert_eq!(r.match_request(&req("api.example.com", "/x")).unwrap().name, "exact");
        assert_eq!(r.match_request(&req("API.Example.com:8080", "/x")).unwrap().name, "exact");
        assert_eq!(r.match_request(&req("foo.example.com", "/x")).unwrap().name, "wild");
        assert_eq!(r.match_request(&req("other.org", "/x")).unwrap().name, "any");
    }

    #[test]
    fn path_precedence() {
        let c = cfg(r#"
            [[routes]]
            name = "api"
            paths = ["/api/*"]
            upstream = "a"
            [[routes]]
            name = "root"
            upstream = "b"
        "#);
        let r = build(&c);
        assert_eq!(r.match_request(&req("h", "/api/users")).unwrap().name, "api");
        assert_eq!(r.match_request(&req("h", "/api/")).unwrap().name, "api");
        assert_eq!(r.match_request(&req("h", "/api")).unwrap().name, "api");
        assert_eq!(r.match_request(&req("h", "/apix")).unwrap().name, "root");
        assert_eq!(r.match_request(&req("h", "/")).unwrap().name, "root");
        assert_eq!(r.match_request(&req("h", "/deep/er/path")).unwrap().name, "root");
    }

    #[test]
    fn falls_back_to_enclosing_catch_all() {
        let c = cfg(r#"
            [[routes]]
            name = "users"
            paths = ["/api/v1/users/*"]
            upstream = "a"
            [[routes]]
            name = "api"
            paths = ["/api/*"]
            upstream = "b"
            [[routes]]
            name = "exact"
            paths = ["/health"]
            upstream = "b"
        "#);
        let r = build(&c);
        assert_eq!(r.match_request(&req("h", "/api/v1/users/42")).unwrap().name, "users");
        assert_eq!(r.match_request(&req("h", "/api/v1/other")).unwrap().name, "api");
        assert_eq!(r.match_request(&req("h", "/api/v1/users")).unwrap().name, "users");
        assert_eq!(r.match_request(&req("h", "/health")).unwrap().name, "exact");
        assert!(r.match_request(&req("h", "/health/x")).is_none());
        assert!(r.match_request(&req("h", "/other")).is_none());
    }

    #[test]
    fn no_match_is_none() {
        let c = cfg(r#"
            [[routes]]
            hosts = ["only.example.com"]
            upstream = "a"
        "#);
        let r = build(&c);
        assert!(r.match_request(&req("nope.com", "/")).is_none());
    }

    #[test]
    fn port_stripping() {
        assert_eq!(strip_port("a.com:443"), "a.com");
        assert_eq!(strip_port("a.com"), "a.com");
        assert_eq!(strip_port("[::1]:80"), "[::1]");
    }
}
