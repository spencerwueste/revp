//! Background service that owns config generations: starts health checks for
//! the initial generation and reloads on SIGHUP or when the config file changes.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use log::{error, info, warn};
use pingora_core::server::ShutdownWatch;
use pingora_core::services::background::BackgroundService;
use tokio::sync::mpsc;

use crate::runtime::Shared;

pub struct RuntimeManager {
    pub shared: Arc<Shared>,
}

#[async_trait]
impl BackgroundService for RuntimeManager {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        self.shared.runtime().start_background();

        let (tx, mut rx) = mpsc::channel::<()>(4);

        // File watcher (debounced below).
        let _watcher = if self.shared.runtime().config.server.watch_config {
            match start_watcher(&self.shared.config_path, tx.clone()) {
                Ok(w) => Some(w),
                Err(e) => {
                    warn!("config watch disabled: {e}");
                    None
                }
            }
        } else {
            None
        };

        #[cfg(unix)]
        let mut sighup = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!("SIGHUP handler unavailable: {e}");
                None
            }
        };

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    info!("runtime manager: shutting down");
                    self.shared.runtime().stop_background();
                    return;
                }
                Some(()) = rx.recv() => {
                    // Debounce bursts of file events (editors write several times).
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    while rx.try_recv().is_ok() {}
                    self.do_reload("file change").await;
                }
                _ = async {
                    #[cfg(unix)]
                    if let Some(s) = sighup.as_mut() { s.recv().await; } else { std::future::pending::<()>().await }
                    #[cfg(not(unix))]
                    std::future::pending::<()>().await
                } => {
                    self.do_reload("SIGHUP").await;
                }
            }
        }
    }
}

impl RuntimeManager {
    async fn do_reload(&self, why: &str) {
        info!("reload requested ({why})");
        if let Err(e) = self.shared.reload().await {
            error!("reload failed, keeping previous config: {e:#}");
        }
    }
}

fn start_watcher(
    path: &std::path::Path,
    tx: mpsc::Sender<()>,
) -> anyhow::Result<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};
    let target = path.canonicalize()?;
    let dir = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config has no parent dir"))?
        .to_path_buf();
    let file_name = target.file_name().map(|s| s.to_os_string());
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if !matches!(
            ev.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
        ) {
            return;
        }
        // Editors often replace the file (rename), so watch the directory and filter by name.
        let hit = ev
            .paths
            .iter()
            .any(|p| p.file_name().map(|s| s.to_os_string()) == file_name);
        if hit {
            let _ = tx.try_send(());
        }
    })?;
    watcher.watch(&dir, RecursiveMode::NonRecursive)?;
    info!("watching {} for changes", target.display());
    Ok(watcher)
}
