//! The lobby island: while a woken server starts, the router logs a
//! Minecraft 1.21 / 1.21.1 player (protocol 767) into a tiny world of its
//! own, shows the start's progress in a boss bar, and moves the player
//! over (a transfer) once the server is up. Other versions get the
//! "join again" message instead.
//!
//! This module is the packets, without sockets; hopperd runs the
//! connection. Packet ids, block states and the synced registries come
//! from Minecraft 1.21.1's own data reports (`--reports`), and the
//! registries' contents from the client's built-in data pack (the client
//! is told the entries; it has their data itself).

use super::router::{packet, read_string, read_varint, write_string, write_varint};
use anyhow::{Context as _, bail};
use serde_json::Value;
use std::collections::BTreeMap;

/// Minecraft 1.21 and 1.21.1.
pub const PROTOCOL: i32 = 767;
/// The version the built-in data pack is offered as.
const CORE_VERSION: &str = "1.21.1";

// Packet ids (1.21.1).
mod id {
    pub const LOGIN_SUCCESS: i32 = 0x02;
    pub const LOGIN_ACK: i32 = 0x03;
    pub const CONFIG_DISCONNECT: i32 = 0x02;
    pub const CONFIG_FINISH: i32 = 0x03;
    pub const CONFIG_REGISTRY: i32 = 0x07;
    pub const CONFIG_KNOWN_PACKS: i32 = 0x0e;
    pub const CONFIG_IN_PLUGIN: i32 = 0x02;
    pub const CONFIG_IN_FINISH: i32 = 0x03;
    pub const CONFIG_IN_KNOWN_PACKS: i32 = 0x07;
    pub const PLAY_LOGIN: i32 = 0x2b;
    pub const PLAY_GAME_EVENT: i32 = 0x22;
    pub const PLAY_CENTER: i32 = 0x54;
    pub const PLAY_CHUNK: i32 = 0x27;
    pub const PLAY_BATCH_START: i32 = 0x0d;
    pub const PLAY_BATCH_END: i32 = 0x0c;
    pub const PLAY_POSITION: i32 = 0x40;
    pub const PLAY_KEEP_ALIVE: i32 = 0x26;
    pub const PLAY_BOSS: i32 = 0x0a;
    pub const PLAY_CHAT: i32 = 0x6c;
    pub const PLAY_TRANSFER: i32 = 0x73;
    pub const PLAY_DISCONNECT: i32 = 0x1d;
    pub const PLAY_SPAWN: i32 = 0x56;
    pub const PLAY_TIME: i32 = 0x64;
}

/// Login phase: the server's plugin request, and the client's answer.
pub const LOGIN_PLUGIN_REQUEST: i32 = 0x04;
pub const LOGIN_PLUGIN_RESPONSE: i32 = 0x02;

/// A Login Plugin Request (clientbound, login phase) on `channel`. A client
/// that doesn't know the channel answers "not understood" and carries on;
/// every packet resets its read timeout, so this keeps a connection alive
/// before the real server is there.
pub fn login_plugin_request(message_id: i32, channel: &str, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    write_varint(&mut body, message_id);
    write_string(&mut body, channel);
    body.extend_from_slice(data);
    packet(LOGIN_PLUGIN_REQUEST, &body)
}

/// The message id a Login Plugin Response (serverbound, packet body) answers.
pub fn login_plugin_response_id(body: &[u8]) -> anyhow::Result<i32> {
    Ok(read_varint(body)?.context("plugin response cut short")?.0)
}

pub use id::{CONFIG_IN_FINISH, CONFIG_IN_KNOWN_PACKS, CONFIG_IN_PLUGIN, LOGIN_ACK};

// ---- text as NBT --------------------------------------------------------------

/// Java's "modified UTF-8", which NBT strings use: UTF-16 units, NUL as two
/// bytes, characters outside the BMP as two three-byte surrogates.
fn modified_utf8(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for u in s.encode_utf16() {
        match u {
            1..=0x7f => out.push(u as u8),
            0 | 0x80..=0x7ff => {
                out.push(0xc0 | (u >> 6) as u8);
                out.push(0x80 | (u & 0x3f) as u8);
            }
            _ => {
                out.push(0xe0 | (u >> 12) as u8);
                out.push(0x80 | ((u >> 6) & 0x3f) as u8);
                out.push(0x80 | (u & 0x3f) as u8);
            }
        }
    }
    out
}

