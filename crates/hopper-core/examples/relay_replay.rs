//! Replays a recorded tunnel stream through the relay codec and prints what
//! it would have sent: `cargo run --profile fast -p hopper-core --example
//! relay_replay -- [options] rec-*.bin`.
//!
//! Recordings are `mctunnel -record` files: one record per server read,
//! `[i64 unix nanos][u32 len][bytes]`, little-endian, one file per player
//! connection, the server-to-player bytes with compression off
//! (`network-compression-threshold=-1`). Several files are merged by time
//! into one stream, as the tunnel would carry them.
//!
//! Without files, `--synthetic` makes a stream of Minecraft-like packets
//! instead (players walking the same path some seconds apart, with a big
//! modded login message, chunks, machines and mobs). Its numbers only show
//! the codec works and how the settings compare; they say nothing about a
//! real pack.
//!
//! Options:
//!   --level N          zstd level (default 11)
//!   --window LOG       zstd window as a power of two (default 28 = 256 MB)
//!   --caches MB,MB     piece cache sizes to try, 0 = off (default 0,512,1024)
//!   --ldm / --no-ldm   zstd long distance matching on or off (default off)
//!   --synthetic P,M,G  P players, M minutes each, G seconds apart (default 2,5,30)
//!   --save FILE        with --synthetic: also write the stream as a recording

use hopper_core::hosting::relay_codec::{CodecConfig, Decoder, Encoder, VanillaEstimate};
use std::io::{BufReader, BufWriter, Read, Write};
use std::time::{Duration, Instant};

struct Rec {
    t: i64,
    conn: usize,
    data: Vec<u8>,
}

fn read_records(path: &str, conn: usize, out: &mut Vec<Rec>) {
    let f = std::fs::File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut r = BufReader::with_capacity(1 << 20, f);
    loop {
        let mut hdr = [0u8; 12];
        if r.read_exact(&mut hdr).is_err() {
            return;
        }
        let n = u32::from_le_bytes(hdr[8..].try_into().unwrap()) as usize;
        let mut data = vec![0u8; n];
        if r.read_exact(&mut data).is_err() {
            return;
        }
        out.push(Rec { t: i64::from_le_bytes(hdr[..8].try_into().unwrap()), conn, data });
    }
}

fn human(n: f64) -> String {
    if n >= (1u64 << 30) as f64 {
        format!("{:.2} GB", n / (1u64 << 30) as f64)
    } else if n >= (1 << 20) as f64 {
        format!("{:.1} MB", n / (1 << 20) as f64)
    } else if n >= 1024.0 {
        format!("{:.1} KB", n / 1024.0)
    } else {
        format!("{n:.0} B")
    }
}

fn pct(a: u64, b: u64) -> f64 {
    if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 }
}

fn rss(sys: &mut sysinfo::System) -> u64 {
    let pid = sysinfo::get_current_pid().unwrap();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map_or(0, |p| p.memory())
}

fn mbps(bytes: u64, d: Duration) -> f64 {
    bytes as f64 / (1 << 20) as f64 / d.as_secs_f64().max(1e-9)
}

struct Run {
    wire: u64,
    hits: u64,
    hit_bytes: u64,
    enc_time: Duration,
    dec_time: Duration,
    peak_codec: u64,
    peak_rss: u64,
}

/// Encodes the records as one stream, flushing after each record (as the
/// tunnel does after each burst), and decodes it again to prove the bytes
/// come back.
fn run(recs: &[&Rec], cfg: &CodecConfig, sys: &mut sysinfo::System) -> Run {
    let mut enc = Encoder::new(cfg).unwrap();
    let mut dec = Decoder::new(cfg).unwrap();
    let (mut enc_time, mut dec_time) = (Duration::ZERO, Duration::ZERO);
    let mut buf = vec![0u8; 256 << 10];
    let (mut peak_codec, mut peak_rss) = (0u64, 0u64);
    for (i, r) in recs.iter().enumerate() {
        let t = Instant::now();
        enc.write(&r.data).unwrap();
        enc.flush().unwrap();
        let wire = enc.take_output();
        enc_time += t.elapsed();
        let t = Instant::now();
        dec.push(&wire);
        let mut got = 0;
        loop {
            let n = dec.read(&mut buf).expect("decode failed");
            if n == 0 {
                break;
            }
            assert_eq!(buf[..n], r.data[got..got + n], "decoded bytes differ in record {i}");
            got += n;
        }
        dec_time += t.elapsed();
        assert_eq!(got, r.data.len(), "record {i} came back short");
        if i % 512 == 0 || i + 1 == recs.len() {
            peak_codec = peak_codec.max(enc.memory() + dec.memory());
            peak_rss = peak_rss.max(rss(sys));
        }
    }
    assert_eq!(enc.cache_digest(), dec.cache_digest(), "caches differ at the end");
    let s = enc.stats();
    Run { wire: s.wire, hits: s.hits, hit_bytes: s.hit_bytes, enc_time, dec_time, peak_codec, peak_rss }
}

