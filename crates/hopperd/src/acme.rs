//! Trusted certificates from Let's Encrypt, so browsers reaching the panel
//! from the internet don't warn about the panel's own certificate.
//!
//! What the certificate names, in order:
//! - the domain in settings (`acme_domain`), when it points at this server;
//! - else this server's public IP. Let's Encrypt issues IP certificates only
//!   in its `shortlived` profile (about 6 days), so they renew every ~3 days;
//! - else, when Let's Encrypt won't issue for the IP, `<ip-with-dashes>.sslip.io`,
//!   a public name that resolves to that IP, with a normal certificate.
//!
//! Validation is TLS-ALPN-01 on port 443 (answered on the panel's own port,
//! which 443 is forwarded to), with HTTP-01 on port 80 as the other try.
//! Everything lives in `<data>/certs`: the account key, the certificate and
//! its key. A new certificate is swapped in without a restart. The panel's
//! own certificate stays for the LAN, Tailscale and anything this one
//! doesn't cover.

use anyhow::Context as _;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder,
    OrderStatus, RetryPolicy,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::sign::CertifiedKey;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::Notify;

/// The ALPN protocol Let's Encrypt's TLS-ALPN-01 check asks for.
pub const ALPN: &[u8] = b"acme-tls/1";

/// What a certificate is for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Target {
    Ip(IpAddr),
    /// A domain or an sslip.io name.
    Name(String),
}

impl Target {
    fn identifier(&self) -> Identifier {
        match self {
            Target::Ip(ip) => Identifier::Ip(*ip),
            Target::Name(n) => Identifier::Dns(n.clone()),
        }
    }

    /// The name the TLS-ALPN-01 check sends (SNI): the name itself, or for
    /// an IP its reverse-DNS name (RFC 8738).
    fn sni(&self) -> String {
        match self {
            Target::Ip(ip) => reverse_name(*ip),
            Target::Name(n) => n.to_ascii_lowercase(),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Target::Ip(ip) => ip.to_string(),
            Target::Name(n) => n.clone(),
        }
    }

    /// The address to share: port 443, so no port in it.
    pub fn url(&self) -> String {
        match self {
            Target::Ip(IpAddr::V6(v6)) => format!("https://[{v6}]"),
            t => format!("https://{}", t.label()),
        }
    }
}

/// "4.3.2.1.in-addr.arpa" for 1.2.3.4; nibbles for IPv6.
pub fn reverse_name(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            format!("{}.{}.{}.{}.in-addr.arpa", o[3], o[2], o[1], o[0])
        }
        IpAddr::V6(v6) => {
            let mut s = String::new();
            for b in v6.octets().iter().rev() {
                s.push_str(&format!("{:x}.{:x}.", b & 0xf, b >> 4));
            }
            s + "ip6.arpa"
        }
    }
}

/// "203-0-113-7.sslip.io"; IPv6 "2a01-4f8--1.sslip.io".
pub fn sslip_name(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => format!("{}.sslip.io", v4.to_string().replace('.', "-")),
        IpAddr::V6(v6) => format!("{}.sslip.io", v6.to_string().replace(':', "-")),
    }
}

/// A public address: not private, loopback, link-local, Tailscale/CGNAT or
/// documentation space. Let's Encrypt only issues for these.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.is_multicast()
                || v.is_documentation()
                || matches!(o, [100, 64..=127, ..])
                || o[0] == 0
                || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            let s0 = v.segments()[0];
            // Global unicast is 2000::/3; 2001:db8::/32 is documentation.
            (s0 & 0xe000) == 0x2000 && !(s0 == 0x2001 && v.segments()[1] == 0x0db8)
        }
    }
}

/// When to renew: half way for short-lived certificates (every ~3 days for
/// a 6-day one), two thirds of the way for normal ones (30 days left of 90).
pub fn renew_at(not_before: i64, not_after: i64) -> i64 {
    let life = (not_after - not_before).max(0);
    if life < 10 * 86_400 { not_before + life / 2 } else { not_before + life * 2 / 3 }
}

/// How long to wait after the `n`th failure in a row (1-based). Let's
/// Encrypt allows 5 failed checks per name an hour; this stays well under.
pub fn backoff(n: u32) -> Duration {
    const STEPS: [u64; 5] = [5 * 60, 15 * 60, 60 * 60, 3 * 3600, 6 * 3600];
    Duration::from_secs(STEPS[(n.max(1) as usize - 1).min(STEPS.len() - 1)])
}

/// Whether Let's Encrypt refused the IP itself (not a failed check): then
/// the sslip.io name is the way.
fn refuses_ip(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        if let Some(instant_acme::Error::Api(p)) = c.downcast_ref::<instant_acme::Error>() {
            let t = p.r#type.as_deref().unwrap_or("");
            return ["rejectedIdentifier", "unsupportedIdentifier", "invalidProfile", "malformed"]
                .iter()
                .any(|k| t.ends_with(k));
        }
        c.to_string().contains("offers no challenge")
    })
}

/// The certificate being served.
pub struct Issued {
    pub target: Target,
    pub ck: Arc<CertifiedKey>,
    pub not_before: i64,
    pub not_after: i64,
    pub issuer: String,
}

/// What Settings › Access & devices shows.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub staging: bool,
    /// Working on a certificate right now.
    pub working: bool,
    /// The address to share (`https://…`), while a certificate is served.
    pub share: Option<String>,
    /// What the served certificate covers.
    pub covers: Option<String>,
    pub issuer: Option<String>,
    pub not_after: Option<i64>,
    pub renew_at: Option<i64>,
    /// This server's public IP, as last found.
    pub public_ip: Option<String>,
    pub domain: Option<String>,
    /// Using sslip.io because Let's Encrypt won't issue for the IP.
    pub sslip: bool,
    pub last_error: Option<String>,
    pub last_try: Option<i64>,
    pub next_try: Option<i64>,
    /// A note that isn't an error (the domain doesn't point here, say).
    pub note: Option<String>,
    /// The public IP changed: from what to what, and the new address.
    pub ip_change: Option<String>,
}

/// Saved next to the certificate.
#[derive(Serialize, Deserialize)]
struct Meta {
    target: Target,
    staging: bool,
}

