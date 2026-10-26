//! Off-site copies of backups: every snapshot of a server can also go to a
//! local or network folder, an SFTP server, S3-compatible storage (AWS, B2,
//! R2, MinIO) or another Hopper Server.
//!
//! Only chunks the target doesn't have yet are sent, and everything is
//! encrypted on this PC first (ChaCha20-Poly1305 with the panel's backup
//! key): the target sees random names and random bytes. Chunk names are an
//! HMAC of the chunk's hash, so nobody can test whether a known file is in
//! there.
//!
//! Targets and their passwords live in `backup-targets.json` in the panel's
//! data folder; passwords never go back to the browser and are never logged.
//! The key is `backup-key`, shown once when it's made so it can be kept
//! somewhere safe (it's what opens the copies if this PC is lost).
//!
//! Remote layout: `<prefix>/hopper-backups/<panel>/<server>/c/<xx>/<chunk>`
//! and `…/s/<snapshot>`.

use crate::App;
use anyhow::{Context as _, bail};
use hopper_core::hosting::backup;
use hopper_core::hosting::chunkstore::{self, Manifest, Store};
use ring::aead;
use ring::hmac;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

const TARGETS: &str = "backup-targets.json";
const COPIES: &str = "backup-copies.json";
const KEY: &str = "backup-key";
const STATE_DIR: &str = "offsite";
/// Bytes sent per request to batching targets.
const BATCH: usize = 16 << 20;

// ---- targets -------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Target {
    pub id: String,
    pub name: String,
    /// folder, sftp, s3, hopper.
    pub kind: String,
    pub enabled: bool,
    // folder
    pub path: String,
    // sftp
    pub host: String,
    pub port: u16,
    pub user: String,
    pub dir: String,
    /// The SFTP server's key ("SHA256:…"), remembered on first connect.
    pub host_key: String,
    // s3
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key: String,
    // hopper
    pub url: String,
    /// The password, S3 secret key or receive key. Never sent to the browser.
    pub secret: String,
}

impl Target {
    /// What the browser sees: everything but the secret.
    pub fn view(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(o) = v.as_object_mut() {
            o.remove("secret");
            o.insert("secret_set".into(), json!(!self.secret.is_empty()));
        }
        v
    }

    /// Where it points, for lists ("\\nas\backups", "sftp://u@h:22/dir").
    pub fn place(&self) -> String {
        match self.kind.as_str() {
            "folder" => self.path.clone(),
            "sftp" => format!("sftp://{}@{}:{}/{}", self.user, self.host, self.port, self.dir.trim_start_matches('/')),
            "s3" => format!("{}/{}/{}", self.endpoint.trim_end_matches('/'), self.bucket, self.prefix).trim_end_matches('/').to_string(),
            _ => self.url.clone(),
        }
    }

    fn check(&self) -> anyhow::Result<()> {
        match self.kind.as_str() {
            "folder" if self.path.trim().is_empty() => bail!("Pick a folder."),
            "sftp" if self.host.trim().is_empty() || self.user.trim().is_empty() => bail!("SFTP needs a host and a user."),
            "s3" if self.endpoint.trim().is_empty() || self.bucket.trim().is_empty() || self.access_key.trim().is_empty() => {
                bail!("S3 needs an endpoint, a bucket and a key.")
            }
            "hopper" if !(self.url.starts_with("http://") || self.url.starts_with("https://")) => bail!("Give the other panel's address, e.g. http://192.168.50.7:8790"),
            "folder" | "sftp" | "s3" | "hopper" => Ok(()),
            _ => bail!("Unknown kind of target."),
        }
    }
}

