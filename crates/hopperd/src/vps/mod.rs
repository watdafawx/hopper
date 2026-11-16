//! Relay through a VPS (design/vps-relay.md, B2): players join a cloud
//! machine with a public address, and home, which has no open port, dials
//! out to it over one TLS connection that carries every player as a stream,
//! compressed as one (the B1 codec).
//!
//! - [`relay_end`]: `hopperd relay`, the VPS end. Takes the home panel's
//!   one connection (pinned TLS, relay key) and players on its game port.
//! - [`home`]: the home panel's client. Keeps the connection up and hands
//!   each stream to the router, tagged with its server.
//! - [`link`]: the key check and settings both ends agree before any
//!   stream; [`wire`]: frames through the codec and TLS.
//!
//! Streams: the relay opens one per player connection, with the head
//! [`Head`] (`kind` "game"; the server it is for; the player's address).
//! Home opens one control stream (`kind` "control") and tells the relay
//! which servers it carries. Player bytes only ever go between a player's
//! socket on the VPS and the paired home panel's router.

pub mod forward;
pub mod health;
pub mod home;
pub mod link;
pub mod relay_end;
pub mod setup;
pub mod voice;
pub mod wire;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::tunnel::frame::code;
use crate::tunnel::mux::{Stream, StreamError};

/// A stream's OPEN head.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Head {
    /// "game" (a player's connection, server list pings too), "control", or
    /// "voice" (voice packets while UDP doesn't get through, B6).
    pub kind: String,
    /// The server id the stream is for (game streams).
    #[serde(default)]
    pub server: String,
    /// The player's address as the VPS saw it, for home's log.
    #[serde(default)]
    pub peer: String,
}

/// What the control stream carries, one JSON object per line.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Control {
    /// Servers home carries through this relay. In B2 the relay sends every
    /// player on its game port to the first one; per-server ports are B7.
    #[serde(default)]
    pub routes: Vec<Route>,
    /// Home asks for the players' pings (B5). The line still carries the
    /// routes, so a relay from before B5 reads it as a routes line and
    /// changes nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pings: bool,
}

/// What the relay sends home on the control stream, one JSON object per
/// line (B5): the answer to a `pings` ask.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RelayLine {
    #[serde(default)]
    pub pings: Vec<Ping>,
}

/// One player's ping as the relay's front port measures it: the OS's
/// smoothed round trip on the player's own TCP connection to the VPS.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Ping {
    pub server: String,
    /// Lowercase, as the player sent it in Login Start.
    pub name: String,
    pub ms: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Route {
    pub server: String,
    #[serde(default)]
    pub name: String,
    /// The server checks accounts through the relay's Velocity (B4): its
    /// players go through Velocity, the rest straight home (B2).
    #[serde(default)]
    pub velocity: bool,
    /// The server runs a voice mod pointed at the relay (B6): the relay
    /// passes players' voice packets home. A relay from before B6 ignores it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub voice: bool,
}

/// The longest control line either end reads.
pub const MAX_CONTROL_LINE: usize = 64 * 1024;

/// Passes bytes between `sock` and `stream` until both directions have
/// ended. Each direction half-closes on its own (a socket's end of input
/// finishes the stream; the stream's end shuts the socket's writing side),
/// and a failure on either side ends both. `to_tunnel` / `from_tunnel` see
/// every chunk, for the counters.
pub async fn pipe<S>(sock: S, stream: Stream, mut to_tunnel: impl FnMut(&[u8]) + Send, mut from_tunnel: impl FnMut(&[u8]) + Send) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Send,
{
    let (mut r, mut w) = tokio::io::split(sock);
    let Stream { mut tx, mut rx, .. } = stream;
    let up = async {
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            match r.read(&mut buf).await {
                Ok(0) => {
                    let _ = tx.finish().await;
                    return Ok::<(), String>(());
                }
                Ok(n) => {
                    to_tunnel(&buf[..n]);
                    tx.send(&buf[..n]).await.map_err(|e| format!("tunnel: {e:?}"))?;
                }
                Err(e) => {
                    tx.reset(code::CANCEL);
                    return Err(format!("socket: {e}"));
                }
            }
        }
    };
    let down = async {
        loop {
            match rx.recv().await {
                Ok(Some(d)) => {
                    from_tunnel(&d);
                    w.write_all(&d).await.map_err(|e| format!("socket: {e}"))?;
                }
                Ok(None) => {
                    let _ = w.shutdown().await;
                    return Ok::<(), String>(());
                }
                Err(StreamError::Reset(c)) => return Err(format!("reset ({c})")),
                Err(StreamError::Closed) => return Err("tunnel closed".into()),
            }
        }
    };
    tokio::try_join!(up, down).map(|_| ())
}
