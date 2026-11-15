//! Opening the relay tunnel: who may connect and with what settings, before
//! a single stream exists (design/vps-relay.md, "Secrets", "Security
//! review notes").
//!
//! Over the TLS connection (home pins the relay's own CA), home speaks
//! first: one length-prefixed JSON message with the relay key and the codec
//! settings home wants for what it receives. The relay checks the key
//! against the SHA-256 it keeps (never the key itself) and answers once.
//! Only then do both ends start the codec and the mux; with a wrong key the
//! relay answers "no" and closes, and nothing else is read or sent.

use std::sync::Arc;
use std::time::Duration;

use hopper_core::hosting::relay_codec::CodecConfig;
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use super::wire::{self, Codec, Counters};
use crate::tunnel::mux::{self, Side, Tunnel};

/// What home offers first in ALPN, so the relay's TLS only ever talks this.
pub const ALPN: &[u8] = b"hopper-relay";
/// The biggest hello either way (a key and two small objects).
const MAX_HELLO: usize = 16 * 1024;
/// How long either end waits for the other's hello.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// A relay key: `hrt_` and 64 hex digits (32 random bytes).
pub const KEY_PREFIX: &str = "hrt_";

/// The biggest settings home lets the relay ask of it for the busy
/// direction (home holds that encoder): the full B1 settings.
fn home_may_send(c: &Codec) -> bool {
    let full = CodecConfig::full();
    c.window_log <= full.window_log && c.cache_bytes <= full.cache_bytes
}

/// The biggest settings the relay lets home ask of it for the quiet
/// direction (the VPS holds that encoder, often on a small machine).
fn relay_may_send(c: &Codec) -> bool {
    c.window_log <= 24 && c.cache_bytes <= 64 << 20
}

#[derive(Serialize, Deserialize)]
struct HomeHello {
    key: String,
    tunnel: u16,
    version: String,
    /// How home wants what it receives compressed.
    recv: Codec,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RelayHello {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub tunnel: u16,
    #[serde(default)]
    pub version: String,
    /// How the relay wants what it receives compressed.
    #[serde(default)]
    pub recv: Option<Codec>,
    /// The port players connect to on the VPS.
    #[serde(default)]
    pub game_port: u16,
    /// The relay's UDP port for voice (B6); 0: none (a relay from before B6,
    /// or its voice port couldn't be opened).
    #[serde(default)]
    pub voice_port: u16,
}

#[derive(Debug)]
pub enum LinkError {
    /// The relay said no (wrong key, versions too far apart): its words.
    Refused(String),
    /// The other end broke the hello, or went quiet.
    Protocol(String),
    Io(std::io::Error),
    Mux(mux::MuxError),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Refused(m) => write!(f, "the relay refused: {m}"),
            LinkError::Protocol(m) => write!(f, "{m}"),
            LinkError::Io(e) => write!(f, "connection error: {e}"),
            LinkError::Mux(e) => write!(f, "{e}"),
        }
    }
}

/// The tunnel's own mux settings: more streams than a node tunnel (one per
/// player connection, server list pings included) with smaller windows,
/// so the worst case stays at 256 x 256 KiB = 64 MiB per connection.
pub fn mux_config() -> mux::Config {
    mux::Config {
        window: 256 * 1024,
        max_streams: 256,
        ping_every: Duration::from_secs(15),
        dead_after: Duration::from_secs(45),
        hello_timeout: HELLO_TIMEOUT,
        ..mux::Config::default()
    }
}

/// A new relay key.
pub fn new_key() -> String {
    format!("{KEY_PREFIX}{}", crate::auth::random_hex(32))
}

/// What the relay keeps of a key.
pub fn key_hash(key: &str) -> [u8; 32] {
    Sha256::digest(key.trim().as_bytes()).into()
}