/// Kept between runs.
#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// Let's Encrypt refused this IP: use its sslip.io name.
    #[serde(default)]
    sslip_for: Option<IpAddr>,
    /// The public IP as last confirmed (a change is noticed across restarts).
    #[serde(default)]
    ip: IpWatch,
    /// The last IP change, in words for the panel.
    #[serde(default)]
    ip_change: Option<String>,
    /// Certificates got lately: (name, unix time). See [`next_allowed`].
    #[serde(default)]
    issued: Vec<(String, i64)>,
}

/// Follows the public IP. A home connection's IP changes now and then; a
/// new one counts once two checks in a row (~15 minutes apart) see it, so
/// one odd answer from a lookup service or a short flap doesn't start a new
/// certificate. A failed lookup keeps the IP as it was.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IpWatch {
    pub current: Option<IpAddr>,
    #[serde(default)]
    pending: Option<IpAddr>,
}

#[derive(Debug, PartialEq)]
pub enum IpSeen {
    /// The lookup failed.
    Unknown,
    First(IpAddr),
    Same,
    /// A different IP, seen once: waiting for the next check to confirm it.
    Waiting(IpAddr),
    Changed { from: IpAddr, to: IpAddr },
}

impl IpWatch {
    pub fn observe(&mut self, seen: Option<IpAddr>) -> IpSeen {
        let Some(ip) = seen else { return IpSeen::Unknown };
        match self.current {
            None => {
                self.current = Some(ip);
                self.pending = None;
                IpSeen::First(ip)
            }
            Some(c) if c == ip => {
                self.pending = None;
                IpSeen::Same
            }
            Some(from) if self.pending == Some(ip) => {
                self.current = Some(ip);
                self.pending = None;
                IpSeen::Changed { from, to: ip }
            }
            Some(_) => {
                self.pending = Some(ip);
                IpSeen::Waiting(ip)
            }
        }
    }
}

/// How often to look up the public IP while the certificate is fine.
pub const IP_CHECK: Duration = Duration::from_secs(15 * 60);

/// Let's Encrypt allows 5 certificates for the same name a week, and an IP
/// that keeps changing could run up new orders. Stay under both: at most 4
/// per name a week and 6 in all a day. `None`: allowed now; else when.
pub fn next_allowed(issued: &[(String, i64)], name: &str, now: i64) -> Option<i64> {
    const WEEK: i64 = 7 * 86_400;
    const DAY: i64 = 86_400;
    let at = |mut times: Vec<i64>, max: usize, window: i64| {
        // Allowed again once the oldest that still counts drops out.
        times.sort();
        (times.len() >= max).then(|| times[times.len() - max] + window)
    };
    let per_name = at(issued.iter().filter(|(n, t)| n == name && now - t < WEEK).map(|x| x.1).collect(), 4, WEEK);
    let all = at(issued.iter().filter(|(_, t)| now - t < DAY).map(|x| x.1).collect(), 6, DAY);
    per_name.max(all).filter(|w| *w > now)
}

pub struct Acme {
    dir: PathBuf,
    cert: RwLock<Option<Arc<Issued>>>,
    /// TLS-ALPN-01 answers, by the name the check sends.
    alpn: Mutex<HashMap<String, Arc<CertifiedKey>>>,
    /// HTTP-01 answers: token → key authorization.
    http: Mutex<HashMap<String, String>>,
    status: Mutex<Status>,
    /// Wakes the renewal loop (settings changed).
    pub wake: Notify,
}

