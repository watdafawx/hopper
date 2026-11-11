//! The file work of a self-update that doesn't depend on the platform's
//! restart rules: reading the program out of a release download, checking
//! it is a program for this computer, and putting it in place of the running
//! one. Linux uses all of it; the pure parts take the OS and CPU as
//! arguments so they are tested on any machine.

use anyhow::{Context as _, bail};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The biggest program accepted from a download (a guard against a
/// download that claims to unpack into something huge).
const MAX_PROGRAM: u64 = 512 * 1024 * 1024;

/// `<exe><suffix>` next to the exe: `hopperd.new`, `hopperd.old`, ...
pub fn beside(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    exe.with_file_name(name)
}

// ---- The download ----------------------------------------------------------------------------

fn octal(field: &[u8]) -> anyhow::Result<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        bail!("The download has a file that is too big.");
    }
    let s: String = field.iter().take_while(|b| **b != 0).map(|b| *b as char).collect();
    u64::from_str_radix(s.trim(), 8).context("The download isn't a valid archive.")
}

fn text(field: &[u8]) -> String {
    String::from_utf8_lossy(&field[..field.iter().position(|b| *b == 0).unwrap_or(field.len())]).into_owned()
}

fn skip(r: &mut impl Read, n: u64) -> anyhow::Result<()> {
    let got = std::io::copy(&mut r.by_ref().take(n), &mut std::io::sink())?;
    if got != n {
        bail!("The download is cut short.");
    }
    Ok(())
}

/// The file called `name` (anywhere in the archive) out of a tar stream.
/// Reads regular files only: links are never followed.
pub fn file_from_tar(mut r: impl Read, name: &str) -> anyhow::Result<Vec<u8>> {
    let mut head = [0u8; 512];
    let mut long_name: Option<String> = None;
    loop {
        if r.read_exact(&mut head).is_err() || head.iter().all(|b| *b == 0) {
            bail!("The download has no {name}.");
        }
        let size = octal(&head[124..136])?;
        let kind = head[156];
        let pad = size.div_ceil(512) * 512 - size;
        match kind {
            // GNU long name: the next entry's path.
            b'L' => {
                if size > 64 * 1024 {
                    bail!("The download isn't a valid archive.");
                }
                let mut b = vec![0u8; size as usize];
                r.read_exact(&mut b).context("The download is cut short.")?;
                skip(&mut r, pad)?;
                long_name = Some(text(&b));
            }
            b'0' | 0 => {
                let mut path = text(&head[0..100]);
                if &head[257..262] == b"ustar" {
                    let prefix = text(&head[345..500]);
                    if !prefix.is_empty() {
                        path = format!("{prefix}/{path}");
                    }
                }
                if let Some(l) = long_name.take() {
                    path = l;
                }
                if path.rsplit('/').next() == Some(name) {
                    if size > MAX_PROGRAM {
                        bail!("The download has a file that is too big.");
                    }
                    let mut out = vec![0u8; size as usize];
                    r.read_exact(&mut out).context("The download is cut short.")?;
                    return Ok(out);
                }
                skip(&mut r, size + pad)?;
            }
            _ => {
                long_name = None;
                skip(&mut r, size + pad)?;
            }
        }
    }
}

/// `name` out of a `.tar.gz`.
pub fn file_from_targz(archive: &Path, name: &str) -> anyhow::Result<Vec<u8>> {
    let f = std::fs::File::open(archive)?;
    file_from_tar(flate2::read::GzDecoder::new(f), name).with_context(|| "opening the download")
}

/// Whether `bytes` is a program for this OS and CPU (a wrong or damaged
/// download is refused before it replaces anything).
pub fn check_program(bytes: &[u8], os: &str, arch: &str) -> Result<(), &'static str> {
    if os == "windows" {
        return if bytes.get(..2) == Some(b"MZ") { Ok(()) } else { Err("The download isn't a Windows program.") };
    }
    if bytes.len() < 20 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
        return Err("The download isn't a Linux program.");
    }
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    match (arch, machine) {
        ("x86_64", 62) | ("aarch64", 183) => Ok(()),
        _ => Err("The download is for a different kind of computer."),
    }
}

// ---- The swap --------------------------------------------------------------------------------