pub fn hash_hex(h: &[u8; 32]) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hash_from_hex(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Compares without stopping at the first difference.
fn same(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn write_msg<W: AsyncWrite + Unpin>(w: &mut W, msg: &impl Serialize) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg).expect("hello");
    w.write_all(&(body.len() as u32).to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await
}

async fn read_msg<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<T, LinkError> {
    let go = async {
        let mut len = [0u8; 4];
        r.read_exact(&mut len).await.map_err(LinkError::Io)?;
        let len = u32::from_be_bytes(len) as usize;
        if len == 0 || len > MAX_HELLO {
            return Err(LinkError::Protocol(format!("hello of {len} bytes")));
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body).await.map_err(LinkError::Io)?;
        serde_json::from_slice(&body).map_err(|e| LinkError::Protocol(format!("hello: {e}")))
    };
    match tokio::time::timeout(HELLO_TIMEOUT, go).await {
        Ok(r) => r,
        Err(_) => Err(LinkError::Protocol("no hello in time".into())),
    }
}

/// A live tunnel and what it has moved.
pub struct Live {
    pub tunnel: Tunnel,
    pub counters: Arc<Counters>,
    /// The voice keys' secret from this connection's TLS exporter, set by
    /// whoever holds the TLS session (B6).
    pub exported: Option<super::voice::Exported>,
}

/// Home's end: says who it is, checks the answer, starts the tunnel.
/// `recv` is how home wants the quiet direction compressed.
pub async fn home<S>(mut io: S, key: &str, recv: Codec, cfg: mux::Config) -> Result<(Live, RelayHello), LinkError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let hello = HomeHello { key: key.trim().to_string(), tunnel: cfg.tunnel, version: cfg.version.clone(), recv };
    write_msg(&mut io, &hello).await.map_err(LinkError::Io)?;
    let answer: RelayHello = read_msg(&mut io).await?;
    if !answer.ok {
        // The relay's own words, cut short and without control characters
        // (they end up on the card).
        let msg: String = answer.error.chars().filter(|c| !c.is_control()).take(200).collect();
        return Err(LinkError::Refused(if msg.is_empty() { "no reason given".into() } else { msg }));
    }
    let send = answer.recv.ok_or_else(|| LinkError::Protocol("the relay named no codec settings".into()))?;
    CodecConfig::from(send).validate().map_err(|e| LinkError::Protocol(e.to_string()))?;
    if !home_may_send(&send) {
        return Err(LinkError::Protocol("the relay asked for more compression memory than home gives".into()));
    }
    let counters = Arc::new(Counters::default());
    let (tx, rx) = wire::split(io, send, recv, counters.clone()).map_err(LinkError::Protocol)?;
    let tunnel = mux::start(Side::Node, cfg, tx, rx).await.map_err(LinkError::Mux)?;
    Ok((Live { tunnel, counters, exported: None }, answer))
}

/// The relay's end: checks home's key before anything else, then starts
/// the tunnel. `recv` is how the relay wants the busy direction compressed.
/// A wrong key gets one "no" and the connection closes.
pub async fn relay<S>(mut io: S, key: &[u8; 32], recv: Codec, ports: (u16, u16), cfg: mux::Config) -> Result<Live, LinkError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let hello: HomeHello = read_msg(&mut io).await?;
    let no = |error: &str| RelayHello { ok: false, error: error.into(), tunnel: cfg.tunnel, version: cfg.version.clone(), recv: None, game_port: 0, voice_port: 0 };
    if !same(&key_hash(&hello.key), key) {
        let _ = write_msg(&mut io, &no("The relay key is wrong.")).await;
        let _ = io.shutdown().await;
        return Err(LinkError::Refused("wrong key".into()));
    }
    if hello.tunnel.abs_diff(cfg.tunnel) > 1 {
        let msg = format!("This relay runs Hopper Server {} (tunnel {}), too far from home's {} (tunnel {}). Update the older one.", cfg.version, cfg.tunnel, hello.version.chars().take(40).collect::<String>(), hello.tunnel);
        let _ = write_msg(&mut io, &no(&msg)).await;
        return Err(LinkError::Refused("tunnel versions too far apart".into()));
    }
    let send = hello.recv;
    if CodecConfig::from(send).validate().is_err() || !relay_may_send(&send) {
        let _ = write_msg(&mut io, &no("Home asked for compression settings this relay doesn't give.")).await;
        return Err(LinkError::Refused("codec settings out of range".into()));
    }
    let ok = RelayHello { ok: true, error: String::new(), tunnel: cfg.tunnel, version: cfg.version.clone(), recv: Some(recv), game_port: ports.0, voice_port: ports.1 };
    write_msg(&mut io, &ok).await.map_err(LinkError::Io)?;
    let counters = Arc::new(Counters::default());
    let (tx, rx) = wire::split(io, send, recv, counters.clone()).map_err(LinkError::Protocol)?;
    let tunnel = mux::start(Side::Main, cfg, tx, rx).await.map_err(LinkError::Mux)?;
    Ok(Live { tunnel, counters, exported: None })
}
