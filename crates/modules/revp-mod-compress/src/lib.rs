//! Compress module: turns on Pingora's response compression for a route.
//!
//! Pingora negotiates the algorithm from `Accept-Encoding` (gzip, br, zstd),
//! skips already-encoded responses, and streams the encoder. This module just
//! sets the levels per route.
//!
//! ```toml
//! [routes.modules.compress]
//! level = 6        # default for every algorithm
//! gzip = 6
//! br = 4
//! zstd = 3
//! decompress = false   # also decompress upstream bodies the client can't read
//! ```

use std::sync::Arc;

use revp_core::pingora_core::modules::http::compression::ResponseCompression;
use revp_core::pingora_core::protocols::http::compression::Algorithm;
use revp_core::{async_trait, parse_module_config, BuildCtx, Ctx, Module, ModuleFactory, Phases, Session};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct Cfg {
    level: u32,
    gzip: Option<u32>,
    br: Option<u32>,
    zstd: Option<u32>,
    decompress: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        Self {
            level: 6,
            gzip: None,
            br: None,
            zstd: None,
            decompress: false,
        }
    }
}

pub struct Compress {
    level: u32,
    per_algo: Vec<(Algorithm, u32)>,
    decompress: bool,
}

#[async_trait]
impl Module for Compress {
    fn name(&self) -> &'static str {
        "compress"
    }
    fn phases(&self) -> Phases {
        // Must run before Pingora's compression module inspects Accept-Encoding.
        Phases::EARLY_REQUEST
    }

    fn on_early_request(&self, s: &mut Session, _cx: &mut Ctx) {
        if let Some(c) = s.downstream_modules_ctx.get_mut::<ResponseCompression>() {
            c.adjust_level(self.level);
            for (algo, lvl) in &self.per_algo {
                c.adjust_algorithm_level(*algo, *lvl);
            }
            if self.decompress {
                c.adjust_decompression(true);
            }
        }
    }
}

pub struct CompressFactory;

impl ModuleFactory for CompressFactory {
    fn name(&self) -> &'static str {
        "compress"
    }
    fn priority(&self) -> i32 {
        400
    }
    fn build(
        &self,
        cfg: &revp_core::toml::Table,
        _bcx: &BuildCtx<'_>,
    ) -> revp_core::anyhow::Result<Arc<dyn Module>> {
        let c: Cfg = parse_module_config(cfg)?;
        let mut per_algo = Vec::new();
        if let Some(l) = c.gzip {
            per_algo.push((Algorithm::Gzip, l));
        }
        if let Some(l) = c.br {
            per_algo.push((Algorithm::Brotli, l));
        }
        if let Some(l) = c.zstd {
            per_algo.push((Algorithm::Zstd, l));
        }
        Ok(Arc::new(Compress {
            level: c.level,
            per_algo,
            decompress: c.decompress,
        }))
    }
}
