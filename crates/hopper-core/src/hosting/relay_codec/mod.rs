//! Stream compression for the relay tunnel (design/vps-relay.md, B1).
//!
//! Home and the relay VPS talk over one connection that carries every
//! player's traffic. Compressing it as one long stream, instead of packet
//! by packet like vanilla, lets what one player receives pay for what the
//! next one receives: the same chunks, the same machines, the same entity
//! updates. Two layers, one [`Encoder`] / [`Decoder`] pair per direction:
//!
//! 1. A piece cache mirrored on both ends ([`dedupe`]): repeats from minutes
//!    ago, longer back than zstd's window reaches, go as a short reference.
//! 2. One zstd stream with a long window (256 MB), flushed after each burst
//!    so a packet never waits for the next one.
//!
//! `cargo run --profile fast -p hopper-core --example relay_replay` replays
//! a recording (or a synthetic stream) through both and prints the numbers.
//!
//! No networking here: the transport (B2) writes what the mux sends into
//! [`Encoder::write`], calls [`Encoder::flush`] when a burst is over, sends
//! [`Encoder::take_output`], and feeds what arrives into [`Decoder::push`] /
//! [`Decoder::read`]. Both ends must use the same [`CodecConfig`] for a
//! direction (agreed in the transport's hello). The state belongs to one
//! connection: a reconnect starts both ends empty.
//!
//! Memory is bounded by the config: the zstd window, the cache cap, and a
//! few fixed buffers. A corrupt or out-of-step stream ends in an error, not
//! a panic or a big allocation, and the transport then drops the
//! connection.

mod dedupe;
mod vanilla;

pub use vanilla::VanillaEstimate;

use dedupe::{DedupeDec, DedupeEnc};
use std::fmt;
use std::sync::Arc;
use zstd::zstd_safe::zstd_sys::ZSTD_EndDirective;
use zstd::zstd_safe::{CCtx, CParameter, DCtx, DParameter, InBuffer, OutBuffer};

const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;

/// How one direction of the tunnel is compressed. Both ends of that
/// direction must use the same values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecConfig {
    /// zstd level (1-22). Only the sender uses it.
    pub level: i32,
    /// zstd window, as a power of two (28 = 256 MB). The receiver refuses
    /// a stream that asks for more.
    pub window_log: u32,
    /// zstd's long distance matching: finds repeats far back in the window
    /// that the normal match finder misses. Costs a little sender memory.
    pub long_distance: bool,
    /// Piece cache size in bytes; 0 turns the cache off.
    pub cache_bytes: u64,
}

impl CodecConfig {
    /// The biggest window a receiver accepts (256 MB).
    pub const MAX_WINDOW_LOG: u32 = 28;
    /// The biggest cache a receiver accepts.
    pub const MAX_CACHE: u64 = 4 * GB;
    /// The smallest cache that isn't off: a few maximum-size pieces, so the
    /// piece just added never has to be dropped at once.
    pub const MIN_CACHE: u64 = MB;

    /// The measured settings from the hand-built tunnel: about klauspost's
    /// "best" (libzstd 11), 256 MB window, 1 GB cache. About 1.1 GB of RAM
    /// on each end of the busy direction (window, cache, zstd's tables).
    /// Long distance matching stays off: klauspost has none, and on the
    /// replay it sent about 15% more, not less (the cache already finds the
    /// far repeats it is for).
    pub fn full() -> Self {
        CodecConfig { level: 11, window_log: 28, long_distance: false, cache_bytes: GB }
    }

    /// For machines under 4 GB of RAM (a small VPS): the same, with a
    /// 512 MB cache.
    pub fn small() -> Self {
        CodecConfig { cache_bytes: 512 * MB, ..Self::full() }
    }

    /// The default for a machine with this much RAM in total.
    pub fn for_memory(total_ram: u64) -> Self {
        if total_ram < 4 * GB { Self::small() } else { Self::full() }
    }

    /// Checks values that came from the other end before using them.
    pub fn validate(&self) -> Result<(), CodecError> {
        let bad = |m: String| Err(CodecError::Config(m));
        if !(1..=22).contains(&self.level) {
            return bad(format!("zstd level {} (1 to 22)", self.level));
        }
        if !(10..=Self::MAX_WINDOW_LOG).contains(&self.window_log) {
            return bad(format!("window 2^{} (2^10 to 2^{})", self.window_log, Self::MAX_WINDOW_LOG));
        }
        if self.cache_bytes != 0 && !(Self::MIN_CACHE..=Self::MAX_CACHE).contains(&self.cache_bytes) {
            return bad(format!("cache {} bytes (1 MB to 4 GB, or 0)", self.cache_bytes));
        }
        Ok(())
    }
}

