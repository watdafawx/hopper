//! The piece cache that sits between the raw tunnel stream and zstd.
//!
//! zstd only finds repeats inside its window, and the cheaper levels only
//! nearby ones. Players near each other get byte-identical chunk and entity
//! packets, often minutes apart, so the sender cuts the stream into
//! content-defined pieces (about 4 KB; the cut points depend on the bytes,
//! so the same packet cuts the same way whatever came before it) and sends
//! a short reference for any piece the other side already holds.
//!
//! Both ends keep the same cache because both apply the same steps in the
//! same order: every literal piece of at least [`MIN_REF`] bytes is added
//! with the next id, every reference moves its piece to the next id (so hot
//! pieces stay), and the oldest ids are dropped once the cache passes its
//! byte cap. The tunnel is one ordered stream, so the two sides can't
//! disagree. An id names one piece for ever (a moved piece gets a new id),
//! so even a receiver whose cache somehow differs can only fail with
//! "unknown piece", never hand out the wrong bytes.
//!
//! Only the sender cuts, so the cut rule (the gear table, the sizes) can
//! change between versions without breaking the other end.
//!
//! Tokens, inside the zstd stream:
//!
//! ```text
//! 0 uvarint(n) bytes   literal, added to the cache (MIN_REF <= n <= MAX_PIECE)
//! 1 uvarint(id)        reference to cached piece id
//! 2 uvarint(n) bytes   literal, not cached (1 <= n < MIN_REF)
//! ```
//!
//! A port of the maintainer's `mctunnel` dedupe.go (same token values and
//! sizes), with the order list replaced by an ordered map so a long session
//! of cache hits doesn't grow a list of stale ids.

use super::CodecError;
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::sync::Arc;

pub(super) const TOK_LIT: u8 = 0;
pub(super) const TOK_REF: u8 = 1;
pub(super) const TOK_RAW: u8 = 2;
/// No cut before this many bytes, so pieces aren't tiny.
pub(super) const MIN_PIECE: usize = 1 << 10;
/// Always cut here, so one piece can't grow without end.
pub(super) const MAX_PIECE: usize = 64 << 10;
/// Average piece about 4 KB past MIN_PIECE.
const CUT_MASK: u64 = (1 << 12) - 1;
/// Shorter pieces aren't worth a cache slot: a reference would save little.
pub(super) const MIN_REF: usize = 256;
/// What one cached piece costs besides its bytes (map node, Arc header,
/// index slot), counted into the cap so the cap is close to real memory.
/// Both ends count the same way, which is all that keeps them in step.
pub(super) const ENTRY_COST: u64 = 64;

/// The gear hash's table: 256 fixed pseudo-random words (splitmix64), made
/// at compile time so every build cuts the same way.
const GEAR: [u64; 256] = {
    let mut t = [0u64; 256];
    let mut s: u64 = 0x6d63_7475_6e00_0001;
    let mut i = 0;
    while i < 256 {
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        t[i] = z ^ (z >> 31);
        i += 1;
    }
    t
};

/// The mirrored piece store. `index` (content hash -> id) only exists on
/// the sending side; the receiver only follows ids.
pub(super) struct Cache {
    /// id -> (bytes, content hash). Ids only grow, so the first entry is
    /// always the oldest.
    pieces: BTreeMap<u64, (Arc<[u8]>, u64)>,
    index: Option<HashMap<u64, u64>>,
    next: u64,
    used: u64,
    cap: u64,
}

impl Cache {
    pub(super) fn new(cap: u64, indexed: bool) -> Self {
        Cache { pieces: BTreeMap::new(), index: indexed.then(HashMap::new), next: 0, used: 0, cap }
    }

    fn add(&mut self, b: Arc<[u8]>, h: u64) {
        let id = self.next;
        self.next += 1;
        self.used += b.len() as u64 + ENTRY_COST;
        self.pieces.insert(id, (b, h));
        if let Some(ix) = &mut self.index {
            ix.insert(h, id);
        }
        // The cap is at least one maximum piece (CodecConfig checks), so the
        // piece just added is never the one dropped.
        while self.used > self.cap {
            let Some((old, (ob, oh))) = self.pieces.pop_first() else { break };
            self.used -= ob.len() as u64 + ENTRY_COST;
            if let Some(ix) = &mut self.index {
                if ix.get(&oh) == Some(&old) {
                    ix.remove(&oh);
                }
            }
        }
    }