fn main() {
    let mut level = 11;
    let mut window = 28;
    let mut ldm = CodecConfig::full().long_distance;
    let mut caches = vec![0u64, 512, 1024];
    let mut synthetic: Option<(usize, u32, u32)> = None;
    let mut save = None;
    let mut files = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--level" => level = args.next().unwrap().parse().unwrap(),
            "--window" => window = args.next().unwrap().parse().unwrap(),
            "--no-ldm" => ldm = false,
            "--ldm" => ldm = true,
            "--caches" => caches = args.next().unwrap().split(',').map(|s| s.parse().unwrap()).collect(),
            "--save" => save = args.next(),
            "--synthetic" => {
                let v: Vec<u32> = args.next().unwrap_or_default().split(',').filter_map(|s| s.parse().ok()).collect();
                synthetic = Some((
                    *v.first().unwrap_or(&2) as usize,
                    *v.get(1).unwrap_or(&5),
                    *v.get(2).unwrap_or(&30),
                ));
            }
            _ => files.push(a),
        }
    }
    let mut recs = Vec::new();
    let conns;
    if let Some((players, minutes, gap)) = synthetic {
        recs = synth::stream(players, minutes, gap);
        conns = players;
        println!("SYNTHETIC stream: {players} players, {minutes} min each, {gap} s apart (not a real recording)");
        if let Some(p) = &save {
            let mut w = BufWriter::new(std::fs::File::create(p).unwrap());
            for r in &recs {
                w.write_all(&r.t.to_le_bytes()).unwrap();
                w.write_all(&(r.data.len() as u32).to_le_bytes()).unwrap();
                w.write_all(&r.data).unwrap();
            }
        }
    } else {
        if files.is_empty() {
            eprintln!("usage: relay_replay [--level N] [--window LOG] [--caches MB,..] [--ldm] rec-*.bin | --synthetic P,M,G");
            std::process::exit(2);
        }
        for (i, f) in files.iter().enumerate() {
            read_records(f, i, &mut recs);
        }
        conns = files.len();
    }
    recs.sort_by_key(|r| r.t);
    let raw: u64 = recs.iter().map(|r| r.data.len() as u64).sum();
    let span = recs.last().map_or(0, |l| l.t - recs[0].t);

    // Vanilla per-packet zlib, one estimate per connection.
    let mut est: Vec<VanillaEstimate> = (0..conns).map(|_| VanillaEstimate::new()).collect();
    for r in &recs {
        est[r.conn].feed(&r.data);
    }
    let vanilla: u64 = est.iter().map(|e| e.vanilla()).sum();
    println!(
        "{conns} connections, {} reads, {:.1} min, raw {}, vanilla zlib (threshold 256) {} ({:.1}% of raw)",
        recs.len(),
        span as f64 / 60e9,
        human(raw as f64),
        human(vanilla as f64),
        pct(vanilla, raw)
    );

    let mut sys = sysinfo::System::new();
    let base_rss = rss(&mut sys);
    println!("process memory before: {}", human(base_rss as f64));
    let all: Vec<&Rec> = recs.iter().collect();
    for &cache_mb in &caches {
        let cfg = CodecConfig { level, window_log: window, long_distance: ldm, cache_bytes: cache_mb << 20 };
        let r = run(&all, &cfg, &mut sys);
        println!(
            "level {level} window 2^{window}{} cache {cache_mb:>4} MB: sent {} ({:.2}% of raw, {:.1}% of vanilla = {:.1}x less), \
             cache hits {} pieces / {} ({:.1}% of raw), encode {:.0} MB/s, decode {:.0} MB/s, \
             codec memory peak {} (both ends), process peak {}",
            if ldm { " ldm" } else { "" },
            human(r.wire as f64),
            pct(r.wire, raw),
            pct(r.wire, vanilla),
            vanilla as f64 / r.wire.max(1) as f64,
            r.hits,
            human(r.hit_bytes as f64),
            pct(r.hit_bytes, raw),
            mbps(raw, r.enc_time),
            mbps(raw, r.dec_time),
            human(r.peak_codec as f64),
            human(r.peak_rss as f64),
        );
        if conns > 1 {
            // Each connection compressed on its own, for comparison: what
            // one shared stream gains over a tunnel per player.
            let mut sep = 0;
            for c in 0..conns {
                let mine: Vec<&Rec> = recs.iter().filter(|r| r.conn == c).collect();
                sep += run(&mine, &cfg, &mut sys).wire;
            }
            println!("    separate streams: {} ({:.1}% of vanilla)", human(sep as f64), pct(sep, vanilla));
        }
    }
}