/// Why a stream can't go on. Every one of these ends the connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    /// A config value out of range.
    Config(String),
    /// zstd refused (bad data, a window bigger than allowed, out of memory).
    Zstd(&'static str),
    /// The piece stream doesn't parse.
    Corrupt(String),
    /// A reference to a piece this end doesn't hold: the two caches aren't
    /// the same any more.
    OutOfStep(u64),
    /// The encoder failed earlier and can't be used again.
    Broken,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::Config(m) => write!(f, "relay compression: bad setting: {m}"),
            CodecError::Zstd(m) => write!(f, "relay compression: zstd: {m}"),
            CodecError::Corrupt(m) => write!(f, "relay compression: bad data: {m}"),
            CodecError::OutOfStep(id) => {
                write!(f, "relay compression: unknown piece {id} (the two ends are out of step)")
            }
            CodecError::Broken => write!(f, "relay compression: stopped after an earlier error"),
        }
    }
}

impl std::error::Error for CodecError {}

fn zerr(code: usize) -> CodecError {
    CodecError::Zstd(zstd::zstd_safe::get_error_name(code))
}

/// Counters for one direction, the same shape on both ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodecStats {
    /// Bytes before compression (written to the encoder / handed out by the
    /// decoder).
    pub raw: u64,
    /// Bytes on the wire (out of the encoder / into the decoder).
    pub wire: u64,
    /// Pieces sent as a reference instead of their bytes.
    pub hits: u64,
    /// Raw bytes those references stood for.
    pub hit_bytes: u64,
    /// What the piece cache holds now (bytes, counting each piece's
    /// bookkeeping) and how many pieces.
    pub cache_used: u64,
    pub cache_pieces: u64,
}

/// zstd's output arrives in pieces of this size, and the encoder hands
/// tokens to zstd once this many are waiting.
const CHUNK: usize = 128 << 10;

/// The sending end of one direction.
pub struct Encoder {
    cctx: CCtx<'static>,
    dd: Option<DedupeEnc>,
    /// Token bytes (or raw bytes when the cache is off) not given to zstd yet.
    tokens: Vec<u8>,
    out: Vec<u8>,
    stats: CodecStats,
    broken: bool,
}

impl Encoder {
    pub fn new(cfg: &CodecConfig) -> Result<Self, CodecError> {
        cfg.validate()?;
        let mut cctx = CCtx::try_create().ok_or(CodecError::Zstd("out of memory"))?;
        cctx.set_parameter(CParameter::CompressionLevel(cfg.level)).map_err(zerr)?;
        cctx.set_parameter(CParameter::WindowLog(cfg.window_log)).map_err(zerr)?;
        cctx.set_parameter(CParameter::EnableLongDistanceMatching(cfg.long_distance)).map_err(zerr)?;
        // TLS already checks every byte; a checksum would also only arrive
        // at the end of a frame, and this frame lasts the whole connection.
        cctx.set_parameter(CParameter::ChecksumFlag(false)).map_err(zerr)?;
        cctx.set_parameter(CParameter::ContentSizeFlag(false)).map_err(zerr)?;
        Ok(Encoder {
            cctx,
            dd: (cfg.cache_bytes > 0).then(|| DedupeEnc::new(cfg.cache_bytes)),
            tokens: Vec::with_capacity(CHUNK + 16),
            out: Vec::new(),
            stats: CodecStats::default(),
            broken: false,
        })
    }

    /// Takes the next raw bytes. They may stay inside until [`flush`].
    ///
    /// [`flush`]: Encoder::flush
    pub fn write(&mut self, raw: &[u8]) -> Result<(), CodecError> {
        if self.broken {
            return Err(CodecError::Broken);
        }
        self.stats.raw += raw.len() as u64;
        // In slices so `tokens` stays near CHUNK however big the write is.
        for part in raw.chunks(CHUNK) {
            match &mut self.dd {
                Some(dd) => dd.write(part, &mut self.tokens),
                None => self.tokens.extend_from_slice(part),
            }
            if self.tokens.len() >= CHUNK {
                self.compress(ZSTD_EndDirective::ZSTD_e_continue)?;
            }
        }
        Ok(())
    }

