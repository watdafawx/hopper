//! Downloading Java runtimes into Hopper's own folder
//! (`<data dir>/java/<name>`, see [`crate::java::hopper_runtimes_dir`]).
//!
//! Sources:
//! - Eclipse Temurin from the Adoptium API (any major it publishes; JDK or JRE).
//! - Oracle GraalVM for JDK 21 and 25 (free under the GraalVM Free Terms),
//!   from Oracle's stable `latest` URLs. GraalVM Community has no stable
//!   "latest" link (it's GitHub releases per version), so it isn't offered.
//!
//! Every archive is checked against the publisher's SHA-256 (and the size,
//! when known) before it is unpacked. SHA-256 is implemented here: no direct
//! dependency provides it (ring and sha2 are only transitive).

use anyhow::{Context, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::jobs::Job;

/// JDK (compiler included) or JRE (runtime only, about a third the size).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Package {
    Jdk,
    Jre,
}

impl Package {
    fn api(self) -> &'static str {
        match self {
            Package::Jdk => "jdk",
            Package::Jre => "jre",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Distribution {
    Temurin,
    OracleGraalVm,
}

impl Distribution {
    pub fn name(self) -> &'static str {
        match self {
            Distribution::Temurin => "Eclipse Temurin",
            Distribution::OracleGraalVm => "Oracle GraalVM",
        }
    }
}

/// A resolved download: where it is and how to check it.
#[derive(Clone, Debug, PartialEq)]
pub struct JavaDownload {
    pub distribution: Distribution,
    pub package: Package,
    pub major: u32,
    /// e.g. "jdk-21.0.12.1+1"; "latest" when the source doesn't say before download.
    pub release: String,
    /// The archive's file name.
    pub file_name: String,
    pub url: String,
    /// Bytes, when known in advance.
    pub size: Option<u64>,
    /// Lower-case hex SHA-256 of the archive.
    pub sha256: String,
}

impl JavaDownload {
    /// e.g. "Eclipse Temurin 21 JRE (jdk-21.0.12.1+1)".
    pub fn label(&self) -> String {
        let pkg = match self.package {
            Package::Jdk => "JDK",
            Package::Jre => "JRE",
        };
        format!(
            "{} {} {pkg} ({})",
            self.distribution.name(),
            self.major,
            self.release
        )
    }
}

/// The Oracle GraalVM majors with a stable "latest" download.
pub const GRAALVM_MAJORS: &[u32] = &[21, 25];

/// Adoptium API query for the latest Windows x64 HotSpot build of `major`.
pub fn adoptium_url(major: u32, package: Package) -> String {
    format!(
        "https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type={}&os=windows&vendor=eclipse",
        package.api()
    )
}

/// Picks the Windows x64 `.zip` out of an Adoptium `assets/latest` response.
pub fn parse_adoptium(json: &str, major: u32, package: Package) -> anyhow::Result<JavaDownload> {
    #[derive(serde::Deserialize)]
    struct Asset {
        binary: Binary,
        #[serde(default)]
        release_name: String,
        version: Option<Version>,
    }
    #[derive(serde::Deserialize)]
    struct Binary {
        #[serde(default)]
        architecture: String,
        #[serde(default)]
        os: String,
        #[serde(default)]
        image_type: String,
        package: Option<Pkg>,
    }
    #[derive(serde::Deserialize)]
    struct Pkg {
        name: String,
        link: String,
        size: Option<u64>,
        checksum: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct Version {
        major: Option<u32>,
    }
    let assets: Vec<Asset> = serde_json::from_str(json).context("reading the Adoptium reply")?;
    let a = assets
        .into_iter()
        .find(|a| {
            a.binary.os == "windows"
                && a.binary.architecture == "x64"
                && a.binary.image_type == package.api()
                && a.binary
                    .package
                    .as_ref()
                    .is_some_and(|p| p.name.to_lowercase().ends_with(".zip"))
        })
        .with_context(|| format!("Adoptium has no Windows x64 Java {major} {}", package.api()))?;
    let p = a.binary.package.expect("checked above");
    let sha256 = p
        .checksum
        .map(|c| c.trim().to_lowercase())
        .filter(|c| is_sha256_hex(c))
        .context("Adoptium gave no SHA-256 for this download")?;
    Ok(JavaDownload {
        distribution: Distribution::Temurin,
        package,
        major: a.version.and_then(|v| v.major).unwrap_or(major),
        release: a.release_name,
        file_name: p.name,
        url: p.link,
        size: p.size,
        sha256,
    })
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn get_text(url: &str) -> anyhow::Result<String> {
    Ok(crate::cf::agent()
        .get(url)
        .call()
        .with_context(|| format!("GET {url}"))?
        .body_mut()
        .with_config()
        .limit(8 << 20)
        .read_to_string()?)
}

/// The latest Temurin `major` for Windows x64 (asks the Adoptium API; no download).
pub fn resolve_temurin(major: u32, package: Package) -> anyhow::Result<JavaDownload> {
    parse_adoptium(&get_text(&adoptium_url(major, package))?, major, package)
}

/// Oracle GraalVM's stable download URL for `major`.
pub fn graalvm_url(major: u32) -> String {
    format!(
        "https://download.oracle.com/graalvm/{major}/latest/graalvm-jdk-{major}_windows-x64_bin.zip"
    )
}

/// The first token of a `.sha256` file ("<hex>" or "<hex>  file.zip").
fn parse_sha256_file(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_lowercase();
    is_sha256_hex(&h).then_some(h)
}

/// The latest Oracle GraalVM JDK `major` (21 or 25): URL, size and SHA-256.
pub fn resolve_graalvm(major: u32) -> anyhow::Result<JavaDownload> {
    if !GRAALVM_MAJORS.contains(&major) {
        bail!("Oracle GraalVM isn't offered for Java {major} (only 21 and 25)");
    }
    let url = graalvm_url(major);
    let sha256 = parse_sha256_file(&get_text(&format!("{url}.sha256"))?)
        .context("Oracle's .sha256 file didn't hold a SHA-256")?;
    let size = crate::cf::agent().head(&url).call().ok().and_then(|r| {
        r.headers()
            .get("content-length")?
            .to_str()
            .ok()?
            .parse::<u64>()
            .ok()
    });
    Ok(JavaDownload {
        distribution: Distribution::OracleGraalVm,
        package: Package::Jdk,
        major,
        release: "latest".into(),
        file_name: url.rsplit('/').next().unwrap_or_default().to_string(),
        url,
        size,
        sha256,
    })
}

/// Downloads and unpacks Temurin `major` (if not already there); returns `java.exe`.
pub fn install_temurin(major: u32, package: Package, job: &Job) -> anyhow::Result<PathBuf> {
    job.detail(format!("Looking up Eclipse Temurin {major}…"));
    let dl = resolve_temurin(major, package)?;
    install(&dl, job)
}

/// Downloads and unpacks Oracle GraalVM `major` (21 or 25); returns `java.exe`.
pub fn install_graalvm(major: u32, job: &Job) -> anyhow::Result<PathBuf> {
    job.detail(format!("Looking up Oracle GraalVM {major}…"));
    let dl = resolve_graalvm(major)?;
    install(&dl, job)
}

const MARKER: &str = ".hopper-sha256";

/// A runtime already unpacked from the archive with this SHA-256.
fn already_installed(root: &Path, sha256: &str) -> Option<PathBuf> {
    std::fs::read_dir(root).ok()?.flatten().find_map(|e| {
        let dir = e.path();
        let mark = std::fs::read_to_string(dir.join(MARKER)).ok()?;
        let exe = dir.join("bin").join("java.exe");
        (mark.trim() == sha256 && exe.exists()).then_some(exe)
    })
}

/// Downloads `dl`, checks its SHA-256 and size, and unpacks it into
/// `<runtimes dir>/<archive's top folder>`. Returns that runtime's `java.exe`.
/// Reuses an earlier install of the same archive.
pub fn install(dl: &JavaDownload, job: &Job) -> anyhow::Result<PathBuf> {
    let root = crate::java::hopper_runtimes_dir().context("no data folder for Hopper")?;
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    if let Some(exe) = already_installed(&root, &dl.sha256) {
        job.detail(format!("{} is already installed", dl.label()));
        return Ok(exe);
    }

    let part = root.join(format!(".{}.part", dl.file_name));
    let result = (|| {
        job.detail(format!("Downloading {}…", dl.label()));
        let (len, sha) = download_hashing(&dl.url, &part, dl.size, job)?;
        if let Some(want) = dl.size
            && want != len
        {
            bail!("download is {len} bytes, expected {want}");
        }
        if sha != dl.sha256 {
            bail!("SHA-256 mismatch: got {sha}, expected {}", dl.sha256);
        }
        job.detail(format!("Unpacking {}…", dl.label()));
        unpack(&part, &root, &dl.sha256, job)
    })();
    let _ = std::fs::remove_file(&part);
    let home = result?;
    let exe = home.join("bin").join("java.exe");
    if !exe.exists() {
        bail!("the archive had no bin\\java.exe");
    }
    job.detail(format!("Installed {}", dl.label()));
    Ok(exe)
}

/// Streams `url` to `dest`, hashing as it goes. Returns (bytes, sha256 hex).
fn download_hashing(
    url: &str,
    dest: &Path,
    expected: Option<u64>,
    job: &Job,
) -> anyhow::Result<(u64, String)> {
    // The shared agent has a 60 s overall timeout; a 300 MB JDK needs longer.
    let resp = crate::cf::agent()
        .get(url)
        .config()
        .timeout_global(None)
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .timeout_recv_response(Some(std::time::Duration::from_secs(60)))
        .build()
        .call()
        .with_context(|| format!("download {url}"))?;
    let total = expected.or_else(|| {
        resp.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()?.parse().ok())
    });
    let mut reader = resp.into_body().into_reader();
    let mut out =
        std::fs::File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    let mut last_report = 0u64;
    loop {
        let n = reader.read(&mut buf).context("download interrupted")?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])?;
        done += n as u64;
        if done - last_report >= 1 << 20 {
            last_report = done;
            job.progress(done as usize, total.unwrap_or(0) as usize);
        }
    }
    out.flush()?;
    job.progress(done as usize, total.unwrap_or(done) as usize);
    Ok((done, hash.finish_hex()))
}

