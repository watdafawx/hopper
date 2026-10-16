//! Hopper installing itself for the current user: a copy in
//! %LOCALAPPDATA%\Programs\Hopper, Start menu (and desktop) shortcuts, and
//! an entry in Windows' Installed apps whose Uninstall runs
//! `hopper.exe --uninstall`. No admin rights needed. Settings, keys and
//! caches are kept on uninstall.

use anyhow::Context as _;
use std::path::{Path, PathBuf};

const UNINSTALL_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Hopper";

/// Where the installed copy lives.
pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Hopper"))
}

/// The installed exe, if Hopper is installed.
pub fn installed_exe() -> Option<PathBuf> {
    install_dir().map(|d| d.join("hopper.exe")).filter(|p| p.is_file())
}

/// Whether this process is the installed copy.
pub fn running_installed() -> bool {
    let here = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    let there = installed_exe().and_then(|p| p.canonicalize().ok());
    here.is_some() && here == there
}

fn start_menu_link() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| {
        PathBuf::from(d)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join("Hopper.lnk")
    })
}

/// Whether the Start menu has a Hopper shortcut.
pub fn in_start_menu() -> bool {
    start_menu_link().is_some_and(|p| p.is_file())
}

/// A Start menu shortcut to this copy of Hopper, without installing it, so
/// it can be found in Start and pinned there. (Windows doesn't let apps pin
/// themselves; the user right-clicks it and picks Pin to Start.)
pub fn add_to_start_menu() -> anyhow::Result<()> {
    let link = start_menu_link().context("no APPDATA folder")?;
    let exe = std::env::current_exe().context("finding hopper.exe")?;
    crate::shortcut::link(&link, &exe, "")
}

/// Removes the Start menu shortcut.
pub fn remove_from_start_menu() -> anyhow::Result<()> {
    let link = start_menu_link().context("no APPDATA folder")?;
    match std::fs::remove_file(&link) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", link.display()))
        }
        _ => Ok(()),
    }
}

fn desktop_link() -> Option<PathBuf> {
    directories::UserDirs::new()
        .and_then(|u| u.desktop_dir().map(|d| d.join("Hopper.lnk")))
}

fn reg(args: &[&str]) -> anyhow::Result<()> {
    let out = std::process::Command::new("reg")
        .args(args)
        .output()
        .context("running reg")?;
    anyhow::ensure!(
        out.status.success(),
        "reg {}: {}",
        args.first().copied().unwrap_or(""),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

/// Copies this Hopper into the install folder, adds its shortcuts and
/// registers it with Windows. Returns the installed exe.
pub fn install(desktop: bool) -> anyhow::Result<PathBuf> {
    let dir = install_dir().context("no LOCALAPPDATA folder")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let exe = dir.join("hopper.exe");
    let me = std::env::current_exe().context("finding this exe")?;
    if !running_installed() {
        // Copy beside it first, so a failed copy leaves the old one working.
        let staged = dir.join("hopper.new.exe");
        std::fs::copy(&me, &staged).context("copying Hopper")?;
        if exe.exists() {
            let old = dir.join("hopper.old.exe");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&exe, &old).context("moving the old copy aside")?;
        }
        std::fs::rename(&staged, &exe).context("putting Hopper in place")?;
    }
    if let Some(link) = start_menu_link() {
        crate::shortcut::link(&link, &exe, "")?;
    }
    if desktop && let Some(link) = desktop_link() {
        crate::shortcut::link(&link, &exe, "")?;
    }
    register(&exe, &dir)?;
    Ok(exe)
}

fn register(exe: &Path, dir: &Path) -> anyhow::Result<()> {
    let exe_s = exe.to_string_lossy();
    let uninstall = format!("\"{exe_s}\" --uninstall");
    let size_kb = std::fs::metadata(exe).map_or(0, |m| m.len() / 1024).to_string();
    let dir_s = dir.to_string_lossy();
    let values: [(&str, &str, &str); 9] = [
        ("DisplayName", "REG_SZ", "Hopper"),
        ("DisplayVersion", "REG_SZ", crate::selfupdate::CURRENT),
        ("Publisher", "REG_SZ", "Hopper"),
        ("DisplayIcon", "REG_SZ", &exe_s),
        ("InstallLocation", "REG_SZ", &dir_s),
        ("UninstallString", "REG_SZ", &uninstall),
        ("EstimatedSize", "REG_DWORD", &size_kb),
        ("NoModify", "REG_DWORD", "1"),
        ("NoRepair", "REG_DWORD", "1"),
    ];
    for (name, kind, value) in values {
        reg(&["add", UNINSTALL_KEY, "/v", name, "/t", kind, "/d", value, "/f"])?;
    }
    Ok(())
}

/// Removes the shortcuts and the Installed apps entry, then the install
/// folder once this process has exited. User data stays.
pub fn uninstall() -> anyhow::Result<()> {
    for link in [start_menu_link(), desktop_link()].into_iter().flatten() {
        let _ = std::fs::remove_file(link);
    }
    let _ = reg(&["delete", UNINSTALL_KEY, "/f"]);
    if let Some(dir) = install_dir().filter(|d| d.is_dir()) {
        // The running exe can't delete its own folder: a detached cmd does
        // it a moment after Hopper exits.
        let script = format!(
            "ping 127.0.0.1 -n 3 > nul & rmdir /s /q \"{}\"",
            dir.display()
        );
        let mut cmd = std::process::Command::new("cmd");
        #[cfg(not(windows))]
        cmd.args(["/c", &script]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // cmd reads its own quoting; Rust's escaping would garble it.
            cmd.raw_arg(format!("/c {script}"));
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
        }
        cmd.spawn().context("scheduling the folder's removal")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installs into a temp LOCALAPPDATA/APPDATA (no desktop shortcut),
    /// checks the copy, shortcut and Installed apps entry, then uninstalls.
    /// Touches HKCU, so it only runs on request.
    #[test]
    #[ignore]
    fn install_and_uninstall() {
        let root = std::env::temp_dir().join(format!("hopper-setup-{}", std::process::id()));
        // SAFETY: this test runs alone (--ignored, single thread).
        unsafe {
            std::env::set_var("LOCALAPPDATA", root.join("local"));
            std::env::set_var("APPDATA", root.join("roaming"));
        }
        let exe = install(false).unwrap();
        assert!(exe.is_file());
        assert!(start_menu_link().unwrap().is_file());
        let q = std::process::Command::new("reg")
            .args(["query", UNINSTALL_KEY, "/v", "UninstallString"])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&q.stdout).contains("--uninstall"));
        uninstall().unwrap();
        assert!(!start_menu_link().unwrap().exists());
        let q = std::process::Command::new("reg").args(["query", UNINSTALL_KEY]).output().unwrap();
        assert!(!q.status.success());
        std::thread::sleep(std::time::Duration::from_secs(4));
        assert!(!install_dir().unwrap().exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
