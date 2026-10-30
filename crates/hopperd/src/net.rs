//! The listener: one port that speaks HTTPS, and plain HTTP where the
//! network is already private (this PC, the LAN, Tailscale). A connection's
//! first byte says which: TLS starts with a handshake record (0x16).
//!
//! Each request carries [`Transport`] (TLS or not) and the client's
//! address, for the security checks in [`crate::security`].

use axum::extract::ConnectInfo;
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt as _;

/// How a request arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transport {
    pub tls: bool,
}

/// Where a client is, by its address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Net {
    Local,
    Lan,
    Tailscale,
    Internet,
}

impl Net {
    pub fn of(ip: IpAddr) -> Net {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 => v4,
        };
        match ip {
            IpAddr::V4(v) if v.is_loopback() => Net::Local,
            IpAddr::V4(v) if matches!(v.octets(), [100, 64..=127, ..]) => Net::Tailscale,
            IpAddr::V4(v) if v.is_private() || v.is_link_local() => Net::Lan,
            IpAddr::V6(v) if v.is_loopback() => Net::Local,
            // fc00::/7 (ULA) and fe80::/10 (link-local).
            IpAddr::V6(v) if (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80 => Net::Lan,
            _ => Net::Internet,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Net::Local => "this PC",
            Net::Lan => "LAN",
            Net::Tailscale => "Tailscale",
            Net::Internet => "internet",
        }
    }

    /// The network itself keeps traffic private: plain HTTP is allowed.
    pub fn private(self) -> bool {
        self != Net::Internet
    }

    /// Where the person behind a request is, for showing them how they
    /// reach the panel. A reverse proxy on this PC or the LAN (say Nginx
    /// Proxy Manager in front of a public name that resolves to a LAN
    /// address at home) makes every request look local, so its
    /// X-Forwarded-For / X-Real-IP is believed, but only when the direct
    /// peer is itself private: from the internet those headers are just
    /// text anyone can send. Display only; security checks use the peer.
    pub fn client(peer: IpAddr, headers: &axum::http::HeaderMap) -> Net {
        let direct = Net::of(peer);
        if !direct.private() {
            return direct;
        }
        forwarded_for(headers).map_or(direct, Net::of)
    }
}

/// The client address a proxy passed on: the last X-Forwarded-For entry
/// (the one the nearest proxy appended itself; earlier ones come from the
/// client and can be made up), else X-Real-IP.
fn forwarded_for(headers: &axum::http::HeaderMap) -> Option<IpAddr> {
    let parse = |s: &str| {
        let s = s.trim().trim_matches('"');
        s.parse::<IpAddr>().ok().or_else(|| s.parse::<SocketAddr>().ok().map(|a| a.ip()))
    };
    let xff = headers.get_all("x-forwarded-for").iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).last();
    match xff {
        Some(last) => parse(last),
        None => headers.get("x-real-ip").and_then(|v| v.to_str().ok()).and_then(parse),
    }
}

/// TLS settings by where the client is: an IP certificate from Let's
/// Encrypt only goes to clients on the internet (see [`crate::tls::Resolver`]).
pub struct Tls {
    pub lan: Arc<rustls::ServerConfig>,
    pub internet: Arc<rustls::ServerConfig>,
}

/// Serves `router` on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    tls: Tls,
    shutdown: impl std::future::Future<Output = ()>,
) {
    let (lan, internet) = (tokio_rustls::TlsAcceptor::from(tls.lan), tokio_rustls::TlsAcceptor::from(tls.internet));
    tokio::pin!(shutdown);
    loop {
        let (stream, addr) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("accept: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            _ = &mut shutdown => return,
        };
        let _ = stream.set_nodelay(true);
        let acceptor = if Net::of(addr.ip()).private() { lan.clone() } else { internet.clone() };
        let router = router.clone();
        tokio::spawn(async move {
            let mut first = [0u8; 1];
            let is_tls = match tokio::time::timeout(Duration::from_secs(15), stream.peek(&mut first)).await {
                Ok(Ok(1)) => first[0] == 0x16,
                _ => return,
            };
            if is_tls {
                match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(stream)).await {
                    // Let's Encrypt's check only needs the handshake.
                    Ok(Ok(s)) if s.get_ref().1.alpn_protocol() == Some(crate::acme::ALPN) => {}
                    Ok(Ok(s)) => serve_conn(TokioIo::new(s), router, addr, true).await,
                    Ok(Err(e)) => log::debug!("TLS from {addr}: {e}"),
                    Err(_) => {}
                }
            } else {
                serve_conn(TokioIo::new(stream), router, addr, false).await;
            }
        });
    }
}

