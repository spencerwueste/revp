//! The `ProxyHttp` implementation: routes requests, drives module pipelines,
//! and hands Pingora an upstream peer.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use http::header::HOST;
use http::HeaderValue;

const X_FORWARDED_FOR: http::HeaderName = http::HeaderName::from_static("x-forwarded-for");
use log::debug;
use pingora_cache::{CacheKey, RespCacheable};
use pingora_core::upstreams::peer::HttpPeer;
use pingora_error::{Error, ErrorType::HTTPStatus, Result};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::{ProxyHttp, Session};

use crate::acme::CHALLENGE_PREFIX;
use crate::config::HashOn;
use crate::ctx::Ctx;
use crate::module::Flow;
use crate::router::{request_host, HostHeader, Route};
use crate::runtime::Shared;

pub struct RevpProxy {
    pub shared: Arc<Shared>,
}

impl RevpProxy {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    #[inline]
    fn route(cx: &Ctx) -> Result<&Arc<Route>> {
        cx.route
            .as_ref()
            .ok_or_else(|| Error::explain(HTTPStatus(500), "request has no route"))
    }

    fn client_ip(session: &Session) -> Option<std::net::IpAddr> {
        session
            .client_addr()
            .and_then(|a| a.as_inet())
            .map(|a| a.ip())
    }

    fn is_tls(session: &Session) -> bool {
        session
            .digest()
            .and_then(|d| d.ssl_digest.as_ref())
            .is_some()
    }

    async fn serve_acme_challenge(&self, session: &mut Session, cx: &mut Ctx) -> Result<bool> {
        let path = session.req_header().uri.path();
        let token = &path[CHALLENGE_PREFIX.len()..];
        let Some(key_auth) = self.shared.acme.get(token) else {
            return Ok(false);
        };
        let mut h = ResponseHeader::build(200, Some(2))?;
        h.insert_header("content-type", "text/plain")?;
        h.insert_header("content-length", key_auth.len().to_string())?;
        session.write_response_header(Box::new(h), false).await?;
        session
            .write_response_body(Some(Bytes::from(key_auth)), true)
            .await?;
        cx.status = 200;
        cx.short_circuit = Some("acme");
        Ok(true)
    }
}

#[async_trait]
impl ProxyHttp for RevpProxy {
    type CTX = Ctx;

    fn new_ctx(&self) -> Ctx {
        Ctx::new()
    }

    /// Routing happens here so that modules can configure Pingora's built-in
    /// downstream modules (compression) before those see the request.
    async fn early_request_filter(&self, session: &mut Session, cx: &mut Ctx) -> Result<()> {
        let route = {
            let rt = self.shared.runtime();
            rt.router.match_request(session.req_header())
        };
        let Some(route) = route else { return Ok(()) };
        if let Some(t) = route.downstream_read_timeout {
            session.set_read_timeout(Some(t));
        }
        if let Some(t) = route.downstream_write_timeout {
            session.set_write_timeout(Some(t));
        }
        for m in route.pipeline.early_request.iter() {
            m.on_early_request(session, cx);
        }
        cx.route = Some(route);
        Ok(())
    }

