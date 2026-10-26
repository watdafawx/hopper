//! Deduplicated backups: files are cut into content-defined chunks, and each
//! chunk is stored once under its SHA-256 in `<backups>/store/chunks/`. A
//! backup ("snapshot") is a small manifest listing every file's chunks, so a
//! second backup of a world where little changed costs only the changed
//! chunks.
//!
//! Chunk boundaries come from a rolling gear hash (FastCDC-style), so an
//! insert near the start of a file only changes the chunks around it.
//! Chunks are 64 KiB to 1 MiB, about 256 KiB on average: small enough that
//! a region file with a few changed chunks shares most of its data with the
//! last backup.
//!
//! A chunk file is one byte of format (0 raw, 1 deflate) and the data.
//! Files whose contents are already compressed (region files, jars) are
//! stored raw; others are deflated when that saves something.
//!
//! Making a snapshot and cleaning up unused chunks take the same lock, so a
//! clean-up never removes a chunk a backup in progress just reused.

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

pub const STORE: &str = "store";
const CHUNKS: &str = "chunks";
const SNAPSHOTS: &str = "snapshots";
/// Snapshot names end in this; zips end in `.zip`.
pub const EXT: &str = ".snap";

const MIN: usize = 64 << 10;
const AVG: usize = 256 << 10;
const MAX: usize = 1 << 20;
/// Harder to cut before the average size, easier after: sizes cluster
/// around it (FastCDC's normalized chunking).
const MASK_S: u64 = mask(20);
const MASK_L: u64 = mask(16);

const fn mask(bits: u32) -> u64 {
    ((1u64 << bits) - 1) << (64 - bits)
}

static GEAR: LazyLock<[u64; 256]> = LazyLock::new(|| {
    // splitmix64: a fixed table, the same on every machine, so the same
    // data always cuts the same way.
    let mut t = [0u64; 256];
    let mut s: u64 = 0x4f70_7065_7248_6f70;
    for v in &mut t {
        s = s.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        *v = z ^ (z >> 31);
    }
    t
});

/// Where the first chunk of `data` ends.
fn cut(data: &[u8]) -> usize {
    let n = data.len().min(MAX);
    if n <= MIN {
        return n;
    }
    let gear = &*GEAR;
    let mut h: u64 = 0;
    let mid = AVG.min(n);
    let mut i = MIN;
    while i < mid {
        h = (h << 1).wrapping_add(gear[data[i] as usize]);
        if h & MASK_S == 0 {
            return i + 1;
        }
        i += 1;
    }
    while i < n {
        h = (h << 1).wrapping_add(gear[data[i] as usize]);
        if h & MASK_L == 0 {
            return i + 1;
        }
        i += 1;
    }
    n
}

/// One file in a snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub size: u64,
    /// Modified time in Unix nanoseconds; with the size, how the next
    /// snapshot knows a file didn't change without reading it.
    #[serde(default)]
    pub mtime: u128,
    /// Chunk hashes (hex), in order.
    pub chunks: Vec<String>,
}

/// A snapshot's manifest, `store/snapshots/<name>.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub v: u32,
    pub name: String,
    pub at: u64,
    pub scope: super::backup::Scope,
    pub kind: super::backup::Kind,
    #[serde(default)]
    pub note: String,
    /// Bytes of all files (what a restore writes).
    pub size: u64,
    /// Bytes this snapshot added to the store (compressed).
    #[serde(default)]
    pub added: u64,
    pub files: Vec<FileRef>,
}

fn locks() -> &'static Mutex<HashMap<PathBuf, Arc<Mutex<()>>>> {
    static L: LazyLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = LazyLock::new(Default::default);
    &L
}

/// A server's chunk store.
#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
}

/// What putting one file in the store did.
pub struct Put {
    pub file: FileRef,
    /// Compressed bytes newly written.
    pub added: u64,
}

pub fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

pub fn valid_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn mtime_of(m: &fs::Metadata) -> u128 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos())
}