async fn serve_conn<I>(io: I, router: axum::Router, addr: SocketAddr, tls: bool)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let svc = router.map_request(move |mut req: axum::http::Request<hyper::body::Incoming>| {
        req.extensions_mut().insert(ConnectInfo(addr));
        req.extensions_mut().insert(Transport { tls });
        req
    });
    let svc = hyper_util::service::TowerToHyperService::new(svc);
    if let Err(e) = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(io, svc)
        .await
    {
        log::debug!("connection from {addr}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nets() {
        let n = |s: &str| Net::of(s.parse().unwrap());
        assert_eq!(n("127.0.0.1"), Net::Local);
        assert_eq!(n("::1"), Net::Local);
        assert_eq!(n("192.168.50.21"), Net::Lan);
        assert_eq!(n("10.0.0.4"), Net::Lan);
        assert_eq!(n("100.101.12.3"), Net::Tailscale);
        assert_eq!(n("8.8.8.8"), Net::Internet);
        assert_eq!(n("::ffff:192.168.1.2"), Net::Lan);
        assert_eq!(n("2a00:1450::1"), Net::Internet);
        assert!(!Net::Internet.private() && Net::Tailscale.private());
    }

    #[test]
    fn client_behind_proxy() {
        let h = |pairs: &[(&str, &str)]| {
            let mut m = axum::http::HeaderMap::new();
            for (k, v) in pairs {
                m.append(axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
            }
            m
        };
        let c = |peer: &str, pairs: &[(&str, &str)]| Net::client(peer.parse().unwrap(), &h(pairs));
        // No proxy: the peer decides.
        assert_eq!(c("192.168.50.2", &[]), Net::Lan);
        assert_eq!(c("8.8.8.8", &[]), Net::Internet);
        // A public name that resolves to the LAN at home: the proxy passes on a LAN client.
        assert_eq!(c("192.168.50.3", &[("x-forwarded-for", "192.168.50.2"), ("x-real-ip", "192.168.50.2")]), Net::Lan);
        assert_eq!(c("127.0.0.1", &[("x-real-ip", "192.168.50.2")]), Net::Lan);
        // The same proxy with someone on the internet.
        assert_eq!(c("172.17.0.2", &[("x-forwarded-for", "81.2.69.160"), ("x-real-ip", "81.2.69.160")]), Net::Internet);
        assert_eq!(c("127.0.0.1", &[("x-forwarded-for", "2a00:1450::1")]), Net::Internet);
        // Through Tailscale.
        assert_eq!(c("127.0.0.1", &[("x-forwarded-for", "100.101.12.3")]), Net::Tailscale);
        // Spoofing: an internet client can't claim to be local by sending the headers itself.
        assert_eq!(c("81.2.69.160", &[("x-forwarded-for", "127.0.0.1"), ("x-real-ip", "192.168.1.5")]), Net::Internet);
        // ...nor by putting a local address first for the proxy to append to.
        assert_eq!(c("192.168.50.3", &[("x-forwarded-for", "192.168.1.5, 81.2.69.160"), ("x-real-ip", "81.2.69.160")]), Net::Internet);
        assert_eq!(c("192.168.50.3", &[("x-forwarded-for", "192.168.1.5"), ("x-forwarded-for", "81.2.69.160")]), Net::Internet);
        // Junk in the header: fall back to the peer.
        assert_eq!(c("192.168.50.3", &[("x-forwarded-for", "unknown")]), Net::Lan);
        assert_eq!(c("127.0.0.1", &[("x-forwarded-for", "\"[::1]:5000\"")]), Net::Local);
    }
}