fn nbt_string(out: &mut Vec<u8>, s: &str) {
    let b = modified_utf8(s);
    out.extend_from_slice(&(b.len().min(u16::MAX as usize) as u16).to_be_bytes());
    out.extend_from_slice(&b[..b.len().min(u16::MAX as usize)]);
}

/// A JSON value as an NBT tag's payload, and its type.
fn nbt_payload(out: &mut Vec<u8>, v: &Value) -> u8 {
    match v {
        Value::String(s) => {
            nbt_string(out, s);
            8
        }
        Value::Bool(b) => {
            out.push(*b as u8);
            1
        }
        Value::Number(n) => {
            out.extend_from_slice(&(n.as_i64().unwrap_or(0) as i32).to_be_bytes());
            3
        }
        Value::Array(items) => {
            // Text components' lists are of compounds.
            out.push(10);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for i in items {
                let i = if i.is_object() { i.clone() } else { serde_json::json!({ "text": i.as_str().unwrap_or_default() }) };
                compound_body(out, &i);
            }
            9
        }
        Value::Object(_) => {
            compound_body(out, v);
            10
        }
        Value::Null => {
            nbt_string(out, "");
            8
        }
    }
}

fn compound_body(out: &mut Vec<u8>, v: &Value) {
    for (k, x) in v.as_object().into_iter().flatten() {
        let mut payload = Vec::new();
        let t = nbt_payload(&mut payload, x);
        out.push(t);
        nbt_string(out, k);
        out.extend_from_slice(&payload);
    }
    out.push(0);
}

/// A chat component (as JSON) the way play and configuration packets carry
/// it since 1.20.3: network NBT, with no name on the outer tag.
pub fn text_nbt(text: &Value) -> Vec<u8> {
    let mut payload = Vec::new();
    let t = nbt_payload(&mut payload, text);
    let mut out = vec![t];
    out.extend_from_slice(&payload);
    out
}

// ---- login and configuration ---------------------------------------------------

/// Login Success: the player as they said they are (the lobby doesn't
/// check names; the real server does, after the transfer).
pub fn login_success(uuid: [u8; 16], name: &str) -> Vec<u8> {
    let mut b = uuid.to_vec();
    write_string(&mut b, name);
    write_varint(&mut b, 0); // properties
    b.push(0); // strict error handling
    packet(id::LOGIN_SUCCESS, &b)
}

/// The offline-mode id a server gives `name` ("OfflinePlayer:<name>", MD5, v3).
pub fn offline_uuid(name: &str) -> [u8; 16] {
    let mut h = md5(format!("OfflinePlayer:{name}").as_bytes());
    h[6] = (h[6] & 0x0f) | 0x30;
    h[8] = (h[8] & 0x3f) | 0x80;
    h
}

/// "Do you have Minecraft's own data?" (minecraft:core, this version).
pub fn known_packs() -> Vec<u8> {
    let mut b = Vec::new();
    write_varint(&mut b, 1);
    write_string(&mut b, "minecraft");
    write_string(&mut b, "core");
    write_string(&mut b, CORE_VERSION);
    packet(id::CONFIG_KNOWN_PACKS, &b)
}