/// A made-up but Minecraft-shaped server-to-player stream (packets framed
/// as with compression off: varint length, varint id, body).
mod synth {
    use super::Rec;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn hash(a: i64, b: i64, c: i64) -> u64 {
        let mut x = (a as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (b as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
            ^ (c as u64).wrapping_mul(0x1656_67b1_9e37_79f9);
        x ^= x >> 31;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^ (x >> 29)
    }

    fn varint(out: &mut Vec<u8>, mut v: u32) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn packet(out: &mut Vec<u8>, id: u8, body: &[u8]) {
        varint(out, body.len() as u32 + 1);
        out.push(id);
        out.extend_from_slice(body);
    }

    /// One chunk column: heightmaps, 24 sections of paletted blocks, light.
    /// The same coordinates always give the same bytes, like an unchanged
    /// part of the world.
    fn chunk(cx: i64, cz: i64) -> Vec<u8> {
        let mut b = Vec::with_capacity(40 << 10);
        b.extend_from_slice(&(cx as i32).to_be_bytes());
        b.extend_from_slice(&(cz as i32).to_be_bytes());
        let height = 64 + (hash(cx, 0, cz) % 24) as i64;
        // Heightmaps: 256 heights as 9-bit values in longs.
        for i in 0..37u64 {
            let h = (height as u64 + (hash(cx, i as i64, cz) % 3)) & 0x1ff;
            b.extend_from_slice(&(h * 0x0040_2010_0804_0201 & 0x7fff_ffff_ffff_ffff).to_be_bytes());
        }
        for s in 0..24i64 {
            let y0 = -64 + s * 16;
            if y0 > height {
                // All air: single-value palette.
                b.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            b.extend_from_slice(&4096u16.to_be_bytes());
            let palette = [1u8, 3, 4, 13, 14, 15, 16, 21, 73, 129];
            b.push(4);
            b.push(palette.len() as u8);
            b.extend_from_slice(&palette);
            // 4096 4-bit entries: mostly stone, some dirt and ores, caves.
            for i in 0..2048i64 {
                let h = hash(cx * 16 + i, y0, cz);
                let pick = |h: u64| -> u8 {
                    match h % 100 {
                        0..=79 => 0,
                        80..=89 => 1,
                        90..=95 => 2,
                        _ => (3 + (h >> 8) % 7) as u8,
                    }
                };
                b.push(pick(h) | pick(h >> 16) << 4);
            }
            // Block light (mostly dark) and sky light (bright near the top).
            for i in 0..2048i64 {
                let near_top = y0 + 16 > height;
                b.push(if near_top { if i % 64 < 40 { 0xff } else { 0xf7 } } else { 0 });
            }
        }
        b
    }

    /// A machine's block entity update: mostly the same NBT, with an energy
    /// and progress counter that change.
    fn machine(id: u64, tick: u64) -> Vec<u8> {
        let mut b = Vec::with_capacity(600);
        b.extend_from_slice(&(id as u32).to_be_bytes());
        b.extend_from_slice(b"\x0a\x00\x00\x08\x00\x02idmekanism:elite_smelting_factory\x0a\x00\x0bcomponents");
        for k in 0..10u64 {
            b.extend_from_slice(format!("\x03\x00\x06slot_{k:02}").as_bytes());
            b.extend_from_slice(&((id * 7 + k) as u32 % 64).to_be_bytes());
        }
        b.extend_from_slice(b"\x04\x00\x06energy");
        b.extend_from_slice(&(1_000_000 + (tick * (id % 5 + 1)) % 90_000).to_be_bytes());
        b.extend_from_slice(b"\x03\x00\x08progress");
        b.extend_from_slice(&((tick % 200) as u32).to_be_bytes());
        b.extend_from_slice(&[0u8; 120]);
        b.push(0);
        b
    }

    /// The modded login push (registries, recipes, tags): about 2 MB, the
    /// same for every player.
    fn login_blob() -> Vec<u8> {
        let mods = ["create", "mekanism", "ae2", "thermal", "botania", "ars_nouveau", "allthemodium", "mysticalagriculture"];
        let items = ["ingot", "block", "nugget", "dust", "plate", "gear", "rod", "ore", "raw", "machine", "cable", "seed"];
        let mut b = Vec::with_capacity(2 << 20);
        let mut i = 0u64;
        while b.len() < 2 << 20 {
            let m = mods[(hash(i as i64, 1, 2) % mods.len() as u64) as usize];
            let it = items[(hash(i as i64, 3, 4) % items.len() as u64) as usize];
            let name = format!("{m}:{it}_{:x}", hash(i as i64, 5, 6) % 4096);
            b.push(name.len() as u8);
            b.extend_from_slice(name.as_bytes());
            b.extend_from_slice(&(hash(i as i64, 7, 8) as u32).to_be_bytes());
            i += 1;
        }
        b
    }

    /// One player's connection, one record per tick.
    fn player(p: usize, minutes: u32, start_ns: i64) -> Vec<Rec> {
        let mut rng = Rng(0x5eed + p as u64 * 977);
        let mut out = Vec::new();
        let view = 8i64;
        let mut first = Vec::new();
        packet(&mut first, 0x01, &login_blob()[..1 << 20]);
        packet(&mut first, 0x01, &login_blob()[1 << 20..]);
        out.push(Rec { t: start_ns, conn: p, data: first });
        // Everyone walks the same path (a base and the route out of it) at
        // 5 blocks a second; what differs is when.
        let (mut x, mut z) = (0.0f64, 0.0f64);
        let mut sent = std::collections::HashSet::new();
        let ticks = minutes as u64 * 60 * 20;
        for tick in 0..ticks {
            let leg = tick / 600; // a turn every 30 s
            let (dx, dz) = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, 1.0)][(leg % 4) as usize];
            x += dx * 0.25;
            z += dz * 0.25;
            let (pcx, pcz) = ((x / 16.0).floor() as i64, (z / 16.0).floor() as i64);
            let mut d = Vec::new();
            for cx in pcx - view..=pcx + view {
                for cz in pcz - view..=pcz + view {
                    if sent.insert((cx, cz)) {
                        packet(&mut d, 0x27, &chunk(cx, cz));
                    }
                }
            }
            // The route runs through a big base: 160 machines in the chunks
            // around the player, each updating every 4 ticks. A modded base
            // is most of a real stream's raw bytes (about 40 MB a minute
            // per player in the ATM10 reference recordings).
            for m in 0..160u64 {
                if (tick + m) % 4 == 0 {
                    packet(&mut d, 0x07, &machine(hash(pcx, m as i64, pcz) % 100_000, tick));
                }
            }
            // Mobs and other players: small moves, different for everyone.
            for _ in 0..20 + rng.below(30) {
                let mut e = Vec::new();
                varint(&mut e, 1000 + rng.below(400) as u32);
                for _ in 0..6 {
                    e.push(rng.below(256) as u8);
                }
                e.push(1);
                packet(&mut d, 0x2f, &e);
            }
            if tick % 20 == 0 {
                packet(&mut d, 0x64, &(tick * 50).to_be_bytes());
            }
            if tick % 300 == 0 {
                packet(&mut d, 0x26, &rng.next().to_be_bytes());
            }
            out.push(Rec { t: start_ns + tick as i64 * 50_000_000, conn: p, data: d });
        }
        out
    }

    pub fn stream(players: usize, minutes: u32, gap_s: u32) -> Vec<Rec> {
        (0..players).flat_map(|p| player(p, minutes, p as i64 * gap_s as i64 * 1_000_000_000)).collect()
    }
}
