//! Hopper updating itself from a GitHub repository's releases: the latest
//! release whose tag is newer than this build, with a Windows exe (or a zip
//! holding one) attached, or any earlier release to go back to.

use anyhow::{Context as _, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// A Hopper release, ready to download.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub name: String,
    pub notes: String,
    /// When it was published, as GitHub gives it (2026-10-18T09:08:51Z).
    pub published: String,
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
    published_at: Option<String>,
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
    // Releases carry only Windows builds so far; one installed over a Linux
    // or macOS Hopper would leave it unable to start.
    if !cfg!(windows) {
        return None;
    }
    let named = |a: &&GhAsset| a.name.to_lowercase().contains("hopper");
    let ext = |a: &GhAsset, e: &str| a.name.to_lowercase().ends_with(e);
    assets
        .iter()
        .filter(named)
        .find(|a| ext(a, ".exe") && !a.name.to_lowercase().contains("setup"))
        .map(|a| (a, false))
        .or_else(|| assets.iter().filter(named).find(|a| ext(a, ".zip")).map(|a| (a, true)))
}

/// Hopper's own release tags are "v0.12.0"; the same repository also has
/// Hopper Server's ("hopperd-v0.7.0"), which aren't Hopper versions.
fn is_hopper_tag(tag: &str) -> bool {
    tag.strip_prefix(['v', 'V']).unwrap_or(tag).starts_with(|c: char| c.is_ascii_digit())
}

/// A published release with something to install.
fn installable(r: GhRelease) -> Option<Release> {
    if r.draft || r.prerelease || !is_hopper_tag(&r.tag_name) {
        return None;
    }
    let (asset, zipped) = pick(&r.assets)?;
    Some(Release {
        version: r.tag_name.trim_start_matches(['v', 'V']).to_string(),
        name: r.name.unwrap_or_else(|| r.tag_name.clone()),
        notes: r.body.unwrap_or_default(),
        published: r.published_at.unwrap_or_default(),
        url: asset.browser_download_url.clone(),
        size: asset.size,
        zipped,
    })
}

fn parse_release(r: GhRelease) -> Option<Release> {
    installable(r).filter(|r| is_newer(&r.version, CURRENT))
}

fn api(repo: &str, path: &str) -> anyhow::Result<ureq::http::Response<ureq::Body>> {
    let repo = repo.trim().trim_matches('/');
    if repo.split('/').filter(|p| !p.is_empty()).count() != 2 {
        bail!("Set the repository as owner/name.");
    }
    let url = format!("https://api.github.com/repos/{repo}/{path}");
    crate::cf::agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .with_context(|| format!("reading Hopper's releases from {repo}"))
}

/// The newest release of `repo` ("owner/name") if it is newer than this
/// build and has something to install.
pub fn check(repo: &str) -> anyhow::Result<Option<Release>> {
    // Not "releases/latest": that is the newest release of any kind, which
    // can be a Hopper Server one and would hide a newer Hopper.
    let all: Vec<GhRelease> = api(repo, "releases?per_page=30")?
        .body_mut()
        .read_json()
        .context("reading the releases")?;
    Ok(sorted(all.into_iter().filter_map(parse_release).collect()).into_iter().next())
}

/// Every release of `repo` that can be installed, newest first.
pub fn list(repo: &str) -> anyhow::Result<Vec<Release>> {
    let all: Vec<GhRelease> = api(repo, "releases?per_page=100")?
        .body_mut()
        .read_json()
        .context("reading the releases")?;
    Ok(sorted(all.into_iter().filter_map(installable).collect()))
}

fn sorted(mut v: Vec<Release>) -> Vec<Release> {
    v.sort_by(|a, b| {
        if is_newer(&a.version, &b.version) {
            std::cmp::Ordering::Less
        } else if is_newer(&b.version, &a.version) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    v
}

/// The releases a move to `target` leaves out of `releases` (newest
/// first): the ones after it, up to the newest.
pub fn after<'a>(releases: &'a [Release], target: &str) -> Vec<&'a Release> {
    releases.iter().filter(|r| is_newer(&r.version, target)).collect()
}

