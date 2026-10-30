//! ACME (Let's Encrypt) client using the HTTP-01 or TLS-ALPN-01 challenge.
//!
//! Runs as a background service: on start it installs any certificates already
//! on disk, then for each configured domain orders a certificate when missing
//! or within `renew_before_days` of expiry, and re-checks twice a day.
//! The proxy answers challenges from [`revp_core::acme::AcmeChallenges`], which
//! this service fills in: HTTP-01 on `/.well-known/acme-challenge/*` over a plain
//! listener, TLS-ALPN-01 inside the handshake on a TLS listener.
//!
//! Storage layout: `<storage>/account.json`, `<storage>/<domain>/fullchain.pem`,
//! `<storage>/<domain>/privkey.pem`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt,
    NewAccount, NewOrder, OrderStatus, RetryPolicy,
};
use log::{error, info, warn};
use revp_core::config::{AcmeCfg, AcmeChallenge};
use revp_core::pingora_core::server::{Server, ShutdownWatch};
use revp_core::pingora_core::services::background::{background_service, BackgroundService};
use revp_core::runtime::Shared;
use revp_core::rustls::sign::CertifiedKey;
use revp_core::tls::certified_key_from_pem;

/// Register the ACME background service if `[acme]` is enabled.
pub fn attach(server: &mut Server, shared: &Arc<Shared>) -> anyhow::Result<()> {
    let cfg = shared.runtime().config.acme.clone();
    let Some(cfg) = cfg.filter(|c| c.enabled) else {
        return Ok(());
    };
    std::fs::create_dir_all(&cfg.storage)
        .with_context(|| format!("creating acme storage {}", cfg.storage.display()))?;

    // Install whatever is already on disk so TLS works from the first request.
    let mut installed = 0;
    for d in &cfg.domains {
        if let Some((cert, key)) = load_cert(&cfg.storage, d)? {
            shared
                .certs
                .install_acme(d, certified_key_from_pem(&cert, &key)?);
            installed += 1;
        }
    }
    info!(
        "acme: {} domains, {} certs loaded from {}",
        cfg.domains.len(),
        installed,
        cfg.storage.display()
    );

    let listeners = &shared.runtime().config.listeners;
    let kinds = challenge_kinds(
        cfg.challenge,
        listeners.iter().any(|l| !l.tls),
        listeners.iter().any(|l| l.tls),
    );
    info!(
        "acme: challenge {}",
        kinds.iter().map(|k| kind_name(k)).collect::<Vec<_>>().join(", then ")
    );

    server.add_service(background_service(
        "revp-acme",
        AcmeService {
            shared: shared.clone(),
            cfg,
            kinds,
            next_kind: AtomicUsize::new(0),
        },
    ));
    Ok(())
}

/// Challenge types to use, in order of preference. Config validation has
/// already made sure the listener each one needs exists.
fn challenge_kinds(setting: AcmeChallenge, plain: bool, tls: bool) -> Vec<ChallengeType> {
    match setting {
        AcmeChallenge::Http01 => vec![ChallengeType::Http01],
        AcmeChallenge::TlsAlpn01 => vec![ChallengeType::TlsAlpn01],
        AcmeChallenge::Auto => {
            let mut v = Vec::new();
            if plain {
                v.push(ChallengeType::Http01);
            }
            if tls {
                v.push(ChallengeType::TlsAlpn01);
            }
            v
        }
    }
}

fn kind_name(kind: &ChallengeType) -> &'static str {
    match kind {
        ChallengeType::Http01 => "http-01",
        ChallengeType::TlsAlpn01 => "tls-alpn-01",
        _ => "other",
    }
}

pub struct AcmeService {
    shared: Arc<Shared>,
    cfg: AcmeCfg,
    /// Usable challenge types, preferred first.
    kinds: Vec<ChallengeType>,
    /// Index into `kinds` for the next order. Moves on after a failure, so a
    /// blocked port 80 falls back to TLS-ALPN-01 on the next attempt.
    next_kind: AtomicUsize,
}

#[async_trait]
impl BackgroundService for AcmeService {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        // Give listeners a moment to bind before the CA calls back.
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut backoff = Duration::from_secs(60);
        loop {
            let all_ok = self.tick().await;
            let sleep = if all_ok {
                backoff = Duration::from_secs(60);
                Duration::from_secs(12 * 3600)
            } else {
                backoff = (backoff * 2).min(Duration::from_secs(6 * 3600));
                warn!("acme: retrying in {:?}", backoff);
                backoff
            };
            tokio::select! {
                _ = tokio::time::sleep(sleep) => {}
                _ = shutdown.changed() => return,
            }
        }
    }
}

