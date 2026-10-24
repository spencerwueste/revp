//! The module API.
//!
//! A module is a unit of behaviour attached to a route. Modules are compiled in
//! (Cargo features) and switched on per route from config. Each module declares
//! which request phases it participates in so that a route only pays for the
//! hooks it actually uses.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use pingora_cache::{CacheKey, RespCacheable};
use pingora_error::{Error, Result};
use pingora_http::{RequestHeader, ResponseHeader};
use pingora_proxy::Session;

use crate::config::{Config, RouteCfg};
use crate::ctx::Ctx;

/// Bit set of request phases a module hooks into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Phases(u8);

impl Phases {
    pub const NONE: Phases = Phases(0);
    /// `on_request` — after routing, before cache lookup / upstream.
    pub const REQUEST: Phases = Phases(1 << 0);
    /// `on_upstream_request` — mutate the request headed to the backend.
    pub const UPSTREAM_REQUEST: Phases = Phases(1 << 1);
    /// `on_response` — mutate response headers before they reach the client.
    pub const RESPONSE: Phases = Phases(1 << 2);
    /// `on_response_body` — observe/mutate body chunks (costs a call per chunk).
    pub const RESPONSE_BODY: Phases = Phases(1 << 3);
    /// `on_log` — after the response is done; for metrics and access logs.
    pub const LOG: Phases = Phases(1 << 4);
    /// Cache hooks. At most one module per route may own this phase.
    pub const CACHE: Phases = Phases(1 << 5);

    pub const fn union(self, o: Phases) -> Phases {
        Phases(self.0 | o.0)
    }
    pub const fn contains(self, o: Phases) -> bool {
        self.0 & o.0 == o.0
    }
}

impl std::ops::BitOr for Phases {
    type Output = Phases;
    fn bitor(self, rhs: Phases) -> Phases {
        self.union(rhs)
    }
}

/// What a request-phase hook wants the proxy to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep processing.
    Continue,
    /// The module already wrote a full response; stop here.
    Done,
}

/// Per-route module instance. Built once at config load, shared by all requests.
#[async_trait]
pub trait Module: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn phases(&self) -> Phases;

    async fn on_request(&self, _session: &mut Session, _cx: &mut Ctx) -> Result<Flow> {
        Ok(Flow::Continue)
    }

    async fn on_upstream_request(
        &self,
        _session: &mut Session,
        _req: &mut RequestHeader,
        _cx: &mut Ctx,
    ) -> Result<()> {
        Ok(())
    }

    async fn on_response(
        &self,
        _session: &mut Session,
        _resp: &mut ResponseHeader,
        _cx: &mut Ctx,
    ) -> Result<()> {
        Ok(())
    }

    fn on_response_body(
        &self,
        _session: &mut Session,
        _body: &mut Option<Bytes>,
        _end_of_stream: bool,
        _cx: &mut Ctx,
    ) -> Result<()> {
        Ok(())
    }

    async fn on_log(&self, _session: &mut Session, _err: Option<&Error>, _cx: &mut Ctx) {}

    // ---- cache phase (only for modules declaring Phases::CACHE) ----

    /// Decide whether to enable caching for this request (call `session.cache.enable(..)`).
    fn cache_enable(&self, _session: &mut Session, _cx: &mut Ctx) {}

    fn cache_key(&self, _session: &Session, _cx: &mut Ctx) -> Result<CacheKey> {
        Err(Error::new_str("cache_key not implemented"))
    }

    fn cache_response(
        &self,
        _session: &Session,
        _resp: &ResponseHeader,
        _cx: &mut Ctx,
    ) -> Result<RespCacheable> {
        Ok(RespCacheable::Uncacheable(
            pingora_cache::NoCacheReason::Custom("module"),
        ))
    }
}

/// Context handed to a factory when it builds a module for one route.
pub struct BuildCtx<'a> {
    pub config: &'a Config,
    pub route: &'a RouteCfg,
    pub route_name: &'a str,
}

/// Builds [`Module`] instances from their TOML config section.
pub trait ModuleFactory: Send + Sync {
    /// Key under `[routes.modules.<name>]`.
    fn name(&self) -> &'static str;

    /// Execution order within a phase. Lower runs first.
    /// Suggested bands: 0–99 observability, 100–199 access control, 200–299 rewriting,
    /// 300–399 caching, 400+ encoding.
    fn priority(&self) -> i32 {
        200
    }

