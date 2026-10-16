//! Desktop shortcuts that open Hopper on one instance.

use std::path::Path;

/// A desktop shortcut that opens this Hopper on `name`.
pub fn desktop_shortcut(name: &str) -> anyhow::Result<()> {
    let desktop = directories::UserDirs::new()
        .and_then(|u| u.desktop_dir().map(|d| d.to_path_buf()))
        .ok_or_else(|| anyhow::anyhow!("no desktop folder"))?;
    shortcut_in(&desktop, name)
}

pub fn shortcut_in(desktop: &Path, name: &str) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let path = desktop.join(format!(
        "{}.lnk",
        crate::instances::Instance::folder_name(name)
    ));
    link(&path, &exe, &format!("--instance \"{name}\""))
}

/// A .lnk at `path` that runs `target` with `args`.
pub fn link(path: &Path, target: &Path, args: &str) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // PowerShell single-quoted strings escape a quote by doubling it.
    let q = |s: &str| s.replace('\'', "''");
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath = '{}'; $s.Arguments = '{}'; $s.WorkingDirectory = '{}'; $s.Save()",
        q(&path.to_string_lossy()),
        q(&target.to_string_lossy()),
        q(args),
        q(&target
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()),
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()?;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn shortcut_with_awkward_name() {
        let dir = std::env::temp_dir().join(format!("hopper-lnk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        super::shortcut_in(&dir, "Steve's Pack: 2").unwrap();
        assert!(dir.join("Steve's Pack_ 2.lnk").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