impl Store {
    /// The store inside a server's backups folder.
    pub fn new(backups_dir: &Path) -> Self {
        Store { dir: backups_dir.join(STORE) }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Held while a snapshot is made or chunks are cleaned up.
    pub fn lock(&self) -> Arc<Mutex<()>> {
        locks().lock().unwrap().entry(self.dir.clone()).or_default().clone()
    }

    pub fn chunk_path(&self, hash: &str) -> PathBuf {
        self.dir.join(CHUNKS).join(&hash[..2]).join(hash)
    }

    fn manifest_path(&self, name: &str) -> PathBuf {
        self.dir.join(SNAPSHOTS).join(format!("{name}.json"))
    }

    pub fn has_chunk(&self, hash: &str) -> bool {
        valid_hash(hash) && self.chunk_path(hash).is_file()
    }

    /// Stores one chunk if it isn't there yet; returns the bytes written.
    fn put_chunk(&self, hash: &str, data: &[u8], compress: bool) -> anyhow::Result<u64> {
        let path = self.chunk_path(hash);
        if path.is_file() {
            return Ok(0);
        }
        let mut body = Vec::with_capacity(data.len() + 1);
        let packed = if compress {
            let mut z = flate2::write::DeflateEncoder::new(Vec::with_capacity(data.len() / 2), flate2::Compression::fast());
            z.write_all(data)?;
            Some(z.finish()?).filter(|p| p.len() + p.len() / 16 < data.len())
        } else {
            None
        };
        match packed {
            Some(p) => {
                body.push(1);
                body.extend_from_slice(&p);
            }
            None => {
                body.push(0);
                body.extend_from_slice(data);
            }
        }
        self.put_raw_chunk(hash, &body)?;
        Ok(body.len() as u64)
    }

    /// Writes a chunk file exactly as stored (format byte + data), e.g. one
    /// brought back from an off-site copy. Checks it first.
    pub fn put_raw_chunk(&self, hash: &str, body: &[u8]) -> anyhow::Result<()> {
        if !valid_hash(hash) {
            bail!("bad chunk name");
        }
        let path = self.chunk_path(hash);
        if path.is_file() {
            return Ok(());
        }
        let data = unpack(body)?;
        if hex(&Sha256::digest(&data)) != hash {
            bail!("chunk {hash} doesn't match its contents");
        }
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        fs::write(&tmp, body)?;
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(&tmp);
            if !path.is_file() {
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// A chunk file as stored (format byte + data).
    pub fn raw_chunk(&self, hash: &str) -> anyhow::Result<Vec<u8>> {
        if !valid_hash(hash) {
            bail!("bad chunk name");
        }
        fs::read(self.chunk_path(hash)).with_context(|| format!("chunk {hash} is missing from the store"))
    }

    pub fn read_chunk(&self, hash: &str) -> anyhow::Result<Vec<u8>> {
        unpack(&self.raw_chunk(hash)?)
    }

    /// Cuts `path` into chunks and stores the new ones. `prev` is the same
    /// file in the last snapshot: when its size and time match, its chunks
    /// are reused without reading the file.
    pub fn put_file(&self, rel: &str, path: &Path, prev: Option<&FileRef>, compress: bool) -> anyhow::Result<Put> {
        let mut f = fs::File::open(path)?;
        let meta = f.metadata()?;
        let (size, mtime) = (meta.len(), mtime_of(&meta));
        if let Some(p) = prev
            && p.size == size
            && p.mtime == mtime
            && mtime != 0
            && p.chunks.iter().all(|h| self.chunk_path(h).is_file())
        {
            return Ok(Put { file: FileRef { path: rel.to_string(), size, mtime, chunks: p.chunks.clone() }, added: 0 });
        }
        let mut chunks = Vec::new();
        let mut added = 0;
        let mut total = 0u64;
        let mut buf: Vec<u8> = Vec::with_capacity(2 * MAX);
        let mut eof = false;
        loop {
            while !eof && buf.len() < MAX {
                let start = buf.len();
                buf.resize(start + MAX, 0);
                let n = f.read(&mut buf[start..])?;
                buf.truncate(start + n);
                if n == 0 {
                    eof = true;
                }
            }
            if buf.is_empty() {
                break;
            }
            let n = if eof && buf.len() <= MIN { buf.len() } else { cut(&buf) };
            let hash = hex(&Sha256::digest(&buf[..n]));
            added += self.put_chunk(&hash, &buf[..n], compress)?;
            total += n as u64;
            chunks.push(hash);
            buf.drain(..n);
        }
        // Sized as read: a file that grew or shrank while read is what was read.
        Ok(Put { file: FileRef { path: rel.to_string(), size: total, mtime: if total == size { mtime } else { 0 }, chunks }, added })
    }

    /// Writes a stored file out to `dest`.
    pub fn write_file(&self, file: &FileRef, dest: &Path) -> anyhow::Result<()> {
        if let Some(p) = dest.parent() {
            fs::create_dir_all(p)?;
        }
        let mut out = std::io::BufWriter::with_capacity(1 << 20, fs::File::create(dest).with_context(|| format!("writing {}", file.path))?);
        for h in &file.chunks {
            out.write_all(&self.read_chunk(h)?)?;
        }
        out.flush()?;
        Ok(())
    }

    /// A reader over a stored file's bytes.
    pub fn reader<'a>(&'a self, file: &'a FileRef) -> FileReader<'a> {
        FileReader { store: self, file, next: 0, cur: Vec::new(), pos: 0 }
    }

    pub fn save_manifest(&self, m: &Manifest) -> anyhow::Result<()> {
        let path = self.manifest_path(&m.name);
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(m)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn manifest(&self, name: &str) -> anyhow::Result<Manifest> {
        let b = fs::read(self.manifest_path(name)).context("That backup is gone.")?;
        Ok(serde_json::from_slice(&b)?)
    }

    pub fn has_manifest(&self, name: &str) -> bool {
        self.manifest_path(name).is_file()
    }

    /// Every snapshot's name.
    pub fn names(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.dir.join(SNAPSHOTS)) else { return Vec::new() };
        rd.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".json").map(str::to_string)).collect()
    }

    pub fn remove_manifest(&self, name: &str) {
        let _ = fs::remove_file(self.manifest_path(name));
    }

    /// Deletes chunks no snapshot uses; returns (chunks, bytes) freed.
    pub fn gc(&self) -> anyhow::Result<(usize, u64)> {
        let lock = self.lock();
        let _g = lock.lock().unwrap();
        let mut used = HashSet::new();
        for n in self.names() {
            // A manifest that can't be read keeps everything: better a
            // bigger store than a broken backup.
            let m = self.manifest(&n).with_context(|| format!("reading backup {n}"))?;
            for f in m.files {
                used.extend(f.chunks);
            }
        }
        let (mut n, mut bytes) = (0, 0);
        let Ok(rd) = fs::read_dir(self.dir.join(CHUNKS)) else { return Ok((0, 0)) };
        for d in rd.flatten() {
            let Ok(inner) = fs::read_dir(d.path()) else { continue };
            for c in inner.flatten() {
                let name = c.file_name().to_string_lossy().into_owned();
                if !used.contains(&name) {
                    let len = c.metadata().map_or(0, |m| m.len());
                    if fs::remove_file(c.path()).is_ok() {
                        n += 1;
                        bytes += len;
                    }
                }
            }
        }
        Ok((n, bytes))
    }

    /// Bytes the store takes on disk.
    pub fn usage(&self) -> u64 {
        fn walk(p: &Path) -> u64 {
            let Ok(rd) = fs::read_dir(p) else { return 0 };
            rd.flatten()
                .map(|e| match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&e.path()),
                    _ => e.metadata().map_or(0, |m| m.len()),
                })
                .sum()
        }
        walk(&self.dir)
    }
}

