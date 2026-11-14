//! What a server's bytes would have cost without the relay.
//!
//! Behind the relay the server runs with `network-compression-threshold=-1`
//! (the tunnel compresses better than per-packet zlib, and Velocity would
//! only undo it). To show an honest "saved" number, each game connection's
//! server-to-player bytes are parsed into packets and each one is priced the
//! way vanilla would send it with the default threshold: packets of 256
//! bytes or more zlib-compressed at level 6, smaller ones as they are. Same
//! rule as `mctunnel`'s `mc_zlib256`.
//!
//! One estimate per connection (packet framing is per connection), fed from
//! the server side before the bytes go into the tunnel.

use flate2::{Compress, Compression, FlushCompress, Status};

/// Vanilla's default `network-compression-threshold`.
const THRESHOLD: usize = 256;
/// The biggest packet vanilla accepts (2^21 - 1 bytes); anything claiming
/// more means we've lost the framing.
const MAX_PACKET: usize = (1 << 21) - 1;

pub struct VanillaEstimate {
    buf: Vec<u8>,
    /// Framing was lost or the server compresses itself: from here on its
    /// bytes count as they are.
    passthrough: bool,
    packets: u64,
    raw: u64,
    vanilla: u64,
    z: Compress,
    scratch: Box<[u8]>,
}

impl Default for VanillaEstimate {
    fn default() -> Self {
        Self::new()
    }
}

fn varint(b: &[u8]) -> Option<Result<(usize, usize), ()>> {
    let mut val = 0usize;
    for i in 0..5 {
        let &x = b.get(i)?;
        val |= usize::from(x & 0x7f) << (7 * i);
        if x & 0x80 == 0 {
            return Some(Ok((val, i + 1)));
        }
    }
    Some(Err(()))
}

fn varint_len(mut v: usize) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

impl VanillaEstimate {
    pub fn new() -> Self {
        VanillaEstimate {
            buf: Vec::new(),
            passthrough: false,
            packets: 0,
            raw: 0,
            vanilla: 0,
            z: Compress::new(Compression::new(6), true),
            scratch: vec![0u8; 32 << 10].into_boxed_slice(),
        }
    }

    /// Bytes fed so far.
    pub fn raw(&self) -> u64 {
        self.raw
    }

    /// What vanilla would have sent for them (whole packets only; a packet
    /// still arriving isn't counted yet).
    pub fn vanilla(&self) -> u64 {
        self.vanilla
    }

    /// Feeds the next bytes the server sent on this connection.
    pub fn feed(&mut self, data: &[u8]) {
        self.raw += data.len() as u64;
        if self.passthrough {
            self.vanilla += data.len() as u64;
            return;
        }
        self.buf.extend_from_slice(data);
        let mut off = 0;
        loop {
            let (n, k) = match varint(&self.buf[off..]) {
                None => break,
                Some(Ok((n, k))) if n > 0 && n <= MAX_PACKET => (n, k),
                Some(_) => {
                    self.give_up(off);
                    return;
                }
            };
            if self.buf.len() - off < k + n {
                break;
            }
            let start = off + k;
            self.packets += 1;
            // Login "Set Compression" (id 0x03, tiny) early on: the server
            // compresses itself, so its bytes already are what vanilla sends.
            if self.packets <= 6 && self.buf[start] == 0x03 && n <= 6 {
                self.give_up(off);
                return;
            }
            let body = if n >= THRESHOLD {
                varint_len(n) + self.zlib_len(start, n)
            } else {
                1 + n
            };
            self.vanilla += (varint_len(body) + body) as u64;
            off = start + n;
        }
        self.buf.drain(..off);
    }

    fn give_up(&mut self, off: usize) {
        self.vanilla += (self.buf.len() - off) as u64;
        self.passthrough = true;
        self.buf = Vec::new();
    }

    fn zlib_len(&mut self, start: usize, n: usize) -> usize {
        self.z.reset();
        let pkt = &self.buf[start..start + n];
        let before_in = self.z.total_in();
        loop {
            let done = (self.z.total_in() - before_in) as usize;
            match self.z.compress(&pkt[done..], &mut self.scratch, FlushCompress::Finish) {
                Ok(Status::StreamEnd) => break,
                Ok(_) => {}
                // Can't happen with valid buffers; count the packet as is.
                Err(_) => return n,
            }
        }
        self.z.total_out() as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(id: u8, body: &[u8]) -> Vec<u8> {
        let mut p = Vec::new();
        let n = body.len() + 1;
        let mut v = n;
        while v >= 0x80 {
            p.push(v as u8 | 0x80);
            v >>= 7;
        }
        p.push(v as u8);
        p.push(id);
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn small_packets_cost_one_byte_more() {
        let mut e = VanillaEstimate::new();
        e.feed(&packet(0x10, &[1, 2, 3]));
        // len varint (1) + "uncompressed" marker (1) + 4 bytes.
        assert_eq!(e.vanilla(), 6);
        assert_eq!(e.raw(), 5);
    }

    #[test]
    fn big_packets_are_zlibbed_and_split_reads_count_once() {
        let mut e = VanillaEstimate::new();
        let p = packet(0x27, &vec![7u8; 10_000]);
        for c in p.chunks(333) {
            e.feed(c);
        }
        assert!(e.vanilla() > 10 && e.vanilla() < 200, "{}", e.vanilla());
        assert_eq!(e.raw(), p.len() as u64);
    }

    #[test]
    fn server_side_compression_counts_bytes_as_they_are() {
        let mut e = VanillaEstimate::new();
        e.feed(&packet(0x03, &[0x80, 0x02]));
        e.feed(&[9u8; 500]);
        assert_eq!(e.vanilla(), 4 + 500);
    }

    #[test]
    fn lost_framing_counts_the_rest_as_is() {
        let mut e = VanillaEstimate::new();
        e.feed(&[0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 1, 2]);
        assert_eq!(e.vanilla(), 8);
        e.feed(&[1, 2, 3]);
        assert_eq!(e.vanilla(), 11);
    }
}
