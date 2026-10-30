//! ACME challenge responder state.
//!
//! The ACME client (separate crate) fills this in; the proxy answers from it.
//! Kept in core so the proxy compiles without ACME.
//!
//! - HTTP-01: `token → key-authorization` pairs. The HTTP proxy answers
//!   `GET /.well-known/acme-challenge/<token>` on any plaintext listener.
//! - TLS-ALPN-01: `domain → challenge certificate`. [`AlpnChallengeResponder`]
//!   runs before every TLS handshake. While a challenge is pending it reads the
//!   ClientHello; if the client offers only the `acme-tls/1` protocol (the CA's
//!   validator), it finishes the handshake itself with the challenge
//!   certificate and closes. Every other connection gets its bytes back and
//!   carries on to the normal handshake. With nothing pending it does one
//!   atomic load and returns.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use log::{debug, info};
use parking_lot::RwLock;
use pingora_core::listeners::PreTlsProcess;
use pingora_core::protocols::l4::stream::Stream as L4Stream;
use pingora_error::{Error, ErrorType, OrErr};
use rustls::server::{Acceptor, ClientHello, ResolvesServerCert, ServerConfig};
use rustls::sign::CertifiedKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";

/// The ALPN protocol name a TLS-ALPN-01 validator offers (RFC 8737).
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// A ClientHello bigger than this is not from an ACME validator.
const MAX_HELLO: usize = 16 * 1024;
/// Time allowed to read the ClientHello, and then to finish the handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct AcmeChallenges {
    tokens: RwLock<HashMap<String, String>>,
    alpn_certs: RwLock<HashMap<String, Arc<CertifiedKey>>>,
    /// `alpn_certs.len()`, readable without the lock on every TLS accept.
    alpn_pending: AtomicUsize,
}

impl AcmeChallenges {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, token: String, key_auth: String) {
        self.tokens.write().insert(token, key_auth);
    }

    pub fn remove(&self, token: &str) {
        self.tokens.write().remove(token);
    }

    pub fn get(&self, token: &str) -> Option<String> {
        self.tokens.read().get(token).cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.read().is_empty()
    }

    /// Serve `cert` to TLS-ALPN-01 validators asking for `domain`.
    pub fn insert_alpn(&self, domain: &str, cert: CertifiedKey) {
        let mut m = self.alpn_certs.write();
        m.insert(domain.to_ascii_lowercase(), Arc::new(cert));
        self.alpn_pending.store(m.len(), Ordering::Release);
    }

    pub fn remove_alpn(&self, domain: &str) {
        let mut m = self.alpn_certs.write();
        m.remove(&domain.to_ascii_lowercase());
        self.alpn_pending.store(m.len(), Ordering::Release);
    }

    pub fn alpn_cert(&self, domain: &str) -> Option<Arc<CertifiedKey>> {
        self.alpn_certs.read().get(&domain.to_ascii_lowercase()).cloned()
    }

    pub fn has_alpn(&self) -> bool {
        self.alpn_pending.load(Ordering::Acquire) > 0
    }
}

/// Picks the challenge certificate by SNI. No match means no certificate, so
/// the handshake fails, which is what a validator should see for a name we are
/// not proving.
#[derive(Debug)]
struct AlpnCertResolver(Arc<AcmeChallenges>);

impl std::fmt::Debug for AcmeChallenges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AcmeChallenges")
    }
}

impl ResolvesServerCert for AlpnCertResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.0.alpn_cert(hello.server_name()?)
    }
}

/// Pre-TLS hook that answers TLS-ALPN-01 challenges on TLS listeners.
pub struct AlpnChallengeResponder {
    challenges: Arc<AcmeChallenges>,
    config: Arc<ServerConfig>,
}

impl AlpnChallengeResponder {
    pub fn new(challenges: Arc<AcmeChallenges>) -> Self {
        let mut config = ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .expect("ring supports TLS 1.2 and 1.3")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(AlpnCertResolver(challenges.clone())));
        config.alpn_protocols = vec![ACME_TLS_ALPN.to_vec()];
        Self {
            challenges,
            config: Arc::new(config),
        }
    }
}