fn unpack(body: &[u8]) -> anyhow::Result<Vec<u8>> {
    match body.first() {
        Some(0) => Ok(body[1..].to_vec()),
        Some(1) => {
            let mut out = Vec::with_capacity(body.len() * 2);
            flate2::read::DeflateDecoder::new(&body[1..]).read_to_end(&mut out)?;
            Ok(out)
        }
        _ => bail!("unknown chunk format"),
    }
}

/// Reads a stored file chunk by chunk.
pub struct FileReader<'a> {
    store: &'a Store,
    file: &'a FileRef,
    next: usize,
    cur: Vec<u8>,
    pos: usize,
}

impl Read for FileReader<'_> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos >= self.cur.len() {
            let Some(h) = self.file.chunks.get(self.next) else { return Ok(0) };
            self.cur = self.store.read_chunk(h).map_err(std::io::Error::other)?;
            self.pos = 0;
            self.next += 1;
        }
        let n = out.len().min(self.cur.len() - self.pos);
        out[..n].copy_from_slice(&self.cur[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-chunks-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Pseudo-random bytes (incompressible, like region files).
    fn noise(n: usize, seed: u64) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s as u8
            })
            .collect()
    }

    #[test]
    fn chunks_dedupe_and_survive_inserts() {
        let d = temp("dedupe");
        let st = Store::new(&d);
        let a = noise(5 << 20, 1);
        fs::write(d.join("a.mca"), &a).unwrap();
        let p1 = st.put_file("a.mca", &d.join("a.mca"), None, false).unwrap();
        assert!(p1.file.chunks.len() >= 5, "{}", p1.file.chunks.len());
        assert_eq!(p1.file.size, a.len() as u64);
        // Same file again: nothing new.
        let p2 = st.put_file("a.mca", &d.join("a.mca"), None, false).unwrap();
        assert_eq!(p2.added, 0);
        // Insert 100 bytes near the start: most chunks still match.
        let mut b = a[..1000].to_vec();
        b.extend(noise(100, 9));
        b.extend_from_slice(&a[1000..]);
        fs::write(d.join("b.mca"), &b).unwrap();
        let p3 = st.put_file("b.mca", &d.join("b.mca"), None, false).unwrap();
        let shared = p3.file.chunks.iter().filter(|h| p1.file.chunks.contains(h)).count();
        assert!(shared + 2 >= p1.file.chunks.len(), "shared {shared} of {}", p1.file.chunks.len());
        assert!(p3.added < (2 * MAX + 200) as u64);
        // Reads back exactly.
        let mut back = Vec::new();
        st.reader(&p3.file).read_to_end(&mut back).unwrap();
        assert_eq!(back, b);
        st.write_file(&p1.file, &d.join("out.mca")).unwrap();
        assert_eq!(fs::read(d.join("out.mca")).unwrap(), a);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn compresses_text_and_skips_unchanged_files() {
        let d = temp("text");
        let st = Store::new(&d);
        let text = "key = value\n".repeat(50_000);
        fs::write(d.join("c.toml"), &text).unwrap();
        let p = st.put_file("c.toml", &d.join("c.toml"), None, true).unwrap();
        assert!(p.added < text.len() as u64 / 10);
        // Unchanged size + time: reused without reading.
        let again = st.put_file("c.toml", &d.join("c.toml"), Some(&p.file), true).unwrap();
        assert_eq!(again.file.chunks, p.file.chunks);
        // Empty files work.
        fs::write(d.join("e"), b"").unwrap();
        let e = st.put_file("e", &d.join("e"), None, true).unwrap();
        assert!(e.file.chunks.is_empty());
        st.write_file(&e.file, &d.join("e2")).unwrap();
        assert_eq!(fs::read(d.join("e2")).unwrap().len(), 0);
        // A chunk that doesn't match its name is refused.
        assert!(st.put_raw_chunk(&"0".repeat(64), b"\0abc").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn cut_points_are_bounded() {
        let data = noise(3 << 20, 5);
        let mut at = 0;
        while at < data.len() {
            let n = cut(&data[at..]);
            assert!(n <= MAX && (n >= MIN || at + n == data.len()));
            at += n;
        }
    }
}