pub fn load_targets(dir: &Path) -> Vec<Target> {
    std::fs::read(dir.join(TARGETS)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn save_targets(dir: &Path, t: &[Target]) -> anyhow::Result<()> {
    write_private(&dir.join(TARGETS), &serde_json::to_vec_pretty(t)?)
}

/// Which targets each server's backups go to.
pub fn load_copies(dir: &Path) -> BTreeMap<String, Vec<String>> {
    std::fs::read(dir.join(COPIES)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save_copies(dir: &Path, c: &BTreeMap<String, Vec<String>>) -> anyhow::Result<()> {
    write_private(&dir.join(COPIES), &serde_json::to_vec_pretty(c)?)
}

// ---- encryption ----------------------------------------------------------------

pub struct Crypto {
    names: hmac::Key,
    seal: aead::LessSafeKey,
    /// The panel's folder name on targets.
    pub panel: String,
}

fn derive(master: &[u8], label: &str) -> [u8; 32] {
    let k = hmac::Key::new(hmac::HMAC_SHA256, master);
    hmac::sign(&k, label.as_bytes()).as_ref().try_into().unwrap()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

impl Crypto {
    pub fn new(master: &[u8]) -> Self {
        let seal = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::CHACHA20_POLY1305, &derive(master, "hopper-backups seal")).unwrap());
        Crypto {
            names: hmac::Key::new(hmac::HMAC_SHA256, &derive(master, "hopper-backups names")),
            seal,
            panel: format!("p-{}", &chunkstore::hex(&derive(master, "hopper-backups panel"))[..12]),
        }
    }

    /// A chunk's name on targets.
    pub fn chunk_name(&self, hash: &str) -> String {
        chunkstore::hex(hmac::sign(&self.names, hash.as_bytes()).as_ref())
    }

    /// `HOP1` + nonce + sealed bytes; `name` is bound in, so blobs can't be
    /// swapped.
    pub fn seal(&self, name: &str, plain: &[u8]) -> anyhow::Result<Vec<u8>> {
        let mut nonce = [0u8; 12];
        SystemRandom::new().fill(&mut nonce).map_err(|_| anyhow::anyhow!("no randomness"))?;
        let mut buf = plain.to_vec();
        self.seal
            .seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::from(name.as_bytes()), &mut buf)
            .map_err(|_| anyhow::anyhow!("encrypting failed"))?;
        let mut out = Vec::with_capacity(16 + buf.len());
        out.extend_from_slice(b"HOP1");
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&buf);
        Ok(out)
    }

    pub fn open(&self, name: &str, blob: &[u8]) -> anyhow::Result<Vec<u8>> {
        if blob.len() < 16 + 16 || &blob[..4] != b"HOP1" {
            bail!("not a Hopper backup copy");
        }
        let nonce: [u8; 12] = blob[4..16].try_into().unwrap();
        let mut buf = blob[16..].to_vec();
        let plain = self
            .seal
            .open_in_place(aead::Nonce::assume_unique_for_key(nonce), aead::Aad::from(name.as_bytes()), &mut buf)
            .map_err(|_| anyhow::anyhow!("This copy can't be opened with this panel's backup key."))?;
        Ok(plain.to_vec())
    }
}

/// The panel's backup key, made on first use. `fresh` is true when it was
/// just made (show it once).
pub fn key(dir: &Path) -> anyhow::Result<(Vec<u8>, bool)> {
    let path = dir.join(KEY);
    if let Some(k) = std::fs::read_to_string(&path).ok().and_then(|s| unhex(&s)).filter(|k| k.len() == 32) {
        return Ok((k, false));
    }
    let mut k = [0u8; 32];
    SystemRandom::new().fill(&mut k).map_err(|_| anyhow::anyhow!("no randomness"))?;
    write_private(&path, chunkstore::hex(&k).as_bytes())?;
    Ok((k.to_vec(), true))
}

pub fn has_key(dir: &Path) -> bool {
    dir.join(KEY).is_file()
}

/// Puts in a key from another panel (to open its copies). Only before this
/// panel has made its own copies.
pub fn import_key(dir: &Path, hex_key: &str) -> anyhow::Result<()> {
    let k = unhex(hex_key.trim()).filter(|k| k.len() == 32).context("A backup key is 64 letters and digits (0-9, a-f).")?;
    if has_key(dir) && dir.join(STATE_DIR).is_dir() {
        bail!("This panel already copies backups with its own key.");
    }
    write_private(&dir.join(KEY), chunkstore::hex(&k).as_bytes())
}

fn crypto(dir: &Path) -> anyhow::Result<Crypto> {
    Ok(Crypto::new(&key(dir)?.0))
}

// ---- backends --------------------------------------------------------------------

/// Somewhere copies go. Keys are `/`-separated paths.
pub trait Backend {
    fn put_many(&mut self, items: &[(String, Vec<u8>)]) -> anyhow::Result<()>;
    fn get(&mut self, key: &str) -> anyhow::Result<Vec<u8>>;
    /// Several keys at once (one request where the target allows).
    fn get_many(&mut self, keys: &[String]) -> anyhow::Result<Vec<Vec<u8>>> {
        keys.iter().map(|k| self.get(k)).collect()
    }
    /// Keys under `prefix` (full keys).
    fn list(&mut self, prefix: &str) -> anyhow::Result<Vec<String>>;
    fn delete_many(&mut self, keys: &[String]) -> anyhow::Result<()>;
}

fn safe_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() < 512
        && k.split('/').all(|s| !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)))
}

pub fn open_backend(t: &Target) -> anyhow::Result<Box<dyn Backend>> {
    Ok(match t.kind.as_str() {
        "folder" => Box::new(Folder(PathBuf::from(t.path.trim()))),
        "s3" => Box::new(S3::new(t)?),
        "hopper" => Box::new(HopperTarget { base: t.url.trim_end_matches('/').to_string(), key: t.secret.clone() }),
        "sftp" => Box::new(Sftp::connect(t)?),
        _ => bail!("Unknown kind of target."),
    })
}

/// A local or network folder.
pub struct Folder(pub PathBuf);

impl Backend for Folder {
    fn put_many(&mut self, items: &[(String, Vec<u8>)]) -> anyhow::Result<()> {
        for (k, v) in items {
            if !safe_key(k) {
                bail!("bad key");
            }
            let p = self.0.join(k);
            std::fs::create_dir_all(p.parent().unwrap()).with_context(|| format!("making {}", p.parent().unwrap().display()))?;
            write_private(&p, v).with_context(|| format!("writing {}", p.display()))?;
        }
        Ok(())
    }
    fn get(&mut self, key: &str) -> anyhow::Result<Vec<u8>> {
        if !safe_key(key) {
            bail!("bad key");
        }
        std::fs::read(self.0.join(key)).with_context(|| format!("{key} isn't in the copy"))
    }
    fn list(&mut self, prefix: &str) -> anyhow::Result<Vec<String>> {
        fn walk(base: &Path, p: &Path, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(p) else { return };
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    walk(base, &path, out);
                } else if let Ok(rel) = path.strip_prefix(base) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.0, &self.0.join(prefix.trim_end_matches('/')), &mut out);
        Ok(out)
    }
    fn delete_many(&mut self, keys: &[String]) -> anyhow::Result<()> {
        for k in keys.iter().filter(|k| safe_key(k)) {
            let _ = std::fs::remove_file(self.0.join(k));
        }
        Ok(())
    }
}

fn agent(timeout: u64) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout)))
        .user_agent(concat!("hopperd/", env!("CARGO_PKG_VERSION")))
        .http_status_as_error(false)
        .build()
        .into()
}

/// Many key/value pairs in one body: `u16 key length, key, u32 length, data`.
pub fn pack(items: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(items.iter().map(|(k, v)| 6 + k.len() + v.len()).sum());
    for (k, v) in items {
        out.extend_from_slice(&(k.len() as u16).to_le_bytes());
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        out.extend_from_slice(v);
    }
    out
}

pub fn unpack(mut b: &[u8]) -> anyhow::Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    while !b.is_empty() {
        if b.len() < 2 {
            bail!("cut short");
        }
        let kl = u16::from_le_bytes([b[0], b[1]]) as usize;
        if b.len() < 2 + kl + 4 {
            bail!("cut short");
        }
        let k = String::from_utf8(b[2..2 + kl].to_vec())?;
        let vl = u32::from_le_bytes(b[2 + kl..6 + kl].try_into().unwrap()) as usize;
        if b.len() < 6 + kl + vl {
            bail!("cut short");
        }
        out.push((k, b[6 + kl..6 + kl + vl].to_vec()));
        b = &b[6 + kl + vl..];
    }
    Ok(out)
}