    /// Ends a burst: everything written so far comes out of
    /// [`take_output`](Encoder::take_output) and can be decoded on its own.
    /// Costs a few bytes, so call it when the sender goes quiet (the Go
    /// tunnel waits about 1 ms), not after every packet.
    pub fn flush(&mut self) -> Result<(), CodecError> {
        if self.broken {
            return Err(CodecError::Broken);
        }
        if let Some(dd) = &mut self.dd {
            dd.flush(&mut self.tokens);
        }
        self.compress(ZSTD_EndDirective::ZSTD_e_flush)
    }

    fn compress(&mut self, end: ZSTD_EndDirective) -> Result<(), CodecError> {
        let mut input = InBuffer::around(&self.tokens);
        loop {
            self.out.reserve(CHUNK);
            let pos = self.out.len();
            let mut output = OutBuffer::around_pos(&mut self.out, pos);
            let left = match self.cctx.compress_stream2(&mut output, &mut input, end) {
                Ok(n) => n,
                Err(code) => {
                    // zstd leaves its state undefined after an error.
                    self.broken = true;
                    return Err(zerr(code));
                }
            };
            let wrote = output.pos() - pos;
            self.stats.wire += wrote as u64;
            let all_in = input.pos() == self.tokens.len();
            let done = match end {
                ZSTD_EndDirective::ZSTD_e_continue => all_in,
                _ => all_in && left == 0,
            };
            if done {
                break;
            }
        }
        self.tokens.clear();
        Ok(())
    }

    /// The compressed bytes ready to send (and forgets them).
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// How many compressed bytes are waiting in [`take_output`](Encoder::take_output).
    pub fn output_len(&self) -> usize {
        self.out.len()
    }

    pub fn stats(&self) -> CodecStats {
        let mut s = self.stats;
        if let Some(dd) = &self.dd {
            s.hits = dd.hits;
            s.hit_bytes = dd.hit_bytes;
            s.cache_used = dd.cache().used();
            s.cache_pieces = dd.cache().len();
        }
        s
    }

    /// What this end holds in memory, about: zstd's own state (window and
    /// tables), the cache, and the buffers.
    pub fn memory(&self) -> u64 {
        let dd = self.dd.as_ref().map_or(0, |d| d.cache().used() + d.pending() as u64);
        self.cctx.sizeof() as u64 + dd + (self.tokens.capacity() + self.out.capacity()) as u64
    }

    /// A fingerprint of the piece cache; equal to the decoder's after both
    /// have handled the same stream.
    pub fn cache_digest(&self) -> u64 {
        self.dd.as_ref().map_or(0, |d| d.cache().digest())
    }
}

/// The receiving end of one direction.
pub struct Decoder {
    dctx: DCtx<'static>,
    dd: Option<DedupeDec>,
    /// Compressed bytes pushed but not given to zstd yet.
    input: Vec<u8>,
    ipos: usize,
    /// zstd's output not parsed yet.
    zbuf: Box<[u8]>,
    zpos: usize,
    zlen: usize,
    /// The piece being handed out, and how much of it is gone.
    cur: Option<(Arc<[u8]>, usize)>,
    stats: CodecStats,
    failed: Option<CodecError>,
}

impl Decoder {
    pub fn new(cfg: &CodecConfig) -> Result<Self, CodecError> {
        cfg.validate()?;
        let mut dctx = DCtx::try_create().ok_or(CodecError::Zstd("out of memory"))?;
        // The window is the receiver's biggest allocation; never let the
        // stream choose a bigger one than agreed.
        dctx.set_parameter(DParameter::WindowLogMax(cfg.window_log)).map_err(zerr)?;
        Ok(Decoder {
            dctx,
            dd: (cfg.cache_bytes > 0).then(|| DedupeDec::new(cfg.cache_bytes)),
            input: Vec::new(),
            ipos: 0,
            zbuf: vec![0u8; CHUNK].into_boxed_slice(),
            zpos: 0,
            zlen: 0,
            cur: None,
            stats: CodecStats::default(),
            failed: None,
        })
    }

