//! Hopper updating itself from a GitHub repository's releases: the latest
//! release whose tag is newer than this build, with a Windows exe (or a zip
//! holding one) attached.

use anyhow::{Context as _, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A newer Hopper, ready to download.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub name: String,
    pub notes: String,
    pub url: String,
    pub size: u64,
    /// The download is a zip with hopper.exe inside.
    pub zipped: bool,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// "1.2.10" as numbers; a leading "v" and any "-beta" tail are ignored.
fn parts(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split(['-', '+'])
        .next()
        .unwrap_or("")
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// Whether `candidate` is a later version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (a, b) = (parts(candidate), parts(current));
    let n = a.len().max(b.len());
    let get = |v: &Vec<u64>, i| v.get(i).copied().unwrap_or(0);
    (0..n)
        .map(|i| get(&a, i).cmp(&get(&b, i)))
        .find(|o| o.is_ne())
        .is_some_and(|o| o.is_gt())
}

/// The release's Windows download: an .exe named like Hopper, else a zip.
fn pick(assets: &[GhAsset]) -> Option<(&GhAsset, bool)> {
    let named = |a: &&GhAsset| a.name.to_lowercase().contains("hopper");
    let ext = |a: &GhAsset, e: &str| a.name.to_lowercase().ends_with(e);
    assets
        .iter()
        .filter(named)
        .find(|a| ext(a, ".exe") && !a.name.to_lowercase().contains("setup"))
        .map(|a| (a, false))
        .or_else(|| assets.iter().filter(named).find(|a| ext(a, ".zip")).map(|a| (a, true)))
}

fn parse_release(r: GhRelease) -> Option<Release> {
    if r.draft || r.prerelease || !is_newer(&r.tag_name, CURRENT) {
        return None;
    }
    let (asset, zipped) = pick(&r.assets)?;
    Some(Release {
        version: r.tag_name.trim_start_matches(['v', 'V']).to_string(),
        name: r.name.unwrap_or_else(|| r.tag_name.clone()),
        notes: r.body.unwrap_or_default(),
        url: asset.browser_download_url.clone(),
        size: asset.size,
        zipped,
    })
}

/// The newest release of `repo` ("owner/name") if it is newer than this
/// build and has something to install.
pub fn check(repo: &str) -> anyhow::Result<Option<Release>> {
    let repo = repo.trim().trim_matches('/');
    if repo.split('/').filter(|p| !p.is_empty()).count() != 2 {
        bail!("Set the repository as owner/name.");
    }
    let url = format!("https://api.github.com/repos/{repo}/releases/latest");
    let mut resp = crate::cf::agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("checking {repo} for a new Hopper"))?;
    let r: GhRelease = resp.body_mut().read_json().context("reading the release")?;
    Ok(parse_release(r))
}

/// The running exe and where the old one waits to be removed.
fn paths() -> anyhow::Result<(PathBuf, PathBuf)> {
    let exe = std::env::current_exe().context("finding Hopper's exe")?;
    let old = exe.with_extension("old.exe");
    Ok((exe, old))
}

/// Downloads `r` and puts it in place of the running exe, which moves aside
/// (Windows lets a running exe be renamed, not replaced). The new Hopper
/// runs from the next start; returns its path.
pub fn install(r: &Release, job: &crate::jobs::Job) -> anyhow::Result<PathBuf> {
    let (exe, old) = paths()?;
    job.detail(format!("Downloading Hopper {}", r.version));
    let bytes = crate::cf::download_agent()
        .get(&r.url)
        .call()
        .context("downloading the update")?
        .body_mut()
        .with_config()
        .limit(512 << 20)
        .read_to_vec()
        .context("downloading the update")?;
    if r.size != 0 && bytes.len() as u64 != r.size {
        bail!("The download was cut short ({} of {} bytes).", bytes.len(), r.size);
    }
    let new_exe = if r.zipped { exe_from_zip(&bytes)? } else { bytes };
    if new_exe.get(..2) != Some(b"MZ") {
        bail!("The download isn't a Windows program.");
    }
    swap(&exe, &old, &new_exe)?;
    job.detail(format!("Hopper {} is ready: restart to use it", r.version));
    Ok(exe)
}