/// Another Hopper Server that lets this one copy backups to it.
pub struct HopperTarget {
    base: String,
    key: String,
}

impl HopperTarget {
    fn call(&self, what: &str, body: &[u8], json_body: bool) -> anyhow::Result<Vec<u8>> {
        let mut r = agent(300)
            .post(&format!("{}/api/v1/backup-store/{what}", self.base))
            .header("X-Backup-Key", &self.key)
            .header("Content-Type", if json_body { "application/json" } else { "application/octet-stream" })
            .send(body)
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.base))?;
        let status = r.status().as_u16();
        let bytes = r.body_mut().with_config().limit(256 << 20).read_to_vec()?;
        if status != 200 {
            let msg = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v["error"].as_str().map(str::to_string));
            bail!("{} answered HTTP {status}{}", self.base, msg.map(|m| format!(": {m}")).unwrap_or_default());
        }
        Ok(bytes)
    }
}

impl Backend for HopperTarget {
    fn put_many(&mut self, items: &[(String, Vec<u8>)]) -> anyhow::Result<()> {
        self.call("put", &pack(items), false).map(|_| ())
    }
    fn get(&mut self, key: &str) -> anyhow::Result<Vec<u8>> {
        let b = self.call("get", &serde_json::to_vec(&json!({ "keys": [key] }))?, true)?;
        unpack(&b)?.into_iter().next().map(|(_, v)| v).context("not in the copy")
    }
    fn get_many(&mut self, keys: &[String]) -> anyhow::Result<Vec<Vec<u8>>> {
        let b = self.call("get", &serde_json::to_vec(&json!({ "keys": keys }))?, true)?;
        let got: HashMap<String, Vec<u8>> = unpack(&b)?.into_iter().collect();
        keys.iter().map(|k| got.get(k).cloned().with_context(|| format!("{k} isn't in the copy"))).collect()
    }
    fn list(&mut self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let b = self.call("list", &serde_json::to_vec(&json!({ "prefix": prefix }))?, true)?;
        Ok(serde_json::from_slice::<Value>(&b)?["keys"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect())
    }
    fn delete_many(&mut self, keys: &[String]) -> anyhow::Result<()> {
        self.call("delete", &serde_json::to_vec(&json!({ "keys": keys }))?, true).map(|_| ())
    }
}

// ---- S3 ----------------------------------------------------------------------------

fn sha256_hex(b: &[u8]) -> String {
    use sha2::Digest as _;
    chunkstore::hex(&sha2::Sha256::digest(b))
}

fn hmac256(key: &[u8], msg: &str) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), msg.as_bytes()).as_ref().to_vec()
}

fn uri_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || (keep_slash && b == b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// AWS Signature V4 for S3. `headers` are lower-case and include `host`,
/// `x-amz-content-sha256` and `x-amz-date`. Returns the Authorization value.
#[allow(clippy::too_many_arguments)]
pub fn sigv4(method: &str, path: &str, query: &[(String, String)], headers: &[(String, String)], payload_hash: &str, access: &str, secret: &str, region: &str, amz_date: &str) -> String {
    let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    q.sort();
    let canon_q = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut h: Vec<&(String, String)> = headers.iter().collect();
    h.sort_by(|a, b| a.0.cmp(&b.0));
    let canon_h: String = h.iter().map(|(k, v)| format!("{k}:{}\n", v.trim())).collect();
    let signed = h.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let creq = format!("{method}\n{}\n{canon_q}\n{canon_h}\n{signed}\n{payload_hash}", uri_encode(path, true));
    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/s3/aws4_request");
    let sts = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(creq.as_bytes()));
    let k = hmac256(format!("AWS4{secret}").as_bytes(), date);
    let k = hmac256(&k, region);
    let k = hmac256(&k, "s3");
    let k = hmac256(&k, "aws4_request");
    let sig = chunkstore::hex(&hmac256(&k, &sts));
    format!("AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={sig}")
}

pub struct S3 {
    base: String,
    host: String,
    bucket: String,
    prefix: String,
    region: String,
    access: String,
    secret: String,
    agent: ureq::Agent,
}

/// The region from an endpoint like `s3.eu-central-003.backblazeb2.com` or
/// `s3.us-west-2.amazonaws.com`; R2 and others use "auto".
pub fn guess_region(endpoint: &str) -> String {
    let host = endpoint.split("://").last().unwrap_or(endpoint).split('/').next().unwrap_or("");
    if host.ends_with(".r2.cloudflarestorage.com") {
        return "auto".into();
    }
    let parts: Vec<&str> = host.split('.').collect();
    match parts.as_slice() {
        ["s3", region, ..] if parts.len() >= 4 && region.contains('-') => region.to_string(),
        [_, "s3", region, ..] if region.contains('-') => region.to_string(),
        _ if host.ends_with("amazonaws.com") => "us-east-1".into(),
        _ => "auto".into(),
    }
}

impl S3 {
    fn new(t: &Target) -> anyhow::Result<Self> {
        let ep = t.endpoint.trim().trim_end_matches('/');
        let base = if ep.contains("://") { ep.to_string() } else { format!("https://{ep}") };
        let host = base.split("://").nth(1).unwrap_or("").split('/').next().unwrap_or("").to_string();
        if host.is_empty() {
            bail!("That endpoint isn't an address.");
        }
        let region = if t.region.trim().is_empty() { guess_region(&base) } else { t.region.trim().to_string() };
        Ok(S3 {
            base,
            host,
            bucket: t.bucket.trim().to_string(),
            prefix: t.prefix.trim().trim_matches('/').to_string(),
            region,
            access: t.access_key.trim().to_string(),
            secret: t.secret.clone(),
            agent: agent(120),
        })
    }

    fn full(&self, key: &str) -> String {
        if self.prefix.is_empty() { key.to_string() } else { format!("{}/{key}", self.prefix) }
    }

