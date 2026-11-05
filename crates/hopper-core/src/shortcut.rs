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

/// A .lnk at `path` that runs `target` with `args`, started in the
/// target's folder. Made through the shell's own link API, so no
/// PowerShell (and no console window) is involved.
#[cfg(windows)]
pub fn link(path: &Path, target: &Path, args: &str) -> anyhow::Result<()> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoUninitialize, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{Interface, PCWSTR};

    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let wide = |s: &std::ffi::OsStr| -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        s.encode_wide().chain(Some(0)).collect()
    };
    let target_w = wide(target.as_os_str());
    let args_w = wide(std::ffi::OsStr::new(args));
    let dir_w = wide(target.parent().map(|p| p.as_os_str()).unwrap_or_default());
    let path_w = wide(path.as_os_str());

    // Another thread may already have set COM up in a different mode; that
    // is fine for this call, so only balance a successful init.
    // SAFETY: plain COM calls on buffers that outlive them.
    unsafe {
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let res = (|| -> windows::core::Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(PCWSTR(target_w.as_ptr()))?;
            link.SetArguments(PCWSTR(args_w.as_ptr()))?;
            link.SetWorkingDirectory(PCWSTR(dir_w.as_ptr()))?;
            let file: IPersistFile = link.cast()?;
            file.Save(PCWSTR(path_w.as_ptr()), true)
        })();
        if init.is_ok() {
            CoUninitialize();
        }
        res.map_err(|e| anyhow::anyhow!("creating {}: {e}", path.display()))
    }
}

#[cfg(not(windows))]
pub fn link(_path: &Path, _target: &Path, _args: &str) -> anyhow::Result<()> {
    anyhow::bail!("shortcuts are only made on Windows")
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg_attr(not(windows), ignore = "uses Windows paths and tools")]
    fn shortcut_with_awkward_name() {
        let dir = std::env::temp_dir().join(format!("hopper-lnk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        super::shortcut_in(&dir, "Steve's Pack: 2").unwrap();
        assert!(dir.join("Steve's Pack_ 2.lnk").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Reads the link back through the shell API: target, args, folder.
    #[test]
    #[cfg_attr(not(windows), ignore = "uses Windows paths and tools")]
    #[cfg(windows)]
    fn link_round_trip() {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            IPersistFile, STGM_READ,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::{Interface, PCWSTR};

        let dir = std::env::temp_dir().join(format!("hopper-lnk-rt-{}", std::process::id()));
        let target = dir.join("app dir").join("app.exe");
        let lnk = dir.join("out").join("Pack.lnk");
        super::link(&lnk, &target, "--instance \"Steve's Pack\"").unwrap();
        let read = |f: &dyn Fn(&mut [u16])| {
            let mut buf = [0u16; 1024];
            f(&mut buf);
            let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..n])
        };
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let l: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let f: IPersistFile = l.cast().unwrap();
            let w: Vec<u16> = lnk.to_string_lossy().encode_utf16().chain(Some(0)).collect();
            f.Load(PCWSTR(w.as_ptr()), STGM_READ).unwrap();
            let path = read(&|b| l.GetPath(b, std::ptr::null_mut(), 0).unwrap());
            let args = read(&|b| l.GetArguments(b).unwrap());
            let wd = read(&|b| l.GetWorkingDirectory(b).unwrap());
            assert_eq!(path, target.to_string_lossy());
            assert_eq!(args, "--instance \"Steve's Pack\"");
            assert_eq!(wd, target.parent().unwrap().to_string_lossy());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