fn exe_from_zip(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("opening the zip")?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = f.name().to_lowercase();
        if name.rsplit('/').next() == Some("hopper.exe") {
            let mut out = Vec::new();
            std::io::copy(&mut f, &mut out)?;
            return Ok(out);
        }
    }
    bail!("The zip has no hopper.exe.")
}

/// Writes `bytes` as `exe`, moving the current one to `old` first and back
/// if anything fails.
fn swap(exe: &Path, old: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let staged = exe.with_extension("new.exe");
    std::fs::write(&staged, bytes).context("saving the update")?;
    let _ = std::fs::remove_file(old);
    rename_retrying(exe, old).context("moving the running Hopper aside")?;
    if let Err(e) = rename_retrying(&staged, exe) {
        let _ = rename_retrying(old, exe);
        return Err(e).context("putting the update in place");
    }
    Ok(())
}

/// Renames, retrying for a while: antivirus (Windows Defender) scans a
/// freshly written exe and holds it open, which makes a rename fail with
/// "being used by another process" for a few seconds.
fn rename_retrying(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut last = None;
    for i in 0..40 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            // 32 sharing violation, 5 access denied: both pass.
            Err(e) if matches!(e.raw_os_error(), Some(32) | Some(5)) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(100 + 25 * i));
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("rename kept failing")))
}

/// Removes what an earlier update left behind; call at start.
pub fn clean_up() {
    if let Ok((exe, old)) = paths() {
        let _ = std::fs::remove_file(old);
        let _ = std::fs::remove_file(exe.with_extension("new.exe"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn swap_waits_for_a_held_file() {
        use std::os::windows::fs::OpenOptionsExt;
        let d = std::env::temp_dir().join(format!("hopper-swap-held-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (exe, old) = (d.join("hopper.exe"), d.join("hopper.old.exe"));
        std::fs::write(&exe, b"MZ old").unwrap();
        let staged = exe.with_extension("new.exe");
        std::fs::write(&staged, b"MZ new").unwrap();
        // Hold the staged file like a scanner does (no delete sharing) for a second.
        let held = std::fs::OpenOptions::new().read(true).share_mode(1).open(&staged).unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(1000));
            drop(held);
        });
        rename_retrying(&exe, &old).unwrap();
        rename_retrying(&staged, &exe).unwrap();
        t.join().unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZ new");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn versions_compare() {
        assert!(is_newer("v0.2.0", "0.1.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0-beta", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    fn asset(name: &str) -> GhAsset {
        GhAsset {
            name: name.into(),
            browser_download_url: format!("https://x/{name}"),
            size: 1,
        }
    }

    #[test]
    fn picks_the_exe_then_a_zip() {
        let a = [asset("hopper-setup.exe"), asset("hopper.zip"), asset("Hopper.exe")];
        assert_eq!(pick(&a).map(|(a, z)| (a.name.as_str(), z)), Some(("Hopper.exe", false)));
        let b = [asset("notes.txt"), asset("hopper-win64.zip")];
        assert_eq!(pick(&b).map(|(a, z)| (a.name.as_str(), z)), Some(("hopper-win64.zip", true)));
        assert!(pick(&[asset("other.exe")]).is_none());
    }

    #[test]
    fn skips_drafts_and_old_releases() {
        let r = |tag: &str, draft| GhRelease {
            tag_name: tag.into(),
            name: None,
            body: None,
            draft,
            prerelease: false,
            assets: vec![asset("hopper.exe")],
        };
        assert!(parse_release(r("v99.0.0", false)).is_some());
        assert!(parse_release(r("v99.0.0", true)).is_none());
        assert!(parse_release(r("v0.0.1", false)).is_none());
    }

    #[test]
    fn swap_moves_the_old_exe_aside() {
        let dir = std::env::temp_dir().join(format!("hopper-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("hopper.exe");
        let old = dir.join("hopper.old.exe");
        std::fs::write(&exe, b"MZold").unwrap();
        swap(&exe, &old, b"MZnew").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"MZnew");
        assert_eq!(std::fs::read(&old).unwrap(), b"MZold");
        std::fs::remove_dir_all(dir).ok();
    }
}