/// Whether the client's answer to [`known_packs`] says it has them.
pub fn has_core(body: &[u8]) -> anyhow::Result<bool> {
    let (n, mut at) = read_varint(body)?.context("known packs cut short")?;
    for _ in 0..n.clamp(0, 64) {
        let mut part = || -> anyhow::Result<String> {
            let (s, used) = read_string(&body[at..], 256)?;
            at += used;
            Ok(s)
        };
        let (ns, path, version) = (part()?, part()?, part()?);
        if ns == "minecraft" && path == "core" && version == CORE_VERSION {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The client brand from a configuration plugin message ("vanilla",
/// "fabric", "neoforge"), if it's that message.
pub fn brand(body: &[u8]) -> Option<String> {
    let (channel, used) = read_string(body, 256).ok()?;
    (channel == "minecraft:brand").then(|| read_string(&body[used..], 256).ok().map(|(b, _)| b)).flatten()
}

/// The synced registries and their entries, in the order sent; the index of
/// an entry is its network id.
fn registries() -> BTreeMap<String, Vec<String>> {
    serde_json::from_str(include_str!("limbo_registries.json")).unwrap_or_default()
}

/// Registry Data packets: each registry's entries by name, with no data
/// (the client has it, see [`known_packs`]).
pub fn registry_packets() -> Vec<Vec<u8>> {
    registries()
        .into_iter()
        .map(|(reg, entries)| {
            let mut b = Vec::new();
            write_string(&mut b, &format!("minecraft:{reg}"));
            write_varint(&mut b, entries.len() as i32);
            for e in &entries {
                write_string(&mut b, &format!("minecraft:{e}"));
                b.push(0);
            }
            packet(id::CONFIG_REGISTRY, &b)
        })
        .collect()
}

fn entry_id(registry: &str, entry: &str) -> i32 {
    registries().get(registry).and_then(|e| e.iter().position(|x| x == entry)).unwrap_or(0) as i32
}

pub fn finish_configuration() -> Vec<u8> {
    packet(id::CONFIG_FINISH, &[])
}

/// Disconnect during configuration.
pub fn config_disconnect(text: &Value) -> Vec<u8> {
    packet(id::CONFIG_DISCONNECT, &text_nbt(text))
}

// ---- play ------------------------------------------------------------------------

/// Login (play): an adventure-mode player in a flat overworld.
pub fn play_login() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&1i32.to_be_bytes()); // entity id
    b.push(0); // hardcore
    write_varint(&mut b, 1);
    write_string(&mut b, "minecraft:overworld");
    write_varint(&mut b, 20); // max players
    write_varint(&mut b, 4); // view distance
    write_varint(&mut b, 4); // simulation distance
    b.push(0); // reduced debug info
    b.push(1); // respawn screen
    b.push(0); // limited crafting
    write_varint(&mut b, entry_id("dimension_type", "overworld"));
    write_string(&mut b, "minecraft:overworld");
    b.extend_from_slice(&0i64.to_be_bytes()); // hashed seed
    b.push(2); // adventure
    b.push(0xff); // no previous game mode
    b.push(0); // debug world
    b.push(1); // flat
    b.push(0); // death location
    write_varint(&mut b, 0); // portal cooldown
    b.push(0); // enforces secure chat
    packet(id::PLAY_LOGIN, &b)
}

/// Midday, with the clock stopped.
pub fn time_packet() -> Vec<u8> {
    let mut b = 0i64.to_be_bytes().to_vec();
    b.extend_from_slice(&(-6000i64).to_be_bytes());
    packet(id::PLAY_TIME, &b)
}

fn block_pos(x: i32, y: i32, z: i32) -> i64 {
    (((x as i64) & 0x3ff_ffff) << 38) | (((z as i64) & 0x3ff_ffff) << 12) | ((y as i64) & 0xfff)
}

/// Where the player stands: on the island, facing the tree.
pub const SPAWN: (f64, f64, f64) = (-0.5, 65.0, 0.5);

pub fn spawn_packets() -> Vec<Vec<u8>> {
    let mut spawn = block_pos(SPAWN.0.floor() as i32, SPAWN.1 as i32, SPAWN.2.floor() as i32).to_be_bytes().to_vec();
    spawn.extend_from_slice(&0f32.to_be_bytes());
    let mut center = Vec::new();
    write_varint(&mut center, 0);
    write_varint(&mut center, 0);
    vec![packet(id::PLAY_SPAWN, &spawn), packet(id::PLAY_CENTER, &center)]
}

/// Puts the player at the spawn (sent again if they fall).
pub fn position_packet(teleport: i32) -> Vec<u8> {
    let mut b = Vec::new();
    for v in [SPAWN.0, SPAWN.1, SPAWN.2] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&(-135f32).to_be_bytes()); // yaw: towards the tree
    b.extend_from_slice(&0f32.to_be_bytes());
    b.push(0); // absolute
    write_varint(&mut b, teleport);
    packet(id::PLAY_POSITION, &b)
}

/// "Start waiting for level chunks": the client leaves the loading screen
/// once the chunk it stands in has come.
pub fn wait_for_chunks() -> Vec<u8> {
    let mut b = vec![13u8];
    b.extend_from_slice(&0f32.to_be_bytes());
    packet(id::PLAY_GAME_EVENT, &b)
}

pub fn keep_alive(n: i64) -> Vec<u8> {
    packet(id::PLAY_KEEP_ALIVE, &n.to_be_bytes())
}

pub fn chat(text: &Value) -> Vec<u8> {
    let mut b = text_nbt(text);
    b.push(0); // not the action bar
    packet(id::PLAY_CHAT, &b)
}

pub fn transfer(host: &str, port: u16) -> Vec<u8> {
    let mut b = Vec::new();
    write_string(&mut b, host);
    write_varint(&mut b, port as i32);
    packet(id::PLAY_TRANSFER, &b)
}

pub fn play_disconnect(text: &Value) -> Vec<u8> {
    packet(id::PLAY_DISCONNECT, &text_nbt(text))
}

const BAR: [u8; 16] = *b"hopper-lobby-bar";

/// Boss bar colours.
#[derive(Clone, Copy)]
pub enum BarColor {
    Blue = 1,
    Red = 2,
    Green = 3,
    Yellow = 4,
}

/// The boss bar, first shown.
pub fn bar_add(title: &Value, progress: f32, color: BarColor) -> Vec<u8> {
    let mut b = BAR.to_vec();
    write_varint(&mut b, 0);
    b.extend_from_slice(&text_nbt(title));
    b.extend_from_slice(&progress.clamp(0.0, 1.0).to_be_bytes());
    write_varint(&mut b, color as i32);
    write_varint(&mut b, 0); // no notches
    b.push(0);
    packet(id::PLAY_BOSS, &b)
}

/// The boss bar's progress, title and colour now.
pub fn bar_update(title: &Value, progress: f32, color: BarColor) -> Vec<Vec<u8>> {
    let mut health = BAR.to_vec();
    write_varint(&mut health, 2);
    health.extend_from_slice(&progress.clamp(0.0, 1.0).to_be_bytes());
    let mut name = BAR.to_vec();
    write_varint(&mut name, 3);
    name.extend_from_slice(&text_nbt(title));
    let mut style = BAR.to_vec();
    write_varint(&mut style, 4);
    write_varint(&mut style, color as i32);
    write_varint(&mut style, 0);
    vec![packet(id::PLAY_BOSS, &health), packet(id::PLAY_BOSS, &name), packet(id::PLAY_BOSS, &style)]
}

// ---- the island ------------------------------------------------------------------

/// Block states (1.21.1) for the island file's letters.
fn state(c: char) -> Option<i32> {
    Some(match c {
        'S' => 1,     // stone
        'G' => 9,     // grass_block[snowy=false]
        'D' => 10,    // dirt
        'W' => 80,    // water[level=0]
        'A' => 112,   // sand
        'L' => 131,   // oak_log[axis=y]
        'F' => 238,   // oak_leaves[distance=1,persistent=true]
        'g' => 2005,  // short_grass
        'd' => 2075,  // dandelion
        'p' => 2077,  // poppy
        'B' => 10366, // barrier
        _ => return None,
    })
}

/// The island's blocks: (x, y, z, block state).
pub fn island() -> Vec<(i32, i32, i32, i32)> {
    island_from(include_str!("limbo_island.txt"))
}

fn island_from(text: &str) -> Vec<(i32, i32, i32, i32)> {
    let mut out = Vec::new();
    let (mut y, mut z) = (None, -8);
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(h) = line.strip_prefix("y ") {
            y = h.trim().parse().ok();
            z = -8;
            continue;
        }
        let Some(y) = y else { continue };
        for (i, c) in line.chars().enumerate() {
            if let Some(s) = state(c) {
                out.push((i as i32 - 8, y, z, s));
            }
        }
        z += 1;
    }
    out
}