    async fn request_filter(&self, session: &mut Session, cx: &mut Ctx) -> Result<bool> {
        if session.req_header().uri.path().starts_with(CHALLENGE_PREFIX)
            && !self.shared.acme.is_empty()
            && self.serve_acme_challenge(session, cx).await?
        {
            return Ok(true);
        }

        let Some(route) = cx.route.clone() else {
            cx.status = 404;
            cx.short_circuit = Some("no_route");
            session.respond_error(404).await?;
            return Ok(true);
        };

        for m in route.pipeline.request.iter() {
            if m.on_request(session, cx).await? == Flow::Done {
                if cx.status == 0 {
                    cx.status = session
                        .response_written()
                        .map(|r| r.status.as_u16())
                        .unwrap_or(0);
                }
                cx.short_circuit.get_or_insert(m.name());
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn request_cache_filter(&self, session: &mut Session, cx: &mut Ctx) -> Result<()> {
        if let Some(m) = cx.route.clone().and_then(|r| r.pipeline.cache.clone()) {
            m.cache_enable(session, cx);
        }
        Ok(())
    }

    fn cache_key_callback(&self, session: &Session, cx: &mut Ctx) -> Result<CacheKey> {
        match cx.route.clone().and_then(|r| r.pipeline.cache.clone()) {
            Some(m) => m.cache_key(session, cx),
            None => Err(Error::new_str("cache enabled without cache module")),
        }
    }

    fn response_cache_filter(
        &self,
        session: &Session,
        resp: &ResponseHeader,
        cx: &mut Ctx,
    ) -> Result<RespCacheable> {
        match cx.route.clone().and_then(|r| r.pipeline.cache.clone()) {
            Some(m) => m.cache_response(session, resp, cx),
            None => Ok(RespCacheable::Uncacheable(
                pingora_cache::NoCacheReason::Custom("no cache module"),
            )),
        }
    }

    async fn upstream_peer(&self, session: &mut Session, cx: &mut Ctx) -> Result<Box<HttpPeer>> {
        let route = Self::route(cx)?.clone();
        let rt = self.shared.runtime();
        let pool = rt
            .upstream(&route.upstream)
            .ok_or_else(|| Error::explain(HTTPStatus(503), "upstream missing"))?;

        let backend = match pool.hash_on() {
            HashOn::Ip => match Self::client_ip(session) {
                Some(std::net::IpAddr::V4(a)) => pool.select(&a.octets()),
                Some(std::net::IpAddr::V6(a)) => pool.select(&a.octets()),
                None => pool.select(b""),
            },
            HashOn::Path => pool.select(session.req_header().uri.path().as_bytes()),
            HashOn::Uri => pool.select(
                session
                    .req_header()
                    .uri
                    .path_and_query()
                    .map(|p| p.as_str().as_bytes())
                    .unwrap_or(b""),
            ),
            HashOn::Header(h) => pool.select(session.get_header_bytes(h.as_str())),
        }
        .ok_or_else(|| Error::explain(HTTPStatus(503), "no healthy upstream"))?;

        cx.upstream_addr = backend.addr.as_inet().copied();
        debug!("route {:?} -> {}", route.name, backend.addr);
        Ok(pool.peer(&backend))
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        req: &mut RequestHeader,
        cx: &mut Ctx,
    ) -> Result<()> {
        let route = Self::route(cx)?.clone();

        if let Some(prefix) = &route.strip_prefix {
            if let Some(rest) = req.uri.path().strip_prefix(prefix.as_str()) {
                let rest = if rest.is_empty() || !rest.starts_with('/') {
                    format!("/{rest}")
                } else {
                    rest.to_string()
                };
                let new_pq = match req.uri.query() {
                    Some(q) => format!("{rest}?{q}"),
                    None => rest,
                };
                req.set_raw_path(new_pq.as_bytes())?;
            }
        }

        match &route.host_header {
            HostHeader::Preserve => {}
            HostHeader::Upstream => {
                if let Some(addr) = cx.upstream_addr {
                    req.insert_header(HOST, addr.to_string())?;
                }
            }
            HostHeader::Literal(v) => {
                req.insert_header(HOST, v.clone())?;
            }
        }

        if route.forwarded_headers {
            if let Some(ip) = Self::client_ip(session) {
                let ip_s = ip.to_string();
                let xff = match session.req_header().headers.get(&X_FORWARDED_FOR) {
                    Some(existing) => {
                        let mut v = existing.as_bytes().to_vec();
                        v.extend_from_slice(b", ");
                        v.extend_from_slice(ip_s.as_bytes());
                        HeaderValue::from_bytes(&v).unwrap_or_else(|_| HeaderValue::from_str(&ip_s).unwrap())
                    }
                    None => HeaderValue::from_str(&ip_s).unwrap(),
                };
                req.insert_header(X_FORWARDED_FOR, xff)?;
                req.insert_header("x-real-ip", ip_s)?;
            }
            req.insert_header(
                "x-forwarded-proto",
                if Self::is_tls(session) { "https" } else { "http" },
            )?;
            if let Some(h) = request_host(session.req_header()) {
                req.insert_header("x-forwarded-host", h.to_string())?;
            }
        }

        for m in route.pipeline.upstream_request.iter() {
            m.on_upstream_request(session, req, cx).await?;
        }
        Ok(())
    }

    async fn response_filter(
        &self,
        session: &mut Session,
        resp: &mut ResponseHeader,
        cx: &mut Ctx,
    ) -> Result<()> {
        let Some(route) = cx.route.clone() else { return Ok(()) };
        for m in route.pipeline.response.iter() {
            m.on_response(session, resp, cx).await?;
        }
        Ok(())
    }

    fn response_body_filter(
        &self,
        session: &mut Session,
        body: &mut Option<Bytes>,
        end_of_stream: bool,
        cx: &mut Ctx,
    ) -> Result<Option<std::time::Duration>> {
        if let Some(route) = cx.route.clone() {
            for m in route.pipeline.response_body.iter() {
                m.on_response_body(session, body, end_of_stream, cx)?;
            }
        }
        Ok(None)
    }

    /// Clients hanging up mid-request is normal traffic, not an error worth a log line.
    fn suppress_error_log(&self, _session: &Session, _cx: &Ctx, e: &Error) -> bool {
        use pingora_error::ErrorSource::Downstream;
        use pingora_error::ErrorType::{ConnectionClosed, ReadError, WriteError};
        e.esource() == &Downstream
            && matches!(e.etype(), ConnectionClosed | ReadError | WriteError)
    }

    async fn logging(&self, session: &mut Session, e: Option<&Error>, cx: &mut Ctx) {
        if let Some(r) = session.response_written() {
            cx.status = r.status.as_u16();
        } else if cx.status == 0 {
            cx.status = e
                .map(|e| match e.etype() {
                    HTTPStatus(c) => *c,
                    _ => 0,
                })
                .unwrap_or(0);
        }
        let Some(route) = cx.route.clone() else { return };
        for m in route.pipeline.log.iter() {
            m.on_log(session, e, cx).await;
        }
    }
}