impl AcmeService {
    /// Renew everything that needs it. Returns false if anything failed.
    async fn tick(&self) -> bool {
        let mut ok = true;
        let mut account: Option<Account> = None;
        for domain in &self.cfg.domains {
            match self.needs_renewal(domain) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(e) => {
                    warn!("acme: {domain}: cannot read existing cert ({e}); re-issuing");
                }
            }
            if account.is_none() {
                match self.account().await {
                    Ok(a) => account = Some(a),
                    Err(e) => {
                        error!("acme: account setup failed: {e:#}");
                        return false;
                    }
                }
            }
            let kind = self.kinds[self.next_kind.load(Ordering::Relaxed) % self.kinds.len()].clone();
            match self.obtain(account.as_ref().unwrap(), domain, &kind).await {
                Ok(()) => info!("acme: {domain}: certificate installed"),
                Err(e) => {
                    error!("acme: {domain}: {} failed: {e:#}", kind_name(&kind));
                    if self.kinds.len() > 1 {
                        let next = self.next_kind.fetch_add(1, Ordering::Relaxed) + 1;
                        let next = &self.kinds[next % self.kinds.len()];
                        warn!("acme: switching to {} for the next attempt", kind_name(next));
                    }
                    ok = false;
                }
            }
        }
        ok
    }

    fn directory(&self) -> String {
        if let Some(d) = &self.cfg.directory {
            return d.clone();
        }
        if self.cfg.staging {
            LetsEncrypt::Staging.url().to_owned()
        } else {
            LetsEncrypt::Production.url().to_owned()
        }
    }

    fn builder(&self) -> anyhow::Result<instant_acme::AccountBuilder> {
        Ok(match &self.cfg.ca_root {
            Some(pem) => Account::builder_with_root(pem)
                .with_context(|| format!("acme ca_root {}", pem.display()))?,
            None => Account::builder()?,
        })
    }

    async fn account(&self) -> anyhow::Result<Account> {
        let path = self.cfg.storage.join("account.json");
        if path.exists() {
            let json = std::fs::read(&path)?;
            let creds: AccountCredentials =
                serde_json::from_slice(&json).context("parsing account.json")?;
            let acct = self
                .builder()?
                .from_credentials(creds)
                .await
                .context("loading ACME account")?;
            return Ok(acct);
        }
        let contact = format!("mailto:{}", self.cfg.email);
        let (acct, creds) = self
            .builder()?
            .create(
                &NewAccount {
                    contact: &[contact.as_str()],
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                self.directory(),
                None,
            )
            .await
            .context("creating ACME account")?;
        write_private(&path, serde_json::to_vec_pretty(&creds)?.as_slice())?;
        info!("acme: created account, saved to {}", path.display());
        Ok(acct)
    }

    fn needs_renewal(&self, domain: &str) -> anyhow::Result<bool> {
        let Some((cert_pem, _)) = load_cert(&self.cfg.storage, domain)? else {
            return Ok(true);
        };
        let not_after = cert_not_after(&cert_pem)?;
        let renew_at = not_after
            .checked_sub(Duration::from_secs(self.cfg.renew_before_days as u64 * 86_400))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Ok(SystemTime::now() >= renew_at)
    }

    async fn obtain(
        &self,
        account: &Account,
        domain: &str,
        kind: &ChallengeType,
    ) -> anyhow::Result<()> {
        info!("acme: {domain}: ordering certificate ({})", kind_name(kind));
        let ids = [Identifier::Dns(domain.to_string())];
        let mut order = account
            .new_order(&NewOrder::new(&ids))
            .await
            .context("new order")?;

        let mut tokens = Vec::new();
        let mut alpn_domains = Vec::new();
        let mut authzs = order.authorizations();
        while let Some(res) = authzs.next().await {
            let mut authz = res.context("authorization")?;
            match authz.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                other => anyhow::bail!("authorization in state {other:?}"),
            }
            let mut challenge = authz
                .challenge(kind.clone())
                .ok_or_else(|| anyhow!("CA offered no {} challenge", kind_name(kind)))?;
            let key_auth = challenge.key_authorization();
            if *kind == ChallengeType::TlsAlpn01 {
                let cert = alpn_challenge_cert(domain, key_auth.digest().as_ref())?;
                self.shared.acme.insert_alpn(domain, cert);
                alpn_domains.push(domain.to_string());
            } else {
                let token = challenge.token.clone();
                self.shared.acme.insert(token.clone(), key_auth.as_str().to_string());
                tokens.push(token);
            }
            challenge.set_ready().await.context("challenge ready")?;
        }
        drop(authzs);

        let result = async {
            let status = order
                .poll_ready(&RetryPolicy::default())
                .await
                .context("waiting for order")?;
            if status != OrderStatus::Ready {
                anyhow::bail!("order ended in state {status:?}");
            }
            let key_pem = order.finalize().await.context("finalize")?;
            let cert_pem = order
                .poll_certificate(&RetryPolicy::default())
                .await
                .context("downloading certificate")?;
            Ok::<_, anyhow::Error>((key_pem, cert_pem))
        }
        .await;

        for t in &tokens {
            self.shared.acme.remove(t);
        }
        for d in &alpn_domains {
            self.shared.acme.remove_alpn(d);
        }
        let (key_pem, cert_pem) = result?;

        let dir = self.cfg.storage.join(domain);
        std::fs::create_dir_all(&dir)?;
        write_private(&dir.join("privkey.pem"), key_pem.as_bytes())?;
        std::fs::write(dir.join("fullchain.pem"), cert_pem.as_bytes())?;
        self.shared
            .certs
            .install_acme(domain, certified_key_from_pem(cert_pem.as_bytes(), key_pem.as_bytes())?);
        Ok(())
    }
}