    /// Moves a referenced piece to the newest id and returns its bytes.
    fn touch(&mut self, id: u64) -> Option<Arc<[u8]>> {
        let (b, h) = self.pieces.remove(&id)?;
        self.used -= b.len() as u64 + ENTRY_COST;
        self.add(b.clone(), h);
        Some(b)
    }

    pub(super) fn used(&self) -> u64 {
        self.used
    }

    pub(super) fn len(&self) -> u64 {
        self.pieces.len() as u64
    }

    /// A fingerprint of what's cached (ids, lengths, bytes), to prove the
    /// two ends hold the same thing. Uses a fixed-key hasher so it's the same
    /// on both sides of one build.
    pub(super) fn digest(&self) -> u64 {
        #[allow(deprecated)]
        let mut d = std::hash::SipHasher::new();
        self.next.hash(&mut d);
        self.used.hash(&mut d);
        for (id, (b, _)) in &self.pieces {
            id.hash(&mut d);
            b.hash(&mut d);
        }
        d.finish()
    }
}

fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// The sending side: cuts what's written into pieces and writes tokens.
pub(super) struct DedupeEnc {
    cache: Cache,
    /// Bytes written but not cut yet (always shorter than MAX_PIECE between
    /// calls).
    pend: Vec<u8>,
    /// How far into `pend` the rolling hash has been run.
    scanned: usize,
    h: u64,
    /// Seeded per process: hash collisions only cost a byte compare, but a
    /// random seed keeps anyone from crafting many of them.
    hasher: RandomState,
    pub(super) hits: u64,
    pub(super) hit_bytes: u64,
}

impl DedupeEnc {
    pub(super) fn new(cap: u64) -> Self {
        DedupeEnc {
            cache: Cache::new(cap, true),
            pend: Vec::with_capacity(MAX_PIECE),
            scanned: 0,
            h: 0,
            hasher: RandomState::new(),
            hits: 0,
            hit_bytes: 0,
        }
    }

    pub(super) fn cache(&self) -> &Cache {
        &self.cache
    }

    pub(super) fn pending(&self) -> usize {
        self.pend.len()
    }

    pub(super) fn write(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.pend.extend_from_slice(data);
        let pend = std::mem::take(&mut self.pend);
        let mut start = 0;
        loop {
            // The gear hash only depends on the last 64 bytes (each step
            // shifts one bit out), and no cut can happen before MIN_PIECE,
            // so the bytes before MIN_PIECE - 64 needn't be hashed at all.
            let skip_to = start + MIN_PIECE - 64;
            let mut i = start + self.scanned;
            if i < skip_to {
                i = skip_to.min(pend.len());
                self.h = 0;
            }
            let mut cut = None;
            while i < pend.len() {
                self.h = (self.h << 1).wrapping_add(GEAR[pend[i] as usize]);
                let n = i + 1 - start;
                if (n >= MIN_PIECE && self.h & CUT_MASK == 0) || n >= MAX_PIECE {
                    cut = Some(i + 1);
                    break;
                }
                i += 1;
            }
            match cut {
                None => {
                    self.scanned = i - start;
                    break;
                }
                Some(c) => {
                    self.piece(&pend[start..c], out);
                    start = c;
                    self.scanned = 0;
                    self.h = 0;
                }
            }
        }
        self.pend = pend;
        // One move per write call, not per piece: a big write cuts into many
        // pieces, and shifting the rest after each would be quadratic.
        self.pend.drain(..start);
    }

    /// Sends whatever is pending. The tail is cut here instead of at a
    /// content-defined point, so it goes as a literal (cached if big enough).
    pub(super) fn flush(&mut self, out: &mut Vec<u8>) {
        if self.pend.is_empty() {
            return;
        }
        let pend = std::mem::take(&mut self.pend);
        self.piece(&pend, out);
        self.pend = pend;
        self.pend.clear();
        self.scanned = 0;
        self.h = 0;
    }

