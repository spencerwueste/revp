//! Cache module: in-memory response cache using pingora-cache.
//!
//! One shared in-memory store with LRU eviction; per-route TTL/key policy.
//! Honors `Cache-Control` from the origin by default (`no-store`, `private`,
//! `max-age`, `s-maxage`); falls back to the configured `ttl`.
//!
//! ```toml
//! [routes.modules.cache]
//! ttl = "60s"                     # when the origin sets no max-age
//! max_size = "256MB"              # shared store size (first route wins)
//! max_file_size = "4MB"
//! lock_timeout = "5s"             # collapse concurrent misses for one key
//! methods = ["GET", "HEAD"]
//! key = "uri"                     # or "path" (ignores the query string)
//! honor_cache_control = true
//! cache_authorized = false
//! stale_while_revalidate = "0s"
//! stale_if_error = "0s"
//! ```

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::{Duration, SystemTime};

use revp_core::pingora_cache::cache_control::CacheControl;
use revp_core::pingora_cache::eviction::simple_lru::Manager as LruManager;
use revp_core::pingora_cache::filters::request_cacheable;
use revp_core::pingora_cache::lock::{CacheKeyLockImpl, CacheLock};
use revp_core::pingora_cache::{CacheKey, CacheMeta, MemCache, NoCacheReason, RespCacheable};
use revp_core::{
    async_trait, parse_module_config, BuildCtx, Ctx, Error, Module, ModuleFactory, Phases,
    ResponseHeader, Result, Session,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Cfg {
    #[serde(with = "humantime_serde")]
    ttl: Duration,
    /// Per-status TTL overrides, e.g. `{ "404" = "10s" }`.
    ttl_by_status: HashMap<String, String>,
    max_size: String,
    max_file_size: String,
    #[serde(with = "humantime_serde")]
    lock_timeout: Duration,
    methods: Vec<String>,
    key: String,
    honor_cache_control: bool,
    cache_authorized: bool,
    #[serde(with = "humantime_serde")]
    stale_while_revalidate: Duration,
    #[serde(with = "humantime_serde")]
    stale_if_error: Duration,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(60),
            ttl_by_status: HashMap::new(),
            max_size: "256MB".into(),
            max_file_size: "4MB".into(),
            lock_timeout: Duration::from_secs(5),
            methods: vec!["GET".into(), "HEAD".into()],
            key: "uri".into(),
            honor_cache_control: true,
            cache_authorized: false,
            stale_while_revalidate: Duration::ZERO,
            stale_if_error: Duration::ZERO,
        }
    }
}

fn parse_size(s: &str) -> revp_core::anyhow::Result<usize> {
    let t = s.trim().to_ascii_uppercase();
    let (num, mult) = if let Some(n) = t.strip_suffix("GB") {
        (n, 1usize << 30)
    } else if let Some(n) = t.strip_suffix("MB") {
        (n, 1 << 20)
    } else if let Some(n) = t.strip_suffix("KB") {
        (n, 1 << 10)
    } else if let Some(n) = t.strip_suffix('B') {
        (n, 1)
    } else {
        (t.as_str(), 1)
    };
    let n: f64 = num
        .trim()
        .parse()
        .map_err(|_| revp_core::anyhow::anyhow!("bad size {s:?}"))?;
    Ok((n * mult as f64) as usize)
}

static STORAGE: LazyLock<MemCache> = LazyLock::new(MemCache::new);
static EVICTION: OnceLock<LruManager> = OnceLock::new();
static LOCK: OnceLock<Box<CacheLock>> = OnceLock::new();

/// Statuses that are cacheable by default (RFC 9110 §15.1) when no explicit TTL applies.
const DEFAULT_CACHEABLE: [u16; 9] = [200, 203, 204, 206, 300, 301, 308, 404, 410];

pub struct Cache {
    ttl: Duration,
    ttl_by_status: HashMap<u16, Duration>,
    max_file_size: usize,
    methods: Vec<revp_core::http::Method>,
    key_path_only: bool,
    honor_cc: bool,
    cache_authorized: bool,
    swr: u32,
    sie: u32,
    tag: String,
}

impl Cache {
    fn fresh_for(&self, resp: &ResponseHeader) -> Option<Duration> {
        let status = resp.status.as_u16();
        if self.honor_cc {
            if let Some(cc) = CacheControl::from_resp_headers(resp) {
                if cc.no_store() || cc.private() || cc.no_cache() {
                    return None;
                }
                let secs = cc
                    .s_maxage()
                    .ok()
                    .flatten()
                    .or_else(|| cc.max_age().ok().flatten());
                if let Some(s) = secs {
                    return if s == 0 { None } else { Some(Duration::from_secs(s as u64)) };
                }
            }
        }
        if let Some(d) = self.ttl_by_status.get(&status) {
            return if d.is_zero() { None } else { Some(*d) };
        }
        if DEFAULT_CACHEABLE.contains(&status) && !self.ttl.is_zero() {
            return Some(self.ttl);
        }
        None
    }
}