#[async_trait]
impl PreTlsProcess for AlpnChallengeResponder {
    async fn process(&self, stream: &mut L4Stream) -> pingora_error::Result<()> {
        if !self.challenges.has_alpn() {
            return Ok(());
        }
        let read = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_client_hello(stream)).await;
        let Ok((seen, accepted)) = read else {
            return Error::e_explain(ErrorType::ReadTimedout, "reading TLS ClientHello");
        };
        let accepted = match accepted {
            Some(a) if offers_only_acme(&a.client_hello()) => a,
            // Not a validator (or not parseable): hand the bytes back untouched
            // and let the real handshake deal with them.
            _ => {
                stream.rewind(&seen);
                return Ok(());
            }
        };
        let sni = accepted.client_hello().server_name().map(str::to_owned);
        let done = tokio::time::timeout(
            HANDSHAKE_TIMEOUT,
            answer_challenge(stream, accepted, self.config.clone()),
        )
        .await;
        match done {
            Ok(Ok(())) => info!("acme: answered tls-alpn-01 challenge for {sni:?}"),
            Ok(Err(e)) => debug!("acme: tls-alpn-01 handshake for {sni:?} failed: {e}"),
            Err(_) => debug!("acme: tls-alpn-01 handshake for {sni:?} timed out"),
        }
        // The validator only needs the handshake. Dropping the connection here
        // keeps it away from the HTTP stack; Pingora logs that as a handshake
        // error, so say plainly what it was.
        Error::e_explain(
            ErrorType::HandshakeError,
            "ACME tls-alpn-01 validation connection answered and closed (expected, not a failure)",
        )
    }
}

/// RFC 8737 §3: the validator offers exactly `acme-tls/1`.
fn offers_only_acme(hello: &ClientHello<'_>) -> bool {
    let Some(mut protos) = hello.alpn() else {
        return false;
    };
    protos.next() == Some(ACME_TLS_ALPN) && protos.next().is_none()
}

/// Read until rustls has a full ClientHello. Returns every byte read (to
/// rewind if this is not a challenge) and the parsed hello, or `None` if the
/// bytes are not a TLS ClientHello we can parse.
async fn read_client_hello(
    stream: &mut L4Stream,
) -> (Vec<u8>, Option<rustls::server::Accepted>) {
    let mut seen = Vec::new();
    let mut acceptor = Acceptor::default();
    let mut buf = [0u8; 4096];
    loop {
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => return (seen, None),
            Ok(n) => n,
        };
        seen.extend_from_slice(&buf[..n]);
        let mut chunk = &buf[..n];
        while !chunk.is_empty() {
            if acceptor.read_tls(&mut chunk).is_err() {
                return (seen, None);
            }
        }
        match acceptor.accept() {
            Ok(Some(accepted)) => return (seen, Some(accepted)),
            Ok(None) if seen.len() < MAX_HELLO => continue,
            _ => return (seen, None),
        }
    }
}

