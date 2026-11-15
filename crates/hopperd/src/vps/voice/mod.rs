//! Voice chat through the relay (design/vps-relay.md, "B6 as built").
//!
//! Proximity voice mods (Simple Voice Chat, Plasmo Voice) send audio over
//! UDP. Players send it to the relay's voice port on the VPS; the relay
//! passes each packet home, and home hands it to the server's voice port on
//! its own machine, from one local socket per player so the mod can tell
//! them apart. Replies go back the same way, to each player's address.
//!
//! Home opens no port: it sends UDP **out** to the relay's voice port and
//! keeps that path open with a keepalive every few seconds (the home
//! router's NAT then lets the relay's packets back in). The relay learns
//! home's address from the first packet that passes the check below, and
//! follows it if the NAT changes it.
//!
//! Between home and the relay every packet is sealed (ChaCha20-Poly1305):
//!
//! ```text
//! 0..3   "HRV"            not a voice mod's first bytes (SVC 0xFF, Plasmo 0x4E9004E9)
//! 3      version (1)
//! 4      kind             1 keepalive (home→relay), 2 its answer, 3 a voice packet
//! 5..9   session          which connection's keys
//! 9..17  counter          big-endian, per direction, never repeats: the nonce
//! 17..   sealed body + 16-byte tag; the 17 bytes above are bound to it
//! ```
//!
//! The keys come from the relay's TLS connection (the one pinned to the
//! relay's certificate and checked with the relay key), through the TLS
//! exporter, mixed with the relay key's SHA-256: one pair per connection,
//! nothing new for the player to keep, gone when the connection ends.
//! A replay window (64 counters) drops repeats.
//!
//! When no keepalive answer gets through (the VPS provider's firewall, or
//! a line that blocks UDP), the packets go over the TCP tunnel instead, on
//! a mux stream of kind "voice" (`u16 length, u32 flow, packet`), which
//! works with more delay; the relay card says which one is used.

pub mod home;
pub mod relay;

use std::time::{Duration, Instant};

use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hkdf;

pub const MAGIC: &[u8; 3] = b"HRV";
pub const VERSION: u8 = 1;
pub const HEADER: usize = 17;
pub const TAG: usize = 16;
/// The biggest voice packet passed on either way. Simple Voice Chat's
/// default audio packets are about 1.3 KB at most (`mtu_size` 1275);
/// anything bigger is dropped unread.
pub const MAX_PACKET: usize = 2048;
/// The biggest sealed datagram: header, flow, packet, tag.
pub const MAX_SEALED: usize = HEADER + 4 + MAX_PACKET + TAG;
/// The TLS exporter's label for the voice keys.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-hopper-relay-voice";
/// How often home sends a keepalive; NAT mappings for UDP often last only
/// 30 s, so well under that.
pub const KEEPALIVE: Duration = Duration::from_secs(5);
/// No keepalive answer for this long: UDP doesn't get through.
pub const STALE: Duration = Duration::from_secs(15);
/// A player's flow with nothing in either direction for this long is dropped.
pub const FLOW_IDLE: Duration = Duration::from_secs(60);
/// Players at once (flows), and per address.
pub const MAX_FLOWS: usize = 512;
pub const MAX_FLOWS_PER_IP: usize = 8;
/// Players that sent voice in this long count as "on voice" for the card.
pub const ACTIVE: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keepalive = 1,
    Ack = 2,
    Data = 3,
}

impl Kind {
    fn from(b: u8) -> Option<Kind> {
        match b {
            1 => Some(Kind::Keepalive),
            2 => Some(Kind::Ack),
            3 => Some(Kind::Data),
            _ => None,
        }
    }
}

/// The 64 bytes both ends take from the TLS exporter of their connection.
pub type Exported = [u8; 64];

/// Keys for one connection, one direction each.
pub struct Keys {
    pub session: u32,
    home_to_relay: [u8; 32],
    relay_to_home: [u8; 32],
}

impl Keys {
    /// From the connection's exported secret and the relay key's SHA-256
    /// (which both ends know: home has the key, the relay keeps its hash).
    pub fn derive(exported: &Exported, key_hash: &[u8; 32]) -> Keys {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, key_hash).extract(exported);
        let take = |info: &[u8]| {
            let mut out = [0u8; 32];
            prk.expand(&[info], hkdf::HKDF_SHA256).expect("32 bytes").fill(&mut out).expect("32 bytes");
            out
        };
        let id = take(b"hopper voice session");
        Keys { session: u32::from_be_bytes(id[..4].try_into().expect("4")), home_to_relay: take(b"hopper voice home to relay"), relay_to_home: take(b"hopper voice relay to home") }
    }

    /// Home's ends: it seals what goes to the relay, opens what comes back.
    pub fn home(&self) -> (Sealer, Opener) {
        (Sealer::new(&self.home_to_relay, self.session), Opener::new(&self.relay_to_home, self.session))
    }

    pub fn relay(&self) -> (Sealer, Opener) {
        (Sealer::new(&self.relay_to_home, self.session), Opener::new(&self.home_to_relay, self.session))
    }
}