/// The overworld's sections: 24, from y -64.
const SECTIONS: i32 = 24;
const MIN_Y: i32 = -64;

fn single(out: &mut Vec<u8>, value: i32) {
    out.push(0);
    write_varint(out, value);
    write_varint(out, 0);
}

/// One section's blocks (index y<<8 | z<<4 | x) as a paletted container.
fn blocks_container(out: &mut Vec<u8>, blocks: &[i32; 4096]) {
    let mut palette: Vec<i32> = Vec::new();
    for b in blocks {
        if !palette.contains(b) {
            palette.push(*b);
        }
    }
    if palette.len() == 1 {
        return single(out, palette[0]);
    }
    let bits = (usize::BITS - (palette.len() - 1).leading_zeros()).max(4) as usize;
    let per_long = 64 / bits;
    out.push(bits as u8);
    write_varint(out, palette.len() as i32);
    for p in &palette {
        write_varint(out, *p);
    }
    let longs = 4096_usize.div_ceil(per_long);
    write_varint(out, longs as i32);
    for l in 0..longs {
        let mut v: u64 = 0;
        for j in 0..per_long {
            let i = l * per_long + j;
            if i < 4096 {
                let p = palette.iter().position(|x| *x == blocks[i]).unwrap_or(0) as u64;
                v |= p << (j * bits);
            }
        }
        out.extend_from_slice(&v.to_be_bytes());
    }
}

