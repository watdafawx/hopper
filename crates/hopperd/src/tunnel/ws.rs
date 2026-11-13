//! The tunnel over a WebSocket: the main's side is an axum route (the same
//! listener and TLS as the panel), the node's side a tungstenite client
//! that pins the main's CA. One binary message is one frame.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::WebSocketStream;

use super::frame::MAX_FRAME;
use super::mux::{FrameRx, FrameTx};

/// WebSocket limits both sides use: nothing bigger than one frame is ever
/// buffered, and the read buffer stays small on an idle node.
pub fn ws_config() -> tungstenite::protocol::WebSocketConfig {
    tungstenite::protocol::WebSocketConfig::default()
        .read_buffer_size(16 * 1024)
        .write_buffer_size(0)
        .max_write_buffer_size(2 * MAX_FRAME)
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME))
}

// ---- main side (axum) ----

pub struct AxumTx(pub SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>);
pub struct AxumRx(pub SplitStream<axum::extract::ws::WebSocket>);

pub fn split_axum(ws: axum::extract::ws::WebSocket) -> (AxumTx, AxumRx) {
    let (tx, rx) = ws.split();
    (AxumTx(tx), AxumRx(rx))
}

impl FrameTx for AxumTx {
    async fn send(&mut self, frame: Vec<u8>) -> io::Result<()> {
        self.0.send(axum::extract::ws::Message::Binary(Bytes::from(frame))).await.map_err(io::Error::other)
    }
}

impl FrameRx for AxumRx {
    async fn recv(&mut self) -> Option<io::Result<Vec<u8>>> {
        use axum::extract::ws::Message;
        loop {
            return match self.0.next().await? {
                Err(e) => Some(Err(io::Error::other(e))),
                Ok(Message::Binary(b)) => Some(Ok(b.into())),
                Ok(Message::Close(_)) => None,
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                Ok(Message::Text(_)) => Some(Err(io::Error::new(io::ErrorKind::InvalidData, "text message on the tunnel"))),
            };
        }
    }
}

// ---- node side (tungstenite client) ----

type ClientWs = WebSocketStream<TlsStream<TcpStream>>;
pub struct ClientTx(SplitSink<ClientWs, tungstenite::Message>);
pub struct ClientRx(SplitStream<ClientWs>);

impl FrameTx for ClientTx {
    async fn send(&mut self, frame: Vec<u8>) -> io::Result<()> {
        self.0.send(tungstenite::Message::Binary(Bytes::from(frame))).await.map_err(io::Error::other)
    }
}

impl FrameRx for ClientRx {
    async fn recv(&mut self) -> Option<io::Result<Vec<u8>>> {
        use tungstenite::Message;
        loop {
            return match self.0.next().await? {
                Err(e) => Some(Err(io::Error::other(e))),
                Ok(Message::Binary(b)) => Some(Ok(b.into())),
                Ok(Message::Close(_)) => None,
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
                Ok(Message::Text(_)) => Some(Err(io::Error::new(io::ErrorKind::InvalidData, "text message on the tunnel"))),
            };
        }
    }
}

/// Which step of connecting failed, so the node can say it plainly
/// (`design/multi-panel.md`, "Adjusted after research" point 7).
#[derive(Debug)]
pub enum ConnectError {
    BadAddress(String),
    /// The name didn't resolve.
    Dns(String),
    /// Nothing answered on that address and port.
    Tcp(String),
    /// The main's certificate isn't the one pinned at pairing.
    Certificate(String),
    /// The main refused the node key (revoked or wrong): stop, don't retry.
    Refused(u16),
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::BadAddress(m) => write!(f, "address not usable: {m}"),
            ConnectError::Dns(m) => write!(f, "address not found: {m}"),
            ConnectError::Tcp(m) => write!(f, "can't connect: {m}"),
            ConnectError::Certificate(m) => write!(f, "the main's certificate doesn't match the one this node trusts: {m}"),
            ConnectError::Refused(s) => write!(f, "the main refused this node's key (HTTP {s})"),
            ConnectError::Other(m) => write!(f, "{m}"),
        }
    }
}

/// A TLS client that trusts only `ca` (the main's own CA, pinned when the
/// node joined), never the system's roots: a certificate from a public CA
/// for the same name is still refused.
pub fn pinned_tls(ca: &rustls::pki_types::CertificateDer<'static>) -> anyhow::Result<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca.clone())?;
    let mut c = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    c.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(c))
}

/// Opens the tunnel's WebSocket: `wss://` only, CA pinned, node key in
/// `Authorization: Bearer` (no browser is involved, so no ticket in the URL
/// or subprotocol: the key never lands in an access log).
pub async fn connect(url: &str, tls: Arc<rustls::ClientConfig>, token: &str) -> Result<(ClientTx, ClientRx), ConnectError> {
    let rest = url.strip_prefix("wss://").ok_or_else(|| ConnectError::BadAddress("must start with wss://".into()))?;
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.ends_with(']') || h.starts_with('[') => {
            (h.trim_start_matches('[').trim_end_matches(']').to_string(), p.parse::<u16>().map_err(|_| ConnectError::BadAddress("bad port".into()))?)
        }
        _ => (authority.to_string(), 443),
    };
    let addrs: Vec<_> = match tokio::net::lookup_host((host.as_str(), port)).await {
        Ok(a) => a.collect(),
        Err(e) => return Err(ConnectError::Dns(e.to_string())),
    };
    if addrs.is_empty() {
        return Err(ConnectError::Dns(format!("{host} has no address")));
    }
    let tcp = match tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&addrs[..])).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(ConnectError::Tcp(e.to_string())),
        Err(_) => return Err(ConnectError::Tcp("timed out".into())),
    };
    let _ = tcp.set_nodelay(true);
    let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| ConnectError::BadAddress(e.to_string()))?;
    let stream = match tokio::time::timeout(Duration::from_secs(15), tokio_rustls::TlsConnector::from(tls).connect(name, tcp)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let cert = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>()).is_some_and(|r| matches!(r, rustls::Error::InvalidCertificate(_)));
            return Err(if cert { ConnectError::Certificate(e.to_string()) } else { ConnectError::Other(format!("TLS: {e}")) });
        }
        Err(_) => return Err(ConnectError::Other("TLS handshake timed out".into())),
    };
    use tungstenite::client::IntoClientRequest;
    let mut req = url.into_client_request().map_err(|e| ConnectError::BadAddress(e.to_string()))?;
    let auth = format!("Bearer {token}").parse().map_err(|_| ConnectError::Other("bad token".into()))?;
    req.headers_mut().insert("authorization", auth);
    match tokio_tungstenite::client_async_with_config(req, stream, Some(ws_config())).await {
        Ok((ws, _)) => {
            let (tx, rx) = ws.split();
            Ok((ClientTx(tx), ClientRx(rx)))
        }
        Err(tungstenite::Error::Http(resp)) if matches!(resp.status().as_u16(), 401 | 403) => Err(ConnectError::Refused(resp.status().as_u16())),
        Err(e) => Err(ConnectError::Other(e.to_string())),
    }
}
