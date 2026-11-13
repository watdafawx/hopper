//! How a node reaches its main: the address, an optional `HTTPS_PROXY`, TLS
//! pinned to the main's own CA, and a small HTTPS client for the few JSON
//! calls joining needs. Every step that can fail says which one it was
//! (`design/multi-panel.md`, "Adjusted after research" point 7).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

/// Which step of connecting failed, so the node can say it plainly.
#[derive(Debug)]
pub enum ConnectError {
    BadAddress(String),
    /// The name didn't resolve.
    Dns(String),
    /// Nothing answered on that address and port (or the proxy didn't).
    Tcp(String),
    /// The main's certificate isn't the one pinned at pairing.
    Certificate(String),
    /// The main refused the node key (revoked or wrong): stop, don't retry.
    Refused(u16),
    Other(String),
}

impl ConnectError {
    /// The step, as `hopperd node status` and the status file name it.
    pub fn step(&self) -> &'static str {
        match self {
            ConnectError::BadAddress(_) => "address",
            ConnectError::Dns(_) => "dns",
            ConnectError::Tcp(_) => "tcp",
            ConnectError::Certificate(_) => "certificate",
            ConnectError::Refused(_) => "key",
            ConnectError::Other(_) => "other",
        }
    }
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::BadAddress(m) => write!(f, "address not usable: {m}"),
            ConnectError::Dns(m) => write!(f, "address not found: {m}"),
            ConnectError::Tcp(m) => write!(f, "can't connect: {m}"),
            ConnectError::Certificate(m) => write!(f, "the main's certificate doesn't match the one this node trusts ({m})"),
            ConnectError::Refused(s) => write!(f, "the main refused this node's key (HTTP {s})"),
            ConnectError::Other(m) => write!(f, "{m}"),
        }
    }
}

/// The main's address: `https://host[:port]` (a path, or `wss://`, is
/// accepted and dropped).
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub host: String,
    pub port: u16,
}