impl std::fmt::Debug for Acme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Acme")
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl Acme {
    pub fn new(data_dir: &std::path::Path) -> Arc<Acme> {
        Arc::new(Acme {
            dir: data_dir.join("certs"),
            cert: RwLock::new(None),
            alpn: Default::default(),
            http: Default::default(),
            status: Default::default(),
            wake: Notify::new(),
        })
    }

    pub fn status(&self) -> Status {
        let mut s = self.status.lock().unwrap().clone();
        if let Some(c) = self.cert.read().unwrap().as_ref() {
            s.share = Some(c.target.url());
            s.covers = Some(c.target.label());
            s.issuer = Some(c.issuer.clone());
            s.not_after = Some(c.not_after);
            s.renew_at = Some(renew_at(c.not_before, c.not_after));
        }
        s
    }

    /// The address to share, while a trusted certificate is served.
    pub fn trusted_url(&self) -> Option<String> {
        self.cert.read().unwrap().as_ref().map(|c| c.target.url())
    }

    /// The certificate for a connection, if this one covers it: by name
    /// (SNI), or by IP for clients from the internet (they send no name).
    pub fn cert_for(&self, sni: Option<&str>, from_internet: bool) -> Option<Arc<CertifiedKey>> {
        let g = self.cert.read().unwrap_or_else(|e| e.into_inner());
        let c = g.as_ref()?;
        let hit = match (&c.target, sni) {
            (Target::Name(n), Some(s)) => n.eq_ignore_ascii_case(s),
            (Target::Ip(_), None) => from_internet,
            _ => false,
        };
        hit.then(|| c.ck.clone())
    }

    /// The TLS-ALPN-01 answer for the name the check sent.
    pub fn alpn_cert(&self, sni: Option<&str>) -> Option<Arc<CertifiedKey>> {
        self.alpn.lock().unwrap().get(&sni?.to_ascii_lowercase()).cloned()
    }

    /// The HTTP-01 answer for a token.
    pub fn http_answer(&self, token: &str) -> Option<String> {
        self.http.lock().unwrap().get(token).cloned()
    }

    fn set_status(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.status.lock().unwrap());
    }

    fn saved(&self) -> Saved {
        std::fs::read_to_string(self.dir.join("state.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save_state(&self, s: &Saved) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(self.dir.join("state.json"), serde_json::to_string_pretty(s).unwrap_or_default());
    }

    /// The saved certificate, if it's for `target` on this directory and
    /// still valid.
    fn load(&self, staging: bool) -> Option<Issued> {
        let meta: Meta = serde_json::from_str(&std::fs::read_to_string(self.dir.join("meta.json")).ok()?).ok()?;
        if meta.staging != staging {
            return None;
        }
        let chain = std::fs::read_to_string(self.dir.join("cert.pem")).ok()?;
        let key = std::fs::read_to_string(self.dir.join("key.pem")).ok()?;
        let i = parse(meta.target, &chain, &key).ok()?;
        (i.not_after > now()).then_some(i)
    }

    /// Saves the certificate as the one served, and under its name, so an
    /// IP that changes back reuses it instead of asking for another.
    fn store(&self, target: &Target, staging: bool, chain: &str, key: &str) -> anyhow::Result<()> {
        for d in [self.dir.clone(), self.by_name(target, staging)] {
            std::fs::create_dir_all(&d)?;
            std::fs::write(d.join("cert.pem"), chain)?;
            std::fs::write(d.join("key.pem"), key)?;
            std::fs::write(d.join("meta.json"), serde_json::to_string_pretty(&Meta { target: target.clone(), staging })?)?;
        }
        Ok(())
    }

    fn by_name(&self, target: &Target, staging: bool) -> PathBuf {
        let safe: String =
            target.label().chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
        self.dir.join(if staging { "by-name-staging" } else { "by-name" }).join(safe)
    }

    /// A saved certificate for `target` that isn't due for renewal yet.
    fn load_for(&self, target: &Target, staging: bool) -> Option<Issued> {
        let d = self.by_name(target, staging);
        let chain = std::fs::read_to_string(d.join("cert.pem")).ok()?;
        let key = std::fs::read_to_string(d.join("key.pem")).ok()?;
        let i = parse(target.clone(), &chain, &key).ok()?;
        (renew_at(i.not_before, i.not_after) > now()).then_some(i)
    }

    async fn account(&self, staging: bool, email: &str) -> anyhow::Result<Account> {
        let file = self.dir.join(if staging { "account-staging.json" } else { "account.json" });
        if let Ok(t) = std::fs::read_to_string(&file) {
            let creds: AccountCredentials = serde_json::from_str(&t).context("reading the saved Let's Encrypt account")?;
            return Ok(Account::builder()?.from_credentials(creds).await?);
        }
        let url = if staging { LetsEncrypt::Staging } else { LetsEncrypt::Production }.url().to_owned();
        let contact = format!("mailto:{}", email.trim());
        let contacts = [contact.as_str()];
        let (account, creds) = Account::builder()?
            .create(
                &NewAccount {
                    contact: if email.trim().is_empty() { &[] } else { &contacts },
                    terms_of_service_agreed: true,
                    only_return_existing: false,
                },
                url,
                None,
            )
            .await
            .context("making a Let's Encrypt account")?;
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(&file, serde_json::to_string_pretty(&creds)?)?;
        log::info!("made a Let's Encrypt account{}", if staging { " (staging)" } else { "" });
        Ok(account)
    }

    /// Gets a certificate for `target`. `alpn_first`: answer the TLS-ALPN-01
    /// check (port 443) rather than HTTP-01 (port 80) when both are offered.
    async fn order(&self, account: &Account, target: &Target, alpn_first: bool) -> anyhow::Result<(String, String)> {
        let ids = [target.identifier()];
        let mut req = NewOrder::new(&ids);
        if matches!(target, Target::Ip(_)) {
            // IP certificates come only in the short-lived profile.
            req = req.profile("shortlived");
        }
        let mut order = account.new_order(&req).await?;
        let mut used: Vec<(ChallengeType, String)> = Vec::new();
        {
            let mut authz = order.authorizations();
            while let Some(a) = authz.next().await {
                let mut a = a?;
                match a.status {
                    AuthorizationStatus::Pending => {}
                    AuthorizationStatus::Valid => continue,
                    s => anyhow::bail!("Let's Encrypt's check for {} is {s:?}", target.label()),
                }
                let offered: Vec<ChallengeType> = a.challenges.iter().map(|c| c.r#type.clone()).collect();
                let ty = pick_challenge(&offered, alpn_first)
                    .with_context(|| format!("Let's Encrypt offers no challenge Hopper Server can answer for {}", target.label()))?;
                let mut ch = a.challenge(ty.clone()).context("challenge")?;
                let ka = ch.key_authorization();
                let key = match ty {
                    ChallengeType::TlsAlpn01 => {
                        let ck = alpn_answer(target, ka.digest().as_ref())?;
                        let sni = target.sni();
                        self.alpn.lock().unwrap().insert(sni.clone(), Arc::new(ck));
                        sni
                    }
                    _ => {
                        let token = ch.token.clone();
                        self.http.lock().unwrap().insert(token.clone(), ka.as_str().to_string());
                        token
                    }
                };
                used.push((ty, key));
                ch.set_ready().await?;
            }
        }
        let polled = order.poll_ready(&RetryPolicy::new().initial_delay(Duration::from_secs(2)).timeout(Duration::from_secs(120))).await;
        let status = match polled {
            Ok(s) => s,
            Err(e) => {
                self.forget(&used);
                return Err(e.into());
            }
        };
        if status != OrderStatus::Ready {
            // Why: the failed challenge's error.
            let mut why = order.state().error.as_ref().map(|p| p.to_string());
            let mut authz = order.authorizations();
            while let Some(Ok(mut a)) = authz.next().await {
                if let Ok(st) = a.refresh().await
                    && let Some(p) = st.challenges.iter().find_map(|c| c.error.as_ref())
                {
                    why = Some(p.to_string());
                }
            }
            drop(authz);
            self.forget(&used);
            let port = used.first().map_or(443, |(t, _)| if *t == ChallengeType::TlsAlpn01 { 443 } else { 80 });
            let why = why.unwrap_or_else(|| format!("{status:?}"));
            log::warn!("Let's Encrypt's check of {} on port {port} failed: {why}", target.label());
            anyhow::bail!("{}", plain_failure(&target.label(), port, &why));
        }
        self.forget(&used);
        let mut p = rcgen::CertificateParams::new(Vec::<String>::new())?;
        p.distinguished_name = rcgen::DistinguishedName::new();
        p.subject_alt_names = vec![san(target)?];
        let key = rcgen::KeyPair::generate()?;
        let csr = p.serialize_request(&key)?;
        order.finalize_csr(csr.der()).await?;
        let chain = order.poll_certificate(&RetryPolicy::new().initial_delay(Duration::from_secs(1)).timeout(Duration::from_secs(60))).await?;
        Ok((chain, key.serialize_pem()))
    }

    fn forget(&self, used: &[(ChallengeType, String)]) {
        for (t, k) in used {
            if *t == ChallengeType::TlsAlpn01 {
                self.alpn.lock().unwrap().remove(k);
            } else {
                self.http.lock().unwrap().remove(k);
            }
        }
    }
}

/// A failed check in words for the panel (the raw error goes to the log).
fn plain_failure(label: &str, port: u16, why: &str) -> String {
    let w = why.to_ascii_lowercase();
    if w.contains("error:tls") || w.contains("unrecognized name") || w.contains("error:unauthorized") || w.contains("acmeidentifier") {
        format!("{PROXY_HINT} Port {port} at {label} is answered by another server, likely a reverse proxy (Nginx Proxy Manager, Caddy…) that already gives the panel its certificate, so you don't need this: turn it off. If not, forward port {port} to this panel.")
    } else if w.contains("timeout") || w.contains("timed out") || w.contains("refused") || w.contains("error:connection") {
        format!("Let's Encrypt couldn't connect to port {port} at {label}. Forward port {port} on your router to this panel's port.")
    } else if w.contains("ratelimited") || w.contains("too many") {
        format!("Let's Encrypt says too many tries for {label} lately. Hopper Server tries again later by itself.")
    } else {
        format!("Let's Encrypt couldn't check {label} on port {port}: {why}")
    }
}

/// Starts the failure text when a reverse proxy holds the port.
const PROXY_HINT: &str = "A reverse proxy seems to handle HTTPS here.";

/// How long to wait before checking again when a proxy holds the port: it
/// rarely goes away, so a retry a day instead of the usual backoff.
const PROXY_RETRY: Duration = Duration::from_secs(24 * 3600);

fn san(target: &Target) -> anyhow::Result<rcgen::SanType> {
    Ok(match target {
        Target::Ip(ip) => rcgen::SanType::IpAddress(*ip),
        Target::Name(n) => rcgen::SanType::DnsName(n.as_str().try_into()?),
    })
}

/// TLS-ALPN-01 first (port 443, the one usually forwarded), or HTTP-01.
fn pick_challenge(offered: &[ChallengeType], alpn_first: bool) -> Option<ChallengeType> {
    let has = |t: &ChallengeType| offered.contains(t);
    let order = if alpn_first { [ChallengeType::TlsAlpn01, ChallengeType::Http01] } else { [ChallengeType::Http01, ChallengeType::TlsAlpn01] };
    order.into_iter().find(has)
}

/// The self-signed certificate TLS-ALPN-01 checks for (RFC 8737): the name
/// or IP, and the key authorization's digest in the acmeIdentifier extension.
fn alpn_answer(target: &Target, digest: &[u8]) -> anyhow::Result<CertifiedKey> {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new())?;
    p.subject_alt_names = vec![san(target)?];
    p.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let key = rcgen::KeyPair::generate()?;
    let cert = p.self_signed(&key)?;
    let pk = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    let signing = rustls::crypto::ring::sign::any_supported_type(&pk).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(CertifiedKey::new(vec![cert.der().clone()], signing))
}

fn pem_blocks(pem: &str) -> Vec<Vec<u8>> {
    use base64::Engine as _;
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for l in pem.lines().map(str::trim) {
        if l.starts_with("-----BEGIN") {
            cur = Some(String::new());
        } else if l.starts_with("-----END") {
            if let Some(b) = cur.take().and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok()) {
                out.push(b);
            }
        } else if let Some(b) = cur.as_mut() {
            b.push_str(l);
        }
    }
    out
}