    fn request(&self, method: &str, key: Option<&str>, query: &[(String, String)], body: &[u8]) -> anyhow::Result<(u16, Vec<u8>)> {
        let path = match key {
            Some(k) => format!("/{}/{}", self.bucket, self.full(k)),
            None => format!("/{}", self.bucket),
        };
        let now = time::OffsetDateTime::now_utc();
        let amz = format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", now.year(), now.month() as u8, now.day(), now.hour(), now.minute(), now.second());
        let ph = sha256_hex(body);
        let headers = vec![("host".to_string(), self.host.clone()), ("x-amz-content-sha256".into(), ph.clone()), ("x-amz-date".into(), amz.clone())];
        let auth = sigv4(method, &path, query, &headers, &ph, &self.access, &self.secret, &self.region, &amz);
        let qs = query.iter().map(|(k, v)| format!("{}={}", uri_encode(k, false), uri_encode(v, false))).collect::<Vec<_>>().join("&");
        let url = format!("{}{}{}", self.base, uri_encode(&path, true), if qs.is_empty() { String::new() } else { format!("?{qs}") });
        let r = match method {
            "PUT" => self.agent.put(&url).header("x-amz-content-sha256", &ph).header("x-amz-date", &amz).header("Authorization", &auth).send(body),
            "DELETE" => self.agent.delete(&url).header("x-amz-content-sha256", &ph).header("x-amz-date", &amz).header("Authorization", &auth).call(),
            _ => self.agent.get(&url).header("x-amz-content-sha256", &ph).header("x-amz-date", &amz).header("Authorization", &auth).call(),
        };
        let mut r = r.map_err(|e| anyhow::anyhow!("{}: {e}", self.host))?;
        let status = r.status().as_u16();
        let bytes = r.body_mut().with_config().limit(64 << 20).read_to_vec()?;
        Ok((status, bytes))
    }

    fn fail(&self, status: u16, body: &[u8]) -> anyhow::Error {
        let text = String::from_utf8_lossy(body);
        let code = text.split("<Code>").nth(1).and_then(|r| r.split("</Code>").next()).unwrap_or("");
        anyhow::anyhow!("{} answered HTTP {status}{}", self.host, if code.is_empty() { String::new() } else { format!(" ({code})") })
    }
}

impl Backend for S3 {
    fn put_many(&mut self, items: &[(String, Vec<u8>)]) -> anyhow::Result<()> {
        for (k, v) in items {
            let (s, b) = self.request("PUT", Some(k), &[], v)?;
            if s != 200 {
                return Err(self.fail(s, &b));
            }
        }
        Ok(())
    }
    fn get(&mut self, key: &str) -> anyhow::Result<Vec<u8>> {
        let (s, b) = self.request("GET", Some(key), &[], b"")?;
        if s != 200 {
            return Err(self.fail(s, &b));
        }
        Ok(b)
    }
    fn list(&mut self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        let strip = if self.prefix.is_empty() { String::new() } else { format!("{}/", self.prefix) };
        loop {
            let mut q = vec![("list-type".to_string(), "2".to_string()), ("prefix".to_string(), self.full(prefix))];
            if let Some(t) = &token {
                q.push(("continuation-token".into(), t.clone()));
            }
            let (s, b) = self.request("GET", None, &q, b"")?;
            if s != 200 {
                return Err(self.fail(s, &b));
            }
            let text = String::from_utf8_lossy(&b);
            for part in text.split("<Key>").skip(1) {
                if let Some(k) = part.split("</Key>").next() {
                    let k = k.replace("&amp;", "&");
                    out.push(k.strip_prefix(&strip).unwrap_or(&k).to_string());
                }
            }
            token = text.split("<NextContinuationToken>").nth(1).and_then(|r| r.split("</NextContinuationToken>").next()).map(|t| t.replace("&amp;", "&"));
            if !text.contains("<IsTruncated>true</IsTruncated>") || token.is_none() {
                return Ok(out);
            }
        }
    }
    fn delete_many(&mut self, keys: &[String]) -> anyhow::Result<()> {
        for k in keys {
            let (s, b) = self.request("DELETE", Some(k), &[], b"")?;
            if !(200..300).contains(&s) && s != 404 {
                return Err(self.fail(s, &b));
            }
        }
        Ok(())
    }
}

// ---- SFTP -------------------------------------------------------------------------

struct SshCheck {
    want: String,
    seen: Arc<Mutex<String>>,
}

impl russh::client::Handler for SshCheck {
    type Error = russh::Error;
    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let fp = match key {
            russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } => key.fingerprint(russh::keys::HashAlg::Sha256).to_string(),
            russh::keys::PublicKeyOrCertificate::Certificate(c) => c.public_key().fingerprint(russh::keys::HashAlg::Sha256).to_string(),
        };
        *self.seen.lock().unwrap() = fp.clone();
        // The first connection learns the key; later ones must match it.
        Ok(self.want.is_empty() || self.want == fp)
    }
}

pub struct Sftp {
    rt: tokio::runtime::Runtime,
    sftp: russh_sftp::client::SftpSession,
    /// The SSH connection the session runs over.
    _ssh: russh::client::Handle<SshCheck>,
    dir: String,
    /// The server's key, as seen on connect.
    pub host_key: String,
    made: HashSet<String>,
}