    fn build(&self, cfg: &toml::Table, bcx: &BuildCtx<'_>) -> anyhow::Result<Arc<dyn Module>>;

    /// Extra admin endpoints this module wants served on the admin listener.
    fn admin_endpoints(&self) -> Vec<AdminEndpoint> {
        vec![]
    }
}

/// A handler on the admin listener, e.g. `/metrics`.
pub struct AdminEndpoint {
    pub path: &'static str,
    pub content_type: &'static str,
    pub handler: Arc<dyn Fn() -> Vec<u8> + Send + Sync>,
}

/// All compiled-in module factories, registered by the binary.
#[derive(Default)]
pub struct Registry {
    factories: Vec<Box<dyn ModuleFactory>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, f: impl ModuleFactory + 'static) -> &mut Self {
        self.factories.push(Box::new(f));
        self.factories.sort_by_key(|f| f.priority());
        self
    }

    pub fn get(&self, name: &str) -> Option<&dyn ModuleFactory> {
        self.factories.iter().find(|f| f.name() == name).map(|b| b.as_ref())
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.factories.iter().map(|f| f.name()).collect()
    }

    pub fn admin_endpoints(&self) -> Vec<AdminEndpoint> {
        self.factories.iter().flat_map(|f| f.admin_endpoints()).collect()
    }

    /// Build the ordered module list for a route from its effective `modules` table.
    pub fn build_for_route(
        &self,
        modules: &toml::Table,
        bcx: &BuildCtx<'_>,
    ) -> anyhow::Result<Vec<Arc<dyn Module>>> {
        let mut out = Vec::with_capacity(modules.len());
        // Iterate factories (already priority-sorted) so execution order is deterministic.
        for f in &self.factories {
            if let Some(v) = modules.get(f.name()) {
                let table = match v {
                    toml::Value::Table(t) => t.clone(),
                    toml::Value::Boolean(false) => continue,
                    toml::Value::Boolean(true) => toml::Table::new(),
                    other => anyhow::bail!(
                        "route {:?}: module {:?} config must be a table or bool, got {}",
                        bcx.route_name,
                        f.name(),
                        other.type_str()
                    ),
                };
                let m = f.build(&table, bcx).map_err(|e| {
                    anyhow::anyhow!("route {:?}: module {:?}: {e}", bcx.route_name, f.name())
                })?;
                out.push(m);
            }
        }
        for key in modules.keys() {
            if self.get(key).is_none() {
                anyhow::bail!(
                    "route {:?}: unknown module {key:?} (compiled-in: {:?})",
                    bcx.route_name,
                    self.names()
                );
            }
        }
        Ok(out)
    }
}

/// Modules of one route, pre-split per phase so the hot path never filters.
#[derive(Default)]
pub struct Pipeline {
    pub all: Box<[Arc<dyn Module>]>,
    pub request: Box<[Arc<dyn Module>]>,
    pub upstream_request: Box<[Arc<dyn Module>]>,
    pub response: Box<[Arc<dyn Module>]>,
    pub response_body: Box<[Arc<dyn Module>]>,
    pub log: Box<[Arc<dyn Module>]>,
    pub cache: Option<Arc<dyn Module>>,
    pub phases: Phases,
}

impl Pipeline {
    pub fn new(modules: Vec<Arc<dyn Module>>) -> anyhow::Result<Self> {
        let pick = |p: Phases| -> Box<[Arc<dyn Module>]> {
            modules
                .iter()
                .filter(|m| m.phases().contains(p))
                .cloned()
                .collect()
        };
        let cache: Vec<_> = modules
            .iter()
            .filter(|m| m.phases().contains(Phases::CACHE))
            .cloned()
            .collect();
        if cache.len() > 1 {
            anyhow::bail!(
                "only one cache module per route, found {:?}",
                cache.iter().map(|m| m.name()).collect::<Vec<_>>()
            );
        }
        let phases = modules
            .iter()
            .fold(Phases::NONE, |acc, m| acc | m.phases());
        Ok(Self {
            request: pick(Phases::REQUEST),
            upstream_request: pick(Phases::UPSTREAM_REQUEST),
            response: pick(Phases::RESPONSE),
            response_body: pick(Phases::RESPONSE_BODY),
            log: pick(Phases::LOG),
            cache: cache.into_iter().next(),
            all: modules.into_boxed_slice(),
            phases,
        })
    }
}
