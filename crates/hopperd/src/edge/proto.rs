//! What hopperd and the edge say to each other over the local channel.
//!
//! Every frame is `[u32 length][u8 kind][body]` (little endian). Kind 0 is a
//! [`Msg`] as JSON; kind 1 is player bytes for one connection
//! (`[u64 conn][bytes]`), kept binary because it is most of the traffic.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// The channel's protocol. An edge and a hopperd speak when their versions
/// are the same (phase 4 adds N and N-1).
pub const PROTO: u16 = 1;
/// The largest frame either side accepts. Player bytes go in pieces of at
/// most [`CHUNK`]; a settings message with every server's answer stays well
/// under this.
pub const MAX_FRAME: usize = 4 << 20;
pub const CHUNK: usize = 16 * 1024;
/// Before the token is checked, only a small hello is read.
pub const MAX_HELLO: usize = 4096;
/// The longest a splice takes in the edge: connecting to the server and
/// writing the replay, all of it. Past this the edge gives up on it.
pub const SPLICE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// How long hopperd waits for `Spliced`: the edge's limit plus room for the
/// channel, so the edge's answer (good or not) comes first.
pub const SPLICE_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Bytes of player data on the way over the channel, per direction, before
/// the side sending them waits (the edge) or refuses more (hopperd).
pub const CHANNEL_BUDGET: usize = 8 << 20;
/// Bytes queued for one connection that its other end hasn't taken yet.
/// Past this the connection is closed: before a splice a player sends a
/// handshake and a login, and hopperd writes answers and, for the lobby
/// island, one small world (well under this).
pub const CONN_QUEUE: usize = 4 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    // ---- hopperd -> edge ----
    /// The first frame of every connection.
    Hello { token: String, proto: u16 },
    /// Listen for players on `bind:port`, or stop listening (`port` None).
    Listen { bind: String, port: Option<u16> },
    /// What a new join gets while hopperd is away: per set of addresses,
    /// a server list answer to show (see `fallback`).
    Answers { list: Vec<Answer>, unknown: Value },
    /// Connect `conn` to `127.0.0.1:port`: write `replay`, then the player's
    /// bytes past the first `consumed`, and pipe from then on. `server` and
    /// `player` name the pipe for the Players tab's ping.
    Splice { conn: u64, port: u16, replay: String, consumed: u64, server: String, player: String },
    /// hopperd is done with a connection it didn't splice.
    Close { conn: u64 },
    /// Close the listener and quit; `drain` lets live pipes finish first.
    Stop { drain: bool },

    // ---- edge -> hopperd ----
    Welcome { proto: u16, version: String, pid: u32, listening: Option<u16>, error: Option<String>, pipes: Vec<PipeInfo> },
    /// The listener's state after a `Listen` (or when it changed).
    Listening { port: Option<u16>, error: Option<String> },
    /// A player connected: their bytes follow as data frames.
    Open { conn: u64, peer: String },
    /// The answer to `Splice`. A good one names the pipe, so a hopperd that
    /// stopped waiting for it (see [`SPLICE_WAIT`]) still lists the player.
    Spliced {
        conn: u64,
        ok: bool,
        error: Option<String>,
        #[serde(default)]
        pipe: Option<PipeInfo>,
    },
    /// A piped player left.
    Ended { conn: u64 },
    /// Each pipe's round-trip time as the OS keeps it, in ms.
    Pings { list: Vec<(u64, Option<u32>)> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub addresses: Vec<String>,
    pub status: Value,
}

/// A live pipe, as the edge reports it to a hopperd that (re)connects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PipeInfo {
    pub conn: u64,
    pub server: String,
    pub player: String,
    /// Unix seconds it was spliced.
    pub since: u64,
}

#[derive(Debug, PartialEq)]
pub enum Frame {
    Msg(Msg),
    Data(u64, Vec<u8>),
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let (kind, body) = match self {
            Frame::Msg(m) => (0u8, serde_json::to_vec(m).unwrap_or_default()),
            Frame::Data(conn, b) => {
                let mut v = Vec::with_capacity(8 + b.len());
                v.extend_from_slice(&conn.to_le_bytes());
                v.extend_from_slice(b);
                (1u8, v)
            }
        };
        let mut out = Vec::with_capacity(5 + body.len());
        out.extend_from_slice(&((body.len() + 1) as u32).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(&body);
        out
    }
}

pub async fn write<W: AsyncWrite + Unpin>(w: &mut W, f: &Frame) -> std::io::Result<()> {
    w.write_all(&f.encode()).await
}

/// The next frame, `None` at a clean end. Anything over `max` or not a
/// frame is an error, and the connection is dropped.
pub async fn read<R: AsyncRead + Unpin>(r: &mut R, max: usize) -> anyhow::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len == 0 || len > max + 1 {
        anyhow::bail!("frame of {len} bytes");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    let kind = body[0];
    let body = &body[1..];
    Ok(Some(match kind {
        0 => Frame::Msg(serde_json::from_slice(body)?),
        1 if body.len() >= 8 => Frame::Data(u64::from_le_bytes(body[..8].try_into().unwrap()), body[8..].to_vec()),
        _ => anyhow::bail!("unknown frame kind {kind}"),
    }))
}

/// Compares without leaking where they differ.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn b64(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

pub fn unb64(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}