impl Sftp {
    pub fn connect(t: &Target) -> anyhow::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let seen = Arc::new(Mutex::new(String::new()));
        let check = SshCheck { want: t.host_key.clone(), seen: seen.clone() };
        let port = if t.port == 0 { 22 } else { t.port };
        let (host, user, pass) = (t.host.trim().to_string(), t.user.trim().to_string(), t.secret.clone());
        let sftp = rt.block_on(async move {
            let cfg = Arc::new(russh::client::Config { inactivity_timeout: Some(Duration::from_secs(120)), ..Default::default() });
            let mut h = tokio::time::timeout(Duration::from_secs(20), russh::client::connect(cfg, (host.as_str(), port), check))
                .await
                .map_err(|_| anyhow::anyhow!("{host}:{port} didn't answer"))?
                .map_err(|e| anyhow::anyhow!("{host}:{port}: {e}"))?;
            match h.authenticate_password(user, pass).await? {
                russh::client::AuthResult::Success => {}
                _ => bail!("The SFTP server refused the user name or password."),
            }
            let ch = h.channel_open_session().await?;
            ch.request_subsystem(true, "sftp").await?;
            let s = russh_sftp::client::SftpSession::new(ch.into_stream()).await?;
            anyhow::Ok((s, h))
        });
        let host_key = seen.lock().unwrap().clone();
        let (sftp, ssh) = sftp.map_err(|e| {
            if !t.host_key.is_empty() && !host_key.is_empty() && host_key != t.host_key {
                anyhow::anyhow!("The SFTP server's key changed (now {host_key}). If that's expected, edit the target and save it again.")
            } else {
                e
            }
        })?;
        Ok(Sftp { rt, sftp, _ssh: ssh, dir: t.dir.trim().trim_end_matches('/').to_string(), host_key, made: HashSet::new() })
    }

    fn path(&self, key: &str) -> String {
        if self.dir.is_empty() { key.to_string() } else { format!("{}/{key}", self.dir) }
    }

    fn mkdirs(&mut self, dir: &str) -> anyhow::Result<()> {
        if dir.is_empty() || self.made.contains(dir) {
            return Ok(());
        }
        let mut cur = String::new();
        for part in dir.split('/') {
            if part.is_empty() {
                cur.push('/');
                continue;
            }
            if !cur.is_empty() && !cur.ends_with('/') {
                cur.push('/');
            }
            cur.push_str(part);
            if self.made.contains(&cur) {
                continue;
            }
            let c = cur.clone();
            let exists = self.rt.block_on(self.sftp.try_exists(c.clone())).unwrap_or(false);
            if !exists {
                self.rt.block_on(self.sftp.create_dir(c.clone())).with_context(|| format!("making {c}"))?;
            }
            self.made.insert(c);
        }
        Ok(())
    }
}

impl Backend for Sftp {
    fn put_many(&mut self, items: &[(String, Vec<u8>)]) -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt as _;
        for (k, v) in items {
            if !safe_key(k) {
                bail!("bad key");
            }
            let p = self.path(k);
            let parent = p.rsplit_once('/').map(|x| x.0.to_string()).unwrap_or_default();
            self.mkdirs(&parent)?;
            let tmp = format!("{p}.part");
            self.rt.block_on(async {
                let mut f = self.sftp.create(tmp.clone()).await?;
                f.write_all(&v[..]).await?;
                f.shutdown().await?;
                let _ = self.sftp.remove_file(p.clone()).await;
                self.sftp.rename(tmp.clone(), p.clone()).await?;
                anyhow::Ok(())
            })
            .with_context(|| format!("writing {p}"))?;
        }
        Ok(())
    }
    fn get(&mut self, key: &str) -> anyhow::Result<Vec<u8>> {
        use tokio::io::AsyncReadExt as _;
        let p = self.path(key);
        self.rt.block_on(async {
            let mut f = self.sftp.open(p.clone()).await?;
            let mut out = Vec::new();
            f.read_to_end(&mut out).await?;
            anyhow::Ok(out)
        })
    }
    fn list(&mut self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        let mut todo = vec![prefix.trim_end_matches('/').to_string()];
        while let Some(rel) = todo.pop() {
            let Ok(rd) = self.rt.block_on(self.sftp.read_dir(self.path(&rel))) else { continue };
            for e in rd {
                let name = e.file_name();
                if name == "." || name == ".." || name.ends_with(".part") {
                    continue;
                }
                let k = format!("{rel}/{name}");
                if e.file_type().is_dir() { todo.push(k) } else { out.push(k) }
            }
        }
        Ok(out)
    }
    fn delete_many(&mut self, keys: &[String]) -> anyhow::Result<()> {
        for k in keys.iter().filter(|k| safe_key(k)) {
            let _ = self.rt.block_on(self.sftp.remove_file(self.path(k)));
        }
        Ok(())
    }
}

/// Opens a target for copying. An SFTP server's key is remembered on the
/// first connection (when the target was saved without a Test).
fn open_pinned(dir: &Path, t: &Target) -> anyhow::Result<Box<dyn Backend>> {
    if t.kind != "sftp" || !t.host_key.is_empty() {
        return open_backend(t);
    }
    let s = Sftp::connect(t)?;
    if !s.host_key.is_empty() {
        let mut all = load_targets(dir);
        if let Some(x) = all.iter_mut().find(|x| x.id == t.id && x.host_key.is_empty()) {
            x.host_key = s.host_key.clone();
            save_targets(dir, &all)?;
            log::info!("off-site: remembered {}'s SSH key {}", t.name, s.host_key);
        }
    }
    Ok(Box::new(s))
}

/// Connects, writes, reads back and deletes a test file. Returns what the
/// target looked like (an SFTP server's key, for pinning).
pub fn test(t: &Target) -> anyhow::Result<Option<String>> {
    t.check()?;
    let probe = format!("hopper-backups/probe-{}", crate::webhooks::now());
    let body = b"Hopper Server checks it can write here.".to_vec();
    let mut host_key = None;
    let mut b: Box<dyn Backend> = if t.kind == "sftp" {
        let s = Sftp::connect(t)?;
        host_key = Some(s.host_key.clone());
        Box::new(s)
    } else {
        open_backend(t)?
    };
    b.put_many(&[(probe.clone(), body.clone())])?;
    let back = b.get(&probe)?;
    b.delete_many(std::slice::from_ref(&probe))?;
    if back != body {
        bail!("What came back wasn't what was written.");
    }
    Ok(host_key)
}

// ---- copying -------------------------------------------------------------------

/// Where each (target, server) stands.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    /// Snapshots fully copied.
    pub done: Vec<String>,
    pub last_ok: Option<u64>,
    pub last_error: Option<String>,
    /// Snapshots still to copy, on the last look.
    pub pending: usize,
    /// What's being copied right now.
    pub working: Option<String>,
}

fn state_dir(dir: &Path) -> PathBuf {
    dir.join(STATE_DIR)
}

fn status_path(dir: &Path, target: &str, server: &str) -> PathBuf {
    state_dir(dir).join(target).join(format!("{server}.json"))
}

fn index_path(dir: &Path, target: &str, server: &str) -> PathBuf {
    state_dir(dir).join(target).join(format!("{server}.chunks"))
}

