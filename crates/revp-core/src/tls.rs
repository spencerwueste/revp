//! SNI certificate store with live reload.
//!
//! One `CertStore` lives for the whole process. Config reloads replace the
//! file-based certs; the ACME client adds/replaces issued certs. rustls asks
//! `resolve()` on every handshake, which is a single atomic load plus a map lookup.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Context};
use arc_swap::ArcSwap;
use log::info;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::Config;

#[derive(Default)]
struct Table {
    /// exact host → cert
    exact: HashMap<String, Arc<CertifiedKey>>,
    /// `example.com` → cert for `*.example.com`
    wildcard: HashMap<String, Arc<CertifiedKey>>,
    default: Option<Arc<CertifiedKey>>,
}

pub struct CertStore {
    file_certs: ArcSwap<Table>,
    acme_certs: ArcSwap<HashMap<String, Arc<CertifiedKey>>>,
}

impl Default for CertStore {
    fn default() -> Self {
        Self {
            file_certs: ArcSwap::from_pointee(Table::default()),
            acme_certs: ArcSwap::from_pointee(HashMap::new()),
        }
    }
}

impl CertStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace all file-based certs from `[tls].certs`.
    pub fn reload_from_config(&self, cfg: &Config) -> anyhow::Result<()> {
        let mut t = Table::default();
        for c in &cfg.tls.certs {
            let ck = Arc::new(load_certified_key(&c.cert, &c.key)?);
            if c.hosts.is_empty() {
                t.default = Some(ck.clone());
            }
            for h in &c.hosts {
                let h = h.to_ascii_lowercase();
                if let Some(suffix) = h.strip_prefix("*.") {
                    t.wildcard.insert(suffix.to_string(), ck.clone());
                } else {
                    t.exact.insert(h, ck.clone());
                }
            }
            if t.default.is_none() {
                t.default = Some(ck.clone());
            }
        }
        info!(
            "tls: loaded {} file certs ({} exact, {} wildcard names)",
            cfg.tls.certs.len(),
            t.exact.len(),
            t.wildcard.len()
        );
        self.file_certs.store(Arc::new(t));
        Ok(())
    }

    /// Install a cert obtained via ACME for `domain`.
    pub fn install_acme(&self, domain: &str, ck: CertifiedKey) {
        let ck = Arc::new(ck);
        self.acme_certs.rcu(|m| {
            let mut m = HashMap::clone(m);
            m.insert(domain.to_ascii_lowercase(), ck.clone());
            m
        });
    }

    pub fn has_any(&self) -> bool {
        let t = self.file_certs.load();
        t.default.is_some() || !self.acme_certs.load().is_empty()
    }

    fn lookup(&self, sni: Option<&str>) -> Option<Arc<CertifiedKey>> {
        let t = self.file_certs.load();
        if let Some(name) = sni {
            let lowered;
            let name = if name.bytes().any(|b| b.is_ascii_uppercase()) {
                lowered = name.to_ascii_lowercase();
                lowered.as_str()
            } else {
                name
            };
            // ACME-issued certs win: they are the freshest for that exact name.
            if let Some(ck) = self.acme_certs.load().get(name) {
                return Some(ck.clone());
            }
            if let Some(ck) = t.exact.get(name) {
                return Some(ck.clone());
            }
            if let Some((_, rest)) = name.split_once('.') {
                if let Some(ck) = t.wildcard.get(rest) {
                    return Some(ck.clone());
                }
            }
        }
        t.default.clone()
    }
}

impl std::fmt::Debug for CertStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CertStore")
    }
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.lookup(hello.server_name())
    }
}

/// Load a PEM chain + PEM key into a rustls `CertifiedKey`.
pub fn load_certified_key(cert_path: &Path, key_path: &Path) -> anyhow::Result<CertifiedKey> {
    let cert_pem = std::fs::read(cert_path)
        .with_context(|| format!("reading cert {}", cert_path.display()))?;
    let key_pem = std::fs::read(key_path)
        .with_context(|| format!("reading key {}", key_path.display()))?;
    certified_key_from_pem(&cert_pem, &key_pem)
        .with_context(|| format!("{} / {}", cert_path.display(), key_path.display()))
}

pub fn certified_key_from_pem(cert_pem: &[u8], key_pem: &[u8]) -> anyhow::Result<CertifiedKey> {
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &cert_pem[..])
        .collect::<Result<_, _>>()
        .context("parsing certificate PEM")?;
    if certs.is_empty() {
        bail!("no certificates found in PEM");
    }
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut &key_pem[..])
        .context("parsing private key PEM")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in PEM"))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key).map_err(|e| {
        let hint = match &key {
            PrivateKeyDer::Sec1(_) => {
                " (SEC1 \"EC PRIVATE KEY\" is not supported; convert with \
                 `openssl pkcs8 -topk8 -nocrypt -in key.pem -out key-pkcs8.pem`)"
            }
            PrivateKeyDer::Pkcs8(_) => {
                " (PKCS#8 EC keys must use a named curve: generate with \
                 `-pkeyopt ec_param_enc:named_curve`, or use RSA/Ed25519)"
            }
            _ => "",
        };
        anyhow::anyhow!("unsupported private key: {e}{hint}")
    })?;
    Ok(CertifiedKey::new(certs, signing))
}
