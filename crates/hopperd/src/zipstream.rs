//! A zip writer that never seeks, so a folder can stream to the browser as
//! it is read: no temp file, the download starts at once.
//!
//! Entries are stored (not compressed): jars, worlds and images are already
//! compressed, and on a LAN the disk is the limit, not the wire. Every entry
//! carries zip64 fields, so files and archives over 4 GB work. CRCs go in a
//! data descriptor after each file (bit 3), since they're only known after
//! reading it. Sizes come from the file's metadata up front; a file that
//! changes while it's read (a running server's world) is cut or padded to
//! that size so the archive stays valid.

use std::io::{self, Read, Write};
use std::path::Path;

const VERSION: u16 = 45; // zip64
const FLAGS: u16 = 0x0808; // data descriptor + UTF-8 names

struct Central {
    name: Vec<u8>,
    crc: u32,
    size: u64,
    offset: u64,
    dir: bool,
    time: u16,
    date: u16,
}

/// Writes a zip to `out` entry by entry.
pub struct ZipStream<W: Write> {
    out: W,
    pos: u64,
    entries: Vec<Central>,
}

/// DOS time and date for a file's modified time (UTC).
fn dos_time(m: Option<std::time::SystemTime>) -> (u16, u16) {
    let secs = m
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    // Civil date from days since 1970 (Howard Hinnant's algorithm).
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    if y < 1980 {
        return (0, (1 << 5) | 1);
    }
    let time = ((rem / 3600) << 11) | (((rem % 3600) / 60) << 5) | ((rem % 60) / 2);
    let date = ((y - 1980) << 9) | (m << 5) | d;
    (time as u16, date as u16)
}

impl<W: Write> ZipStream<W> {
    pub fn new(out: W) -> Self {
        ZipStream { out, pos: 0, entries: Vec::new() }
    }

    fn put(&mut self, b: &[u8]) -> io::Result<()> {
        self.out.write_all(b)?;
        self.pos += b.len() as u64;
        Ok(())
    }

