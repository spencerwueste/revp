//! Raw TCP (layer-4) proxying: accept, pick a backend, splice bytes both ways.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use log::{debug, warn};
use pingora_core::apps::ServerApp;
use pingora_core::connectors::TransportConnector;
use pingora_core::protocols::Stream;
use pingora_core::server::ShutdownWatch;
use pingora_core::upstreams::peer::BasicPeer;

use crate::runtime::Shared;

pub struct L4Proxy {
    shared: Arc<Shared>,
    upstream: String,
    connector: TransportConnector,
    connect_timeout: std::time::Duration,
    pub connections: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
}

impl L4Proxy {
    pub fn new(shared: Arc<Shared>, upstream: String, connect_timeout: std::time::Duration) -> Self {
        Self {
            shared,
            upstream,
            connector: TransportConnector::new(None),
            connect_timeout,
            connections: AtomicU64::new(0),
            bytes_up: AtomicU64::new(0),
            bytes_down: AtomicU64::new(0),
        }
    }
}

#[async_trait]
impl ServerApp for L4Proxy {
    async fn process_new(
        self: &Arc<Self>,
        mut downstream: Stream,
        _shutdown: &ShutdownWatch,
    ) -> Option<Stream> {
        let rt = self.shared.runtime();
        let Some(pool) = rt.upstream(&self.upstream) else {
            warn!("l4: upstream {:?} missing in current config", self.upstream);
            return None;
        };
        let Some(backend) = pool.select(b"") else {
            warn!("l4: upstream {:?} has no healthy backend", self.upstream);
            return None;
        };
        drop(rt);

        let mut peer = BasicPeer::new(&backend.addr.to_string());
        peer.options.connection_timeout = Some(self.connect_timeout);
        let mut upstream = match self.connector.new_stream(&peer).await {
            Ok(s) => s,
            Err(e) => {
                warn!("l4: connect to {} failed: {e}", backend.addr);
                return None;
            }
        };
        self.connections.fetch_add(1, Ordering::Relaxed);
        debug!("l4: {} <-> {}", self.upstream, backend.addr);

        match tokio::io::copy_bidirectional(&mut downstream, &mut upstream).await {
            Ok((up, down)) => {
                self.bytes_up.fetch_add(up, Ordering::Relaxed);
                self.bytes_down.fetch_add(down, Ordering::Relaxed);
            }
            Err(e) => debug!("l4: stream ended: {e}"),
        }
        None
    }
}
