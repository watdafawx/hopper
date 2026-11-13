//! The tunnel over a WebSocket: the main's side is an axum route (the same
//! listener and TLS as the panel), the node's side a tungstenite client
//! that pins the main's CA. One binary message is one frame.

use std::io;
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

pub use super::dial::ConnectError;

/// Where nodes connect on the main.
pub const PATH: &str = "/api/v1/node/connect";

/// Opens the tunnel's WebSocket to the main: TLS pinned (see [`super::dial`]),
/// node key in `Authorization: Bearer` (no browser is involved, so no
/// ticket in the URL or subprotocol: the key never lands in an access log).
pub async fn connect(t: &super::dial::Target, pin: &super::dial::Pin, token: &str) -> Result<(ClientTx, ClientRx), ConnectError> {
    let (stream, _) = super::dial::tls(t, pin).await?;
    use tungstenite::client::IntoClientRequest;
    let url = format!("wss://{}{PATH}", t.authority());
    let mut req = url.into_client_request().map_err(|e| ConnectError::BadAddress(e.to_string()))?;
    let auth = format!("Bearer {token}").parse().map_err(|_| ConnectError::Other("bad token".into()))?;
    req.headers_mut().insert("authorization", auth);
    // Bounded like the steps before it: a main that finishes TLS and then
    // never answers the upgrade would otherwise hold the node here for ever,
    // never reaching its reconnect loop.
    let upgrade = tokio_tungstenite::client_async_with_config(req, stream, Some(ws_config()));
    match tokio::time::timeout(Duration::from_secs(15), upgrade).await {
        Err(_) => Err(ConnectError::Other("the main didn't answer the connection request".into())),
        Ok(Ok((ws, _))) => {
            let (tx, rx) = ws.split();
            Ok((ClientTx(tx), ClientRx(rx)))
        }
        Ok(Err(tungstenite::Error::Http(resp))) if matches!(resp.status().as_u16(), 401 | 403) => Err(ConnectError::Refused(resp.status().as_u16())),
        Ok(Err(tungstenite::Error::Http(resp))) if resp.status().as_u16() == 429 => {
            Err(ConnectError::Other("the main is refusing this address for a while (too many wrong keys)".into()))
        }
        Ok(Err(e)) => Err(ConnectError::Other(e.to_string())),
    }
}