/// The served certificate from its PEM chain and key, with its dates and
/// issuer read from the leaf.
fn parse(target: Target, chain: &str, key: &str) -> anyhow::Result<Issued> {
    let certs: Vec<CertificateDer<'static>> = pem_blocks(chain).into_iter().map(CertificateDer::from).collect();
    let leaf = certs.first().context("no certificate in the chain")?;
    let (_, x) = x509_parser::parse_x509_certificate(leaf).map_err(|e| anyhow::anyhow!("reading the certificate: {e}"))?;
    let not_before = x.validity().not_before.timestamp();
    let not_after = x.validity().not_after.timestamp();
    let org = x.issuer().iter_organization().next().and_then(|a| a.as_str().ok()).unwrap_or("");
    let cn = x.issuer().iter_common_name().next().and_then(|a| a.as_str().ok()).unwrap_or("");
    let issuer = match (org, cn) {
        ("", cn) => cn.to_string(),
        (o, "") => o.to_string(),
        (o, cn) if cn.contains(o) => cn.to_string(),
        (o, cn) => format!("{o} ({cn})"),
    };
    let key = pem_blocks(key).into_iter().next().context("no key")?;
    let pk = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key));
    let signing = rustls::crypto::ring::sign::any_supported_type(&pk).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Issued { target, ck: Arc::new(CertifiedKey::new(certs, signing)), not_before, not_after, issuer })
}

/// This server's public IP: one set in settings (`public_names`), a public
/// address on a network card (a VPS), else what an IP echo service sees
/// (behind a home router).
async fn public_ip(public_names: &[String]) -> Option<IpAddr> {
    if let Some(ip) = public_names.iter().filter_map(|n| n.trim().parse::<IpAddr>().ok()).find(|ip| is_public(*ip)) {
        return Some(ip);
    }
    if let Ok(list) = if_addrs::get_if_addrs()
        && let Some(ip) = list.iter().map(|i| i.ip()).filter(|ip| ip.is_ipv4()).find(|ip| is_public(*ip))
    {
        return Some(ip);
    }
    tokio::task::spawn_blocking(|| {
        let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into();
        ["https://api.ipify.org", "https://ipv4.icanhazip.com"].iter().find_map(|u| {
            let text = agent.get(*u).call().ok()?.body_mut().read_to_string().ok()?;
            text.trim().parse::<IpAddr>().ok().filter(|ip| is_public(*ip))
        })
    })
    .await
    .ok()
    .flatten()
}