/// Drive the rest of the handshake with the challenge certificate.
async fn answer_challenge(
    stream: &mut L4Stream,
    accepted: rustls::server::Accepted,
    config: Arc<ServerConfig>,
) -> pingora_error::Result<()> {
    let mut conn = match accepted.into_connection(config) {
        Ok(c) => c,
        Err((e, mut alert)) => {
            let mut out = Vec::new();
            let _ = alert.write_all(&mut out);
            let _ = stream.write_all(&out).await;
            return Error::e_explain(ErrorType::HandshakeError, format!("challenge cert: {e}"));
        }
    };
    let mut buf = [0u8; 4096];
    loop {
        if conn.wants_write() {
            while conn.wants_write() {
                let mut out = Vec::new();
                conn.write_tls(&mut out)
                    .or_err(ErrorType::WriteError, "tls write")?;
                stream
                    .write_all(&out)
                    .await
                    .or_err(ErrorType::WriteError, "tls write")?;
            }
            // The L4 stream is buffered: without this the server's flight sits
            // in the buffer while we wait for a reply that cannot come.
            stream.flush().await.or_err(ErrorType::WriteError, "tls flush")?;
        }
        if !conn.is_handshaking() {
            break;
        }
        let n = stream
            .read(&mut buf)
            .await
            .or_err(ErrorType::ReadError, "tls read")?;
        if n == 0 {
            return Error::e_explain(ErrorType::ConnectionClosed, "validator closed mid-handshake");
        }
        let mut chunk = &buf[..n];
        while !chunk.is_empty() {
            conn.read_tls(&mut chunk)
                .or_err(ErrorType::ReadError, "tls read")?;
            conn.process_new_packets()
                .or_err(ErrorType::HandshakeError, "tls handshake")?;
        }
    }
    conn.send_close_notify();
    let mut out = Vec::new();
    let _ = conn.write_tls(&mut out);
    let _ = stream.write_all(&out).await;
    let _ = stream.flush().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, ClientConnection, DigitallySignedStruct, SignatureScheme};
    use std::io::Write;

    /// DER of OID 1.3.6.1.5.5.7.1.31 (id-pe-acmeIdentifier).
    const ACME_ID_OID: &[u8] = &[0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1f];

    /// Accepts any server certificate: the challenge cert is self-signed.
    #[derive(Debug)]
    struct AnyCert(Arc<rustls::crypto::CryptoProvider>);

    impl ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        // webpki rejects the critical acmeIdentifier extension when it parses
        // the cert to check signatures; real validators do not, so skip it.
        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    fn client(alpn: &[&[u8]], sni: &str) -> ClientConnection {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut cfg = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
            .with_no_client_auth();
        cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        ClientConnection::new(Arc::new(cfg), sni.to_string().try_into().unwrap()).unwrap()
    }

    /// A stand-in challenge cert: SAN = domain, critical acmeIdentifier extension.
    fn challenge_cert(domain: &str) -> CertifiedKey {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).unwrap();
        params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(&[7u8; 32])];
        let cert = params.self_signed(&key).unwrap();
        crate::tls::certified_key_from_pem(cert.pem().as_bytes(), key.serialize_pem().as_bytes())
            .unwrap()
    }

    async fn accept_one() -> (std::net::SocketAddr, tokio::task::JoinHandle<L4Stream>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let h = tokio::spawn(async move { listener.accept().await.unwrap().0.into() });
        (addr, h)
    }

    #[tokio::test]
    async fn validator_gets_challenge_cert() {
        let challenges = Arc::new(AcmeChallenges::new());
        challenges.insert_alpn("Revp.Test", challenge_cert("revp.test"));
        let responder = AlpnChallengeResponder::new(challenges.clone());
        let (addr, accepted) = accept_one().await;

        let client = std::thread::spawn(move || {
            let mut conn = client(&[ACME_TLS_ALPN], "revp.test");
            let mut sock = std::net::TcpStream::connect(addr).unwrap();
            while conn.is_handshaking() {
                conn.complete_io(&mut sock).unwrap();
            }
            let alpn = conn.alpn_protocol().map(<[u8]>::to_vec);
            let cert = conn.peer_certificates().unwrap()[0].to_vec();
            (alpn, cert)
        });

        let mut stream = accepted.await.unwrap();
        // The challenge connection is answered and then dropped.
        assert!(responder.process(&mut stream).await.is_err());
        let (alpn, cert) = client.join().unwrap();
        assert_eq!(alpn.as_deref(), Some(ACME_TLS_ALPN));
        assert!(cert.windows(ACME_ID_OID.len()).any(|w| w == ACME_ID_OID));
    }

    #[tokio::test]
    async fn normal_client_bytes_are_rewound() {
        let challenges = Arc::new(AcmeChallenges::new());
        // A challenge is pending, so the responder does inspect the hello.
        challenges.insert_alpn("revp.test", challenge_cert("revp.test"));
        let responder = AlpnChallengeResponder::new(challenges);
        let (addr, accepted) = accept_one().await;

        // A browser-like hello (h2 + http/1.1) for the same name.
        let mut conn = client(&[b"h2", b"http/1.1"], "revp.test");
        let mut hello = Vec::new();
        conn.write_tls(&mut hello).unwrap();
        let sent = hello.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let client = std::thread::spawn(move || {
            let mut sock = std::net::TcpStream::connect(addr).unwrap();
            sock.write_all(&hello).unwrap();
            // Hold the socket open until the server has checked.
            let _ = done_rx.recv();
        });

        let mut stream = accepted.await.unwrap();
        responder.process(&mut stream).await.unwrap();
        // The real handshake must see exactly what the client sent.
        let mut got = vec![0u8; sent.len()];
        stream.read_exact(&mut got).await.unwrap();
        assert_eq!(got, sent);
        done_tx.send(()).unwrap();
        client.join().unwrap();
    }

    #[tokio::test]
    async fn nothing_pending_reads_nothing() {
        let responder = AlpnChallengeResponder::new(Arc::new(AcmeChallenges::new()));
        let (addr, accepted) = accept_one().await;
        // Connect but never send: if the responder tried to read, this would hang.
        let _sock = std::net::TcpStream::connect(addr).unwrap();
        let mut stream = accepted.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), responder.process(&mut stream))
            .await
            .expect("returned without reading")
            .unwrap();
    }

    #[test]
    fn alpn_state_counts() {
        let c = AcmeChallenges::new();
        assert!(!c.has_alpn());
        c.insert_alpn("a.test", challenge_cert("a.test"));
        assert!(c.has_alpn());
        assert!(c.alpn_cert("A.TEST").is_some());
        c.remove_alpn("a.test");
        assert!(!c.has_alpn());
    }
}