/// A release's notes as shown in Hopper: GitHub's "Full Changelog" link
/// line dropped, the rest kept as Markdown.
pub fn clean_notes(body: &str) -> String {
    body.lines()
        .filter(|l| !l.trim_start().starts_with("**Full Changelog**"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
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
    // To a file first, so the task shows how far along it is (and can be
    // cancelled); a cut connection is retried once.
    let tmp = crate::paths::temp_dir().join(format!("hopper-{}.download", r.version));
    let started = std::time::Instant::now();
    let mut shown = started;
    let mb = |b: u64| b as f64 / 1_048_576.0;
    let expect = crate::cf::Expect { len: r.size, ..Default::default() };
    let got = crate::cf::download_checked(&r.url, &tmp, &expect, Some(job), |n| {
        if r.size > 0 {
            job.progress(n as usize, r.size as usize);
        }
        // A few times a second is plenty for the words.
        if shown.elapsed().as_millis() >= 250 {
            shown = std::time::Instant::now();
            let speed = mb(n) / started.elapsed().as_secs_f64().max(0.1);
            job.detail(if r.size > 0 {
                format!("Downloading Hopper {} · {:.1} of {:.1} MB · {speed:.1} MB/s", r.version, mb(n), mb(r.size))
            } else {
                format!("Downloading Hopper {} · {:.1} MB · {speed:.1} MB/s", r.version, mb(n))
            });
        }
    });
    let bytes = got.and_then(|_| std::fs::read(&tmp).context("reading the download"));
    let _ = std::fs::remove_file(&tmp);
    let bytes = bytes.context("downloading the update")?;
    job.progress(0, 0);
    if r.size != 0 && bytes.len() as u64 != r.size {
        bail!("The download was cut short ({} of {} bytes).", bytes.len(), r.size);
    }
    if r.zipped {
        job.detail(format!("Unpacking Hopper {}", r.version));
    }
    let new_exe = if r.zipped { exe_from_zip(&bytes)? } else { bytes };
    if new_exe.get(..2) != Some(b"MZ") {
        bail!("The download isn't a Windows program.");
    }
    job.detail(format!("Installing Hopper {}", r.version));
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
    #[cfg_attr(not(windows), ignore = "releases only offer Windows builds")]
    fn picks_the_exe_then_a_zip() {
        let a = [asset("hopper-setup.exe"), asset("hopper.zip"), asset("Hopper.exe")];
        assert_eq!(pick(&a).map(|(a, z)| (a.name.as_str(), z)), Some(("Hopper.exe", false)));
        let b = [asset("notes.txt"), asset("hopper-win64.zip")];
        assert_eq!(pick(&b).map(|(a, z)| (a.name.as_str(), z)), Some(("hopper-win64.zip", true)));
        assert!(pick(&[asset("other.exe")]).is_none());
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "releases only offer Windows builds")]
    fn skips_drafts_and_old_releases() {
        let r = |tag: &str, draft| GhRelease {
            tag_name: tag.into(),
            name: None,
            body: None,
            draft,
            prerelease: false,
            published_at: None,
            assets: vec![asset("hopper.exe")],
        };
        assert!(parse_release(r("v99.0.0", false)).is_some());
        assert!(parse_release(r("v99.0.0", true)).is_none());
        assert!(parse_release(r("v0.0.1", false)).is_none());
        // Hopper Server's releases share the repository, not Hopper's versions.
        let mut server = r("hopperd-v99.0.0", false);
        server.assets = vec![asset("hopperd-99.0.0-windows-x64.zip")];
        assert!(parse_release(server).is_none());
        assert!(super::is_hopper_tag("v0.12.0") && super::is_hopper_tag("0.12.0") && !super::is_hopper_tag("hopperd-v0.7.0"));
    }

    fn rel(v: &str) -> Release {
        Release {
            version: v.into(),
            name: v.into(),
            notes: String::new(),
            published: String::new(),
            url: String::new(),
            size: 0,
            zipped: false,
        }
    }

    #[test]
    fn lists_newest_first_and_what_a_rollback_drops() {
        let v = sorted(vec![rel("0.8.9"), rel("0.8.20"), rel("0.8.10")]);
        let names: Vec<_> = v.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(names, ["0.8.20", "0.8.10", "0.8.9"]);
        let lost: Vec<_> = after(&v, "0.8.9").iter().map(|r| r.version.as_str()).collect();
        assert_eq!(lost, ["0.8.20", "0.8.10"]);
        assert!(after(&v, "0.8.20").is_empty());
    }

    #[test]
    fn notes_drop_the_compare_link() {
        let body = "Fixed a thing.\n\n- Commit\n\n**Full Changelog**: https://x/compare/a...b\n";
        assert_eq!(clean_notes(body), "Fixed a thing.\n\n- Commit");
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