/// What to get a certificate for, given the settings and the public IP.
/// Also a note for the panel when the domain is skipped.
async fn choose(
    domain: Option<&str>,
    ip: Option<IpAddr>,
    sslip_for: Option<IpAddr>,
    serving: Option<&Target>,
) -> (Option<Target>, Option<String>) {
    let mut note = None;
    if let Some(d) = domain.map(str::trim).filter(|d| !d.is_empty()) {
        // Already serving the domain: keep it. After an IP change a dynamic
        // DNS name takes a while to follow; don't flip to the IP meanwhile.
        if serving == Some(&Target::Name(d.to_string())) {
            return (serving.cloned(), None);
        }
        let resolved: Vec<IpAddr> = tokio::net::lookup_host((d, 443)).await.map(|a| a.map(|s| s.ip()).collect()).unwrap_or_default();
        match ip {
            // Can't tell where it points without knowing our IP: try it.
            None => return (Some(Target::Name(d.to_string())), None),
            Some(ip) if resolved.contains(&ip) => return (Some(Target::Name(d.to_string())), None),
            Some(ip) => {
                note = Some(if resolved.is_empty() {
                    format!("{d} doesn't resolve, so the certificate is for {ip} instead.")
                } else {
                    format!("{d} points at {}, not this server ({ip}), so the certificate is for {ip} instead.", resolved[0])
                });
            }
        }
    }
    let target = ip.map(|ip| if sslip_for == Some(ip) { Target::Name(sslip_name(ip)) } else { Target::Ip(ip) });
    (target, note)
}

/// Keeps a trusted certificate while the setting is on: loads the saved one,
/// gets a new one when it's missing, for another name, or due for renewal,
/// and retries failures with growing pauses.
pub fn start(app: Arc<crate::App>) {
    let acme = app.acme.clone();
    tokio::spawn(async move {
        let mut failures: u32 = 0;
        loop {
            let cfg = app.cfg.read().unwrap().clone();
            let enabled = cfg.auto_cert_on();
            let staging = cfg.acme_staging;
            acme.set_status(|s| {
                s.enabled = enabled;
                s.staging = staging;
                s.domain = cfg.acme_domain.clone().filter(|d| !d.trim().is_empty());
            });
            if !enabled {
                *acme.cert.write().unwrap() = None;
                acme.set_status(|s| {
                    s.next_try = None;
                    s.working = false;
                });
                failures = 0;
                acme.wake.notified().await;
                continue;
            }
            // What's saved, until something better comes.
            if acme.cert.read().unwrap().is_none()
                && let Some(i) = acme.load(staging)
            {
                log::info!("serving the saved certificate for {} until {}", i.target.label(), i.not_after);
                *acme.cert.write().unwrap() = Some(Arc::new(i));
            }
            let mut saved = acme.saved();
            let ip_seen = saved.ip.observe(public_ip(&cfg.public_names).await);
            let ip = saved.ip.current;
            let current = acme.cert.read().unwrap().clone();
            let (target, note) =
                choose(cfg.acme_domain.as_deref(), ip, saved.sslip_for, current.as_ref().map(|c| &c.target)).await;
            match ip_seen {
                IpSeen::Changed { from, to } => {
                    log::info!("public IP changed from {from} to {to}");
                    let share = target.as_ref().map_or(String::new(), |t| format!(" The address to share is now {}.", t.url()));
                    saved.ip_change = Some(format!("Your public IP changed from {from} to {to}.{share}"));
                    acme.save_state(&saved);
                }
                IpSeen::First(_) | IpSeen::Waiting(_) => acme.save_state(&saved),
                IpSeen::Same | IpSeen::Unknown => {}
            }
            acme.set_status(|s| {
                s.public_ip = ip.map(|i| i.to_string());
                s.sslip = matches!((&target, ip), (Some(Target::Name(n)), Some(ip)) if *n == sslip_name(ip));
                s.note = note;
                s.ip_change = saved.ip_change.clone();
            });
            let Some(target) = target else {
                acme.set_status(|s| s.last_error = Some("Couldn't find this server's public IP. Add it under public names in hopperd.toml.".into()));
                wait(&acme, IP_CHECK).await;
                continue;
            };
            let due = match &current {
                Some(c) if c.target == target => renew_at(c.not_before, c.not_after) - now(),
                _ => 0,
            };
            if due > 0 {
                // Look at the public IP again every 15 minutes meanwhile.
                failures = 0;
                let pause = due.min(IP_CHECK.as_secs() as i64);
                acme.set_status(|s| s.next_try = Some(now() + pause));
                wait(&acme, Duration::from_secs(pause as u64)).await;
                continue;
            }
            // A name that still has a good certificate (the IP changed
            // back): reuse it. Until a new one is ready the old one is served.
            if let Some(i) = acme.load_for(&target, staging) {
                log::info!("reusing the saved certificate for {}", i.target.label());
                *acme.cert.write().unwrap() = Some(Arc::new(i));
                continue;
            }
            if let Some(at) = next_allowed(&saved.issued, &target.label(), now()) {
                log::warn!("not asking Let's Encrypt for {} again until {at}: many certificates lately", target.label());
                acme.set_status(|s| {
                    s.last_error = Some(format!(
                        "Many certificates for {} lately (does the IP keep changing?). Waiting, to stay under Let's Encrypt's limits.",
                        target.label()
                    ));
                    s.next_try = Some(at);
                });
                wait(&acme, Duration::from_secs((at - now()).clamp(60, 86_400) as u64)).await;
                continue;
            }
            acme.set_status(|s| {
                s.working = true;
                s.last_try = Some(now());
            });
            log::info!("getting a Let's Encrypt certificate for {}{}", target.label(), if staging { " (staging)" } else { "" });
            let got = async {
                let account = acme.account(staging, &cfg.acme_email).await?;
                // Port 443 first; every other try port 80.
                let (chain, key) = acme.order(&account, &target, failures % 2 == 0).await?;
                let issued = parse(target.clone(), &chain, &key)?;
                acme.store(&target, staging, &chain, &key)?;
                anyhow::Ok(issued)
            }
            .await;
            acme.set_status(|s| s.working = false);
            match got {
                Ok(i) => {
                    log::info!("got a certificate for {} from {}, valid until {}", i.target.label(), i.issuer, i.not_after);
                    let mut saved = acme.saved();
                    saved.issued.retain(|(_, t)| now() - t < 7 * 86_400);
                    saved.issued.push((i.target.label(), now()));
                    acme.save_state(&saved);
                    *acme.cert.write().unwrap() = Some(Arc::new(i));
                    acme.set_status(|s| s.last_error = None);
                    failures = 0;
                }
                Err(e) if matches!(target, Target::Ip(_)) && refuses_ip(&e) => {
                    let Target::Ip(ip) = target else { unreachable!() };
                    log::warn!("Let's Encrypt won't issue for {ip} ({e:#}); using {}", sslip_name(ip));
                    let mut saved = acme.saved();
                    saved.sslip_for = Some(ip);
                    acme.save_state(&saved);
                    acme.set_status(|s| s.last_error = Some(format!("Let's Encrypt won't issue a certificate for the IP address, so {} is used. ({e:#})", sslip_name(ip))));
                }
                Err(e) => {
                    failures += 1;
                    // A proxy on the port: stop the usual retries; switching
                    // the setting off and on tries again right away.
                    let proxy = format!("{e:#}").starts_with(PROXY_HINT);
                    let pause = if proxy { PROXY_RETRY } else { backoff(failures) };
                    log::warn!("Let's Encrypt: {e:#}; trying again in {} min", pause.as_secs() / 60);
                    acme.set_status(|s| {
                        s.last_error = Some(format!("{e:#}"));
                        s.next_try = Some(now() + pause.as_secs() as i64);
                    });
                    wait(&acme, pause).await;
                }
            }
        }
    });
}

