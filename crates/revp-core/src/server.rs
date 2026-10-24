//! Turn a validated config into a running Pingora server.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use log::info;
use pingora_core::apps::http_app::HttpServer;
use pingora_core::listeners::tls::TlsSettings;
use pingora_core::listeners::TcpSocketOptions;
use pingora_core::server::configuration::{Opt, ServerConf};
use pingora_core::server::Server;
use pingora_core::services::background::background_service;
use pingora_core::services::listening::Service;

use crate::acme::AcmeChallenges;
use crate::admin::AdminApp;
use crate::config::Config;
use crate::l4::L4Proxy;
use crate::manager::RuntimeManager;
use crate::module::Registry;
use crate::proxy::RevpProxy;
use crate::runtime::{Runtime, Shared};
use crate::tls::CertStore;

#[derive(Debug, Clone, Default)]
pub struct CliOpts {
    pub config: PathBuf,
    pub upgrade: bool,
    pub daemon: bool,
    pub test: bool,
}

pub fn init_logging(level: &str) {
    // rustls warns on every handshake whose SNI is an IP literal (load testers,
    // health checkers); that's not actionable, so keep it at error.
    let filter = format!("{level},rustls::msgs::handshake=error");
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(&filter))
        .format_timestamp_millis()
        .try_init();
}

fn server_conf(cfg: &Config) -> ServerConf {
    let s = &cfg.server;
    let threads = if s.threads == 0 {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    } else {
        s.threads
    };
    ServerConf {
        threads,
        work_stealing: s.work_stealing,
        pid_file: s.pid_file.to_string_lossy().into_owned(),
        upgrade_sock: s.upgrade_sock.to_string_lossy().into_owned(),
        grace_period_seconds: Some(s.grace_period.as_secs()),
        graceful_shutdown_timeout_seconds: Some(s.graceful_shutdown_timeout.as_secs()),
        upstream_keepalive_pool_size: s.upstream_keepalive_pool_size,
        error_log: s.error_log.as_ref().map(|p| p.to_string_lossy().into_owned()),
        user: s.user.clone(),
        group: s.group.clone(),
        ca_file: s.ca_file.as_ref().map(|p| p.to_string_lossy().into_owned()),
        ..ServerConf::default()
    }
}

fn sock_opts(reuseport: bool, fastopen: Option<usize>, ipv6_only: Option<bool>) -> TcpSocketOptions {
    let mut o = TcpSocketOptions::default();
    o.so_reuseport = Some(reuseport);
    o.tcp_fastopen = fastopen;
    o.ipv6_only = ipv6_only;
    o
}

/// Build and run the server. `extra` lets the binary attach feature-gated services
/// (e.g. ACME) that need access to the shared state.
pub fn run(
    opts: CliOpts,
    registry: Registry,
    extra: impl FnOnce(&mut Server, &Arc<Shared>) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let cfg = Config::load(&opts.config)?;
    init_logging(&cfg.server.log_level);
    info!(
        "revp {} starting; modules compiled in: {:?}",
        env!("CARGO_PKG_VERSION"),
        registry.names()
    );

    let conf = server_conf(&cfg);
    let opt = Opt {
        upgrade: opts.upgrade,
        daemon: opts.daemon,
        test: opts.test,
        nocapture: false,
        conf: None,
    };
    let mut server = Server::new_with_opt_and_conf(opt, conf);

    let certs = Arc::new(CertStore::new());
    certs.reload_from_config(&cfg)?;

    // Compile the first generation. Pingora's runtime isn't up yet, so use a
    // throwaway one for the async bits (backend discovery).
    let boot = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let runtime = boot
        .block_on(Runtime::build(cfg.clone(), &registry))
        .context("compiling config")?;
    drop(boot);
    info!(
        "config ok: {} listeners, {} routes, {} upstreams, {} l4",
        cfg.listeners.len(),
        runtime.router.routes.len(),
        runtime.upstreams.len(),
        cfg.l4.len()
    );

    let shared = Arc::new(Shared {
        runtime: arc_swap::ArcSwap::from_pointee(runtime),
        certs: certs.clone(),
        acme: Arc::new(AcmeChallenges::new()),
        registry,
        config_path: opts.config.canonicalize().unwrap_or(opts.config.clone()),
        started: std::time::Instant::now(),
    });

    server.bootstrap();

    if !cfg.listeners.is_empty() {
        let mut http = pingora_proxy::http_proxy_service_with_name(
            &server.configuration,
            RevpProxy::new(shared.clone()),
            "revp-http",
        );
        for l in &cfg.listeners {
            let so = sock_opts(l.reuseport, l.tcp_fastopen, l.ipv6_only);
            if l.tls {
                let mut tls = TlsSettings::intermediate("", "")
                    .map_err(|e| anyhow::anyhow!("tls settings: {e}"))?;
                tls.set_cert_resolver(certs.clone());
                if l.h2 {
                    tls.enable_h2();
                }
                tls.set_offload_threadpool_from_server_conf(&server.configuration);
                http.add_tls_with_settings(&l.addr, Some(so), tls);
                info!("listening https://{} (h2={})", l.addr, l.h2);
            } else {
                http.add_tcp_with_settings(&l.addr, so);
                info!("listening http://{}", l.addr);
            }
        }
        server.add_service(http);
    }

    for l in &cfg.l4 {
        let app = L4Proxy::new(shared.clone(), l.upstream.clone(), l.connect_timeout);
        let mut svc = Service::new(format!("revp-l4-{}", l.listen), app);
        svc.add_tcp_with_settings(&l.listen, sock_opts(l.reuseport, None, None));
        info!("listening tcp://{} -> upstream {:?}", l.listen, l.upstream);
        server.add_service(svc);
    }

    if let Some(admin) = &cfg.admin {
        let endpoints = shared.registry.admin_endpoints();
        let app = HttpServer::new_app(AdminApp::new(shared.clone(), endpoints));
        let mut svc = Service::new("revp-admin".to_string(), app);
        svc.add_tcp(&admin.listen);
        info!("admin on http://{}", admin.listen);
        server.add_service(svc);
    }

    server.add_service(background_service(
        "revp-runtime-manager",
        RuntimeManager {
            shared: shared.clone(),
        },
    ));

    extra(&mut server, &shared)?;

    server.run_forever();
}