pub fn status(dir: &Path, target: &str, server: &str) -> Status {
    let mut s: Status = std::fs::read(status_path(dir, target, server)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if let Some(w) = working().lock().unwrap().get(&(target.to_string(), server.to_string())) {
        s.working = Some(w.clone());
    }
    s
}

fn save_status(dir: &Path, target: &str, server: &str, s: &Status) -> anyhow::Result<()> {
    let p = status_path(dir, target, server);
    std::fs::create_dir_all(p.parent().unwrap())?;
    write_private(&p, &serde_json::to_vec_pretty(s)?)
}

fn load_index(dir: &Path, target: &str, server: &str) -> HashSet<String> {
    std::fs::read_to_string(index_path(dir, target, server)).map(|s| s.lines().map(str::to_string).collect()).unwrap_or_default()
}

fn working() -> &'static Mutex<HashMap<(String, String), String>> {
    static W: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();
    W.get_or_init(Default::default)
}

fn remote_root(c: &Crypto, server: &str) -> String {
    format!("hopper-backups/{}/{server}", c.panel)
}

/// Copies every snapshot of `root` that `target` doesn't have yet, newest
/// first, then mirrors deletions (see [`mirror_deletions`]). Returns how
/// many snapshots were copied.
pub fn sync_one(dir: &Path, t: &Target, server: &str, root: &Path, c: &Crypto) -> anyhow::Result<usize> {
    let store = backup::store(root);
    let local: Vec<backup::Backup> = backup::list(root).into_iter().filter(|b| !b.is_zip()).collect();
    let mut st = status(dir, &t.id, server);
    st.working = None;
    let todo: Vec<&backup::Backup> = local.iter().filter(|b| !st.done.contains(&b.name)).collect();
    let base = remote_root(c, server);
    let mut be: Option<Box<dyn Backend>> = None;
    let mut copied = 0;
    if !todo.is_empty() {
        st.pending = todo.len();
        save_status(dir, &t.id, server, &st)?;
        let mut have = load_index(dir, &t.id, server);
        let b = be.insert(open_pinned(dir, t)?);
        for snap in todo {
            working().lock().unwrap().insert((t.id.clone(), server.to_string()), snap.name.clone());
            let m = store.manifest(&snap.name)?;
            let mut want: Vec<&String> = Vec::new();
            let mut seen = HashSet::new();
            for f in &m.files {
                for h in &f.chunks {
                    if !have.contains(h) && seen.insert(h) {
                        want.push(h);
                    }
                }
            }
            let mut batch: Vec<(String, Vec<u8>)> = Vec::new();
            let mut hashes: Vec<String> = Vec::new();
            let mut size = 0;
            let flush = |b: &mut Box<dyn Backend>, batch: &mut Vec<(String, Vec<u8>)>, hashes: &mut Vec<String>, have: &mut HashSet<String>| -> anyhow::Result<()> {
                if batch.is_empty() {
                    return Ok(());
                }
                b.put_many(batch)?;
                let p = index_path(dir, &t.id, server);
                std::fs::create_dir_all(p.parent().unwrap())?;
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
                for h in hashes.drain(..) {
                    writeln!(f, "{h}")?;
                    have.insert(h);
                }
                batch.clear();
                Ok(())
            };
            for h in want {
                let raw = store.raw_chunk(h)?;
                let name = c.chunk_name(h);
                let key = format!("{base}/c/{}/{name}", &name[..2]);
                size += raw.len();
                batch.push((key.clone(), c.seal(&key, &raw)?));
                hashes.push(h.clone());
                if size >= BATCH || batch.len() >= 2000 {
                    flush(b, &mut batch, &mut hashes, &mut have)?;
                    size = 0;
                }
            }
            flush(b, &mut batch, &mut hashes, &mut have)?;
            let key = format!("{base}/s/{}", snap.name);
            b.put_many(&[(key.clone(), c.seal(&key, &serde_json::to_vec(&m)?)?)])?;
            st.done.push(snap.name.clone());
            st.pending = st.pending.saturating_sub(1);
            st.last_ok = Some(crate::webhooks::now());
            st.last_error = None;
            save_status(dir, &t.id, server, &st)?;
            copied += 1;
            log::info!("off-site: {} copied to {}", snap.name, t.name);
        }
        working().lock().unwrap().remove(&(t.id.clone(), server.to_string()));
    }
    mirror_deletions(dir, t, server, root, c, &local, &mut st, &mut be)?;
    st.pending = 0;
    st.last_error = None;
    st.last_ok = Some(crate::webhooks::now());
    save_status(dir, &t.id, server, &st)?;
    Ok(copied)
}

/// Snapshots thinned or deleted here go from the target too, but only ones
/// older than a snapshot that's still here: if the backups folder is lost
/// or emptied, the copies stay. Chunks no copied snapshot uses are then
/// deleted from the target.
fn mirror_deletions(
    dir: &Path,
    t: &Target,
    server: &str,
    root: &Path,
    c: &Crypto,
    local: &[backup::Backup],
    st: &mut Status,
    be: &mut Option<Box<dyn Backend>>,
) -> anyhow::Result<()> {
    let newest = local.iter().map(|b| b.at).max().unwrap_or(0);
    let names: HashSet<&str> = local.iter().map(|b| b.name.as_str()).collect();
    // A snapshot's time is in its name ("2026-10-26_04-00-00-…"); compare
    // names, which sort by time.
    let newest_name = local.iter().max_by_key(|b| b.at).map(|b| b.name.clone()).unwrap_or_default();
    let gone: Vec<String> = st.done.iter().filter(|n| !names.contains(n.as_str()) && newest > 0 && n.as_str() < newest_name.as_str()).cloned().collect();
    if gone.is_empty() {
        return Ok(());
    }
    let base = remote_root(c, server);
    let b = match be {
        Some(b) => b,
        None => be.insert(open_pinned(dir, t)?),
    };
    b.delete_many(&gone.iter().map(|n| format!("{base}/s/{n}")).collect::<Vec<_>>())?;
    st.done.retain(|n| !gone.contains(n));
    save_status(dir, &t.id, server, st)?;
    // Clean up chunks only when every copied snapshot is still readable here.
    if st.done.iter().any(|n| !names.contains(n.as_str())) {
        return Ok(());
    }
    let store = backup::store(root);
    let mut used = HashSet::new();
    for n in &st.done {
        for f in store.manifest(n)?.files {
            used.extend(f.chunks);
        }
    }
    let have = load_index(dir, &t.id, server);
    let drop: Vec<String> = have.iter().filter(|h| !used.contains(*h)).cloned().collect();
    if !drop.is_empty() {
        let keys: Vec<String> = drop
            .iter()
            .map(|h| {
                let n = c.chunk_name(h);
                format!("{base}/c/{}/{n}", &n[..2])
            })
            .collect();
        b.delete_many(&keys)?;
        let keep: Vec<&String> = have.iter().filter(|h| used.contains(*h)).collect();
        let body: String = keep.iter().map(|h| format!("{h}\n")).collect();
        write_private(&index_path(dir, &t.id, server), body.as_bytes())?;
        log::info!("off-site: {} unused chunks of {server} deleted from {}", drop.len(), t.name);
    }
    Ok(())
}