impl Target {
    pub fn parse(url: &str) -> Result<Target, ConnectError> {
        let url = url.trim();
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("wss://"))
            .ok_or_else(|| ConnectError::BadAddress("must start with https://".into()))?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return Err(ConnectError::BadAddress("no user name in the address".into()));
        }
        let (host, port) = host_port(authority)?;
        Ok(Target { host, port })
    }

    /// `host:port`, with brackets around an IPv6 address.
    pub fn authority(&self) -> String {
        if self.host.contains(':') { format!("[{}]:{}", self.host, self.port) } else { format!("{}:{}", self.host, self.port) }
    }

    pub fn url(&self) -> String {
        format!("https://{}", self.authority())
    }
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`; 443 when no port is given.
pub fn host_port(authority: &str) -> Result<(String, u16), ConnectError> {
    let bad = |m: &str| ConnectError::BadAddress(m.into());
    let port = |p: &str| p.parse::<u16>().ok().filter(|p| *p != 0).ok_or_else(|| bad("bad port"));
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (h, after) = rest.split_once(']').ok_or_else(|| bad("missing ]"))?;
        match after {
            "" => (h, 443),
            p => (h, port(p.strip_prefix(':').ok_or_else(|| bad("bad port"))?)?),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, port(p)?),
            None => (authority, 443),
        }
    };
    if host.is_empty() {
        return Err(bad("no host"));
    }
    Ok((host.to_string(), port))
}

/// What the node trusts when it connects.
#[derive(Clone, Debug)]
pub enum Pin {
    /// The main's CA, pinned at joining: everything after that.
    Ca(CertificateDer<'static>),
    /// The CA's SHA-256 from a join command ("AB:CD:…"): the chain the main
    /// sends must contain that CA, and the certificate must be signed by it.
    Fingerprint(String),
    /// Code pairing: anything is accepted and its CA kept, because the
    /// 6-digit code (made from that CA's fingerprint) is what proves it.
    /// The owner compares the codes before anything is trusted.
    Any,
}

/// Accepts the main's certificate when it chains to the pinned CA. The name
/// in it doesn't have to match the address: the CA is this one panel's own
/// (made by it, never shared), so whatever it signed is that panel, under
/// any address the node uses (a tailnet name, an IP the certificate
/// predates, an SSH forward).
#[derive(Debug)]
struct Verifier {
    pin: Pin,
    provider: Arc<rustls::crypto::CryptoProvider>,
    /// The CA the accepted chain led to.
    seen: Arc<Mutex<Option<CertificateDer<'static>>>>,
}

fn cert_error(m: &str) -> rustls::Error {
    rustls::Error::InvalidCertificate(CertificateError::Other(rustls::OtherError(Arc::new(std::io::Error::other(m.to_string())))))
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let ca: CertificateDer<'static> = match &self.pin {
            Pin::Ca(d) => d.clone(),
            Pin::Fingerprint(fp) => intermediates
                .iter()
                .find(|c| same_fingerprint(fp, &crate::tls::fingerprint(c)))
                .map(|c| c.clone().into_owned())
                .ok_or_else(|| cert_error("not the certificate in the join command"))?,
            Pin::Any => intermediates.last().map(|c| c.clone().into_owned()).ok_or_else(|| cert_error("not a Hopper Server certificate"))?,
        };
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.clone()).map_err(|_| cert_error("unreadable certificate"))?;
        let webpki = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), self.provider.clone())
            .build()
            .map_err(|e| rustls::Error::General(e.to_string()))?;
        match webpki.verify_server_cert(end, intermediates, name, ocsp, now) {
            Ok(_) => {}
            Err(rustls::Error::InvalidCertificate(CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. })) => {}
            Err(e) => return Err(e),
        }
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(ca);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Fingerprints compare without case, colons or spaces.
pub fn same_fingerprint(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.chars().filter(|c| c.is_ascii_hexdigit()).map(|c| c.to_ascii_uppercase()).collect::<String>();
    let (a, b) = (norm(a), norm(b));
    a.len() == 64 && a == b
}

fn client_config(pin: &Pin) -> (Arc<rustls::ClientConfig>, Arc<Mutex<Option<CertificateDer<'static>>>>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let seen = Arc::new(Mutex::new(None));
    let verifier = Arc::new(Verifier { pin: pin.clone(), provider: provider.clone(), seen: seen.clone() });
    let mut c = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    // Offered first so the main answers with its own certificate, never a
    // Let's Encrypt one (see `tls::Resolver`); it then picks http/1.1.
    c.alpn_protocols = vec![crate::tls::NODE_ALPN.to_vec(), b"http/1.1".to_vec()];
    (Arc::new(c), seen)
}

/// `HTTPS_PROXY` for this host, unless `NO_PROXY` names it.
fn proxy_for(host: &str) -> Option<String> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let proxy = get("HTTPS_PROXY").or_else(|| get("https_proxy"))?;
    let no = get("NO_PROXY").or_else(|| get("no_proxy")).unwrap_or_default();
    (!no_proxy_matches(&no, host)).then_some(proxy)
}

fn no_proxy_matches(list: &str, host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    list.split(',').map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase()).filter(|e| !e.is_empty()).any(|e| {
        e == "*" || host == e || host.ends_with(&format!(".{e}"))
    })
}

/// A proxy address: `http://[user:pass@]host[:port]`.
#[derive(Debug, PartialEq)]
struct Proxy {
    host: String,
    port: u16,
    auth: Option<String>,
}