    fn piece(&mut self, b: &[u8], out: &mut Vec<u8>) {
        if b.len() < MIN_REF {
            out.push(TOK_RAW);
            put_uvarint(out, b.len() as u64);
            out.extend_from_slice(b);
            return;
        }
        let h = self.hasher.hash_one(b);
        let found = self.cache.index.as_ref().and_then(|ix| ix.get(&h)).copied();
        if let Some(id) = found {
            if self.cache.pieces.get(&id).is_some_and(|(e, _)| **e == *b) {
                self.cache.touch(id);
                self.hits += 1;
                self.hit_bytes += b.len() as u64;
                out.push(TOK_REF);
                put_uvarint(out, id);
                return;
            }
        }
        self.cache.add(Arc::from(b), h);
        out.push(TOK_LIT);
        put_uvarint(out, b.len() as u64);
        out.extend_from_slice(b);
    }
}

enum State {
    Tag,
    Varint { tok: u8, val: u64, shift: u32 },
    Body { tok: u8, buf: Vec<u8>, need: usize },
}

/// The receiving side: reads tokens and hands out the original pieces.
pub(super) struct DedupeDec {
    cache: Cache,
    state: State,
    pub(super) hits: u64,
    pub(super) hit_bytes: u64,
}

impl DedupeDec {
    pub(super) fn new(cap: u64) -> Self {
        DedupeDec { cache: Cache::new(cap, false), state: State::Tag, hits: 0, hit_bytes: 0 }
    }

    pub(super) fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Reads token bytes from `input` until one piece is complete or the
    /// input runs out. Returns how many bytes it used and the piece, if one
    /// finished. Every length is checked before anything is allocated, so a
    /// bad stream costs at most one MAX_PIECE buffer.
    pub(super) fn parse(&mut self, input: &[u8]) -> Result<(usize, Option<Arc<[u8]>>), CodecError> {
        let mut used = 0;
        while used < input.len() {
            match &mut self.state {
                State::Tag => {
                    let tok = input[used];
                    used += 1;
                    if tok > TOK_RAW {
                        return Err(CodecError::Corrupt(format!("unknown token {tok}")));
                    }
                    self.state = State::Varint { tok, val: 0, shift: 0 };
                }
                State::Varint { tok, val, shift } => {
                    let b = input[used];
                    used += 1;
                    if *shift > 63 || (*shift == 63 && b > 1) {
                        return Err(CodecError::Corrupt("number too long".into()));
                    }
                    *val |= u64::from(b & 0x7f) << *shift;
                    *shift += 7;
                    if b & 0x80 != 0 {
                        continue;
                    }
                    let (tok, v) = (*tok, *val);
                    match tok {
                        TOK_REF => {
                            self.state = State::Tag;
                            let Some(p) = self.cache.touch(v) else {
                                return Err(CodecError::OutOfStep(v));
                            };
                            self.hits += 1;
                            self.hit_bytes += p.len() as u64;
                            return Ok((used, Some(p)));
                        }
                        _ => {
                            let ok = if tok == TOK_LIT {
                                (MIN_REF as u64..=MAX_PIECE as u64).contains(&v)
                            } else {
                                (1..MIN_REF as u64).contains(&v)
                            };
                            if !ok {
                                return Err(CodecError::Corrupt(format!("piece length {v} out of range")));
                            }
                            let need = v as usize;
                            self.state = State::Body { tok, buf: Vec::with_capacity(need), need };
                        }
                    }
                }
                State::Body { tok, buf, need } => {
                    let take = (*need - buf.len()).min(input.len() - used);
                    buf.extend_from_slice(&input[used..used + take]);
                    used += take;
                    if buf.len() == *need {
                        let tok = *tok;
                        let b: Arc<[u8]> = Arc::from(std::mem::take(buf));
                        self.state = State::Tag;
                        if tok == TOK_LIT {
                            self.cache.add(b.clone(), 0);
                        }
                        return Ok((used, Some(b)));
                    }
                }
            }
        }
        Ok((used, None))
    }

    /// True between tokens: a stream may only stop here.
    pub(super) fn at_token_start(&self) -> bool {
        matches!(self.state, State::Tag)
    }
}