/// A snapshot in a copy.
#[derive(Clone, Debug, Serialize)]
pub struct RemoteSnap {
    pub name: String,
    pub at: u64,
    pub size: u64,
    pub files: usize,
    pub note: String,
    pub scope: backup::Scope,
    pub kind: backup::Kind,
    /// Already in this server's backups.
    pub here: bool,
}

/// The servers a target holds copies of (for this panel's key).
pub fn remote_servers(dir: &Path, t: &Target) -> anyhow::Result<Vec<String>> {
    let c = crypto(dir)?;
    let mut b = open_backend(t)?;
    let prefix = format!("hopper-backups/{}/", c.panel);
    let mut out: Vec<String> = b
        .list(&prefix)?
        .into_iter()
        .filter_map(|k| k.strip_prefix(&prefix).and_then(|r| r.split('/').next()).map(str::to_string))
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// The snapshots of `from` (a server id) in a copy.
pub fn remote_snaps(dir: &Path, t: &Target, from: &str, root: &Path) -> anyhow::Result<Vec<RemoteSnap>> {
    let c = crypto(dir)?;
    let mut b = open_backend(t)?;
    let base = format!("{}/s/", remote_root(&c, from));
    let store = backup::store(root);
    let mut out = Vec::new();
    for k in b.list(&base)? {
        let m: Manifest = serde_json::from_slice(&c.open(&k, &b.get(&k)?)?)?;
        out.push(RemoteSnap { here: store.has_manifest(&m.name), name: m.name, at: m.at, size: m.size, files: m.files.len(), note: m.note, scope: m.scope, kind: m.kind });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.at));
    Ok(out)
}

/// Brings a snapshot back from a copy into `root`'s backups (only the
/// chunks missing here are downloaded).
pub fn bring_back(dir: &Path, t: &Target, from: &str, name: &str, root: &Path, job: &hopper_core::jobs::Job) -> anyhow::Result<backup::Backup> {
    backup::check(name)?;
    let c = crypto(dir)?;
    let mut b = open_backend(t)?;
    let base = remote_root(&c, from);
    let key = format!("{base}/s/{name}");
    job.detail("Reading the copy");
    let m: Manifest = serde_json::from_slice(&c.open(&key, &b.get(&key)?)?)?;
    let store: Store = backup::store(root);
    let mut want: Vec<String> = m.files.iter().flat_map(|f| f.chunks.iter()).filter(|h| !store.has_chunk(h)).cloned().collect();
    want.sort();
    want.dedup();
    let total = want.len();
    for (i, part) in want.chunks(200).enumerate() {
        job.check_cancelled()?;
        job.detail(format!("Downloading ({}/{total} pieces)", i * 200));
        job.progress(i * 200, total);
        let keys: Vec<String> = part
            .iter()
            .map(|h| {
                let n = c.chunk_name(h);
                format!("{base}/c/{}/{n}", &n[..2])
            })
            .collect();
        for ((h, k), blob) in part.iter().zip(&keys).zip(b.get_many(&keys)?) {
            store.put_raw_chunk(h, &c.open(k, &blob)?)?;
        }
    }
    backup::adopt(root, &m)
}

// ---- the worker ----------------------------------------------------------------

fn wake() -> &'static (Mutex<bool>, Condvar) {
    static W: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
    W.get_or_init(|| (Mutex::new(false), Condvar::new()))
}

/// Asks the worker to look now (a backup was just made).
pub fn kick() {
    let (m, c) = wake();
    *m.lock().unwrap() = true;
    c.notify_all();
}

/// Copies new snapshots to their targets in the background, every few
/// minutes and right after a backup.
pub fn start(app: Arc<App>) {
    std::thread::Builder::new()
        .name("offsite".into())
        .spawn(move || {
            // Let the panel settle first.
            std::thread::sleep(Duration::from_secs(20));
            let mut failing: HashSet<String> = HashSet::new();
            loop {
                run_once(&app, &mut failing);
                let (m, c) = wake();
                let g = m.lock().unwrap();
                let (mut g, _) = c.wait_timeout_while(g, Duration::from_secs(300), |k| !*k).unwrap();
                *g = false;
            }
        })
        .ok();
}

