use super::dedupe::{MAX_PIECE, MIN_REF, TOK_LIT, TOK_RAW, TOK_REF};
use super::*;

/// Small, fast settings: the tests run many encoders at once, and a 256 MB
/// window each would be a lot of memory for nothing.
fn cfg(cache: u64) -> CodecConfig {
    CodecConfig { level: 3, window_log: 22, long_distance: false, cache_bytes: cache }
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

/// Encodes `writes` (flushing where `flush_after` says), decodes the result
/// in odd-sized pushes, and checks the bytes and both caches match.
fn round_trip(c: &CodecConfig, writes: &[Vec<u8>], flush_every: usize, rng: &mut Rng) -> (Encoder, Decoder) {
    let mut enc = Encoder::new(c).unwrap();
    let mut dec = Decoder::new(c).unwrap();
    let mut want = Vec::new();
    let mut got = Vec::new();
    for (i, w) in writes.iter().enumerate() {
        // Odd-sized slices, like reads off a socket.
        let mut rest = &w[..];
        while !rest.is_empty() {
            let k = rest.len().min(1 + rng.below(20_000));
            enc.write(&rest[..k]).unwrap();
            rest = &rest[k..];
        }
        want.extend_from_slice(w);
        if i % flush_every == 0 || i + 1 == writes.len() {
            enc.flush().unwrap();
            let wire = enc.take_output();
            let mut rest = &wire[..];
            while !rest.is_empty() {
                let k = rest.len().min(1 + rng.below(5_000));
                dec.decode(&rest[..k], &mut got).unwrap();
                rest = &rest[k..];
            }
            // A flush makes everything so far decodable on its own.
            assert_eq!(got.len(), want.len(), "after flush {i}");
            assert!(dec.at_boundary());
            assert_eq!(enc.cache_digest(), dec.cache_digest(), "caches differ after write {i}");
        }
    }
    assert!(got == want, "output differs");
    assert_eq!(enc.stats().raw, want.len() as u64);
    assert_eq!(dec.stats().raw, want.len() as u64);
    assert_eq!(enc.stats().wire, dec.stats().wire);
    assert_eq!(enc.stats().hits, dec.stats().hits);
    assert_eq!(enc.stats().cache_used, dec.stats().cache_used);
    (enc, dec)
}

fn blocks(rng: &mut Rng, n: usize) -> Vec<Vec<u8>> {
    (0..n).map(|_| { let len = 2000 + rng.below(90_000); rng.bytes(len) }).collect()
}

#[test]
fn round_trip_random_data_every_cache_size() {
    let mut rng = Rng(0x1234_5678);
    let writes: Vec<Vec<u8>> = (0..60).map(|_| { let n = rng.below(70_000); rng.bytes(n) }).collect();
    for cache in [0, CodecConfig::MIN_CACHE, 64 * MB] {
        round_trip(&cfg(cache), &writes, 3, &mut rng);
    }
}

#[test]
fn repeats_go_as_references() {
    let mut rng = Rng(42);
    let pool = blocks(&mut rng, 40);
    let writes: Vec<Vec<u8>> = (0..600).map(|_| pool[rng.below(pool.len())].clone()).collect();
    let raw: usize = writes.iter().map(|w| w.len()).sum();
    // A small zstd window, so it's the cache that finds the repeats.
    let c = CodecConfig { window_log: 16, ..cfg(64 * MB) };
    let (enc, _) = round_trip(&c, &writes, 2, &mut rng);
    let s = enc.stats();
    assert!(s.wire < raw as u64 / 5, "big cache should drop most repeats: {} of {raw}", s.wire);
    assert!(s.hit_bytes > raw as u64 / 2, "{s:?}");
    // The same without the cache: random blocks don't compress.
    let (plain, _) = round_trip(&CodecConfig { window_log: 16, ..cfg(0) }, &writes, 2, &mut rng);
    assert!(plain.stats().wire > raw as u64 * 9 / 10);
}

#[test]
fn shifted_copy_is_found_after_the_cut_points_resync() {
    // Another player's stream: the same bytes behind different packet
    // boundaries.
    let mut rng = Rng(7);
    let data = rng.bytes(1 << 20);
    let c = CodecConfig { window_log: 16, ..cfg(64 * MB) };
    let mut enc = Encoder::new(&c).unwrap();
    enc.write(&data).unwrap();
    enc.flush().unwrap();
    let first = enc.take_output().len();
    enc.write(b"some other header bytes").unwrap();
    enc.write(&data).unwrap();
    enc.flush().unwrap();
    let second = enc.take_output().len();
    assert!(first > data.len() * 9 / 10);
    assert!(second < data.len() / 10, "shifted repeat cost {second} bytes");
}

#[test]
fn caches_stay_identical_through_long_runs_and_evictions() {
    let mut rng = Rng(99);
    let pool = blocks(&mut rng, 120);
    // About 30 MB through a 1 MB cache: it evicts all the time, and hot
    // pieces get moved again and again.
    let writes: Vec<Vec<u8>> = (0..600)
        .map(|_| {
            // Mostly a few hot blocks, sometimes a cold one.
            let i = if rng.below(4) == 0 { rng.below(pool.len()) } else { rng.below(6) };
            pool[i].clone()
        })
        .collect();
    let (enc, dec) = round_trip(&cfg(CodecConfig::MIN_CACHE), &writes, 1, &mut rng);
    let s = enc.stats();
    assert!(s.cache_used <= CodecConfig::MIN_CACHE, "{s:?}");
    assert!(s.hits > 100, "hot pieces should stay cached: {s:?}");
    assert_eq!(enc.cache_digest(), dec.cache_digest());
}

#[test]
fn memory_cap_is_respected() {
    let mut rng = Rng(5);
    let cap = 4 * MB;
    let mut enc = Encoder::new(&cfg(cap)).unwrap();
    let mut dec = Decoder::new(&cfg(cap)).unwrap();
    let mut out = Vec::new();
    let mut peak = 0;
    for _ in 0..400 {
        let n = 10_000 + rng.below(60_000);
        enc.write(&rng.bytes(n)).unwrap();
        enc.flush().unwrap();
        out.clear();
        dec.decode(&enc.take_output(), &mut out).unwrap();
        peak = peak.max(enc.stats().cache_used).max(dec.stats().cache_used);
    }
    assert!(peak <= cap, "cache grew to {peak}");
    assert!(peak > cap / 2, "cache should fill up: {peak}");
    // zstd state for a 4 MB window, the 4 MB cache and buffers: well under
    // 64 MB on each side.
    assert!(enc.memory() < 64 * MB, "{}", enc.memory());
    assert!(dec.memory() < 64 * MB, "{}", dec.memory());
}

#[test]
fn small_and_full_settings() {
    assert_eq!(CodecConfig::for_memory(2 * GB).cache_bytes, 512 * MB);
    assert_eq!(CodecConfig::for_memory(4 * GB - 1), CodecConfig::small());
    assert_eq!(CodecConfig::for_memory(4 * GB), CodecConfig::full());
    assert_eq!(CodecConfig::full().cache_bytes, GB);
    assert_eq!(CodecConfig::full().window_log, 28);
    for c in [CodecConfig::small(), CodecConfig::full()] {
        c.validate().unwrap();
        // The real settings work end to end (one short run: the 256 MB
        // window costs real memory).
        let mut rng = Rng(11);
        let pool = blocks(&mut rng, 4);
        let writes: Vec<Vec<u8>> = (0..12).map(|i| pool[i % 4].clone()).collect();
        let (enc, dec) = round_trip(&c, &writes, 2, &mut rng);
        assert!(enc.stats().hits > 0);
        // The 256 MB window is reserved on the receiving side.
        assert!(dec.memory() > 200 * MB, "{}", dec.memory());
    }
}

#[test]
fn bad_settings_are_refused() {
    for c in [
        CodecConfig { level: 0, ..cfg(0) },
        CodecConfig { level: 23, ..cfg(0) },
        CodecConfig { window_log: 29, ..cfg(0) },
        CodecConfig { window_log: 9, ..cfg(0) },
        CodecConfig { cache_bytes: 1000, ..cfg(0) },
        CodecConfig { cache_bytes: 5 * GB, ..cfg(0) },
    ] {
        assert!(matches!(Decoder::new(&c), Err(CodecError::Config(_))), "{c:?}");
        assert!(matches!(Encoder::new(&c), Err(CodecError::Config(_))), "{c:?}");
    }
}

#[test]
fn a_window_bigger_than_agreed_is_refused() {
    let mut enc = Encoder::new(&CodecConfig { window_log: 24, ..cfg(0) }).unwrap();
    enc.write(&[1u8; 1000]).unwrap();
    enc.flush().unwrap();
    let mut dec = Decoder::new(&CodecConfig { window_log: 20, ..cfg(0) }).unwrap();
    let r = dec.decode(&enc.take_output(), &mut Vec::new());
    assert!(matches!(r, Err(CodecError::Zstd(_))), "{r:?}");
}

/// A hand-made token stream, zstd-compressed (an encoder without a cache
/// passes bytes through as they are).
fn tokens(raw: &[u8]) -> Vec<u8> {
    let mut enc = Encoder::new(&cfg(0)).unwrap();
    enc.write(raw).unwrap();
    enc.flush().unwrap();
    enc.take_output()
}

fn decode_tokens(raw: &[u8]) -> (Result<(), CodecError>, Vec<u8>, Decoder) {
    let mut dec = Decoder::new(&cfg(CodecConfig::MIN_CACHE)).unwrap();
    let mut out = Vec::new();
    let r = dec.decode(&tokens(raw), &mut out);
    (r, out, dec)
}

#[test]
fn reference_to_an_unknown_piece_fails_and_stays_failed() {
    let mut t = vec![TOK_LIT, 0x80, 0x02]; // 256 bytes, cached as id 0
    t.extend_from_slice(&[9u8; MIN_REF]);
    t.extend_from_slice(&[TOK_REF, 0]); // fine: id 0 moves to id 1
    t.extend_from_slice(&[TOK_REF, 0]); // id 0 is gone now
    let (r, out, mut dec) = decode_tokens(&t);
    assert_eq!(r, Err(CodecError::OutOfStep(0)));
    assert_eq!(out.len(), 2 * MIN_REF);
    assert_eq!(dec.read(&mut [0u8; 16]), Err(CodecError::OutOfStep(0)));
}

#[test]
fn bad_tokens_and_lengths_fail_before_allocating() {
    let cases: Vec<Vec<u8>> = vec![
        vec![7, 1, 0],                         // unknown token
        vec![TOK_LIT, 0x10],                   // cached literal shorter than MIN_REF
        vec![TOK_RAW, 0x80, 0x02],             // uncached literal of MIN_REF bytes
        vec![TOK_RAW, 0],                      // empty literal
        vec![TOK_LIT, 0x81, 0x80, 0x04],       // 65 537 bytes: over MAX_PIECE
        vec![TOK_LIT, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f], // u64 overflow
        vec![TOK_REF, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01], // too long
    ];
    for t in cases {
        let (r, out, _) = decode_tokens(&t);
        assert!(matches!(r, Err(CodecError::Corrupt(_))), "{t:?}: {r:?}");
        assert!(out.is_empty());
    }
    // The longest allowed literal is fine.
    let mut t = vec![TOK_LIT, 0x80, 0x80, 0x04];
    t.extend_from_slice(&vec![1u8; MAX_PIECE]);
    let (r, out, _) = decode_tokens(&t);
    r.unwrap();
    assert_eq!(out.len(), MAX_PIECE);
}

#[test]
fn a_stream_cut_mid_piece_is_not_at_a_boundary() {
    let mut t = vec![TOK_LIT, 0x80, 0x04]; // 512 bytes promised
    t.extend_from_slice(&[3u8; 100]);
    let (r, out, dec) = decode_tokens(&t);
    r.unwrap();
    assert!(out.is_empty());
    assert!(!dec.at_boundary());
}

#[test]
fn caches_out_of_step_fail_instead_of_giving_wrong_bytes() {
    // The receiver's cache is smaller than the sender's: it drops pieces
    // the sender still refers to.
    let mut rng = Rng(3);
    let pool = blocks(&mut rng, 60);
    let mut enc = Encoder::new(&cfg(64 * MB)).unwrap();
    let mut dec = Decoder::new(&cfg(CodecConfig::MIN_CACHE)).unwrap();
    let mut want = Vec::new();
    let mut got = Vec::new();
    let mut err = None;
    for i in 0..300 {
        let b = &pool[(i * 7) % pool.len()];
        enc.write(b).unwrap();
        want.extend_from_slice(b);
        enc.flush().unwrap();
        if let Err(e) = dec.decode(&enc.take_output(), &mut got) {
            err = Some(e);
            break;
        }
    }
    assert!(matches!(err, Some(CodecError::OutOfStep(_))), "{err:?}");
    // Whatever came out before the error is right.
    assert!(got.len() <= want.len() && got == want[..got.len()]);
}

#[test]
fn garbage_and_damaged_streams_never_panic() {
    let mut rng = Rng(0xdead_beef);
    let pool = blocks(&mut rng, 8);
    let mut enc = Encoder::new(&cfg(8 * MB)).unwrap();
    let mut good = Vec::new();
    for i in 0..40 {
        enc.write(&pool[i % pool.len()]).unwrap();
        enc.flush().unwrap();
        good.extend_from_slice(&enc.take_output());
    }
    for round in 0..300 {
        let mut wire = if round % 3 == 0 {
            let n = rng.below(100_000);
            rng.bytes(n)
        } else {
            good.clone()
        };
        if round % 3 != 0 {
            for _ in 0..1 + rng.below(8) {
                let i = rng.below(wire.len());
                wire[i] ^= 1 << rng.below(8);
            }
            if rng.below(4) == 0 {
                wire.truncate(rng.below(wire.len()));
            }
        }
        let mut dec = Decoder::new(&cfg(8 * MB)).unwrap();
        let mut buf = vec![0u8; 50_000];
        let mut produced = 0usize;
        // Pushed in pieces, read until it stops or fails.
        'feed: for part in wire.chunks(1 + rng.below(30_000)) {
            dec.push(part);
            loop {
                match dec.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => produced += n,
                    Err(_) => break 'feed,
                }
            }
        }
        // Bounded: never more than the window, the cache and its buffers.
        assert!(dec.memory() < 64 * MB, "round {round}: {}", dec.memory());
        assert!(dec.stats().cache_used <= 8 * MB);
        let _ = produced;
    }
}

#[test]
fn encoder_output_is_stable_for_empty_flushes() {
    let mut enc = Encoder::new(&cfg(MB)).unwrap();
    enc.flush().unwrap();
    enc.flush().unwrap();
    let wire = enc.take_output();
    let mut dec = Decoder::new(&cfg(MB)).unwrap();
    let mut out = Vec::new();
    dec.decode(&wire, &mut out).unwrap();
    assert!(out.is_empty());
    assert!(dec.at_boundary());
}