fn parse_proxy(url: &str) -> Result<Proxy, ConnectError> {
    let bad = |m: &str| ConnectError::BadAddress(format!("HTTPS_PROXY: {m}"));
    let rest = match url.trim().split_once("://") {
        Some(("http", r)) => r,
        Some(_) => return Err(bad("only http:// proxies are supported")),
        None => url.trim(),
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let (auth, hostport) = match authority.rsplit_once('@') {
        Some((a, h)) => (Some(percent_decode(a)), h),
        None => (None, authority),
    };
    let (host, port) = if hostport.contains(':') || hostport.starts_with('[') {
        host_port(hostport).map_err(|_| bad("bad address"))?
    } else {
        (hostport.to_string(), 80)
    };
    if host.is_empty() {
        return Err(bad("no host"));
    }
    use base64::Engine as _;
    Ok(Proxy { host, port, auth: auth.map(|a| base64::engine::general_purpose::STANDARD.encode(a)) })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = b.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        if let (b'%', Some(v)) = (b[i], hex) {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn tcp_to(host: &str, port: u16, what: &str) -> Result<TcpStream, ConnectError> {
    let addrs: Vec<_> = match tokio::time::timeout(Duration::from_secs(10), tokio::net::lookup_host((host, port))).await {
        Ok(Ok(a)) => a.collect(),
        Ok(Err(e)) => return Err(ConnectError::Dns(format!("{what}{host}: {e}"))),
        Err(_) => return Err(ConnectError::Dns(format!("{what}{host}: timed out"))),
    };
    if addrs.is_empty() {
        return Err(ConnectError::Dns(format!("{what}{host} has no address")));
    }
    match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&addrs[..])).await {
        Ok(Ok(s)) => {
            let _ = s.set_nodelay(true);
            Ok(s)
        }
        Ok(Err(e)) => Err(ConnectError::Tcp(format!("{what}{host}:{port}: {e}"))),
        Err(_) => Err(ConnectError::Tcp(format!("{what}{host}:{port}: timed out"))),
    }
}

/// A TCP connection to the main, through the proxy when there is one.
async fn tcp(t: &Target) -> Result<TcpStream, ConnectError> {
    let Some(proxy) = proxy_for(&t.host) else { return tcp_to(&t.host, t.port, "").await };
    let p = parse_proxy(&proxy)?;
    let mut s = tcp_to(&p.host, p.port, "the proxy ").await?;
    let auth = p.auth.map(|a| format!("Proxy-Authorization: Basic {a}\r\n")).unwrap_or_default();
    let req = format!("CONNECT {0} HTTP/1.1\r\nHost: {0}\r\n{auth}\r\n", t.authority());
    let talk = async {
        s.write_all(req.as_bytes()).await?;
        // Read the proxy's answer byte by byte so nothing of the TLS that
        // follows is taken with it.
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if head.len() > 8192 || s.read(&mut b).await? == 0 {
                return Err(std::io::Error::other("the proxy closed the connection"));
            }
            head.push(b[0]);
        }
        Ok::<_, std::io::Error>(head)
    };
    let head = match tokio::time::timeout(Duration::from_secs(15), talk).await {
        Ok(Ok(h)) => h,
        Ok(Err(e)) => return Err(ConnectError::Tcp(format!("the proxy: {e}"))),
        Err(_) => return Err(ConnectError::Tcp("the proxy didn't answer".into())),
    };
    let line = String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_string();
    if line.split_whitespace().nth(1) != Some("200") {
        return Err(ConnectError::Tcp(format!("the proxy refused: {line}")));
    }
    Ok(s)
}

/// TLS to the main under `pin`, with the CA the certificate chained to.
pub async fn tls(t: &Target, pin: &Pin) -> Result<(TlsStream<TcpStream>, CertificateDer<'static>), ConnectError> {
    let tcp = tcp(t).await?;
    let (config, seen) = client_config(pin);
    let name = ServerName::try_from(t.host.clone()).map_err(|e| ConnectError::BadAddress(e.to_string()))?;
    match tokio::time::timeout(Duration::from_secs(15), tokio_rustls::TlsConnector::from(config).connect(name, tcp)).await {
        Ok(Ok(s)) => {
            let ca = seen.lock().unwrap_or_else(|e| e.into_inner()).take().ok_or_else(|| ConnectError::Other("TLS: no certificate".into()))?;
            Ok((s, ca))
        }
        Ok(Err(e)) => {
            let cert = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()).is_some_and(|r| matches!(r, rustls::Error::InvalidCertificate(_)));
            Err(if cert { ConnectError::Certificate(e.to_string()) } else { ConnectError::Other(format!("TLS: {e}")) })
        }
        Err(_) => Err(ConnectError::Other("the port is open but the main didn't answer (a firewall dropping traffic?)".into())),
    }
}

/// One JSON request to the main over a fresh pinned connection. Returns
/// the status, the JSON body (null when there is none) and the CA.
pub async fn request(
    t: &Target,
    pin: &Pin,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, Value, CertificateDer<'static>), ConnectError> {
    let (mut s, ca) = tls(t, pin).await?;
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let auth = bearer.map(|b| format!("Authorization: Bearer {b}\r\n")).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: hopperd/{}\r\nAccept: application/json\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{auth}Connection: close\r\n\r\n{body}",
        t.authority(),
        env!("CARGO_PKG_VERSION"),
        body.len()
    );
    let talk = async {
        s.write_all(req.as_bytes()).await?;
        s.flush().await?;
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match s.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                // Some servers close without TLS's goodbye; what arrived counts.
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => break,
                Err(e) => return Err(e),
            }
            if out.len() > 64 * 1024 {
                return Err(std::io::Error::other("answer too big"));
            }
        }
        Ok(out)
    };
    let raw = match tokio::time::timeout(Duration::from_secs(20), talk).await {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return Err(ConnectError::Other(format!("the main: {e}"))),
        Err(_) => return Err(ConnectError::Other("the main didn't answer".into())),
    };
    let (status, json) = parse_response(&raw).ok_or_else(|| ConnectError::Other("the main's answer wasn't readable".into()))?;
    Ok((status, json, ca))
}