fn run_once(app: &Arc<App>, failing: &mut HashSet<String>) {
    let dir = app.data_dir.clone();
    let targets: Vec<Target> = load_targets(&dir).into_iter().filter(|t| t.enabled).collect();
    if targets.is_empty() {
        return;
    }
    let copies = load_copies(&dir);
    let Ok(c) = crypto(&dir) else { return };
    for e in app.servers.list() {
        let Some(ids) = copies.get(&e.id) else { continue };
        for t in targets.iter().filter(|t| ids.contains(&t.id)) {
            let tag = format!("{}|{}", t.id, e.id);
            match sync_one(&dir, t, &e.id, &e.root, &c) {
                Ok(_) => {
                    failing.remove(&tag);
                }
                Err(err) => {
                    working().lock().unwrap().remove(&(t.id.clone(), e.id.clone()));
                    let msg = format!("{err:#}");
                    log::warn!("off-site copy of {} to {}: {msg}", e.name, t.name);
                    let mut st = status(&dir, &t.id, &e.id);
                    st.working = None;
                    st.last_error = Some(msg.clone());
                    let _ = save_status(&dir, &t.id, &e.id, &st);
                    // Tell webhooks once per failure, not every retry.
                    if failing.insert(tag) {
                        crate::webhooks::emit(
                            app,
                            vec![crate::webhooks::Event {
                                event: "copy_failed",
                                server_id: e.id.clone(),
                                server: e.name.clone(),
                                text: format!("Copying **{}**'s backups to {} failed: {msg}", e.name, t.name),
                                at: crate::webhooks::now(),
                            }],
                        );
                    }
                }
            }
        }
    }
}

/// Forgets a removed target's progress.
pub fn forget(dir: &Path, target: &str) {
    if safe_key(target) {
        let _ = std::fs::remove_dir_all(state_dir(dir).join(target));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigv4_matches_aws_example() {
        // From AWS's "Signature Calculations for the Authorization Header"
        // GET Object example.
        let headers = vec![
            ("host".to_string(), "examplebucket.s3.amazonaws.com".to_string()),
            ("range".to_string(), "bytes=0-9".to_string()),
            ("x-amz-content-sha256".to_string(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()),
            ("x-amz-date".to_string(), "20130524T000000Z".to_string()),
        ];
        let auth = sigv4(
            "GET",
            "/test.txt",
            &[],
            &headers,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "20130524T000000Z",
        );
        assert!(auth.ends_with("Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"), "{auth}");
        assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }

    #[test]
    fn regions_from_endpoints() {
        assert_eq!(guess_region("https://s3.eu-central-003.backblazeb2.com"), "eu-central-003");
        assert_eq!(guess_region("s3.us-west-2.amazonaws.com"), "us-west-2");
        assert_eq!(guess_region("https://abc.r2.cloudflarestorage.com"), "auto");
        assert_eq!(guess_region("http://192.168.50.3:9000"), "auto");
    }

    #[test]
    fn seals_and_packs() {
        let c = Crypto::new(&[7u8; 32]);
        let blob = c.seal("a/b", b"hello").unwrap();
        assert_eq!(c.open("a/b", &blob).unwrap(), b"hello");
        assert!(c.open("a/c", &blob).is_err(), "names are bound in");
        assert!(Crypto::new(&[8u8; 32]).open("a/b", &blob).is_err());
        assert_eq!(c.chunk_name("x").len(), 64);
        assert!(c.panel.starts_with("p-"));
        let items = vec![("k1".to_string(), b"one".to_vec()), ("k/2".to_string(), vec![0u8; 70_000])];
        assert_eq!(unpack(&pack(&items)).unwrap(), items);
        assert!(unpack(&pack(&items)[..10]).is_err());
        assert!(safe_key("hopper-backups/p-1/srv/c/ab/x") && !safe_key("a/../b") && !safe_key("/abs") && !safe_key("a\\b"));
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopperd-offsite-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn copies_to_a_folder_and_brings_back() {
        let base = temp("folder");
        let (data, srv, dest, other) = (base.join("data"), base.join("srv"), base.join("dest"), base.join("other"));
        for d in [&data, &srv, &dest, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(srv.join("server.properties"), "level-name=world\n").unwrap();
        std::fs::create_dir_all(srv.join("world")).unwrap();
        std::fs::write(srv.join("world/level.dat"), vec![3u8; 300_000]).unwrap();
        let job = hopper_core::jobs::Jobs::default().start("t");
        let b1 = backup::make(&srv, backup::Scope::Worlds, backup::Kind::Manual, "one", None, &job).unwrap();
        let t = Target { id: "t1".into(), name: "NAS".into(), kind: "folder".into(), enabled: true, path: dest.to_string_lossy().into(), ..Default::default() };
        test(&t).unwrap();
        let (k, fresh) = key(&data).unwrap();
        assert!(fresh && !key(&data).unwrap().1);
        let c = Crypto::new(&k);
        assert_eq!(sync_one(&data, &t, "srv", &srv, &c).unwrap(), 1);
        // Nothing new: nothing copied.
        assert_eq!(sync_one(&data, &t, "srv", &srv, &c).unwrap(), 0);
        // The copy holds only random-looking names and bytes.
        let keys = Folder(dest.clone()).list("hopper-backups").unwrap();
        assert!(keys.iter().any(|k| k.contains("/s/")) && keys.iter().any(|k| k.contains("/c/")));
        assert!(!keys.iter().any(|k| k.contains("level")));
        assert_eq!(remote_servers(&data, &t).unwrap(), ["srv"]);
        let snaps = remote_snaps(&data, &t, "srv", &other).unwrap();
        assert_eq!(snaps.len(), 1);
        assert!(!snaps[0].here);
        // Bring it back into another server and restore it there.
        let got = bring_back(&data, &t, "srv", &b1.name, &other, &job).unwrap();
        assert_eq!(got.name, b1.name);
        backup::restore(&other, &b1.name, &[], &job).unwrap();
        assert_eq!(std::fs::read(other.join("world/level.dat")).unwrap(), vec![3u8; 300_000]);

        // A newer backup, then the old one deleted here: it goes from the copy too.
        std::fs::write(srv.join("world/level.dat"), vec![4u8; 300_000]).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        let b2 = backup::make(&srv, backup::Scope::Worlds, backup::Kind::Manual, "two", None, &job).unwrap();
        backup::delete(&srv, &b1.name).unwrap();
        sync_one(&data, &t, "srv", &srv, &c).unwrap();
        let names: Vec<String> = remote_snaps(&data, &t, "srv", &srv).unwrap().into_iter().map(|s| s.name).collect();
        assert_eq!(names, [b2.name.clone()]);
        // Losing every local backup never empties the copy.
        backup::delete(&srv, &b2.name).unwrap();
        sync_one(&data, &t, "srv", &srv, &c).unwrap();
        assert_eq!(remote_snaps(&data, &t, "srv", &srv).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }
}