#[async_trait]
impl Module for Cache {
    fn name(&self) -> &'static str {
        "cache"
    }
    fn phases(&self) -> Phases {
        Phases::CACHE
    }

    fn cache_enable(&self, s: &mut Session, _cx: &mut Ctx) {
        let req = s.req_header();
        if !self.methods.contains(&req.method) {
            return;
        }
        if !request_cacheable(req) {
            return;
        }
        if !self.cache_authorized && req.headers.contains_key("authorization") {
            return;
        }
        let lock: Option<&'static CacheKeyLockImpl> = LOCK.get().map(|b| b.as_ref() as _);
        s.cache.enable(
            &*STORAGE,
            EVICTION.get().map(|e| e as _),
            None,
            lock,
            None,
        );
        if self.max_file_size > 0 {
            s.cache.set_max_file_size_bytes(self.max_file_size);
        }
    }

    fn cache_key(&self, s: &Session, _cx: &mut Ctx) -> Result<CacheKey> {
        let req = s.req_header();
        let host = revp_core::router::request_host(req).unwrap_or("");
        let pq = if self.key_path_only {
            req.uri.path()
        } else {
            req.uri
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or_else(|| req.uri.path())
        };
        // Length-prefix the components so "a"+"bc" never equals "ab"+"c".
        let mut primary = Vec::with_capacity(host.len() + pq.len() + 8);
        primary.extend_from_slice(&(host.len() as u32).to_le_bytes());
        primary.extend_from_slice(host.as_bytes());
        primary.extend_from_slice(pq.as_bytes());
        Ok(CacheKey::new(primary, self.tag.clone()))
    }

    fn cache_response(
        &self,
        _s: &Session,
        resp: &ResponseHeader,
        _cx: &mut Ctx,
    ) -> Result<RespCacheable> {
        match self.fresh_for(resp) {
            None => Ok(RespCacheable::Uncacheable(NoCacheReason::OriginNotCache)),
            Some(fresh) => {
                let now = SystemTime::now();
                let meta = CacheMeta::new(now + fresh, now, self.swr, self.sie, resp.clone());
                Ok(RespCacheable::Cacheable(meta))
            }
        }
    }
}

pub struct CacheFactory;

impl ModuleFactory for CacheFactory {
    fn name(&self) -> &'static str {
        "cache"
    }
    fn priority(&self) -> i32 {
        300
    }
    fn build(
        &self,
        cfg: &revp_core::toml::Table,
        bcx: &BuildCtx<'_>,
    ) -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(cfg)?;
        let max_size = parse_size(&c.max_size)?;
        let max_file_size = parse_size(&c.max_file_size)?;
        if EVICTION.set(LruManager::new(max_size)).is_err() {
            log::debug!("cache: store already initialised; max_size for {:?} ignored", bcx.route_name);
        }
        let _ = LOCK.set(CacheLock::new_boxed(c.lock_timeout));

        let mut ttl_by_status = HashMap::new();
        for (k, v) in &c.ttl_by_status {
            let status: u16 = k.parse().map_err(|_| revp_core::anyhow::anyhow!("ttl_by_status key {k:?}"))?;
            let d: Duration = humantime_serde::re::humantime::parse_duration(v)
                .map_err(|e| revp_core::anyhow::anyhow!("ttl_by_status[{k}] = {v:?}: {e}"))?;
            ttl_by_status.insert(status, d);
        }
        let methods = c
            .methods
            .iter()
            .map(|m| {
                revp_core::http::Method::from_bytes(m.as_bytes())
                    .map_err(|e| revp_core::anyhow::anyhow!("method {m:?}: {e}"))
            })
            .collect::<revp_core::anyhow::Result<Vec<_>>>()?;
        let key_path_only = match c.key.as_str() {
            "uri" => false,
            "path" => true,
            other => revp_core::anyhow::bail!("cache.key must be `uri` or `path`, got {other:?}"),
        };
        Ok(Arc::new(Cache {
            ttl: c.ttl,
            ttl_by_status,
            max_file_size,
            methods,
            key_path_only,
            honor_cc: c.honor_cache_control,
            cache_authorized: c.cache_authorized,
            swr: c.stale_while_revalidate.as_secs() as u32,
            sie: c.stale_if_error.as_secs() as u32,
            tag: bcx.route_name.to_string(),
        }))
    }
}

#[allow(dead_code)]
fn _unused(_: Error) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("4MB").unwrap(), 4 << 20);
        assert_eq!(parse_size("1.5GB").unwrap(), (1.5 * (1u64 << 30) as f64) as usize);
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert!(parse_size("lots").is_err());
    }
}
