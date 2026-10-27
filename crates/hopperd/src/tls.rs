//! TLS for the panel. hopperd is its own small certificate authority: the
//! CA (made once, kept in the data folder) signs a fresh server certificate
//! at every start, naming this machine's current addresses. Hopper desktop
//! pins the CA when it pairs, so a new IP or hostname never breaks it.
//!
//! With a domain set (and port 443 reaching the panel), Let's Encrypt
//! certificates come from ACME (TLS-ALPN-01) and are served to clients
//! asking for that name; everyone else gets the panel's own certificate.

use anyhow::Context as _;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

const CA_CERT: &str = "ca.pem";
const CA_KEY: &str = "ca-key.pem";
const CA_INFO: &str = "ca.json";

#[derive(Serialize, Deserialize)]
struct CaInfo {
    common_name: String,
}

pub struct Ca {
    pub pem: String,
    pub der: CertificateDer<'static>,
    params: CertificateParams,
    key: KeyPair,
}

fn ca_params(common_name: &str) -> anyhow::Result<CertificateParams> {
    let mut p = CertificateParams::new(Vec::<String>::new())?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    dn.push(DnType::OrganizationName, "Hopper Server");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    Ok(p)
}

/// The panel's CA, made on first start.
pub fn load_or_create_ca(dir: &Path, host: &str) -> anyhow::Result<Ca> {
    let (cert_p, key_p, info_p) = (dir.join(CA_CERT), dir.join(CA_KEY), dir.join(CA_INFO));
    if let (Ok(pem), Ok(key), Ok(info)) =
        (std::fs::read_to_string(&cert_p), std::fs::read_to_string(&key_p), std::fs::read_to_string(&info_p))
    {
        let info: CaInfo = serde_json::from_str(&info)?;
        let der = pem_to_der(&pem).context("reading ca.pem")?;
        return Ok(Ca { pem, der, params: ca_params(&info.common_name)?, key: KeyPair::from_pem(&key)? });
    }
    let common_name = format!("Hopper Server CA ({host} {})", crate::auth::random_hex(3));
    let key = KeyPair::generate()?;
    let mut params = ca_params(&common_name)?;
    let now = time_now();
    params.not_before = now - std::time::Duration::from_secs(86_400);
    params.not_after = now + std::time::Duration::from_secs(20 * 365 * 86_400);
    let cert = params.self_signed(&key)?;
    std::fs::write(&cert_p, cert.pem())?;
    std::fs::write(&key_p, key.serialize_pem())?;
    std::fs::write(&info_p, serde_json::to_string(&CaInfo { common_name: common_name.clone() })?)?;
    log::info!("made the panel's certificate authority");
    Ok(Ca { pem: cert.pem(), der: cert.der().clone(), params: ca_params(&common_name)?, key })
}

fn time_now() -> time::OffsetDateTime {
    time::OffsetDateTime::now_utc()
}

fn pem_to_der(pem: &str) -> Option<CertificateDer<'static>> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(b64).ok().map(CertificateDer::from)
}

/// "AB:CD:…" of the CA, as both sides show it.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// A server certificate for these names and addresses, signed by the CA.
pub fn issue(ca: &Ca, names: &[String], ips: &[IpAddr]) -> anyhow::Result<CertifiedKey> {
    let mut p = CertificateParams::new(Vec::<String>::new())?;
    p.distinguished_name.push(DnType::CommonName, names.first().cloned().unwrap_or_else(|| "hopperd".into()));
    for n in names {
        p.subject_alt_names.push(SanType::DnsName(n.as_str().try_into()?));
    }
    for ip in ips {
        p.subject_alt_names.push(SanType::IpAddress(*ip));
    }
    p.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let now = time_now();
    p.not_before = now - std::time::Duration::from_secs(86_400);
    p.not_after = now + std::time::Duration::from_secs(397 * 86_400);
    let key = KeyPair::generate()?;
    let issuer = Issuer::from_params(&ca.params, &ca.key);
    let cert = p.signed_by(&key, &issuer)?;
    let chain = vec![cert.der().clone(), ca.der.clone()];
    let pk = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let signing = rustls::crypto::ring::sign::any_supported_type(&pk).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(CertifiedKey::new(chain, signing))
}

