//! The relay tunnel's byte stream (design/vps-relay.md, "Codec and
//! transport"): the mux's frames, each with a 4-byte length in front, go
//! through one relay codec per direction (piece cache + zstd, B1) and then
//! into the TLS connection. Every player's data shares one compressed
//! stream, which is what makes the tunnel send so much less than vanilla.
//!
//! [`split`] turns a connection into the mux's [`FrameTx`] / [`FrameRx`].
//! A writer task batches what the mux has ready, compresses it and flushes
//! once per batch (so a packet never waits for the next one); a reader task
//! decodes and cuts frames. Both buffers are bounded: the writer by the
//! mux's own queue, the reader by one frame plus one decoder read.

use std::io;
use std::sync::{Arc, Mutex};

use hopper_core::hosting::relay_codec::{CodecConfig, CodecStats, Decoder, Encoder};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

use crate::tunnel::frame::{HEADER, MAX_FRAME};
use crate::tunnel::mux::{FrameRx, FrameTx};

/// Raw bytes the writer takes into one compressed burst at most.
const BATCH: usize = 256 * 1024;

/// One direction's codec settings as the two ends agree them. The end that
/// receives a direction picks them (it holds the decoder's window and
/// cache, so it knows what it can afford); the sender checks them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Codec {
    pub level: i32,
    pub window_log: u32,
    #[serde(default)]
    pub long_distance: bool,
    pub cache_bytes: u64,
}

impl From<CodecConfig> for Codec {
    fn from(c: CodecConfig) -> Self {
        Codec { level: c.level, window_log: c.window_log, long_distance: c.long_distance, cache_bytes: c.cache_bytes }
    }
}

impl From<Codec> for CodecConfig {
    fn from(c: Codec) -> Self {
        CodecConfig { level: c.level, window_log: c.window_log, long_distance: c.long_distance, cache_bytes: c.cache_bytes }
    }
}

impl Codec {
    /// The quiet direction (players to the server: movement, chat, a few
    /// KB/s): a small window and no cache, so it costs a few MB at most.
    pub fn quiet() -> Self {
        Codec { level: 3, window_log: 22, long_distance: false, cache_bytes: 0 }
    }
}

/// What one connection has moved so far, both directions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    /// What this end sent (its encoder's counters).
    pub sent: CodecStats,
    /// What it received (its decoder's counters).
    pub received: CodecStats,
}

/// Counters the writer and reader keep up to date; read by the status.
#[derive(Default)]
pub struct Counters(Mutex<LinkStats>);

impl Counters {
    pub fn get(&self) -> LinkStats {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn set_sent(&self, s: CodecStats) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).sent = s;
    }
    fn set_received(&self, s: CodecStats) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).received = s;
    }
}

pub struct WireTx(mpsc::Sender<Vec<u8>>);

pub struct WireRx(mpsc::Receiver<io::Result<Vec<u8>>>);

impl FrameTx for WireTx {
    async fn send(&mut self, frame: Vec<u8>) -> io::Result<()> {
        self.0.send(frame).await.map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the relay connection's writer stopped"))
    }
}

impl FrameRx for WireRx {
    async fn recv(&mut self) -> Option<io::Result<Vec<u8>>> {
        self.0.recv().await
    }
}

/// Starts the writer and reader for `io`: frames this end sends are
/// compressed with `send`, frames it receives are decoded with `recv`.
pub fn split<S>(io: S, send: Codec, recv: Codec, counters: Arc<Counters>) -> Result<(WireTx, WireRx), String>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let enc = Encoder::new(&send.into()).map_err(|e| e.to_string())?;
    let dec = Decoder::new(&recv.into()).map_err(|e| e.to_string())?;
    let (r, w) = tokio::io::split(io);
    // A few frames waiting: the mux keeps its own queue in front of this.
    let (tx, frames) = mpsc::channel::<Vec<u8>>(16);
    let (got, rx) = mpsc::channel::<io::Result<Vec<u8>>>(16);
    tokio::spawn(write_loop(w, enc, frames, counters.clone()));
    tokio::spawn(read_loop(r, dec, got, counters));
    Ok((WireTx(tx), WireRx(rx)))
}

fn put(enc: &mut Encoder, frame: &[u8]) -> Result<(), String> {
    enc.write(&(frame.len() as u32).to_be_bytes()).map_err(|e| e.to_string())?;
    enc.write(frame).map_err(|e| e.to_string())
}

async fn write_loop<W: AsyncWrite + Unpin>(mut w: W, mut enc: Encoder, mut frames: mpsc::Receiver<Vec<u8>>, counters: Arc<Counters>) {
    while let Some(first) = frames.recv().await {
        let mut taken = first.len();
        if let Err(e) = put(&mut enc, &first) {
            log::warn!("relay tunnel: {e}");
            return;
        }
        // Whatever else is ready goes in the same burst; one yield lets a
        // stream that is about to send join it too. No timer: a burst ends
        // as soon as nothing more is waiting.
        let mut yielded = false;
        while taken < BATCH {
            match frames.try_recv() {
                Ok(f) => {
                    taken += f.len();
                    if let Err(e) = put(&mut enc, &f) {
                        log::warn!("relay tunnel: {e}");
                        return;
                    }
                }
                Err(_) if !yielded => {
                    yielded = true;
                    tokio::task::yield_now().await;
                }
                Err(_) => break,
            }
        }
        if let Err(e) = enc.flush() {
            log::warn!("relay tunnel: {e}");
            return;
        }
        let out = enc.take_output();
        counters.set_sent(enc.stats());
        if w.write_all(&out).await.is_err() || w.flush().await.is_err() {
            return;
        }
    }
    // The mux is done with the connection: say so (TLS close_notify).
    let _ = w.shutdown().await;
}

async fn read_loop<R: AsyncRead + Unpin>(mut r: R, mut dec: Decoder, got: mpsc::Sender<io::Result<Vec<u8>>>, counters: Arc<Counters>) {
    let mut net = vec![0u8; 64 * 1024];
    let mut plain = vec![0u8; 64 * 1024];
    // At most one frame plus one decoder read: frames are taken out as soon
    // as they are whole, and the next read waits until they've been passed on.
    let mut pending: Vec<u8> = Vec::new();
    let fail = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    loop {
        let n = match r.read(&mut net).await {
            Ok(0) => {
                if !dec.at_boundary() || !pending.is_empty() {
                    let _ = got.send(Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the relay connection was cut mid-frame"))).await;
                }
                return;
            }
            Ok(n) => n,
            Err(e) => {
                let _ = got.send(Err(e)).await;
                return;
            }
        };
        dec.push(&net[..n]);
        loop {
            let k = match dec.read(&mut plain) {
                Ok(0) => break,
                Ok(k) => k,
                Err(e) => {
                    let _ = got.send(Err(fail(e.to_string()))).await;
                    return;
                }
            };
            pending.extend_from_slice(&plain[..k]);
            let mut off = 0;
            while pending.len() - off >= 4 {
                let len = u32::from_be_bytes(pending[off..off + 4].try_into().expect("4 bytes")) as usize;
                if !(HEADER..=MAX_FRAME).contains(&len) {
                    let _ = got.send(Err(fail(format!("frame of {len} bytes")))).await;
                    return;
                }
                if pending.len() - off - 4 < len {
                    break;
                }
                let frame = pending[off + 4..off + 4 + len].to_vec();
                off += 4 + len;
                if got.send(Ok(frame)).await.is_err() {
                    return;
                }
            }
            pending.drain(..off);
        }
        counters.set_received(dec.stats());
    }
}