/// Status and JSON body of an HTTP/1.1 answer (plain or chunked).
fn parse_response(raw: &[u8]) -> Option<(u16, Value)> {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..split]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    let chunked = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("transfer-encoding:") && l.contains("chunked")
    });
    let mut body = raw[split + 4..].to_vec();
    if chunked {
        let mut out = Vec::new();
        let mut rest = &body[..];
        loop {
            let nl = rest.windows(2).position(|w| w == b"\r\n")?;
            let size = usize::from_str_radix(std::str::from_utf8(&rest[..nl]).ok()?.split(';').next()?.trim(), 16).ok()?;
            rest = &rest[nl + 2..];
            if size == 0 {
                break;
            }
            out.extend_from_slice(rest.get(..size)?);
            rest = rest.get(size + 2..)?;
        }
        body = out;
    }
    let json = if body.iter().all(u8::is_ascii_whitespace) { Value::Null } else { serde_json::from_slice(&body).ok()? };
    Some((status, json))
}

/// The CA in PEM, as the node keeps it.
pub fn to_pem(der: &[u8]) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let lines: Vec<&str> = b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect();
    format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
}

pub fn from_pem(pem: &str) -> Option<CertificateDer<'static>> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).map(str::trim).collect();
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(b64).ok().map(CertificateDer::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_with_and_without_ports() {
        let ok = |a: &str| host_port(a).map_err(|e| e.to_string()).unwrap();
        assert_eq!(ok("main.example"), ("main.example".into(), 443));
        assert_eq!(ok("main.example:8790"), ("main.example".into(), 8790));
        assert_eq!(ok("127.0.0.1:18790"), ("127.0.0.1".into(), 18790));
        assert_eq!(ok("[::1]:8790"), ("::1".into(), 8790));
        // Used to be read as host "[:" and port "1]".
        assert_eq!(ok("[::1]"), ("::1".into(), 443));
        for bad in ["", ":8790", "h:", "h:99999", "h:0", "[::1", "[::1]x", "[::1]:"] {
            assert!(host_port(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn main_addresses() {
        let t = Target::parse("https://fox.tail.ts.net:8790/settings/nodes").unwrap();
        assert_eq!(t, Target { host: "fox.tail.ts.net".into(), port: 8790 });
        assert_eq!(t.url(), "https://fox.tail.ts.net:8790");
        assert_eq!(Target::parse("wss://[::1]:9/api").unwrap().authority(), "[::1]:9");
        assert!(Target::parse("http://fox:8790").is_err(), "never in the clear");
        assert!(Target::parse("https://user@fox:8790").is_err());
    }

    #[test]
    fn proxies() {
        assert!(no_proxy_matches("localhost, .internal,example.com", "a.internal"));
        assert!(no_proxy_matches("example.com", "EXAMPLE.com"));
        assert!(no_proxy_matches("example.com", "x.example.com"));
        assert!(!no_proxy_matches("example.com", "badexample.com"));
        assert!(no_proxy_matches("*", "anything"));
        assert!(!no_proxy_matches("", "anything"));
        let p = parse_proxy("http://bob:p%40ss@proxy.lan:3128/").unwrap();
        assert_eq!((p.host.as_str(), p.port), ("proxy.lan", 3128));
        use base64::Engine as _;
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(p.auth.unwrap()).unwrap(), b"bob:p@ss");
        assert_eq!(parse_proxy("proxy.lan").unwrap().port, 80);
        assert!(parse_proxy("socks5://proxy:1080").is_err());
    }

    #[test]
    fn answers_plain_and_chunked() {
        let plain = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 8\r\n\r\n{\"a\":1}\n";
        assert_eq!(parse_response(plain), Some((200, serde_json::json!({"a": 1}))));
        let chunked = b"HTTP/1.1 401 Unauthorized\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:2}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Some((401, serde_json::json!({"a": 2}))));
        assert_eq!(parse_response(b"HTTP/1.1 204 No Content\r\n\r\n"), Some((204, Value::Null)));
        assert_eq!(parse_response(b"garbage"), None);
    }

    #[test]
    fn fingerprints_compare_loosely_but_whole() {
        let fp = (0..32).map(|i| format!("{i:02X}")).collect::<Vec<_>>().join(":");
        assert!(same_fingerprint(&fp, &fp.to_lowercase().replace(':', "")));
        assert!(!same_fingerprint(&fp[..20], &fp[..20]), "a short prefix never matches");
        assert!(!same_fingerprint("", ""));
    }

    #[test]
    fn pem_round_trip() {
        let der = vec![7u8; 300];
        assert_eq!(from_pem(&to_pem(&der)).unwrap().as_ref(), &der[..]);
    }
}