fn bitset(out: &mut Vec<u8>, bits: u64) {
    if bits == 0 {
        write_varint(out, 0);
    } else {
        write_varint(out, 1);
        out.extend_from_slice(&bits.to_be_bytes());
    }
}

/// One chunk: the island's blocks in it, plains, full daylight where there
/// is something to see.
pub fn chunk(cx: i32, cz: i32, blocks: &[(i32, i32, i32, i32)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&cx.to_be_bytes());
    b.extend_from_slice(&cz.to_be_bytes());
    b.extend_from_slice(&[10, 0]); // heightmaps: an empty compound
    let plains = entry_id("worldgen/biome", "plains");
    let mine: Vec<_> = blocks.iter().filter(|(x, _, z, _)| x.div_euclid(16) == cx && z.div_euclid(16) == cz).collect();
    let mut data = Vec::new();
    for s in 0..SECTIONS {
        let bottom = MIN_Y + s * 16;
        let mut grid = [0i32; 4096];
        let mut count: i16 = 0;
        for (x, y, z, st) in &mine {
            if (bottom..bottom + 16).contains(y) {
                grid[(((y - bottom) as usize) << 8) | ((z.rem_euclid(16) as usize) << 4) | x.rem_euclid(16) as usize] = *st;
                count += 1;
            }
        }
        data.extend_from_slice(&count.to_be_bytes());
        blocks_container(&mut data, &grid);
        single(&mut data, plains);
    }
    write_varint(&mut b, data.len() as i32);
    b.extend_from_slice(&data);
    write_varint(&mut b, 0); // block entities
    // Light: 26 sections (one below and one above the world). Daylight on
    // all of them in a chunk with blocks, none elsewhere (the void).
    let lit: u64 = if mine.is_empty() { 0 } else { (1 << (SECTIONS + 2)) - 1 };
    bitset(&mut b, lit); // sky light
    bitset(&mut b, 0); // block light
    bitset(&mut b, 0); // empty sky light
    bitset(&mut b, 0); // empty block light
    write_varint(&mut b, lit.count_ones() as i32);
    for _ in 0..lit.count_ones() {
        write_varint(&mut b, 2048);
        b.extend(std::iter::repeat_n(0xffu8, 2048));
    }
    write_varint(&mut b, 0);
    packet(id::PLAY_CHUNK, &b)
}

/// The chunks around the island (radius 2), as one batch.
pub fn chunk_packets() -> Vec<Vec<u8>> {
    let blocks = island();
    let mut out = vec![packet(id::PLAY_BATCH_START, &[])];
    let mut n = 0;
    for cx in -2..=1 {
        for cz in -2..=1 {
            out.push(chunk(cx, cz, &blocks));
            n += 1;
        }
    }
    let mut end = Vec::new();
    write_varint(&mut end, n);
    out.push(packet(id::PLAY_BATCH_END, &end));
    out
}

/// A position report's y (Set Player Position, with or without rotation).
pub fn reported_y(id: i32, body: &[u8]) -> Option<f64> {
    matches!(id, 0x1a | 0x1b).then(|| body.get(8..16).map(|b| f64::from_be_bytes(b.try_into().unwrap_or_default()))).flatten()
}