/// Picks the certificate for a connection: Let's Encrypt's for the public
/// domain, the panel's own for everything else.
#[derive(Debug)]
pub struct Resolver {
    /// Swapped when this machine's addresses change (see [`OwnCert`]).
    pub own: OwnCert,
    pub acme: Option<(String, Arc<rustls_acme::ResolvesServerCertAcme>)>,
}

/// The panel's own certificate, re-issued when the machine's addresses
/// change (a service starts before the network card has its address).
pub type OwnCert = Arc<std::sync::RwLock<Arc<CertifiedKey>>>;

impl ResolvesServerCert for Resolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if let Some((domain, acme)) = &self.acme
            && (hello.server_name().is_some_and(|n| n.eq_ignore_ascii_case(domain))
                || hello.alpn().is_some_and(|mut a| a.any(|p| p == rustls_acme::acme::ACME_TLS_ALPN_NAME)))
        {
            return acme.resolve(hello);
        }
        Some(self.own.read().unwrap_or_else(|e| e.into_inner()).clone())
    }
}

/// The server's TLS settings.
pub fn server_config(resolver: Resolver) -> Arc<rustls::ServerConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut c = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver));
    let mut alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    alpn.push(rustls_acme::acme::ACME_TLS_ALPN_NAME.to_vec());
    c.alpn_protocols = alpn;
    Arc::new(c)
}

/// Names this machine goes by on the LAN and Tailscale.
pub fn local_names(extra: &[String]) -> Vec<String> {
    let host = crate::config::hostname();
    let mut v = vec!["localhost".to_string(), host.clone(), format!("{host}.local")];
    if let Some(ts) = tailscale_name() {
        v.push(ts);
    }
    v.extend(extra.iter().filter(|e| e.parse::<IpAddr>().is_err()).cloned());
    v.sort();
    v.dedup();
    v
}

/// This machine's Tailscale DNS name (e.g. "rig.tail3c9e.ts.net"), when it
/// runs Tailscale.
pub fn tailscale_name() -> Option<String> {
    let mut cmd = std::process::Command::new("tailscale");
    cmd.args(["status", "--json"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no console flash (checked every few seconds)
    }
    let out = cmd.output().ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let name = v["Self"]["DNSName"].as_str()?.trim_end_matches('.').to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    use rustls::client::danger::ServerCertVerifier;

    #[test]
    fn ca_signs_a_server_cert_that_verifies_and_survives_restarts() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("hopperd-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ca = load_or_create_ca(&dir, "rig").unwrap();
        let again = load_or_create_ca(&dir, "rig").unwrap();
        assert_eq!(fingerprint(&ca.der), fingerprint(&again.der), "the CA is kept");
        let ck = issue(&again, &["rig".into(), "localhost".into()], &["192.168.50.3".parse().unwrap()]).unwrap();
        // A client trusting only the CA accepts the server cert for its names.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der.clone()).unwrap();
        let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots)).build().unwrap();
        let now = rustls::pki_types::UnixTime::now();
        for name in ["rig", "192.168.50.3"] {
            let sn = rustls::pki_types::ServerName::try_from(name).unwrap();
            verifier.verify_server_cert(&ck.cert[0], &ck.cert[1..], &sn, &[], now).unwrap();
        }
        let other = rustls::pki_types::ServerName::try_from("evil.example").unwrap();
        assert!(verifier.verify_server_cert(&ck.cert[0], &ck.cert[1..], &other, &[], now).is_err());
        assert_eq!(fingerprint(&ca.der).len(), 32 * 3 - 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
