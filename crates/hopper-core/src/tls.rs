//! rustls client setups: the usual roots, any certificate (FTPS when the
//! user said the server makes its own), or one pinned certificate for a
//! panel that makes its own ([`pinned_config`], also behind ureq through
//! [`pinned_agent`]).

use std::sync::Arc;

/// "AB:CD:…": the SHA-256 of a certificate (its DER bytes), as Hopper shows
/// and pins it.
pub fn fingerprint(der: &[u8]) -> String {
    let hex = crate::javadl::sha256_hex(der).to_uppercase();
    hex.as_bytes().chunks(2).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(":")
}

/// Whether two fingerprints are the same certificate, however they're
/// written (case, colons, spaces, a "SHA256:" in front).
pub fn same_fingerprint(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        let s = s.trim();
        let s = s.strip_prefix("SHA256:").or_else(|| s.strip_prefix("sha256:")).unwrap_or(s);
        s.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_uppercase()
    };
    let (a, b) = (norm(a), norm(b));
    a.len() == 64 && a == b
}

/// Decides on a certificate by its fingerprint: Ok to go on, Err(why) to
/// stop before anything is sent. The flag says an authority signs it for
/// the address; such a certificate passes whatever the answer.
pub type PinCheck = Arc<dyn Fn(&str, bool) -> Result<(), String> + Send + Sync>;

/// A client setup for a server that may make its own certificate: one an
/// authority signs for the host passes as usual; any other goes to `check`
/// with its fingerprint. Signatures are checked either way, so the server
/// must hold the certificate's key.
pub fn pinned_config(check: PinCheck) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = Arc::new(rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() });
    let web = rustls::client::WebPkiServerVerifier::builder_with_provider(roots, provider.clone())
        .build()
        .expect("web roots");
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { provider, web, check }))
        .with_no_client_auth();
    Arc::new(config)
}

struct Pinned {
    provider: Arc<rustls::crypto::CryptoProvider>,
    web: Arc<rustls::client::WebPkiServerVerifier>,
    check: PinCheck,
}

impl std::fmt::Debug for Pinned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pinned")
    }
}

impl rustls::client::danger::ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end: &rustls::pki_types::CertificateDer<'_>,
        middle: &[rustls::pki_types::CertificateDer<'_>],
        name: &rustls::pki_types::ServerName<'_>,
        ocsp: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // A panel behind a real certificate (Let's Encrypt) stays on the
        // usual check, so its renewals never look like a change.
        let vouched = self.web.verify_server_cert(end, middle, name, ocsp, now).is_ok();
        let r = (self.check)(&fingerprint(end), vouched);
        if vouched {
            return Ok(rustls::client::danger::ServerCertVerified::assertion());
        }
        r.map(|_| rustls::client::danger::ServerCertVerified::assertion()).map_err(rustls::Error::General)
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// A ureq agent whose https connections use `tls` (ureq has no way to give
/// its own rustls setup a verifier). Proxies work as in ureq's own chain.
pub fn pinned_agent(config: ureq::config::Config, tls: Arc<rustls::ClientConfig>) -> ureq::Agent {
    use ureq::unversioned::transport::{ConnectProxyConnector, Connector as _, TcpConnector};
    let connector = ().chain(ConnectProxyConnector::default()).chain(TcpConnector::default()).chain(PinnedTls(tls));
    ureq::Agent::with_parts(config, connector, ureq::unversioned::resolver::DefaultResolver::default())
}

#[derive(Debug)]
struct PinnedTls(Arc<rustls::ClientConfig>);

impl<In: ureq::unversioned::transport::Transport> ureq::unversioned::transport::Connector<In> for PinnedTls {
    type Out = ureq::unversioned::transport::Either<In, TlsTransport>;

    fn connect(
        &self,
        details: &ureq::unversioned::transport::ConnectionDetails,
        chained: Option<In>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        use ureq::unversioned::transport::{Either, LazyBuffers, TransportAdapter};
        let Some(t) = chained else { return Ok(None) };
        if !details.needs_tls() || t.is_tls() {
            return Ok(Some(Either::A(t)));
        }
        let host = details.uri.authority().map(|a| a.host()).unwrap_or("");
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| ureq::Error::Tls("not a host name"))?;
        let mut conn = rustls::ClientConnection::new(self.0.clone(), name)?;
        let mut sock = TransportAdapter::new(t.boxed());
        sock.set_timeout(details.timeout);
        if let Err(e) = conn.complete_io(&mut sock) {
            // A refused certificate says why in plain words.
            if let Some(rustls::Error::General(why)) = e.get_ref().and_then(|x| x.downcast_ref::<rustls::Error>()) {
                return Err(ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, why.clone())));
            }
            return Err(e.into());
        }
        let buffers = LazyBuffers::new(details.config.input_buffer_size(), details.config.output_buffer_size());
        Ok(Some(Either::B(TlsTransport { buffers, stream: rustls::StreamOwned { conn, sock } })))
    }
}

pub struct TlsTransport {
    buffers: ureq::unversioned::transport::LazyBuffers,
    stream: rustls::StreamOwned<rustls::ClientConnection, ureq::unversioned::transport::TransportAdapter>,
}

impl std::fmt::Debug for TlsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TlsTransport")
    }
}

impl ureq::unversioned::transport::Transport for TlsTransport {
    fn buffers(&mut self) -> &mut dyn ureq::unversioned::transport::Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: ureq::unversioned::transport::NextTimeout) -> Result<(), ureq::Error> {
        use std::io::Write as _;
        use ureq::unversioned::transport::Buffers as _;
        self.stream.get_mut().set_timeout(timeout);
        let output = &self.buffers.output()[..amount];
        self.stream.write_all(output)?;
        Ok(())
    }

    fn await_input(&mut self, timeout: ureq::unversioned::transport::NextTimeout) -> Result<bool, ureq::Error> {
        use std::io::Read as _;
        use ureq::unversioned::transport::Buffers as _;
        self.stream.get_mut().set_timeout(timeout);
        let input = self.buffers.input_append_buf();
        let n = self.stream.read(input)?;
        self.buffers.input_appended(n);
        Ok(n > 0)
    }

    fn is_open(&mut self) -> bool {
        self.stream.get_mut().get_mut().is_open()
    }

    fn is_tls(&self) -> bool {
        true
    }
}

pub fn client_config(self_signed: bool) -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("TLS versions");
    let config = if self_signed {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AnyCert(provider)))
            .with_no_client_auth()
    } else {
        let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
        builder.with_root_certificates(roots).with_no_client_auth()
    };
    Arc::new(config)
}

/// Accepts a self-signed certificate, like the panels' HTTP clients do when
/// "self-signed" is on. Signatures are still checked.
#[derive(Debug)]
struct AnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCert {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls::pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
