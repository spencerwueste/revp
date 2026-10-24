//! Upstream pools: backend set, load-balancing algorithm, health checks, peer options.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use log::{debug, info, warn};
use pingora_core::protocols::ALPN;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_load_balancing::discovery::Static;
use pingora_load_balancing::health_check::{HttpHealthCheck, TcpHealthCheck};
use pingora_load_balancing::selection::{Consistent, Random, RoundRobin};
use pingora_load_balancing::{Backend, Backends, LoadBalancer};
use tokio::sync::watch;

use crate::config::{HashOn, HealthCheckKind, LbAlgo, UpstreamCfg};

enum Selector {
    RoundRobin(LoadBalancer<RoundRobin>),
    Random(LoadBalancer<Random>),
    Consistent(LoadBalancer<Consistent>),
}

pub struct UpstreamPool {
    pub name: String,
    selector: Selector,
    pub cfg: UpstreamCfg,
    hc_interval: Option<Duration>,
    hc_parallel: bool,
}

impl UpstreamPool {
    pub async fn build(name: &str, cfg: &UpstreamCfg) -> anyhow::Result<Arc<UpstreamPool>> {
        let mut set = BTreeSet::new();
        for s in &cfg.servers {
            set.insert(
                Backend::new_with_weight(s.addr(), s.weight())
                    .with_context(|| format!("upstream {name:?} backend {:?}", s.addr()))?,
            );
        }
        let mut backends = Backends::new(Static::new(set));

        let mut hc_interval = None;
        let mut hc_parallel = false;
        if let Some(hc) = &cfg.health_check {
            let check: Box<dyn pingora_load_balancing::health_check::HealthCheck + Send + Sync> =
                match hc.kind {
                    HealthCheckKind::Tcp => {
                        let mut t = if cfg.tls {
                            TcpHealthCheck::new_tls(cfg.sni.as_deref().unwrap_or(name))
                        } else {
                            TcpHealthCheck::new()
                        };
                        t.consecutive_success = hc.consecutive_success;
                        t.consecutive_failure = hc.consecutive_failure;
                        t
                    }
                    HealthCheckKind::Http => {
                        let host = hc
                            .host
                            .clone()
                            .or_else(|| cfg.sni.clone())
                            .unwrap_or_else(|| name.to_string());
                        let mut h = HttpHealthCheck::new(&host, cfg.tls);
                        h.consecutive_success = hc.consecutive_success;
                        h.consecutive_failure = hc.consecutive_failure;
                        h.req.set_uri(
                            hc.path
                                .parse()
                                .with_context(|| format!("upstream {name:?} health_check.path"))?,
                        );
                        h.peer_template.options.verify_cert = cfg.verify_cert;
                        Box::new(h)
                    }
                };
            backends.set_health_check(check);
            hc_interval = Some(hc.interval);
            hc_parallel = hc.parallel;
        }

        let selector = match cfg.lb {
            LbAlgo::RoundRobin => {
                let lb = LoadBalancer::<RoundRobin>::from_backends(backends);
                lb.update().await.context("initial backend update")?;
                Selector::RoundRobin(lb)
            }
            LbAlgo::Random => {
                let lb = LoadBalancer::<Random>::from_backends(backends);
                lb.update().await.context("initial backend update")?;
                Selector::Random(lb)
            }
            LbAlgo::ConsistentHash => {
                let lb = LoadBalancer::<Consistent>::from_backends(backends);
                lb.update().await.context("initial backend update")?;
                Selector::Consistent(lb)
            }
        };

        Ok(Arc::new(UpstreamPool {
            name: name.to_string(),
            selector,
            cfg: cfg.clone(),
            hc_interval,
            hc_parallel,
        }))
    }

    fn backends(&self) -> &Backends {
        match &self.selector {
            Selector::RoundRobin(lb) => lb.backends(),
            Selector::Random(lb) => lb.backends(),
            Selector::Consistent(lb) => lb.backends(),
        }
    }

    /// Pick a healthy backend. `key` only matters for consistent hashing.
    #[inline]
    pub fn select(&self, key: &[u8]) -> Option<Backend> {
        match &self.selector {
            Selector::RoundRobin(lb) => lb.select(key, 16),
            Selector::Random(lb) => lb.select(key, 16),
            Selector::Consistent(lb) => lb.select(key, 16),
        }
    }

    pub fn hash_on(&self) -> &HashOn {
        &self.cfg.hash_on
    }

    /// Build the Pingora peer for a selected backend, applying pool options.
    pub fn peer(&self, backend: &Backend) -> Box<HttpPeer> {
        let sni = self
            .cfg
            .sni
            .clone()
            .unwrap_or_else(|| backend.addr.to_string());
        let mut peer = HttpPeer::new(backend.addr.clone(), self.cfg.tls, sni);
        let o = &mut peer.options;
        o.connection_timeout = Some(self.cfg.connect_timeout);
        o.total_connection_timeout = Some(self.cfg.connect_timeout * 2);
        o.read_timeout = Some(self.cfg.read_timeout);
        o.write_timeout = Some(self.cfg.write_timeout);
        o.idle_timeout = Some(self.cfg.idle_timeout);
        o.verify_cert = self.cfg.verify_cert;
        o.verify_hostname = self.cfg.verify_cert;
        if self.cfg.http_version == "2" {
            o.alpn = ALPN::H2;
            o.set_http_version(2, 2);
        }
        Box::new(peer)
    }

    /// Snapshot of backend health for the admin endpoint.
    pub fn status(&self) -> Vec<(String, usize, bool)> {
        let b = self.backends();
        b.get_backend()
            .iter()
            .map(|be| (be.addr.to_string(), be.weight, b.ready(be)))
            .collect()
    }

    /// Run health checks until `cancel` flips to true. No-op when none configured.
    pub async fn run_health_checks(self: Arc<Self>, mut cancel: watch::Receiver<bool>) {
        let Some(interval) = self.hc_interval else {
            return;
        };
        info!("upstream {:?}: health checks every {:?}", self.name, interval);
        loop {
            self.backends().run_health_check(self.hc_parallel).await;
            debug!("upstream {:?}: health check round done", self.name);
            tokio::select! {
                _ = tokio::time::sleep(interval) => {}
                r = cancel.changed() => {
                    if r.is_err() || *cancel.borrow() {
                        debug!("upstream {:?}: health checks stopped", self.name);
                        return;
                    }
                }
            }
        }
    }
}

impl Drop for UpstreamPool {
    fn drop(&mut self) {
        if self.hc_interval.is_some() {
            warn!("upstream pool {:?} dropped", self.name);
        }
    }
}
