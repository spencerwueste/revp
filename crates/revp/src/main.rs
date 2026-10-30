use std::path::PathBuf;

use clap::Parser;
use revp_core::module::Registry;
use revp_core::server::{run, CliOpts};

/// revp — a lean, modular reverse proxy built on Pingora.
#[derive(Parser, Debug)]
#[command(name = "revp", version, about)]
struct Cli {
    /// Path to revp.toml
    #[arg(short, long, default_value = "revp.toml")]
    config: PathBuf,

    /// Validate the config and exit.
    #[arg(long)]
    check: bool,

    /// List compiled-in modules and exit.
    #[arg(long)]
    modules: bool,

    /// Take over listening sockets from a running revp (zero-downtime upgrade, Linux).
    #[arg(short, long)]
    upgrade: bool,

    /// Run in the background.
    #[arg(short, long)]
    daemon: bool,

    /// Start, check that services can bind, then exit.
    #[arg(short, long)]
    test: bool,
}

fn registry() -> Registry {
    #[allow(unused_mut)]
    let mut r = Registry::new();
    #[cfg(feature = "metrics")]
    r.register(revp_mod_metrics::MetricsFactory);
    #[cfg(feature = "accesslog")]
    r.register(revp_mod_accesslog::AccessLogFactory);
    #[cfg(feature = "ratelimit")]
    r.register(revp_mod_ratelimit::RateLimitFactory);
    #[cfg(feature = "headers")]
    r.register(revp_mod_headers::HeadersFactory);
    #[cfg(feature = "cache")]
    r.register(revp_mod_cache::CacheFactory);
    #[cfg(feature = "compress")]
    r.register(revp_mod_compress::CompressFactory);
    #[cfg(feature = "static")]
    r.register(revp_mod_static::StaticFactory);
    r
}

fn main() {
    let cli = Cli::parse();
    let reg = registry();

    if cli.modules {
        for n in reg.names() {
            println!("{n}");
        }
        return;
    }

    if cli.check {
        match revp_core::config::Config::load(&cli.config) {
            Ok(cfg) => {
                // Also compile it (builds routers/pipelines) so module config errors surface.
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                match rt.block_on(revp_core::runtime::Runtime::build(cfg, &reg)) {
                    Ok(r) => {
                        println!(
                            "ok: {} routes, {} upstreams",
                            r.router.routes.len(),
                            r.upstreams.len()
                        );
                    }
                    Err(e) => {
                        eprintln!("config error: {e:#}");
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("config error: {e:#}");
                std::process::exit(1);
            }
        }
        return;
    }

    let opts = CliOpts {
        config: cli.config,
        upgrade: cli.upgrade,
        daemon: cli.daemon,
        test: cli.test,
    };

    let result = run(opts, reg, |_server, _shared| {
        #[cfg(feature = "acme")]
        revp_acme::attach(_server, _shared)?;
        Ok(())
    });
    if let Err(e) = result {
        eprintln!("revp: {e:#}");
        std::process::exit(1);
    }
}