/// Whether files can be made in `dir` (what the swap needs).
pub fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".hopperd-write-test-{}", std::process::id()));
    let ok = std::fs::OpenOptions::new().write(true).create_new(true).open(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Puts `bytes` in place of the program at `exe`, which may be running.
/// The new file is written beside it and synced, the old one stays as
/// `<exe>.old` (a hard link, else a copy), and the new one is renamed over
/// `exe` (one step: a start at any moment gets the old or the new program,
/// never half of one). It keeps the old file's permissions, else 0755.
pub fn swap_in(exe: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let staged = beside(exe, ".new");
    let old = beside(exe, ".old");
    let _ = std::fs::remove_file(&staged);
    let wrote = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&staged)?;
        f.write_all(bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(exe).map(|m| m.permissions().mode() & 0o777).unwrap_or(0o755);
            f.set_permissions(std::fs::Permissions::from_mode(if mode & 0o100 == 0 { 0o755 } else { mode }))?;
        }
        f.sync_all()
    })();
    if let Err(e) = wrote {
        let _ = std::fs::remove_file(&staged);
        return Err(e).context("saving the update");
    }
    let _ = std::fs::remove_file(&old);
    if std::fs::hard_link(exe, &old).is_err()
        && let Err(e) = std::fs::copy(exe, &old)
    {
        let _ = std::fs::remove_file(&staged);
        return Err(e).context("keeping the old copy");
    }
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::remove_file(&staged);
        let _ = std::fs::remove_file(&old);
        return Err(e).context("putting the update in place");
    }
    // Make the rename itself survive a power cut.
    #[cfg(unix)]
    {
        if let Some(dir) = exe.parent()
            && let Ok(d) = std::fs::File::open(dir)
        {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_entry(name: &str, kind: u8, body: &[u8]) -> Vec<u8> {
        let mut h = [0u8; 512];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[124..135].copy_from_slice(format!("{:011o}", body.len()).as_bytes());
        h[156] = kind;
        let mut v = h.to_vec();
        v.extend_from_slice(body);
        v.resize(v.len().div_ceil(512) * 512, 0);
        v
    }

    fn tar(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut v: Vec<u8> = entries.concat();
        v.extend([0u8; 1024]);
        v
    }

    #[test]
    fn finds_the_program_in_a_tar() {
        let t = tar(&[tar_entry("README.md", b'0', b"hi"), tar_entry("dist/hopperd", b'0', b"\x7fELF program")]);
        assert_eq!(file_from_tar(&t[..], "hopperd").unwrap(), b"\x7fELF program");
        // Same name as a link: not read.
        let t = tar(&[tar_entry("hopperd", b'2', b"")]);
        assert!(file_from_tar(&t[..], "hopperd").is_err());
        // Cut short, or not there.
        let t = tar(&[tar_entry("hopperd", b'0', &[7u8; 2000])]);
        assert!(file_from_tar(&t[..700], "hopperd").is_err());
        assert!(file_from_tar(&tar(&[tar_entry("other", b'0', b"x")])[..], "hopperd").is_err());
        // A long name before it (GNU).
        let long = "a/".repeat(80) + "hopperd";
        let t = tar(&[tar_entry("././@LongLink", b'L', long.as_bytes()), tar_entry("short", b'0', b"payload")]);
        assert_eq!(file_from_tar(&t[..], "hopperd").unwrap(), b"payload");
    }

    #[test]
    fn reads_a_tar_gz() {
        let d = std::env::temp_dir().join(format!("hopperd-targz-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("x.tar.gz");
        let t = tar(&[tar_entry("hopperd", b'0', b"\x7fELF gz")]);
        let mut w = flate2::write::GzEncoder::new(std::fs::File::create(&p).unwrap(), flate2::Compression::fast());
        w.write_all(&t).unwrap();
        w.finish().unwrap();
        assert_eq!(file_from_targz(&p, "hopperd").unwrap(), b"\x7fELF gz");
        let _ = std::fs::remove_dir_all(&d);
    }

    fn elf(machine: u16) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[18..20].copy_from_slice(&machine.to_le_bytes());
        b
    }

    #[test]
    fn only_a_program_for_this_computer_is_accepted() {
        assert!(check_program(&elf(62), "linux", "x86_64").is_ok());
        assert!(check_program(&elf(183), "linux", "aarch64").is_ok());
        // The other CPU's build, a Windows exe, a script, nothing.
        assert!(check_program(&elf(183), "linux", "x86_64").is_err());
        assert!(check_program(&elf(62), "linux", "aarch64").is_err());
        assert!(check_program(b"MZ\x90\0", "linux", "x86_64").is_err());
        assert!(check_program(b"#!/bin/sh\nrm -rf /\n", "linux", "x86_64").is_err());
        assert!(check_program(&[], "linux", "x86_64").is_err());
        assert!(check_program(&elf(62), "linux", "riscv64").is_err());
        assert!(check_program(b"MZ\x90\0", "windows", "x86_64").is_ok());
        assert!(check_program(&elf(62), "windows", "x86_64").is_err());
    }

    #[test]
    fn swap_keeps_the_old_one_and_puts_the_new_one_in_place() {
        let d = std::env::temp_dir().join(format!("hopperd-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("hopperd");
        std::fs::write(&exe, b"old program").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        swap_in(&exe, b"new program").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new program");
        assert_eq!(std::fs::read(beside(&exe, ".old")).unwrap(), b"old program");
        assert!(!beside(&exe, ".new").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o755);
        }
        // A second update replaces the rollback copy with the one it left.
        swap_in(&exe, b"newer program").unwrap();
        assert_eq!(std::fs::read(beside(&exe, ".old")).unwrap(), b"new program");
        assert!(dir_writable(&d));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_swap_leaves_the_running_program_alone() {
        let d = std::env::temp_dir().join(format!("hopperd-swapfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // No program at the path: nothing to keep, nothing replaced, no litter.
        let exe = d.join("hopperd");
        assert!(swap_in(&exe, b"new").is_err());
        assert!(!exe.exists() && !beside(&exe, ".new").exists() && !beside(&exe, ".old").exists());
        let _ = std::fs::remove_dir_all(&d);
    }
}
