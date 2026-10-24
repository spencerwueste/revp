//! Headers module: static set/remove on the upstream request and the response.
//!
//! ```toml
//! [routes.modules.headers]
//! request.set    = { x-api-version = "2" }
//! request.remove = ["cookie"]
//! response.set   = { x-frame-options = "DENY" }
//! response.remove = ["server", "x-powered-by"]
//! ```

use std::collections::BTreeMap;
use std::sync::Arc;

use revp_core::http::{HeaderName, HeaderValue};
use revp_core::{
    async_trait, parse_module_config, BuildCtx, Ctx, Module, ModuleFactory, Phases, RequestHeader,
    ResponseHeader, Result, Session,
};
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct Side {
    set: BTreeMap<String, String>,
    remove: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
struct Cfg {
    request: Side,
    response: Side,
}

struct Compiled {
    set: Vec<(HeaderName, HeaderValue)>,
    remove: Vec<HeaderName>,
}

impl Compiled {
    fn from(side: &Side) -> revp_core::anyhow::Result<Self> {
        let mut set = Vec::with_capacity(side.set.len());
        for (k, v) in &side.set {
            set.push((
                HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| revp_core::anyhow::anyhow!("header name {k:?}: {e}"))?,
                HeaderValue::from_str(v)
                    .map_err(|e| revp_core::anyhow::anyhow!("header value for {k:?}: {e}"))?,
            ));
        }
        let mut remove = Vec::with_capacity(side.remove.len());
        for k in &side.remove {
            remove.push(
                HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| revp_core::anyhow::anyhow!("header name {k:?}: {e}"))?,
            );
        }
        Ok(Self { set, remove })
    }
    fn is_empty(&self) -> bool {
        self.set.is_empty() && self.remove.is_empty()
    }
}

pub struct Headers {
    request: Compiled,
    response: Compiled,
    phases: Phases,
}

#[async_trait]
impl Module for Headers {
    fn name(&self) -> &'static str {
        "headers"
    }
    fn phases(&self) -> Phases {
        self.phases
    }

    async fn on_upstream_request(
        &self,
        _s: &mut Session,
        req: &mut RequestHeader,
        _cx: &mut Ctx,
    ) -> Result<()> {
        for n in &self.request.remove {
            req.remove_header(n);
        }
        for (n, v) in &self.request.set {
            req.insert_header(n.clone(), v.clone())?;
        }
        Ok(())
    }

    async fn on_response(
        &self,
        _s: &mut Session,
        resp: &mut ResponseHeader,
        _cx: &mut Ctx,
    ) -> Result<()> {
        for n in &self.response.remove {
            resp.remove_header(n);
        }
        for (n, v) in &self.response.set {
            resp.insert_header(n.clone(), v.clone())?;
        }
        Ok(())
    }
}

pub struct HeadersFactory;

impl ModuleFactory for HeadersFactory {
    fn name(&self) -> &'static str {
        "headers"
    }
    fn priority(&self) -> i32 {
        250
    }
    fn build(
        &self,
        cfg: &revp_core::toml::Table,
        _bcx: &BuildCtx<'_>,
    ) -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(cfg)?;
        let request = Compiled::from(&c.request)?;
        let response = Compiled::from(&c.response)?;
        let mut phases = Phases::NONE;
        if !request.is_empty() {
            phases = phases | Phases::UPSTREAM_REQUEST;
        }
        if !response.is_empty() {
            phases = phases | Phases::RESPONSE;
        }
        Ok(Arc::new(Headers {
            request,
            response,
            phases,
        }))
    }
}