/// Unpacks `zip` (one top-level folder, as JDK archives have) into
/// `root/<that folder>`, via a staging folder. Returns the new home.
fn unpack(zip: &Path, root: &Path, sha256: &str, job: &Job) -> anyhow::Result<PathBuf> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?)
        .context("the download isn't a valid zip")?;
    let staging = root.join(format!(".unpack-{}", &sha256[..12]));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let n = archive.len();
    let mut top: Option<String> = None;
    let result = (|| {
        for i in 0..n {
            let mut f = archive.by_index(i)?;
            let Some(path) = f.enclosed_name() else {
                continue;
            };
            let mut comps = path.components();
            let first = comps
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            let rest: PathBuf = comps.collect();
            match &top {
                None => top = Some(first.clone()),
                Some(t) if *t != first => bail!("the archive has more than one top folder"),
                _ => {}
            }
            if rest.as_os_str().is_empty() {
                continue;
            }
            let out = staging.join(&rest);
            if f.is_dir() {
                std::fs::create_dir_all(&out)?;
            } else {
                if let Some(p) = out.parent() {
                    std::fs::create_dir_all(p)?;
                }
                let mut w = std::fs::File::create(&out)
                    .with_context(|| format!("creating {}", out.display()))?;
                std::io::copy(&mut f, &mut w)?;
            }
            if i % 64 == 0 {
                job.progress(i, n);
            }
        }
        job.progress(n, n);
        let top = top.clone().context("the archive is empty")?;
        let dest = root.join(sanitize(&top));
        if dest.exists() {
            // Ours (Hopper's folder), but left half-done or from another archive.
            std::fs::remove_dir_all(&dest)
                .with_context(|| format!("replacing {}", dest.display()))?;
        }
        std::fs::write(staging.join(MARKER), sha256)?;
        std::fs::rename(&staging, &dest)
            .with_context(|| format!("moving into {}", dest.display()))?;
        Ok(dest)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// A folder name safe to create (the archive's top folder, e.g. "jdk-21.0.4+7-jre").
fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "+-._".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('.').to_string();
    if s.is_empty() { "java".into() } else { s }
}

// ---------------------------------------------------------------------------
// SHA-256 (FIPS 180-4).

/// Streaming SHA-256.
#[derive(Clone)]
pub struct Sha256 {
    h: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub fn new() -> Self {
        Sha256 {
            h: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buf: [0; 64],
            buf_len: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let take = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + take].copy_from_slice(&data[..take]);
            self.buf_len += take;
            data = &data[take..];
            if self.buf_len < 64 {
                return;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        let mut chunks = data.chunks_exact(64);
        for block in &mut chunks {
            self.compress(block.try_into().expect("64 bytes"));
        }
        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }

    pub fn finish(mut self) -> [u8; 32] {
        let bits = self.total.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buf_len != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        let mut out = [0u8; 32];
        for (o, h) in out.chunks_exact_mut(4).zip(self.h) {
            o.copy_from_slice(&h.to_be_bytes());
        }
        out
    }

    pub fn finish_hex(self) -> String {
        self.finish().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// SHA-256 of `data`, lower-case hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    h.finish_hex()
}

/// SHA-256 of a file, lower-case hex.
pub fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finish_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256_hex(&million),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        // Odd-sized pieces give the same digest as one go.
        let mut h = Sha256::new();
        for piece in million.chunks(997) {
            h.update(piece);
        }
        assert_eq!(h.finish_hex(), sha256_hex(&million));
    }

    const SAMPLE: &str = r#"[
    {
        "binary": {
            "architecture": "x64",
            "download_count": 1188356,
            "heap_size": "normal",
            "image_type": "jre",
            "installer": {
                "checksum": "536eeb1dc3f743e5932a92e3549430fc75392c5d379101c1456a1c201aa6e66b",
                "link": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12.1%2B1/OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.msi",
                "name": "OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.msi",
                "size": 34963456
            },
            "jvm_impl": "hotspot",
            "os": "windows",
            "package": {
                "checksum": "D35F31E712F0FCF6AC5A093EDC90204FBFF22F720BA3950BD09D331D5E621636",
                "checksum_link": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12.1%2B1/OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.zip.sha256.txt",
                "link": "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12.1%2B1/OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.zip",
                "name": "OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.zip",
                "size": 48999141
            },
            "project": "jdk",
            "updated_at": "2026-08-21T11:43:59Z"
        },
        "release_link": "https://github.com/adoptium/temurin21-binaries/releases/tag/jdk-21.0.12.1%2B1",
        "release_name": "jdk-21.0.12.1+1",
        "vendor": "eclipse",
        "version": {
            "build": 1, "major": 21, "minor": 0, "patch": 1, "security": 12,
            "openjdk_version": "21.0.12.1+1-LTS",
            "semver": "21.0.12+101.0.LTS"
        }
    }
]"#;

    #[test]
    fn adoptium_parsing() {
        let d = parse_adoptium(SAMPLE, 21, Package::Jre).unwrap();
        assert_eq!(d.distribution, Distribution::Temurin);
        assert_eq!(d.major, 21);
        assert_eq!(d.release, "jdk-21.0.12.1+1");
        assert_eq!(
            d.file_name,
            "OpenJDK21U-jre_x64_windows_hotspot_21.0.12.1_1.zip"
        );
        assert!(d.url.ends_with(".zip") && d.url.starts_with("https://github.com/adoptium/"));
        assert_eq!(d.size, Some(48_999_141));
        // The zip's checksum (not the MSI's), lower-cased.
        assert_eq!(
            d.sha256,
            "d35f31e712f0fcf6ac5a093edc90204fbff22f720ba3950bd09d331d5e621636"
        );
        assert_eq!(d.label(), "Eclipse Temurin 21 JRE (jdk-21.0.12.1+1)");

        // Asked for a JDK but the reply only has a JRE.
        assert!(parse_adoptium(SAMPLE, 21, Package::Jdk).is_err());
        // Empty reply (unknown major) and junk.
        assert!(parse_adoptium("[]", 99, Package::Jre).is_err());
        assert!(parse_adoptium("<html>", 21, Package::Jre).is_err());
        // No checksum: refuse rather than install unverified.
        let no_sum = SAMPLE.replace(
            "\"checksum\": \"D35F31E712F0FCF6AC5A093EDC90204FBFF22F720BA3950BD09D331D5E621636\",",
            "",
        );
        assert!(parse_adoptium(&no_sum, 21, Package::Jre).is_err());
    }

    #[test]
    fn urls() {
        assert_eq!(
            adoptium_url(17, Package::Jdk),
            "https://api.adoptium.net/v3/assets/latest/17/hotspot?architecture=x64&image_type=jdk&os=windows&vendor=eclipse"
        );
        assert_eq!(
            graalvm_url(21),
            "https://download.oracle.com/graalvm/21/latest/graalvm-jdk-21_windows-x64_bin.zip"
        );
        assert_eq!(
            parse_sha256_file(
                "7560971E52B236DDBA7A22E7A682A9A185E56A2F6EE9CD6C4ECBB800E9643990  x.zip\n"
            )
            .as_deref(),
            Some("7560971e52b236ddba7a22e7a682a9a185e56a2f6ee9cd6c4ecbb800e9643990")
        );
        assert_eq!(parse_sha256_file("nope"), None);
        assert!(resolve_graalvm(17).is_err());
    }

    #[test]
    fn folder_names() {
        assert_eq!(sanitize("jdk-21.0.4+7-jre"), "jdk-21.0.4+7-jre");
        assert_eq!(
            sanitize("graalvm-jdk-21.0.11+9.1"),
            "graalvm-jdk-21.0.11+9.1"
        );
        assert_eq!(sanitize(".."), "java");
        assert_eq!(sanitize("a b:c"), "a_b_c");
    }

    #[test]
    fn unpack_strips_top_folder() {
        let dir = std::env::temp_dir().join(format!("hopper-javadl-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("j.zip");
        {
            let mut z = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
            let o = zip::write::SimpleFileOptions::default();
            z.add_directory("jdk-21.0.4+7-jre/", o).unwrap();
            z.start_file("jdk-21.0.4+7-jre/bin/java.exe", o).unwrap();
            z.write_all(b"MZ").unwrap();
            z.start_file("jdk-21.0.4+7-jre/release", o).unwrap();
            z.write_all(b"JAVA_VERSION=\"21.0.4\"\n").unwrap();
            z.finish().unwrap();
        }
        let root = dir.join("java");
        std::fs::create_dir_all(&root).unwrap();
        let jobs = crate::jobs::Jobs::default();
        let job = jobs.start("test");
        let sha = sha256_file(&zip_path).unwrap();
        let home = unpack(&zip_path, &root, &sha, &job).unwrap();
        assert_eq!(home, root.join("jdk-21.0.4+7-jre"));
        assert!(home.join("bin").join("java.exe").exists());
        assert_eq!(
            already_installed(&root, &sha),
            Some(home.join("bin").join("java.exe"))
        );
        assert_eq!(already_installed(&root, "0000"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Network: resolves (doesn't download) the latest Temurin 21 JRE.
    #[test]
    #[ignore]
    fn resolve_temurin_21_jre() {
        let d = resolve_temurin(21, Package::Jre).unwrap();
        println!("{d:#?}");
        assert_eq!(d.major, 21);
        assert!(d.url.starts_with("https://") && d.url.ends_with(".zip"));
        assert!(d.size.unwrap_or(0) > 10_000_000);
        assert_eq!(d.sha256.len(), 64);
    }

    /// Network: resolves Oracle GraalVM 21 (fetches the .sha256 and a HEAD only).
    #[test]
    #[ignore]
    fn resolve_graalvm_21() {
        let d = resolve_graalvm(21).unwrap();
        println!("{d:#?}");
        assert_eq!(d.sha256.len(), 64);
        assert!(d.size.unwrap_or(0) > 100_000_000);
    }
}
