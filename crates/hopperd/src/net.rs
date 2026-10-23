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
}

/// Serves `router` on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: tokio::net::TcpListener,
    router: axum::Router,
    tls: Arc<rustls::ServerConfig>,
    shutdown: impl std::future::Future<Output = ()>,
) {
    let acceptor = tokio_rustls::TlsAcceptor::from(tls);
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
        let (acceptor, router) = (acceptor.clone(), router.clone());
        tokio::spawn(async move {
            let mut first = [0u8; 1];
            let is_tls = match tokio::time::timeout(Duration::from_secs(15), stream.peek(&mut first)).await {
                Ok(Ok(1)) => first[0] == 0x16,
                _ => return,
            };
            if is_tls {
                match tokio::time::timeout(Duration::from_secs(15), acceptor.accept(stream)).await {
                    // Let's Encrypt's check only needs the handshake.
                    Ok(Ok(s)) if s.get_ref().1.alpn_protocol() == Some(rustls_acme::acme::ACME_TLS_ALPN_NAME) => {}
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
}