fn key(k: &[u8; 32]) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&aead::CHACHA20_POLY1305, k).expect("32-byte key"))
}

fn nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::assume_unique_for_key(n)
}

/// Seals one direction's packets. Its counter only goes up, so no nonce is
/// ever used twice with its key.
pub struct Sealer {
    key: LessSafeKey,
    session: u32,
    next: u64,
}

impl Sealer {
    fn new(k: &[u8; 32], session: u32) -> Sealer {
        Sealer { key: key(k), session, next: 0 }
    }

    pub fn seal(&mut self, kind: Kind, body: &[u8]) -> Vec<u8> {
        let n = self.next;
        self.next += 1;
        let mut out = Vec::with_capacity(HEADER + body.len() + TAG);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.push(kind as u8);
        out.extend_from_slice(&self.session.to_be_bytes());
        out.extend_from_slice(&n.to_be_bytes());
        let mut sealed = body.to_vec();
        let header: [u8; HEADER] = out[..HEADER].try_into().expect("header");
        self.key.seal_in_place_append_tag(nonce(n), Aad::from(header), &mut sealed).expect("seal");
        out.extend_from_slice(&sealed);
        out
    }

    /// A voice packet for `flow`.
    pub fn data(&mut self, flow: u32, packet: &[u8]) -> Vec<u8> {
        let mut body = Vec::with_capacity(4 + packet.len());
        body.extend_from_slice(&flow.to_be_bytes());
        body.extend_from_slice(packet);
        self.seal(Kind::Data, &body)
    }
}

/// Why a sealed packet was dropped (counted, never answered).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// Not ours, cut short, too big, another version or kind.
    Garbage,
    /// Another connection's (an old session's keys, or a guess).
    Session,
    /// A counter already seen, or too far behind.
    Replay,
    /// The tag didn't match: changed on the way, or not from the other end.
    Forged,
}

/// Opens the other direction's packets.
pub struct Opener {
    key: LessSafeKey,
    session: u32,
    seen: Replay,
}

impl Opener {
    fn new(k: &[u8; 32], session: u32) -> Opener {
        Opener { key: key(k), session, seen: Replay::default() }
    }

    /// Checks and opens `pkt`: its kind, its body and its counter. Cheap
    /// checks first (length, magic, session, replay window), the tag last;
    /// the counter is only taken as seen once the tag matched, so a forged
    /// packet can't block a real one.
    pub fn open(&mut self, pkt: &[u8]) -> Result<(Kind, Vec<u8>, u64), Rejected> {
        let h = header(pkt).ok_or(Rejected::Garbage)?;
        if h.session != self.session {
            return Err(Rejected::Session);
        }
        if !self.seen.fresh(h.counter) {
            return Err(Rejected::Replay);
        }
        let mut body = pkt[HEADER..].to_vec();
        let aad: [u8; HEADER] = pkt[..HEADER].try_into().expect("header");
        let plain = self.key.open_in_place(nonce(h.counter), Aad::from(aad), &mut body).map_err(|_| Rejected::Forged)?;
        let len = plain.len();
        body.truncate(len);
        self.seen.mark(h.counter);
        Ok((h.kind, body, h.counter))
    }

    /// The cheap part of [`Opener::open`] without the tag: whether `pkt`
    /// could be genuine. The relay asks this before spending its shared
    /// budget of tag checks, so junk with a wrong session never touches it.
    pub fn plausible(&self, pkt: &[u8]) -> Result<(), Rejected> {
        let h = header(pkt).ok_or(Rejected::Garbage)?;
        if h.session != self.session {
            return Err(Rejected::Session);
        }
        if !self.seen.fresh(h.counter) {
            return Err(Rejected::Replay);
        }
        Ok(())
    }

    /// The newest counter opened so far.
    pub fn newest(&self) -> Option<u64> {
        self.seen.any.then_some(self.seen.max)
    }
}

pub struct Header {
    pub kind: Kind,
    pub session: u32,
    pub counter: u64,
}

