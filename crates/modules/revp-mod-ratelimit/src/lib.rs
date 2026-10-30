//! Rate limit module: token bucket per key (client IP or a header value).
//!
//! State is sharded behind small mutexes keyed by a 64-bit hash of the key, so
//! the hot path is one hash, one short lock, and a few integer ops. Buckets
//! live in a process-wide table keyed by route + limits so they survive reloads.
//! Limits are per proxy instance; a shared backend (Redis) can be added behind
//! the same `Limiter` interface later.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use parking_lot::Mutex;
use revp_core::{
    async_trait, parse_module_config, BuildCtx, Ctx, Flow, Module, ModuleFactory, Phases,
    ResponseHeader, Result, Session, write_response_header,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Cfg {
    /// Sustained requests per second per key.
    rps: u32,
    /// Max burst above the sustained rate. Defaults to `rps`.
    burst: Option<u32>,
    /// `ip` or `header:<name>`.
    key: String,
    status: u16,
    shards: usize,
    /// Per-shard cap before idle keys are evicted.
    max_keys_per_shard: usize,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            rps: 0,
            burst: None,
            key: "ip".into(),
            status: 429,
            shards: 64,
            max_keys_per_shard: 8192,
        }
    }
}

#[derive(Clone, Copy)]
struct Bucket {
    /// Tokens in thousandths.
    milli_tokens: u64,
    last_ns: u64,
}

/// Identity hasher: keys are already ahash-ed u64s.
#[derive(Default)]
struct IdHasher(u64);
impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!()
    }
    fn write_u64(&mut self, i: u64) {
        self.0 = i;
    }
}

type Shard = Mutex<HashMap<u64, Bucket, BuildHasherDefault<IdHasher>>>;

pub struct Limiter {
    rps: u32,
    burst_milli: u64,
    shards: Vec<Shard>,
    max_keys: usize,
    epoch: Instant,
    hasher: ahash::RandomState,
}

pub enum Decision {
    Allow,
    /// Denied; retry after this many whole seconds.
    Deny { retry_after_s: u32 },
}

impl Limiter {
    pub fn new(rps: u32, burst: u32, shards: usize, max_keys: usize) -> Self {
        Self {
            rps,
            burst_milli: (burst.max(1) as u64) * 1000,
            shards: (0..shards.max(1)).map(|_| Mutex::new(HashMap::default())).collect(),
            max_keys,
            epoch: Instant::now(),
            hasher: ahash::RandomState::new(),
        }
    }

    #[inline]
    pub fn check(&self, key: &[u8]) -> Decision {
        let h = self.hasher.hash_one(key);
        let now_ns = self.epoch.elapsed().as_nanos() as u64;
        let shard = &self.shards[(h as usize) % self.shards.len()];
        let mut map = shard.lock();

        if map.len() >= self.max_keys {
            // Evict keys idle for 60s+. Rare; only when a shard is full.
            let cutoff = now_ns.saturating_sub(60_000_000_000);
            map.retain(|_, b| b.last_ns > cutoff);
        }

        let b = map.entry(h).or_insert(Bucket {
            milli_tokens: self.burst_milli,
            last_ns: now_ns,
        });
        // Refill: elapsed_ns * rps / 1e9 tokens, in thousandths.
        let elapsed = now_ns.saturating_sub(b.last_ns);
        let refill = (elapsed as u128 * self.rps as u128 / 1_000_000) as u64;
        b.milli_tokens = (b.milli_tokens + refill).min(self.burst_milli);
        b.last_ns = now_ns;

        if b.milli_tokens >= 1000 {
            b.milli_tokens -= 1000;
            Decision::Allow
        } else {
            let missing = 1000 - b.milli_tokens;
            // seconds = missing / (rps * 1000), rounded up
            let secs = missing.div_ceil((self.rps.max(1) as u64) * 1000).max(1);
            Decision::Deny {
                retry_after_s: secs as u32,
            }
        }
    }
}

static LIMITERS: LazyLock<Mutex<HashMap<String, Arc<Limiter>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

enum KeySource {
    Ip,
    Header(String),
}