    /// Hands over compressed bytes as they arrive. Call [`read`](Decoder::read)
    /// until it returns 0 before pushing more, so nothing piles up here.
    pub fn push(&mut self, compressed: &[u8]) {
        self.stats.wire += compressed.len() as u64;
        if self.ipos == self.input.len() {
            self.input.clear();
            self.ipos = 0;
        }
        self.input.extend_from_slice(compressed);
    }

    /// Fills `out` with decoded bytes. 0 means everything pushed so far is
    /// out (push more). Bytes decoded before a fault still come out; the
    /// next call returns the error, and so does every call after it.
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, CodecError> {
        if let Some(e) = &self.failed {
            return Err(e.clone());
        }
        let mut n = 0;
        let r = self.read_inner(out, &mut n);
        self.stats.raw += n as u64;
        match r {
            Ok(()) => Ok(n),
            Err(e) => {
                self.failed = Some(e.clone());
                if n > 0 { Ok(n) } else { Err(e) }
            }
        }
    }

    fn read_inner(&mut self, out: &mut [u8], n: &mut usize) -> Result<(), CodecError> {
        while *n < out.len() {
            if let Some((p, off)) = &mut self.cur {
                let k = (p.len() - *off).min(out.len() - *n);
                out[*n..*n + k].copy_from_slice(&p[*off..*off + k]);
                *off += k;
                *n += k;
                if *off == p.len() {
                    self.cur = None;
                }
                continue;
            }
            if self.zpos == self.zlen && !self.refill()? {
                break;
            }
            match &mut self.dd {
                None => {
                    let k = (self.zlen - self.zpos).min(out.len() - *n);
                    out[*n..*n + k].copy_from_slice(&self.zbuf[self.zpos..self.zpos + k]);
                    self.zpos += k;
                    *n += k;
                }
                Some(dd) => {
                    let (used, piece) = dd.parse(&self.zbuf[self.zpos..self.zlen])?;
                    self.zpos += used;
                    if let Some(p) = piece {
                        self.cur = Some((p, 0));
                    }
                }
            }
        }
        Ok(())
    }

    /// Runs zstd on what's pushed. False when it has nothing more to give.
    fn refill(&mut self) -> Result<bool, CodecError> {
        loop {
            let mut input = InBuffer::around(&self.input[self.ipos..]);
            let mut output = OutBuffer::around(&mut self.zbuf[..]);
            self.dctx.decompress_stream(&mut output, &mut input).map_err(zerr)?;
            let used = input.pos();
            self.ipos += used;
            self.zpos = 0;
            self.zlen = output.pos();
            // zstd can take input without giving output yet (a frame or
            // block header); go on while it's still taking some.
            if self.zlen > 0 || used == 0 || self.ipos == self.input.len() {
                return Ok(self.zlen > 0);
            }
        }
    }

    /// Decodes `compressed` and appends the result to `out`. For tests and
    /// the replay tool; the transport uses push / read to bound its buffers.
    pub fn decode(&mut self, compressed: &[u8], out: &mut Vec<u8>) -> Result<(), CodecError> {
        self.push(compressed);
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = self.read(&mut buf)?;
            if n == 0 {
                return Ok(());
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    /// True when the stream so far ended cleanly between pieces (what a
    /// flushed encoder leaves). A connection that closes with this false
    /// was cut mid-piece.
    pub fn at_boundary(&self) -> bool {
        self.cur.is_none()
            && self.zpos == self.zlen
            && self.ipos == self.input.len()
            && self.dd.as_ref().is_none_or(|d| d.at_token_start())
    }

    pub fn stats(&self) -> CodecStats {
        let mut s = self.stats;
        if let Some(dd) = &self.dd {
            s.hits = dd.hits;
            s.hit_bytes = dd.hit_bytes;
            s.cache_used = dd.cache().used();
            s.cache_pieces = dd.cache().len();
        }
        s
    }

    /// What this end holds in memory, about (see [`Encoder::memory`]).
    pub fn memory(&self) -> u64 {
        let dd = self.dd.as_ref().map_or(0, |d| d.cache().used());
        self.dctx.sizeof() as u64 + dd + (self.input.capacity() + self.zbuf.len()) as u64
    }

    /// See [`Encoder::cache_digest`].
    pub fn cache_digest(&self) -> u64 {
        self.dd.as_ref().map_or(0, |d| d.cache().digest())
    }
}

#[cfg(test)]
mod tests;
