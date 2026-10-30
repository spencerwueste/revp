//! Admin listener: `/healthz`, `/upstreams`, `/config/generation`, plus module
//! endpoints such as `/metrics`. Bind it to localhost or a private interface.

use std::sync::Arc;

use async_trait::async_trait;
use http::{Response, StatusCode};
use pingora_core::apps::http_app::ServeHttp;
use pingora_core::protocols::http::ServerSession;

use crate::module::AdminEndpoint;
use crate::runtime::Shared;

pub struct AdminApp {
    shared: Arc<Shared>,
    endpoints: Vec<AdminEndpoint>,
}

impl AdminApp {
    pub fn new(shared: Arc<Shared>, endpoints: Vec<AdminEndpoint>) -> Self {
        Self { shared, endpoints }
    }

    fn respond(status: StatusCode, ctype: &str, body: Vec<u8>) -> Response<Vec<u8>> {
        Response::builder()
            .status(status)
            .header("content-type", ctype)
            .header("content-length", body.len())
            .header("cache-control", "no-store")
            .body(body)
            .unwrap()
    }
}

#[async_trait]
impl ServeHttp for AdminApp {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        let path = session.req_header().uri.path();
        match path {
            "/healthz" => Self::respond(StatusCode::OK, "text/plain", b"ok\n".to_vec()),
            "/upstreams" => {
                let rt = self.shared.runtime();
                let mut out = String::new();
                for (name, pool) in &rt.upstreams {
                    for (addr, weight, healthy) in pool.status() {
                        out.push_str(&format!(
                            "{name}\t{addr}\tweight={weight}\t{}\n",
                            if healthy { "healthy" } else { "unhealthy" }
                        ));
                    }
                }
                Self::respond(StatusCode::OK, "text/plain", out.into_bytes())
            }
            "/config/generation" => {
                let rt = self.shared.runtime();
                Self::respond(
                    StatusCode::OK,
                    "text/plain",
                    format!("{}\n", rt.generation).into_bytes(),
                )
            }
            "/reload" if session.req_header().method == http::Method::POST => {
                match self.shared.reload().await {
                    Ok(gen) => Self::respond(
                        StatusCode::OK,
                        "text/plain",
                        format!("reloaded: generation {gen}\n").into_bytes(),
                    ),
                    Err(e) => Self::respond(
                        StatusCode::BAD_REQUEST,
                        "text/plain",
                        format!("reload failed: {e:#}\n").into_bytes(),
                    ),
                }
            }
            _ => {
                if let Some(ep) = self.endpoints.iter().find(|e| e.path == path) {
                    let query = session.req_header().uri.query().unwrap_or("");
                    let body = (ep.handler)(query);
                    return Self::respond(StatusCode::OK, ep.content_type, body);
                }
                let mut list = String::from("revp admin\n\n/healthz\n/upstreams\n/config/generation\nPOST /reload\n");
                for ep in &self.endpoints {
                    list.push_str(ep.path);
                    list.push('\n');
                }
                Self::respond(StatusCode::NOT_FOUND, "text/plain", list.into_bytes())
            }
        }
    }
}
