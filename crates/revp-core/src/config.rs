//! TOML configuration model.
//!
//! Everything the proxy knows at runtime comes from one `revp.toml`. The file
//! is parsed into these structs, validated, then compiled into a [`crate::runtime::Runtime`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use serde::Deserialize;

/// Top-level configuration file.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub server: ServerCfg,
    pub listeners: Vec<ListenerCfg>,
    pub tls: TlsCfg,
    pub acme: Option<AcmeCfg>,
    pub admin: Option<AdminCfg>,
    pub upstreams: BTreeMap<String, UpstreamCfg>,
    pub routes: Vec<RouteCfg>,
    pub l4: Vec<L4Cfg>,
    pub defaults: Defaults,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerCfg {
    /// Worker threads. `0` = one per CPU core.
    pub threads: usize,
    pub work_stealing: bool,
    pub pid_file: PathBuf,
    pub upgrade_sock: PathBuf,
    /// Time to keep serving existing connections after a shutdown signal.
    #[serde(with = "humantime_serde")]
    pub grace_period: Duration,
    #[serde(with = "humantime_serde")]
    pub graceful_shutdown_timeout: Duration,
    /// Max idle keep-alive connections to upstreams, per worker.
    pub upstream_keepalive_pool_size: usize,
    pub error_log: Option<PathBuf>,
    pub user: Option<String>,
    pub group: Option<String>,
    pub ca_file: Option<PathBuf>,
    /// `error`, `warn`, `info`, `debug`, `trace`.
    pub log_level: String,
    /// Reload routes/upstreams/certs when the config file changes on disk.
    pub watch_config: bool,
}