// ---- small MD5, for offline ids -------------------------------------------------

fn md5(input: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11,
        16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = input.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((input.len() as u64) * 8).to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    let mut out = [0u8; 16];
    for (i, w) in [a0, b0, c0, d0].iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

/// Checks a packet id and returns its body, for the connection's reads.
pub fn expect(id: i32, want: i32, what: &str) -> anyhow::Result<()> {
    if id != want {
        bail!("expected {what} (packet {want:#x}), got {id:#x}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosting::router::{Read, read_packet};
    use serde_json::json;

    fn body(p: &[u8]) -> (i32, Vec<u8>) {
        match read_packet(p, 1 << 21).unwrap() {
            Read::Done((id, b), n) => {
                assert_eq!(n, p.len(), "one whole packet");
                (id, b.to_vec())
            }
            Read::More => panic!("cut short"),
        }
    }

    #[test]
    fn text_becomes_network_nbt() {
        // A plain string: type 8, then the string.
        assert_eq!(text_nbt(&json!("Hi")), [8, 0, 2, b'H', b'i']);
        // A compound: type 10, named tags, end.
        let t = text_nbt(&json!({ "text": "A", "color": "red" }));
        assert_eq!(t[0], 10);
        assert_eq!(*t.last().unwrap(), 0);
        let s = String::from_utf8_lossy(&t);
        assert!(s.contains("text") && s.contains("color") && s.contains("red"));
        // Emoji: two three-byte surrogates, as Java writes them.
        assert_eq!(modified_utf8("💤"), [0xed, 0xa0, 0xbd, 0xed, 0xb2, 0xa4]);
        assert_eq!(modified_utf8("\0"), [0xc0, 0x80]);
        // extra: a list of compounds.
        let t = text_nbt(&json!({ "text": "", "extra": [{ "text": "x" }, "y"] }));
        let i = t.windows(5).position(|w| w == [9, 0, 5, b'e', b'x']).unwrap();
        assert_eq!(&t[i + 8..i + 13], &[10, 0, 0, 0, 2], "list of 2 compounds");
    }

    #[test]
    fn offline_ids_match_minecraft() {
        // Minecraft's own offline id for "Notch".
        let u = offline_uuid("Notch");
        let hex: String = u.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "b50ad385829d3141a2167e7d7539ba7f");
        assert_eq!(md5(b""), [0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8, 0x42, 0x7e]);
    }

    #[test]
    fn login_and_configuration_packets() {
        let (id, b) = body(&login_success([7; 16], "Alex"));
        assert_eq!(id, 2);
        assert_eq!(&b[..16], &[7; 16]);
        assert_eq!(read_string(&b[16..], 16).unwrap(), ("Alex".to_string(), 5));
        assert_eq!(&b[21..], &[0, 0], "no properties, lenient");
        let (id, b) = body(&known_packs());
        assert_eq!(id, 0x0e);
        assert!(has_core(&b).unwrap(), "our own offer reads back as having core");
        let mut none = Vec::new();
        write_varint(&mut none, 0);
        assert!(!has_core(&none).unwrap());
        let mut brand_msg = Vec::new();
        write_string(&mut brand_msg, "minecraft:brand");
        write_string(&mut brand_msg, "neoforge");
        assert_eq!(brand(&brand_msg).as_deref(), Some("neoforge"));
        let regs = registry_packets();
        assert_eq!(regs.len(), 11, "every registry 1.21.1 syncs");
        let mut names = Vec::new();
        for p in &regs {
            let (id, b) = body(p);
            assert_eq!(id, 7);
            let (name, used) = read_string(&b, 256).unwrap();
            let (n, _) = read_varint(&b[used..]).unwrap().unwrap();
            assert!(n > 0, "{name} has entries");
            names.push(name);
        }
        for r in ["minecraft:worldgen/biome", "minecraft:dimension_type", "minecraft:damage_type", "minecraft:enchantment", "minecraft:wolf_variant", "minecraft:painting_variant"] {
            assert!(names.iter().any(|n| n == r), "{r}");
        }
        assert_eq!(entry_id("dimension_type", "overworld"), 0);
        assert_eq!(entry_id("worldgen/biome", "plains"), 39);
    }

    #[test]
    fn play_packets_have_their_ids() {
        assert_eq!(body(&play_login()).0, 0x2b);
        assert_eq!(body(&wait_for_chunks()), (0x22, vec![13, 0, 0, 0, 0]));
        assert_eq!(body(&transfer("a.example.com", 25565)).0, 0x73);
        assert_eq!(body(&keep_alive(5)), (0x26, 5i64.to_be_bytes().to_vec()));
        let (id, b) = body(&bar_add(&json!("Starting"), 0.5, BarColor::Blue));
        assert_eq!((id, &b[..16], b[16]), (0x0a, &BAR[..], 0));
        assert_eq!(bar_update(&json!("x"), 2.0, BarColor::Red).len(), 3);
        let (id, b) = body(&position_packet(1));
        assert_eq!((id, b.len()), (0x40, 8 * 3 + 4 + 4 + 1 + 1));
        let mut moved = Vec::new();
        for v in [1.0f64, 12.5, 3.0] {
            moved.extend_from_slice(&v.to_be_bytes());
        }
        assert_eq!(reported_y(0x1a, &moved), Some(12.5));
        assert_eq!(reported_y(0x18, &moved), None);
        assert_eq!(block_pos(-1, 65, 1), (0x3ff_ffff_i64 << 38) | (1 << 12) | 65);
    }

    #[test]
    fn island_reads_and_chunks_encode() {
        let blocks = island();
        assert!(blocks.len() > 300, "{}", blocks.len());
        assert!(blocks.iter().any(|b| b.3 == 131), "a tree");
        assert!(blocks.iter().any(|b| b.3 == 80), "water");
        assert!(blocks.iter().all(|(x, y, z, _)| (-8..8).contains(x) && (-8..8).contains(z) && (55..80).contains(y)));
        // Standing on grass, with air above.
        let (sx, sz) = (SPAWN.0.floor() as i32, SPAWN.2.floor() as i32);
        assert!(blocks.contains(&(sx, 64, sz, 9)));
        assert!(!blocks.iter().any(|b| b.0 == sx && b.2 == sz && b.1 == 65 || b.0 == sx && b.2 == sz && b.1 == 66));
        let p = chunk_packets();
        assert_eq!(p.len(), 16 + 2);
        // Parse one island chunk back: 24 sections of blocks and biomes.
        let (id, b) = body(&chunk(0, 0, &blocks));
        assert_eq!(id, 0x27);
        assert_eq!(&b[..8], &[0; 8]);
        assert_eq!(&b[8..10], &[10, 0]);
        let (size, n) = read_varint(&b[10..]).unwrap().unwrap();
        let mut at = 10 + n;
        let data = &b[at..at + size as usize];
        let mut i = 0;
        let mut total = 0;
        for _ in 0..24 {
            total += i16::from_be_bytes([data[i], data[i + 1]]) as i32;
            i += 2;
            for _ in 0..2 {
                let bits = data[i];
                i += 1;
                if bits == 0 {
                    let (_, n) = read_varint(&data[i..]).unwrap().unwrap();
                    i += n;
                    // Single-value containers carry an empty data array.
                    let (len, n) = read_varint(&data[i..]).unwrap().unwrap();
                    assert_eq!(len, 0);
                    i += n;
                } else {
                    let (len, n) = read_varint(&data[i..]).unwrap().unwrap();
                    i += n;
                    for _ in 0..len {
                        let (_, n) = read_varint(&data[i..]).unwrap().unwrap();
                        i += n;
                    }
                    let per = 64 / bits as usize;
                    let (longs, n) = read_varint(&data[i..]).unwrap().unwrap();
                    assert_eq!(longs as usize, 4096_usize.div_ceil(per));
                    i += n;
                    i += longs as usize * 8;
                }
            }
        }
        assert_eq!(i, data.len(), "sections fill the data exactly");
        assert_eq!(total, blocks.iter().filter(|(x, _, z, _)| x.div_euclid(16) == 0 && z.div_euclid(16) == 0).count() as i32);
        at += size as usize;
        assert_eq!(b[at], 0, "no block entities");
        // An empty chunk is tiny and has no light.
        assert!(chunk(5, 5, &blocks).len() < 300);
    }
}