    fn local_header(&mut self, name: &[u8], size: u64, time: u16, date: u16) -> io::Result<()> {
        let mut h = Vec::with_capacity(30 + name.len() + 20);
        h.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        h.extend_from_slice(&VERSION.to_le_bytes());
        h.extend_from_slice(&FLAGS.to_le_bytes());
        h.extend_from_slice(&0u16.to_le_bytes()); // stored
        h.extend_from_slice(&time.to_le_bytes());
        h.extend_from_slice(&date.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // crc: in the descriptor
        h.extend_from_slice(&u32::MAX.to_le_bytes());
        h.extend_from_slice(&u32::MAX.to_le_bytes());
        h.extend_from_slice(&(name.len() as u16).to_le_bytes());
        h.extend_from_slice(&20u16.to_le_bytes());
        h.extend_from_slice(name);
        h.extend_from_slice(&1u16.to_le_bytes()); // zip64 extra
        h.extend_from_slice(&16u16.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        h.extend_from_slice(&size.to_le_bytes());
        self.put(&h)
    }

    /// An empty folder (`name` ends with '/').
    pub fn add_dir(&mut self, name: &str, modified: Option<std::time::SystemTime>) -> io::Result<()> {
        let name = format!("{}/", name.trim_end_matches('/'));
        let (time, date) = dos_time(modified);
        let offset = self.pos;
        self.local_header(name.as_bytes(), 0, time, date)?;
        self.descriptor(0, 0)?;
        self.entries.push(Central { name: name.into_bytes(), crc: 0, size: 0, offset, dir: true, time, date });
        Ok(())
    }

    fn descriptor(&mut self, crc: u32, size: u64) -> io::Result<()> {
        let mut d = Vec::with_capacity(24);
        d.extend_from_slice(&0x0807_4b50u32.to_le_bytes());
        d.extend_from_slice(&crc.to_le_bytes());
        d.extend_from_slice(&size.to_le_bytes());
        d.extend_from_slice(&size.to_le_bytes());
        self.put(&d)
    }

    /// Adds the file at `path` as `name`.
    pub fn add_file(&mut self, name: &str, path: &Path) -> io::Result<()> {
        let mut f = std::fs::File::open(path)?;
        let meta = f.metadata()?;
        let size = meta.len();
        let (time, date) = dos_time(meta.modified().ok());
        let offset = self.pos;
        self.local_header(name.as_bytes(), size, time, date)?;
        let mut crc = crc32fast::Hasher::new();
        let mut left = size;
        let mut buf = vec![0u8; 256 * 1024];
        while left > 0 {
            let want = buf.len().min(left as usize);
            let n = f.read(&mut buf[..want])?;
            if n == 0 {
                // The file shrank: pad to the size already promised.
                buf[..want].fill(0);
                crc.update(&buf[..want]);
                self.put(&buf[..want])?;
                left -= want as u64;
                continue;
            }
            crc.update(&buf[..n]);
            self.out.write_all(&buf[..n])?;
            self.pos += n as u64;
            left -= n as u64;
        }
        let crc = crc.finalize();
        self.descriptor(crc, size)?;
        self.entries.push(Central { name: name.as_bytes().to_vec(), crc, size, offset, dir: false, time, date });
        Ok(())
    }

    /// Writes the central directory and returns the sink.
    pub fn finish(mut self) -> io::Result<W> {
        let cd_start = self.pos;
        let entries = std::mem::take(&mut self.entries);
        for e in &entries {
            let mut c = Vec::with_capacity(46 + e.name.len() + 28);
            c.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            c.extend_from_slice(&VERSION.to_le_bytes()); // made by (DOS, 4.5)
            c.extend_from_slice(&VERSION.to_le_bytes());
            c.extend_from_slice(&FLAGS.to_le_bytes());
            c.extend_from_slice(&0u16.to_le_bytes());
            c.extend_from_slice(&e.time.to_le_bytes());
            c.extend_from_slice(&e.date.to_le_bytes());
            c.extend_from_slice(&e.crc.to_le_bytes());
            c.extend_from_slice(&u32::MAX.to_le_bytes());
            c.extend_from_slice(&u32::MAX.to_le_bytes());
            c.extend_from_slice(&(e.name.len() as u16).to_le_bytes());
            c.extend_from_slice(&28u16.to_le_bytes());
            c.extend_from_slice(&0u16.to_le_bytes()); // comment
            c.extend_from_slice(&0u16.to_le_bytes()); // disk
            c.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            c.extend_from_slice(&(if e.dir { 0x10u32 } else { 0 }).to_le_bytes());
            c.extend_from_slice(&u32::MAX.to_le_bytes());
            c.extend_from_slice(&e.name);
            c.extend_from_slice(&1u16.to_le_bytes());
            c.extend_from_slice(&24u16.to_le_bytes());
            c.extend_from_slice(&e.size.to_le_bytes());
            c.extend_from_slice(&e.size.to_le_bytes());
            c.extend_from_slice(&e.offset.to_le_bytes());
            self.put(&c)?;
        }
        let cd_size = self.pos - cd_start;
        let eocd64 = self.pos;
        let n = entries.len() as u64;
        let mut t = Vec::with_capacity(56 + 20 + 22);
        t.extend_from_slice(&0x0606_4b50u32.to_le_bytes());
        t.extend_from_slice(&44u64.to_le_bytes());
        t.extend_from_slice(&VERSION.to_le_bytes());
        t.extend_from_slice(&VERSION.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&n.to_le_bytes());
        t.extend_from_slice(&cd_size.to_le_bytes());
        t.extend_from_slice(&cd_start.to_le_bytes());
        t.extend_from_slice(&0x0706_4b50u32.to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        t.extend_from_slice(&eocd64.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        t.extend_from_slice(&u16::MAX.to_le_bytes());
        t.extend_from_slice(&u16::MAX.to_le_bytes());
        t.extend_from_slice(&u32::MAX.to_le_bytes());
        t.extend_from_slice(&u32::MAX.to_le_bytes());
        t.extend_from_slice(&0u16.to_le_bytes());
        self.put(&t)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Zips `items` (each a file or folder under `base`), naming entries by
/// their path relative to `base`. `skip` leaves out matching names.
pub fn zip_paths<W: Write>(out: W, base: &Path, items: &[std::path::PathBuf], skip: &dyn Fn(&str) -> bool) -> io::Result<W> {
    let mut z = ZipStream::new(out);
    for item in items {
        walk(&mut z, base, item, skip)?;
    }
    z.finish()
}

fn rel_name(base: &Path, p: &Path) -> String {
    p.strip_prefix(base)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

fn walk<W: Write>(z: &mut ZipStream<W>, base: &Path, p: &Path, skip: &dyn Fn(&str) -> bool) -> io::Result<()> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if skip(&name) {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(p)?;
    if meta.is_dir() {
        let mut kids: Vec<_> = std::fs::read_dir(p)?.flatten().map(|e| e.path()).collect();
        kids.sort();
        if kids.is_empty() {
            z.add_dir(&rel_name(base, p), meta.modified().ok())?;
        }
        for k in kids {
            walk(z, base, &k, skip)?;
        }
    } else if meta.is_file() {
        // A file locked by a running server (session.lock) is skipped
        // rather than failing the whole download.
        match z.add_file(&rel_name(base, p), p) {
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32) => {
                log::warn!("zip: skipped {} ({e})", p.display());
            }
            r => r?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_readers_open_it() {
        let dir = std::env::temp_dir().join(format!("hopperd-zip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("world/region")).unwrap();
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        std::fs::write(dir.join("server.properties"), "motd=hi\n").unwrap();
        let big: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("world/region/r.0.0.mca"), &big).unwrap();
        std::fs::write(dir.join("world/ünïcode.txt"), "ok").unwrap();
        let items = vec![dir.join("server.properties"), dir.join("world"), dir.join("empty")];
        let bytes = zip_paths(Vec::new(), &dir, &items, &|n| n == "skip.me").unwrap();
        let mut a = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut names: Vec<String> = a.file_names().map(str::to_string).collect();
        names.sort();
        assert_eq!(names, ["empty/", "server.properties", "world/region/r.0.0.mca", "world/ünïcode.txt"]);
        let mut out = Vec::new();
        a.by_name("world/region/r.0.0.mca").unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, big);
        let mut s = String::new();
        a.by_name("server.properties").unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "motd=hi\n");
        assert!(a.by_name("empty/").unwrap().is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dos_dates() {
        use std::time::{Duration, UNIX_EPOCH};
        // 2026-10-23 14:12:06 UTC
        let t = UNIX_EPOCH + Duration::from_secs(1_792_764_726);
        let (time, date) = dos_time(Some(t));
        assert_eq!((date >> 9) + 1980, 2026);
        assert_eq!((date >> 5) & 0xF, 10);
        assert_eq!(date & 0x1F, 23);
        assert_eq!(time >> 11, 14);
        assert_eq!((time >> 5) & 0x3F, 12);
    }
}
