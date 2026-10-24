//! One immutable, compiled configuration generation.
//!
//! Requests grab the current `Arc<Runtime>` with a single atomic load. A reload
//! builds a new one and swaps the pointer; in-flight requests keep the old one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Context;
use arc_swap::ArcSwap;
use log::info;
use tokio::sync::watch;

use crate::acme::AcmeChallenges;
use crate::config::Config;
use crate::module::{BuildCtx, Pipeline, Registry};
use crate::router::Router;
use crate::tls::CertStore;
use crate::upstream::UpstreamPool;

static GENERATION: AtomicU64 = AtomicU64::new(0);

pub struct Runtime {
    pub generation: u64,
    pub config: Arc<Config>,
    pub router: Router,
    pub upstreams: HashMap<String, Arc<UpstreamPool>>,
    cancel_tx: watch::Sender<bool>,
    cancel_rx: watch::Receiver<bool>,
}

impl Runtime {
    pub async fn build(config: Config, registry: &Registry) -> anyhow::Result<Runtime> {
        let config = Arc::new(config);
        let mut upstreams = HashMap::with_capacity(config.upstreams.len());
        for (name, ucfg) in &config.upstreams {
            upstreams.insert(name.clone(), UpstreamPool::build(name, ucfg).await?);
        }
        let router = Router::build(&config, |rc, name| {
            let modules_cfg = config.route_modules(rc);
            let bcx = BuildCtx {
                config: &config,
                route: rc,
                route_name: name,
            };
            let modules = registry.build_for_route(&modules_cfg, &bcx)?;
            Pipeline::new(modules).with_context(|| format!("route {name:?}"))
        })?;
        let (cancel_tx, cancel_rx) = watch::channel(false);
        Ok(Runtime {
            generation: GENERATION.fetch_add(1, Ordering::Relaxed) + 1,
            config,
            router,
            upstreams,
            cancel_tx,
            cancel_rx,
        })
    }

    /// Spawn health-check loops for this generation. Must run inside tokio.
    pub fn start_background(&self) {
        for pool in self.upstreams.values() {
            tokio::spawn(pool.clone().run_health_checks(self.cancel_rx.clone()));
        }
    }

    /// Stop this generation's background tasks.
    pub fn stop_background(&self) {
        let _ = self.cancel_tx.send(true);
    }

    pub fn upstream(&self, name: &str) -> Option<&Arc<UpstreamPool>> {
        self.upstreams.get(name)
    }
}

/// State shared across all config generations and all services.
pub struct Shared {
    pub runtime: ArcSwap<Runtime>,
    pub certs: Arc<CertStore>,
    pub acme: Arc<AcmeChallenges>,
    pub registry: Registry,
    pub config_path: std::path::PathBuf,
    pub started: std::time::Instant,
}

impl Shared {
    pub fn runtime(&self) -> arc_swap::Guard<Arc<Runtime>> {
        self.runtime.load()
    }

    /// Re-read the config file, compile it, swap it in. Listeners cannot change live.
    pub async fn reload(&self) -> anyhow::Result<u64> {
        let cfg = Config::load(&self.config_path)?;
        let old = self.runtime.load_full();
        if cfg.listeners.len() != old.config.listeners.len()
            || cfg
                .listeners
                .iter()
                .zip(&old.config.listeners)
                .any(|(a, b)| a.addr != b.addr || a.tls != b.tls)
            || cfg.l4.len() != old.config.l4.len()
        {
            log::warn!("listeners changed in config; restart (or --upgrade) to apply them");
        }
        self.certs.reload_from_config(&cfg)?;
        let new = Runtime::build(cfg, &self.registry).await?;
        new.start_background();
        let gen = new.generation;
        self.runtime.store(Arc::new(new));
        old.stop_background();
        info!(
            "config reloaded: generation {gen}, {} routes, {} upstreams",
            self.runtime.load().router.routes.len(),
            self.runtime.load().upstreams.len()
        );
        Ok(gen)
    }
}