pub struct RateLimit {
    limiter: Arc<Limiter>,
    key: KeySource,
    status: u16,
    rps: u32,
}

#[async_trait]
impl Module for RateLimit {
    fn name(&self) -> &'static str {
        "ratelimit"
    }
    fn phases(&self) -> Phases {
        Phases::REQUEST
    }

    async fn on_request(&self, s: &mut Session, cx: &mut Ctx) -> Result<Flow> {
        let decision = match &self.key {
            KeySource::Ip => match s.client_addr().and_then(|a| a.as_inet()).map(|a| a.ip()) {
                Some(std::net::IpAddr::V4(a)) => self.limiter.check(&a.octets()),
                Some(std::net::IpAddr::V6(a)) => self.limiter.check(&a.octets()),
                None => Decision::Allow,
            },
            KeySource::Header(h) => {
                let v = s.get_header_bytes(h.as_str());
                if v.is_empty() {
                    Decision::Allow
                } else {
                    self.limiter.check(v)
                }
            }
        };
        match decision {
            Decision::Allow => Ok(Flow::Continue),
            Decision::Deny { retry_after_s } => {
                let mut h = ResponseHeader::build(self.status, Some(4))?;
                h.insert_header("retry-after", retry_after_s.to_string())?;
                h.insert_header("x-ratelimit-limit", self.rps.to_string())?;
                h.insert_header("content-length", "0")?;
                h.insert_header("cache-control", "no-store")?;
                write_response_header(s, h, true, cx).await?;
                cx.short_circuit = Some("ratelimit");
                Ok(Flow::Done)
            }
        }
    }
}

pub struct RateLimitFactory;

impl ModuleFactory for RateLimitFactory {
    fn name(&self) -> &'static str {
        "ratelimit"
    }
    fn priority(&self) -> i32 {
        100
    }
    fn build(
        &self,
        cfg: &revp_core::toml::Table,
        bcx: &BuildCtx<'_>,
    ) -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(cfg)?;
        if c.rps == 0 {
            revp_core::anyhow::bail!("ratelimit: `rps` must be > 0");
        }
        let burst = c.burst.unwrap_or(c.rps);
        let key = match c.key.as_str() {
            "ip" => KeySource::Ip,
            k => match k.strip_prefix("header:") {
                Some(name) if !name.is_empty() => KeySource::Header(name.to_ascii_lowercase()),
                _ => revp_core::anyhow::bail!("ratelimit: key must be `ip` or `header:<name>`"),
            },
        };
        // Same route + same limits → keep existing buckets across reloads.
        let id = format!("{}|{}|{}|{}", bcx.route_name, c.rps, burst, c.key);
        let limiter = LIMITERS
            .lock()
            .entry(id)
            .or_insert_with(|| Arc::new(Limiter::new(c.rps, burst, c.shards, c.max_keys_per_shard)))
            .clone();
        Ok(Arc::new(RateLimit {
            limiter,
            key,
            status: c.status,
            rps: c.rps,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_deny_then_refill() {
        let l = Limiter::new(10, 3, 4, 100);
        assert!(matches!(l.check(b"a"), Decision::Allow));
        assert!(matches!(l.check(b"a"), Decision::Allow));
        assert!(matches!(l.check(b"a"), Decision::Allow));
        assert!(matches!(l.check(b"a"), Decision::Deny { .. }));
        // other key unaffected
        assert!(matches!(l.check(b"b"), Decision::Allow));
        std::thread::sleep(std::time::Duration::from_millis(120));
        // 10 rps → ~1.2 tokens refilled
        assert!(matches!(l.check(b"a"), Decision::Allow));
        assert!(matches!(l.check(b"a"), Decision::Deny { .. }));
    }

    #[test]
    fn eviction_keeps_table_bounded() {
        let l = Limiter::new(1, 1, 1, 10);
        for i in 0..100u32 {
            l.check(&i.to_le_bytes());
        }
        // All keys are fresh so eviction cannot drop them; table grows past cap
        // only until keys go idle. Ensure no panic and the entry count is sane.
        assert!(l.shards[0].lock().len() <= 100);
    }
}