/// The self-signed certificate a TLS-ALPN-01 validator expects (RFC 8737 §3):
/// the domain as its only SAN, plus a critical `acmeIdentifier` extension
/// holding the SHA-256 of the key authorization.
fn alpn_challenge_cert(domain: &str, digest: &[u8]) -> anyhow::Result<CertifiedKey> {
    let key = rcgen::KeyPair::generate().context("challenge key")?;
    let mut params =
        rcgen::CertificateParams::new(vec![domain.to_string()]).context("challenge cert")?;
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let cert = params.self_signed(&key).context("challenge cert")?;
    certified_key_from_pem(cert.pem().as_bytes(), key.serialize_pem().as_bytes())
}

fn load_cert(storage: &Path, domain: &str) -> anyhow::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let dir = storage.join(domain);
    let cert = dir.join("fullchain.pem");
    let key = dir.join("privkey.pem");
    if !cert.exists() || !key.exists() {
        return Ok(None);
    }
    Ok(Some((std::fs::read(cert)?, std::fs::read(key)?)))
}

fn write_private(path: &PathBuf, data: &[u8]) -> anyhow::Result<()> {
    std::fs::write(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// `notAfter` of the first certificate in a PEM chain.
pub fn cert_not_after(pem: &[u8]) -> anyhow::Result<SystemTime> {
    let (_, pem) = x509_parser::pem::parse_x509_pem(pem).map_err(|e| anyhow!("pem: {e}"))?;
    let cert = pem.parse_x509().map_err(|e| anyhow!("x509: {e}"))?;
    let ts = cert.validity().not_after.timestamp();
    Ok(SystemTime::UNIX_EPOCH + Duration::from_secs(ts.max(0) as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_prefers_http01_and_falls_back() {
        use ChallengeType::*;
        assert_eq!(challenge_kinds(AcmeChallenge::Auto, true, true), vec![Http01, TlsAlpn01]);
        assert_eq!(challenge_kinds(AcmeChallenge::Auto, false, true), vec![TlsAlpn01]);
        assert_eq!(challenge_kinds(AcmeChallenge::Auto, true, false), vec![Http01]);
        assert_eq!(challenge_kinds(AcmeChallenge::TlsAlpn01, true, true), vec![TlsAlpn01]);
        assert_eq!(challenge_kinds(AcmeChallenge::Http01, true, true), vec![Http01]);
    }

    #[test]
    fn alpn_cert_matches_rfc8737() {
        let digest = [0xabu8; 32];
        let ck = alpn_challenge_cert("revp.test", &digest).unwrap();
        let (_, cert) = x509_parser::parse_x509_certificate(&ck.cert[0]).unwrap();
        let san = cert.subject_alternative_name().unwrap().unwrap();
        assert_eq!(
            san.value.general_names,
            vec![x509_parser::extensions::GeneralName::DNSName("revp.test")]
        );
        let ext = cert
            .extensions()
            .iter()
            .find(|e| e.oid.to_id_string() == "1.3.6.1.5.5.7.1.31")
            .expect("acmeIdentifier extension");
        assert!(ext.critical);
        // DER OCTET STRING wrapping the 32-byte digest.
        assert_eq!(&ext.value[..2], &[0x04, 0x20]);
        assert_eq!(&ext.value[2..], &digest);
    }
}