impl Default for ServerCfg {
    fn default() -> Self {
        Self {
            threads: 0,
            work_stealing: true,
            pid_file: "/tmp/revp.pid".into(),
            upgrade_sock: "/tmp/revp_upgrade.sock".into(),
            grace_period: Duration::from_secs(5),
            graceful_shutdown_timeout: Duration::from_secs(5),
            upstream_keepalive_pool_size: 256,
            error_log: None,
            user: None,
            group: None,
            ca_file: None,
            log_level: "info".into(),
            watch_config: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ListenerCfg {
    pub addr: String,
    pub tls: bool,
    /// Offer HTTP/2 over ALPN (TLS listeners only).
    pub h2: bool,
    pub reuseport: bool,
    pub tcp_fastopen: Option<usize>,
    pub ipv6_only: Option<bool>,
}

impl Default for ListenerCfg {
    fn default() -> Self {
        Self {
            addr: "0.0.0.0:8080".into(),
            tls: false,
            h2: true,
            reuseport: false,
            tcp_fastopen: None,
            ipv6_only: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct TlsCfg {
    pub certs: Vec<CertCfg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CertCfg {
    /// SNI names this cert serves. Supports `*.example.com`. Empty = default cert.
    #[serde(default)]
    pub hosts: Vec<String>,
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AcmeCfg {
    pub enabled: bool,
    pub email: String,
    pub domains: Vec<String>,
    /// Use Let's Encrypt staging. Set while testing to avoid rate limits.
    pub staging: bool,
    /// Override the ACME directory URL entirely.
    pub directory: Option<String>,
    /// Extra root CA (PEM) to trust when talking to the directory. For private CAs / Pebble.
    pub ca_root: Option<PathBuf>,
    pub storage: PathBuf,
    pub renew_before_days: u32,
}

impl Default for AcmeCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            email: String::new(),
            domains: vec![],
            staging: false,
            directory: None,
            ca_root: None,
            storage: "/var/lib/revp/acme".into(),
            renew_before_days: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AdminCfg {
    pub listen: String,
}

impl Default for AdminCfg {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:9090".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum LbAlgo {
    #[default]
    RoundRobin,
    Random,
    ConsistentHash,
}

/// How to pick the key for consistent hashing.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HashOn {
    #[default]
    Ip,
    Path,
    Uri,
    Header(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum ServerEntry {
    Addr(String),
    Weighted { addr: String, weight: usize },
}

impl ServerEntry {
    pub fn addr(&self) -> &str {
        match self {
            ServerEntry::Addr(a) => a,
            ServerEntry::Weighted { addr, .. } => addr,
        }
    }
    pub fn weight(&self) -> usize {
        match self {
            ServerEntry::Addr(_) => 1,
            ServerEntry::Weighted { weight, .. } => *weight,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UpstreamCfg {
    pub servers: Vec<ServerEntry>,
    pub lb: LbAlgo,
    pub hash_on: HashOn,
    /// Connect to backends over TLS.
    pub tls: bool,
    pub sni: Option<String>,
    pub verify_cert: bool,
    /// `"1"` (HTTP/1.1, default), `"2"` (HTTP/2, h2c when `tls = false` — use for gRPC).
    pub http_version: String,
    #[serde(with = "humantime_serde")]
    pub connect_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub read_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub write_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub idle_timeout: Duration,
    pub health_check: Option<HealthCheckCfg>,
}

impl Default for UpstreamCfg {
    fn default() -> Self {
        Self {
            servers: vec![],
            lb: LbAlgo::RoundRobin,
            hash_on: HashOn::Ip,
            tls: false,
            sni: None,
            verify_cert: true,
            http_version: "1".into(),
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(60),
            write_timeout: Duration::from_secs(60),
            idle_timeout: Duration::from_secs(90),
            health_check: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum HealthCheckKind {
    #[default]
    Tcp,
    Http,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HealthCheckCfg {
    pub kind: HealthCheckKind,
    pub path: String,
    pub host: Option<String>,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    pub consecutive_success: usize,
    pub consecutive_failure: usize,
    pub parallel: bool,
}

impl Default for HealthCheckCfg {
    fn default() -> Self {
        Self {
            kind: HealthCheckKind::Tcp,
            path: "/".into(),
            host: None,
            interval: Duration::from_secs(5),
            consecutive_success: 1,
            consecutive_failure: 1,
            parallel: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RouteCfg {
    /// Label used in logs/metrics. Defaults to `route-<index>`.
    pub name: Option<String>,
    /// Host names to match. Empty = any host. `*.example.com` matches one label.
    pub hosts: Vec<String>,
    /// Path patterns. `/api/*` or `/api/{*rest}` are catch-alls; `/users/{id}` captures.
    pub paths: Vec<String>,
    pub upstream: String,
    /// Remove this prefix from the path before forwarding.
    pub strip_prefix: Option<String>,
    /// `preserve` (client's Host), `upstream` (backend address), or a literal host.
    pub host_header: String,
    /// Add `X-Forwarded-For/Proto/Host` and `X-Real-IP`.
    pub forwarded_headers: bool,
    #[serde(with = "humantime_serde")]
    pub downstream_read_timeout: Option<Duration>,
    #[serde(with = "humantime_serde")]
    pub downstream_write_timeout: Option<Duration>,
    /// Per-route module config: `modules.ratelimit = { rps = 100 }`.
    pub modules: toml::Table,
}

impl Default for RouteCfg {
    fn default() -> Self {
        Self {
            name: None,
            hosts: vec![],
            paths: vec!["/{*rest}".into()],
            upstream: String::new(),
            strip_prefix: None,
            host_header: "preserve".into(),
            forwarded_headers: true,
            downstream_read_timeout: None,
            downstream_write_timeout: None,
            modules: toml::Table::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct L4Cfg {
    pub listen: String,
    pub upstream: String,
    pub reuseport: bool,
    #[serde(with = "humantime_serde")]
    pub connect_timeout: Duration,
}

impl Default for L4Cfg {
    fn default() -> Self {
        Self {
            listen: String::new(),
            upstream: String::new(),
            reuseport: false,
            connect_timeout: Duration::from_secs(5),
        }
    }
}

/// Settings applied to every route unless the route overrides them.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct Defaults {
    /// Module config merged under each route's `modules` table.
    pub modules: toml::Table,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> anyhow::Result<Config> {
        let cfg: Config = toml::from_str(text).context("parsing TOML")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.listeners.is_empty() && self.l4.is_empty() {
            bail!("no [[listeners]] or [[l4]] defined");
        }
        for l in &self.listeners {
            l.addr
                .parse::<std::net::SocketAddr>()
                .with_context(|| format!("listener addr {:?}", l.addr))?;
        }
        let has_tls_listener = self.listeners.iter().any(|l| l.tls);
        let acme_on = self.acme.as_ref().is_some_and(|a| a.enabled);
        if has_tls_listener && self.tls.certs.is_empty() && !acme_on {
            bail!("a TLS listener is configured but [tls] has no certs and ACME is off");
        }
        if let Some(acme) = &self.acme {
            if acme.enabled {
                if acme.domains.is_empty() {
                    bail!("[acme] enabled but no domains listed");
                }
                if acme.email.is_empty() {
                    bail!("[acme] needs an email");
                }
                if !self.listeners.iter().any(|l| !l.tls) {
                    bail!("[acme] HTTP-01 needs a plain (non-TLS) listener, normally on port 80");
                }
            }
        }
        for (name, up) in &self.upstreams {
            if up.servers.is_empty() {
                bail!("upstream {name:?} has no servers");
            }
            for s in &up.servers {
                s.addr()
                    .parse::<std::net::SocketAddr>()
                    .with_context(|| format!("upstream {name:?} server {:?} (use ip:port)", s.addr()))?;
                if s.weight() == 0 {
                    bail!("upstream {name:?} server {:?} has weight 0", s.addr());
                }
            }
            if up.http_version != "1" && up.http_version != "2" {
                bail!("upstream {name:?}: http_version must be \"1\" or \"2\"");
            }
        }
        for (i, r) in self.routes.iter().enumerate() {
            let label = r.name.clone().unwrap_or_else(|| format!("route-{i}"));
            if !self.upstreams.contains_key(&r.upstream) {
                bail!("route {label:?} references unknown upstream {:?}", r.upstream);
            }
            if r.paths.is_empty() {
                bail!("route {label:?} has no paths");
            }
            for p in &r.paths {
                if !p.starts_with('/') {
                    bail!("route {label:?} path {p:?} must start with '/'");
                }
            }
        }
        for l in &self.l4 {
            l.listen
                .parse::<std::net::SocketAddr>()
                .with_context(|| format!("l4 listen {:?}", l.listen))?;
            if !self.upstreams.contains_key(&l.upstream) {
                bail!("l4 {:?} references unknown upstream {:?}", l.listen, l.upstream);
            }
        }
        Ok(())
    }

    /// Effective module table for a route: `[defaults.modules]` overlaid by the route's own.
    pub fn route_modules(&self, route: &RouteCfg) -> toml::Table {
        let mut out = self.defaults.modules.clone();
        for (k, v) in &route.modules {
            match (out.get_mut(k), v) {
                (Some(toml::Value::Table(base)), toml::Value::Table(over)) => {
                    for (ik, iv) in over {
                        base.insert(ik.clone(), iv.clone());
                    }
                }
                _ => {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
        out
    }
}

/// Convert friendly `/foo/*` into matchit's `/foo/{*rest}`.
pub fn normalize_path_pattern(p: &str) -> String {
    if let Some(prefix) = p.strip_suffix("/*") {
        format!("{prefix}/{{*rest}}")
    } else if p == "*" {
        "/{*rest}".to_string()
    } else {
        p.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_parses() {
        let cfg = Config::parse(
            r#"
            [[listeners]]
            addr = "0.0.0.0:8080"
            [upstreams.app]
            servers = ["127.0.0.1:3000"]
            [[routes]]
            upstream = "app"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.routes.len(), 1);
        assert_eq!(cfg.routes[0].paths, vec!["/{*rest}"]);
    }

    #[test]
    fn unknown_upstream_rejected() {
        let err = Config::parse(
            r#"
            [[listeners]]
            addr = "0.0.0.0:8080"
            [[routes]]
            upstream = "nope"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown upstream"));
    }

    #[test]
    fn defaults_merge_under_route() {
        let cfg = Config::parse(
            r#"
            [[listeners]]
            addr = "0.0.0.0:8080"
            [defaults.modules.ratelimit]
            rps = 10
            burst = 5
            [upstreams.app]
            servers = ["127.0.0.1:3000"]
            [[routes]]
            upstream = "app"
            [routes.modules.ratelimit]
            rps = 100
            "#,
        )
        .unwrap();
        let m = cfg.route_modules(&cfg.routes[0]);
        let rl = m["ratelimit"].as_table().unwrap();
        assert_eq!(rl["rps"].as_integer(), Some(100));
        assert_eq!(rl["burst"].as_integer(), Some(5));
    }

    #[test]
    fn path_normalization() {
        assert_eq!(normalize_path_pattern("/api/*"), "/api/{*rest}");
        assert_eq!(normalize_path_pattern("/x/{id}"), "/x/{id}");
    }
}