/// The clear part of a sealed packet, if `pkt` is one (it says nothing
/// about whether it's genuine).
pub fn header(pkt: &[u8]) -> Option<Header> {
    if pkt.len() < HEADER + TAG || pkt.len() > MAX_SEALED || &pkt[..3] != MAGIC || pkt[3] != VERSION {
        return None;
    }
    Some(Header {
        kind: Kind::from(pkt[4])?,
        session: u32::from_be_bytes(pkt[5..9].try_into().ok()?),
        counter: u64::from_be_bytes(pkt[9..17].try_into().ok()?),
    })
}

/// A sealed packet's first bytes (so the relay sends it to the check, not
/// on as a player's).
pub fn is_sealed(pkt: &[u8]) -> bool {
    pkt.len() >= 4 && &pkt[..3] == MAGIC
}

/// A voice body: flow and packet.
pub fn split_data(body: &[u8]) -> Option<(u32, &[u8])> {
    if body.len() < 4 || body.len() > 4 + MAX_PACKET {
        return None;
    }
    Some((u32::from_be_bytes(body[..4].try_into().ok()?), &body[4..]))
}

/// The counters seen, as a sliding window of 64 (like WireGuard's): a
/// counter newer than any seen is fresh; an older one is fresh only once
/// and only within the window.
#[derive(Default)]
struct Replay {
    max: u64,
    bits: u64,
    any: bool,
}

impl Replay {
    fn fresh(&self, n: u64) -> bool {
        if !self.any || n > self.max {
            return true;
        }
        let d = self.max - n;
        d < 64 && self.bits & (1u64 << d) == 0
    }

    fn mark(&mut self, n: u64) {
        if !self.any {
            self.any = true;
            self.max = n;
            self.bits = 1;
        } else if n > self.max {
            let s = n - self.max;
            self.bits = if s >= 64 { 0 } else { self.bits << s };
            self.bits |= 1;
            self.max = n;
        } else {
            self.bits |= 1u64 << (self.max - n);
        }
    }
}

/// A player's packet as the voice mods send them to the server: Simple
/// Voice Chat's start with 0xFF and the 16-byte connection id; Plasmo
/// Voice's with its magic number, a kind and a 16-byte secret. Anything else
/// (scanners, garbage, too big) is dropped before it costs more.
pub fn player_packet_ok(p: &[u8]) -> bool {
    if p.len() > MAX_PACKET {
        return false;
    }
    let svc = p.len() >= 18 && p[0] == 0xFF;
    let plasmo = p.len() >= 4 + 1 + 16 + 8 && p[..4] == [0x4E, 0x90, 0x04, 0xE9];
    svc || plasmo
}

/// Packets per second for one player, with a burst.
pub struct Bucket {
    tokens: f64,
    rate: f64,
    burst: f64,
    last: Instant,
}

impl Bucket {
    pub fn new(rate: f64, burst: f64) -> Bucket {
        Bucket { tokens: burst, rate, burst, last: Instant::now() }
    }

    pub fn take(&mut self) -> bool {
        self.take_at(Instant::now())
    }

    pub fn take_at(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// From a player: one Opus frame every 20 ms plus keepalives and pings is
/// about 55 a second; twice that, with a second of burst.
pub fn player_bucket() -> Bucket {
    Bucket::new(120.0, 240.0)
}

/// To a player: everyone they can hear (each one about 50 a second).
pub fn listener_bucket() -> Bucket {
    Bucket::new(1000.0, 2000.0)
}

// ---- the TCP way: frames on the "voice" mux stream ---------------------------------------

/// One packet on the voice stream: `u16 length` (of flow and packet), the
/// flow, the packet.
pub fn frame(flow: u32, packet: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(6 + packet.len());
    f.extend_from_slice(&((4 + packet.len()) as u16).to_be_bytes());
    f.extend_from_slice(&flow.to_be_bytes());
    f.extend_from_slice(packet);
    f
}

/// Cuts the voice stream into packets. An impossible length ends the
/// stream (the other end isn't speaking this).
#[derive(Default)]
pub struct Frames(Vec<u8>);

impl Frames {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, ()> {
        self.0.extend_from_slice(chunk);
        let mut out = Vec::new();
        let mut off = 0;
        while self.0.len() - off >= 2 {
            let len = u16::from_be_bytes([self.0[off], self.0[off + 1]]) as usize;
            if !(4..=4 + MAX_PACKET).contains(&len) {
                return Err(());
            }
            if self.0.len() - off - 2 < len {
                break;
            }
            let b = &self.0[off + 2..off + 2 + len];
            out.push((u32::from_be_bytes(b[..4].try_into().expect("4")), b[4..].to_vec()));
            off += 2 + len;
        }
        self.0.drain(..off);
        Ok(out)
    }
}

/// Unix milliseconds, for the keepalive's round trip.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests;