async fn wait(acme: &Acme, d: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(d) => {}
        _ = acme.wake.notified() => {}
    }
}

// ---- API ----------------------------------------------------------------------------------

use crate::App;
use crate::api::{ApiResult, bad, require_owner};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::routing::get;
use axum::{Json, Router};

pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/api/v1/settings/certificate", get(get_status).put(put_settings))
}

/// Let's Encrypt's HTTP-01 check, over plain HTTP from the internet: added
/// outside the sign-in guard, and it only ever answers a pending token.
pub fn challenge_route() -> Router<Arc<App>> {
    Router::new().route("/.well-known/acme-challenge/{token}", get(http_challenge))
}

async fn http_challenge(State(app): State<Arc<App>>, Path(token): Path<String>) -> Result<String, StatusCode> {
    app.acme.http_answer(&token).ok_or(StatusCode::NOT_FOUND)
}

async fn get_status(State(app): State<Arc<App>>, headers: HeaderMap, method: Method) -> ApiResult<Json<Status>> {
    require_owner(&app, &headers, &method)?;
    let mut s = app.acme.status();
    s.enabled = app.cfg.read().unwrap().auto_cert_on();
    Ok(Json(s))
}

#[derive(Deserialize)]
struct CertSettings {
    auto: bool,
}

async fn put_settings(State(app): State<Arc<App>>, headers: HeaderMap, method: Method, Json(b): Json<CertSettings>) -> ApiResult<Json<Status>> {
    require_owner(&app, &headers, &method)?;
    {
        let mut cfg = app.cfg.write().unwrap();
        cfg.auto_cert = Some(b.auto);
        cfg.save(&app.data_dir).map_err(|e| bad(format!("{e:#}")))?;
    }
    log::info!("automatic certificates {}", if b.auto { "on" } else { "off" });
    app.acme.set_status(|s| s.enabled = b.auto);
    app.acme.wake.notify_one();
    Ok(Json(app.acme.status()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_for_an_ip() {
        let v4: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(reverse_name(v4), "7.113.0.203.in-addr.arpa");
        assert_eq!(sslip_name(v4), "203-0-113-7.sslip.io");
        let v6: IpAddr = "2001:db8::567:89ab".parse().unwrap();
        assert_eq!(reverse_name(v6), "b.a.9.8.7.6.5.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.0.8.b.d.0.1.0.0.2.ip6.arpa");
        assert_eq!(sslip_name("2a01:4f8::1".parse().unwrap()), "2a01-4f8--1.sslip.io");
        assert_eq!(Target::Ip(v4).url(), "https://203.0.113.7");
        assert_eq!(Target::Ip("2a01:4f8::1".parse().unwrap()).url(), "https://[2a01:4f8::1]");
    }

    #[test]
    fn public_addresses() {
        for ip in ["8.8.8.8", "2a01:4f8::1", "81.2.69.160"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["192.168.50.3", "10.0.0.1", "172.16.4.4", "127.0.0.1", "100.101.102.103", "169.254.1.1", "203.0.113.7", "::1", "fe80::1", "fd00::1", "2001:db8::1", "0.0.0.0"] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn renews_half_way_for_short_certs_and_at_two_thirds_for_long_ones() {
        let h = 3600;
        // 160-hour IP certificate: renew after 80 hours (~3.3 days).
        assert_eq!(renew_at(0, 160 * h), 80 * h);
        // 90-day certificate: renew with 30 days left.
        assert_eq!(renew_at(0, 90 * 86_400), 60 * 86_400);
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(300));
        assert!(backoff(2) > backoff(1) && backoff(3) > backoff(2));
        assert_eq!(backoff(5), backoff(50));
        assert_eq!(backoff(50), Duration::from_secs(6 * 3600));
    }

    #[test]
    fn failures_in_plain_words() {
        let other = "API error: 41.47.116.198: remote error: tls: unrecognized name (urn:ietf:params:acme:error:tls)";
        assert!(plain_failure("41.47.116.198", 443, other).starts_with(PROXY_HINT));
        let none = "API error: 81.2.69.160: Timeout during connect (likely firewall problem) (urn:ietf:params:acme:error:connection)";
        assert!(plain_failure("81.2.69.160", 443, none).contains("couldn't connect to port 443"));
        assert!(plain_failure("x", 80, "odd").ends_with("odd"));
    }

    #[test]
    fn picks_port_443_first() {
        let both = [ChallengeType::Http01, ChallengeType::Dns01, ChallengeType::TlsAlpn01];
        assert_eq!(pick_challenge(&both, true), Some(ChallengeType::TlsAlpn01));
        assert_eq!(pick_challenge(&both, false), Some(ChallengeType::Http01));
        assert_eq!(pick_challenge(&[ChallengeType::Http01], true), Some(ChallengeType::Http01));
        assert_eq!(pick_challenge(&[ChallengeType::Dns01], true), None);
    }

    #[tokio::test]
    async fn picks_the_name() {
        let ip: IpAddr = "81.2.69.160".parse().unwrap();
        assert_eq!(choose(None, Some(ip), None, None).await.0, Some(Target::Ip(ip)));
        assert_eq!(choose(None, Some(ip), Some(ip), None).await.0, Some(Target::Name("81-2-69-160.sslip.io".into())));
        // Refused for an older IP: the new one is tried as an IP again.
        assert_eq!(choose(None, Some(ip), Some("81.2.69.161".parse().unwrap()), None).await.0, Some(Target::Ip(ip)));
        assert_eq!(choose(None, None, None, None).await.0, None);
        // A domain that doesn't resolve: the IP, with a note.
        let (t, note) = choose(Some("nothing-here.invalid"), Some(ip), None, None).await;
        assert_eq!(t, Some(Target::Ip(ip)));
        assert!(note.unwrap().contains("doesn't resolve"));
        // Without our IP, the domain is tried.
        assert_eq!(choose(Some("hopper.example.com"), None, None, None).await.0, Some(Target::Name("hopper.example.com".into())));
        // Serving the domain while the IP just changed (dynamic DNS not caught up): stay.
        let d = Target::Name("nothing-here.invalid".into());
        assert_eq!(choose(Some("nothing-here.invalid"), Some(ip), None, Some(&d)).await.0, Some(d));
    }

    #[test]
    fn notices_an_ip_change_on_the_second_sighting() {
        let (a, b): (IpAddr, IpAddr) = ("81.2.69.160".parse().unwrap(), "81.2.69.161".parse().unwrap());
        let mut w = IpWatch::default();
        assert_eq!(w.observe(None), IpSeen::Unknown);
        assert_eq!(w.observe(Some(a)), IpSeen::First(a));
        assert_eq!(w.observe(Some(a)), IpSeen::Same);
        // A failed lookup keeps the IP.
        assert_eq!(w.observe(None), IpSeen::Unknown);
        assert_eq!(w.current, Some(a));
        // One odd answer, then back: no change.
        assert_eq!(w.observe(Some(b)), IpSeen::Waiting(b));
        assert_eq!(w.observe(Some(a)), IpSeen::Same);
        assert_eq!(w.current, Some(a));
        // Seen twice in a row: changed.
        assert_eq!(w.observe(Some(b)), IpSeen::Waiting(b));
        assert_eq!(w.observe(Some(b)), IpSeen::Changed { from: a, to: b });
        assert_eq!(w.current, Some(b));
        assert_eq!(w.observe(Some(b)), IpSeen::Same);
        // Kept across restarts.
        let back: IpWatch = serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert_eq!(back, w);
    }

    #[test]
    fn a_flapping_ip_stays_under_lets_encrypts_limits() {
        let day = 86_400;
        let now = 100 * day;
        assert_eq!(next_allowed(&[], "a", now), None);
        // 3 for one name this week: a 4th is fine; after 4 the next waits
        // until the oldest is a week old.
        let three: Vec<(String, i64)> = (1..=3).map(|d| ("a".to_string(), now - d * day)).collect();
        assert_eq!(next_allowed(&three, "a", now), None);
        let mut four = three.clone();
        four.push(("a".into(), now - 4 * day));
        assert_eq!(next_allowed(&four, "a", now), Some(now + 3 * day));
        // Another name isn't held up by these.
        assert_eq!(next_allowed(&four, "b", now), None);
        // Older than a week doesn't count.
        let old: Vec<(String, i64)> = (8..=12).map(|d| ("a".to_string(), now - d * day)).collect();
        assert_eq!(next_allowed(&old, "a", now), None);
        // Flapping between addresses: 6 today, whatever the names, waits a day.
        let today: Vec<(String, i64)> = (1..=6).map(|h| (format!("ip{h}"), now - h * 3600)).collect();
        assert_eq!(next_allowed(&today, "new", now), Some(now - 6 * 3600 + day));
    }

    #[test]
    fn reuses_a_saved_certificate_when_the_ip_changes_back() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("hopperd-acme-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Acme::new(&dir);
        let t = |s: &str| Target::Ip(s.parse().unwrap());
        let cert = |life: i64| {
            let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            p.not_before = time::OffsetDateTime::now_utc() - std::time::Duration::from_secs(3600);
            p.not_after = p.not_before + std::time::Duration::from_secs(life as u64);
            let key = rcgen::KeyPair::generate().unwrap();
            (p.self_signed(&key).unwrap().pem(), key.serialize_pem())
        };
        let (c, k) = cert(160 * 3600);
        a.store(&t("81.2.69.160"), false, &c, &k).unwrap();
        let (c2, k2) = cert(160 * 3600);
        a.store(&t("81.2.69.161"), false, &c2, &k2).unwrap();
        // Both kept by name; the last one is the one served at start.
        assert!(a.load_for(&t("81.2.69.160"), false).is_some());
        assert!(a.load_for(&t("81.2.69.160"), true).is_none(), "staging is separate");
        assert_eq!(a.load(false).unwrap().target, t("81.2.69.161"));
        // One past half its life is due: not reused.
        let (c3, k3) = cert(2 * 3600);
        a.store(&t("81.2.69.162"), false, &c3, &k3).unwrap();
        assert!(a.load_for(&t("81.2.69.162"), false).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serves_by_name_or_to_the_internet_by_ip() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let a = Acme::new(&std::env::temp_dir());
        let ip: IpAddr = "81.2.69.160".parse().unwrap();
        let ck = alpn_answer(&Target::Ip(ip), &[0u8; 32]).unwrap();
        let issued = |t: Target| Issued { target: t, ck: Arc::new(ck.clone()), not_before: 0, not_after: i64::MAX, issuer: String::new() };
        *a.cert.write().unwrap() = Some(Arc::new(issued(Target::Ip(ip))));
        assert!(a.cert_for(None, true).is_some());
        assert!(a.cert_for(None, false).is_none(), "LAN clients keep the panel's own certificate");
        assert!(a.cert_for(Some("rig"), true).is_none());
        *a.cert.write().unwrap() = Some(Arc::new(issued(Target::Name("81-2-69-160.sslip.io".into()))));
        assert!(a.cert_for(Some("81-2-69-160.SSLIP.io"), false).is_some());
        assert!(a.cert_for(None, true).is_none());
        // Challenge answers by the name the check sends.
        a.alpn.lock().unwrap().insert(Target::Ip(ip).sni(), Arc::new(ck.clone()));
        assert!(a.alpn_cert(Some("160.69.2.81.in-addr.arpa")).is_some());
        assert!(a.alpn_cert(None).is_none());
    }

    #[test]
    fn reads_dates_and_issuer_from_a_chain() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut p = rcgen::CertificateParams::new(vec!["x.sslip.io".to_string()]).unwrap();
        p.not_before = time::OffsetDateTime::from_unix_timestamp(1_000_000).unwrap();
        p.not_after = time::OffsetDateTime::from_unix_timestamp(1_000_000 + 160 * 3600).unwrap();
        let mut dn = rcgen::DistinguishedName::new();
        dn.push(rcgen::DnType::OrganizationName, "Let's Encrypt");
        dn.push(rcgen::DnType::CommonName, "E7");
        p.distinguished_name = dn;
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = p.self_signed(&key).unwrap();
        let i = parse(Target::Name("x.sslip.io".into()), &cert.pem(), &key.serialize_pem()).unwrap();
        assert_eq!((i.not_before, i.not_after), (1_000_000, 1_000_000 + 160 * 3600));
        assert_eq!(i.issuer, "Let's Encrypt (E7)");
    }

    /// Against Let's Encrypt's staging server (network; run by hand with
    /// `--ignored`): an order for this PC's public IP in the short-lived
    /// profile is taken, and offers a challenge hopperd answers.
    #[tokio::test]
    #[ignore]
    async fn staging_takes_an_ip_order() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("hopperd-acme-{}", std::process::id()));
        let a = Acme::new(&dir);
        let ip = public_ip(&[]).await.expect("public IP");
        let account = a.account(true, "").await.unwrap();
        assert!(account.profiles().any(|p| p.name == "shortlived"));
        let ids = [Target::Ip(ip).identifier()];
        let mut order = account.new_order(&NewOrder::new(&ids).profile("shortlived")).await.unwrap();
        assert_eq!(order.state().profile.as_deref(), Some("shortlived"));
        let mut authz = order.authorizations();
        let a1 = authz.next().await.unwrap().unwrap();
        let offered: Vec<ChallengeType> = a1.challenges.iter().map(|c| c.r#type.clone()).collect();
        eprintln!("{ip}: {offered:?}");
        assert_eq!(pick_challenge(&offered, true), Some(ChallengeType::TlsAlpn01), "{offered:?}");
        // sslip.io names get a normal order too.
        let ids = [Target::Name(sslip_name(ip)).identifier()];
        account.new_order(&NewOrder::new(&ids)).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Accepts any server certificate: the test looks at which one came.
    #[derive(Debug)]
    struct AnyCert;
    impl rustls::client::danger::ServerCertVerifier for AnyCert {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
        }
    }

    /// The leaf certificate a client connecting as `name` (with `alpn`) gets.
    fn handshake(server: Arc<rustls::ServerConfig>, name: &str, alpn: Option<&[u8]>) -> CertificateDer<'static> {
        let mut c = rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(Arc::new(AnyCert)).with_no_client_auth();
        if let Some(a) = alpn {
            c.alpn_protocols = vec![a.to_vec()];
        }
        let sn = rustls::pki_types::ServerName::try_from(name.to_string()).unwrap();
        let mut cl = rustls::ClientConnection::new(Arc::new(c), sn).unwrap();
        let mut sv = rustls::ServerConnection::new(server).unwrap();
        for _ in 0..10 {
            let mut buf = Vec::new();
            cl.write_tls(&mut buf).unwrap();
            sv.read_tls(&mut buf.as_slice()).unwrap();
            sv.process_new_packets().unwrap();
            let mut buf = Vec::new();
            sv.write_tls(&mut buf).unwrap();
            cl.read_tls(&mut buf.as_slice()).unwrap();
            cl.process_new_packets().unwrap();
            if !cl.is_handshaking() {
                break;
            }
        }
        cl.peer_certificates().unwrap()[0].clone().into_owned()
    }

    #[test]
    fn handshakes_pick_the_right_certificate() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join(format!("hopperd-acme-hs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ca = crate::tls::load_or_create_ca(&dir, "rig").unwrap();
        let own = crate::tls::issue(&ca, &["rig".into()], &["192.168.50.3".parse().unwrap()]).unwrap();
        let own_leaf = own.cert[0].clone();
        let own: crate::tls::OwnCert = Arc::new(RwLock::new(Arc::new(own)));
        let a = Acme::new(&dir);
        let ip: IpAddr = "81.2.69.160".parse().unwrap();
        let le = alpn_answer(&Target::Ip(ip), &[1u8; 32]).unwrap();
        let le_leaf = le.cert[0].clone();
        *a.cert.write().unwrap() = Some(Arc::new(Issued { target: Target::Ip(ip), ck: Arc::new(le), not_before: 0, not_after: i64::MAX, issuer: String::new() }));
        let check = alpn_answer(&Target::Ip(ip), &[9u8; 32]).unwrap();
        let check_leaf = check.cert[0].clone();
        a.alpn.lock().unwrap().insert(Target::Ip(ip).sni(), Arc::new(check));
        let cfg = |internet| crate::tls::server_config(crate::tls::Resolver { own: own.clone(), acme: a.clone(), internet });
        // Let's Encrypt's check: ALPN acme-tls/1 and the reverse name.
        assert_eq!(handshake(cfg(true), "160.69.2.81.in-addr.arpa", Some(ALPN)), check_leaf);
        // A browser on the internet by IP (no SNI): the trusted certificate.
        assert_eq!(handshake(cfg(true), "81.2.69.160", None), le_leaf);
        // The LAN, by IP or name: the panel's own.
        assert_eq!(handshake(cfg(false), "192.168.50.3", None), own_leaf);
        assert_eq!(handshake(cfg(true), "rig", None), own_leaf);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alpn_answer_carries_the_digest() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let ck = alpn_answer(&Target::Ip("81.2.69.160".parse().unwrap()), &[7u8; 32]).unwrap();
        let (_, x) = x509_parser::parse_x509_certificate(&ck.cert[0]).unwrap();
        // id-pe-acmeIdentifier, critical.
        let ext = x.extensions().iter().find(|e| e.oid.to_id_string() == "1.3.6.1.5.5.7.1.31").unwrap();
        assert!(ext.critical);
        assert!(ext.value.ends_with(&[7u8; 32]));
        let sans = x.subject_alternative_name().unwrap().unwrap();
        assert!(matches!(sans.value.general_names[0], x509_parser::extensions::GeneralName::IPAddress(b) if b == [81, 2, 69, 160]));
    }
}
