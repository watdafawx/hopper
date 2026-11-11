//! Hopper as a browser choice in Windows' Default apps, so clicked
//! CurseForge and Modrinth links open in Hopper and every other link goes
//! straight on to the player's real browser.
//!
//! Everything is per user (HKCU) and needs no admin. Windows doesn't let an
//! app make itself the default: Hopper only puts itself on the list, and the
//! player picks it in Windows Settings. Turning it off takes every key out
//! again. The real browser is read from the machine's list (read only).

use std::path::Path;

/// A browser on this PC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Browser {
    /// What Windows calls it ("Firefox").
    pub name: String,
    /// The class that opens its https links ("FirefoxURL-308046B0AF4A39CB").
    pub prog_id: String,
}

/// The names Hopper registers under. `HOPPER_LINK_NAME` gives a test build
/// its own ("Hopper (test)"), so it can't touch the real one's keys.
pub struct Names {
    /// The app's name in Default apps and its key under StartMenuInternet.
    pub app: String,
    /// The class links are given to.
    pub prog_id: String,
}

pub fn names() -> Names {
    match std::env::var("HOPPER_LINK_NAME").ok().filter(|n| !n.trim().is_empty()) {
        Some(n) => {
            let tag: String = n.chars().filter(char::is_ascii_alphanumeric).collect();
            Names { app: n.trim().to_string(), prog_id: format!("HopperURL.{tag}") }
        }
        None => Names { app: "Hopper".into(), prog_id: "HopperURL".into() },
    }
}

/// The link with anything that could break out of a command line escaped.
/// Links from the shell never have these; a crafted one could.
pub fn safe_url(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    for c in url.trim().chars() {
        match c {
            '"' => out.push_str("%22"),
            ' ' => out.push_str("%20"),
            c if c.is_control() || c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out
}

/// A browser's open command with the link put in: the program, and the
/// rest of the command line as written (browsers read their own quoting).
pub fn fill_command(template: &str, url: &str) -> Option<(String, String)> {
    let url = safe_url(url);
    let t = template.trim();
    let (exe, rest) = match t.strip_prefix('"') {
        Some(r) => {
            let (exe, rest) = r.split_once('"')?;
            (exe.to_string(), rest.trim_start().to_string())
        }
        None => match t.split_once(' ') {
            Some((exe, rest)) => (exe.to_string(), rest.trim_start().to_string()),
            None => (t.to_string(), String::new()),
        },
    };
    if exe.is_empty() {
        return None;
    }
    let args = if rest.contains("%1") {
        rest.replace("%1", &url)
    } else if rest.contains("%L") || rest.contains("%l") {
        rest.replace("%L", &url).replace("%l", &url)
    } else if rest.is_empty() {
        format!("\"{url}\"")
    } else {
        format!("{rest} \"{url}\"")
    };
    Some((expand_env(&exe), args))
}

/// `%ProgramFiles%` and friends, for commands stored as REG_EXPAND_SZ.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%') {
            Some(j) => {
                let name = &after[..j];
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A command that would come back to Hopper: never used to open a link,
/// so a link can't bounce between Hopper and itself.
pub fn is_ours(command: &str) -> bool {
    let c = command.to_ascii_lowercase();
    if c.contains("--open-link") {
        return true;
    }
    std::env::current_exe().is_ok_and(|e| c.contains(&e.to_string_lossy().to_ascii_lowercase()))
}

#[cfg(windows)]
pub use imp::*;

#[cfg(not(windows))]
mod other {
    use super::*;
    pub fn installed() -> Vec<Browser> {
        Vec::new()
    }
    pub fn current_default() -> Option<String> {
        None
    }
    pub fn registered() -> bool {
        false
    }
    pub fn is_default() -> bool {
        false
    }
    pub fn register(_exe: &Path, _browser: &str) -> anyhow::Result<()> {
        anyhow::bail!("only on Windows")
    }
    pub fn unregister() -> anyhow::Result<()> {
        Ok(())
    }
    pub fn open_in_browser(_preferred: Option<&str>, _url: &str) -> anyhow::Result<()> {
        anyhow::bail!("only on Windows")
    }
}
#[cfg(not(windows))]
pub use other::*;

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_registry::{CLASSES_ROOT, CURRENT_USER, Key, LOCAL_MACHINE};

    const CLIENTS: &str = r"Software\Clients\StartMenuInternet";
    const CLIENTS_32: &str = r"Software\WOW6432Node\Clients\StartMenuInternet";
    const REGISTERED: &str = r"Software\RegisteredApplications";

    fn text(k: &Key, name: &str) -> Option<String> {
        k.get_hstring(name).ok().map(|s| s.to_string_lossy()).filter(|s| !s.trim().is_empty())
    }

    /// A "@dll,-123" name, read from its resource.
    fn indirect(s: &str) -> Option<String> {
        use windows::Win32::UI::Shell::SHLoadIndirectString;
        use windows::core::HSTRING;
        let mut buf = [0u16; 260];
        // SAFETY: the source is a valid wide string and the buffer ours.
        unsafe { SHLoadIndirectString(&HSTRING::from(s), &mut buf, None) }.ok()?;
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n])).filter(|s| !s.is_empty())
    }

    fn client(root: &Key, base: &str, key: &str) -> Option<Browser> {
        let k = root.open(format!(r"{base}\{key}")).ok()?;
        let caps = k.open("Capabilities").ok();
        let prog_id = caps.as_ref().and_then(|c| c.open("URLAssociations").ok()).and_then(|u| text(&u, "https"))?;
        let name = caps
            .as_ref()
            .and_then(|c| text(c, "ApplicationName"))
            .and_then(|n| if n.starts_with('@') { indirect(&n) } else { Some(n) })
            .or_else(|| text(&k, ""))
            .unwrap_or_else(|| key.to_string());
        Some(Browser { name, prog_id })
    }

    /// The browsers on this PC, Hopper left out, by name.
    pub fn installed() -> Vec<Browser> {
        let ours = names();
        let mut out: Vec<Browser> = Vec::new();
        for (root, base) in [(CURRENT_USER, CLIENTS), (LOCAL_MACHINE, CLIENTS), (LOCAL_MACHINE, CLIENTS_32)] {
            let Ok(k) = root.open(base) else { continue };
            let Ok(keys) = k.keys() else { continue };
            for key in keys {
                let Some(b) = client(root, base, &key) else { continue };
                if key == ours.app || b.prog_id.starts_with("HopperURL") || out.iter().any(|o| o.prog_id == b.prog_id) {
                    continue;
                }
                // One whose links can't be opened isn't offered.
                if command_of(&b.prog_id).is_some_and(|c| !is_ours(&c)) {
                    out.push(b);
                }
            }
        }
        out.sort_by_key(|b| b.name.to_lowercase());
        out
    }

    /// The class Windows gives https links to now.
    pub fn current_default() -> Option<String> {
        let base = r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https";
        // Newer Windows 11 builds keep the choice in UserChoiceLatest.
        ["UserChoiceLatest", "UserChoice"]
            .iter()
            .find_map(|k| CURRENT_USER.open(format!(r"{base}\{k}")).ok().and_then(|k| text(&k, "ProgId")))
    }

    /// The command that opens a class's links.
    fn command_of(prog_id: &str) -> Option<String> {
        CLASSES_ROOT.open(format!(r"{prog_id}\shell\open\command")).ok().and_then(|k| text(&k, ""))
    }

    /// Whether Hopper is on Windows' list.
    pub fn registered() -> bool {
        CURRENT_USER.open(format!(r"{CLIENTS}\{}", names().app)).is_ok()
    }

    /// Whether the player picked Hopper for links in Windows.
    pub fn is_default() -> bool {
        current_default().is_some_and(|p| p == names().prog_id)
    }

    /// Puts Hopper on Windows' list of browsers, with `browser` (a class
    /// from [`installed`]) for every link that isn't a mod. Run again to
    /// change the browser.
    pub fn register(exe: &Path, browser: &str) -> anyhow::Result<()> {
        anyhow::ensure!(!browser.starts_with("HopperURL"), "pick a browser other than Hopper");
        let n = names();
        let exe = exe.display().to_string();
        let icon = format!("\"{exe}\",0");
        let desc = "Opens CurseForge and Modrinth links in Hopper, and every other link in your browser.";
        let class = CURRENT_USER.create(format!(r"Software\Classes\{}", n.prog_id))?;
        class.set_string("", format!("{} link", n.app))?;
        class.set_string("FriendlyTypeName", format!("{} link", n.app))?;
        class.create("DefaultIcon")?.set_string("", &icon)?;
        let app = class.create("Application")?;
        app.set_string("ApplicationName", &n.app)?;
        app.set_string("ApplicationIcon", &icon)?;
        app.set_string("ApplicationDescription", desc)?;
        class
            .create(r"shell\open\command")?
            .set_string("", format!("\"{exe}\" --open-link \"%1\" --browser \"{browser}\""))?;

        let client = CURRENT_USER.create(format!(r"{CLIENTS}\{}", n.app))?;
        client.set_string("", &n.app)?;
        client.create("DefaultIcon")?.set_string("", &icon)?;
        client.create(r"shell\open\command")?.set_string("", format!("\"{exe}\""))?;
        let caps = client.create("Capabilities")?;
        caps.set_string("ApplicationName", &n.app)?;
        caps.set_string("ApplicationIcon", &icon)?;
        caps.set_string("ApplicationDescription", desc)?;
        caps.create("StartMenu")?.set_string("StartMenuInternet", &n.app)?;
        let urls = caps.create("URLAssociations")?;
        urls.set_string("http", &n.prog_id)?;
        urls.set_string("https", &n.prog_id)?;
        CURRENT_USER
            .create(REGISTERED)?
            .set_string(&n.app, format!(r"{CLIENTS}\{}\Capabilities", n.app))?;
        changed();
        Ok(())
    }

    /// Takes Hopper off Windows' list: every key [`register`] wrote.
    pub fn unregister() -> anyhow::Result<()> {
        let n = names();
        let gone = |r: windows_registry::Result<()>| match r {
            // Already gone is fine.
            Err(e) if e.code() == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.to_hresult() => Ok(()),
            r => r,
        };
        gone(CURRENT_USER.remove_tree(format!(r"Software\Classes\{}", n.prog_id)))?;
        gone(CURRENT_USER.remove_tree(format!(r"{CLIENTS}\{}", n.app)))?;
        if let Ok(k) = CURRENT_USER.create(REGISTERED) {
            gone(k.remove_value(&n.app))?;
        }
        changed();
        Ok(())
    }

    /// Tells Explorer and Settings the list changed.
    fn changed() {
        use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
        // SAFETY: no items are passed for this event.
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }

    fn run(command: &str, url: &str) -> anyhow::Result<()> {
        use std::os::windows::process::CommandExt as _;
        anyhow::ensure!(!is_ours(command), "that browser is Hopper");
        let (exe, args) = fill_command(command, url).ok_or_else(|| anyhow::anyhow!("a browser command Hopper can't read"))?;
        std::process::Command::new(&exe).raw_arg(&args).spawn()?;
        Ok(())
    }

    /// Opens `url` in a real browser: `preferred` (a class), else the one
    /// Windows uses when that isn't Hopper, else any browser on the PC.
    /// Never through the shell, which could hand it back to Hopper.
    pub fn open_in_browser(preferred: Option<&str>, url: &str) -> anyhow::Result<()> {
        let ours = names().prog_id;
        let mut tried: Vec<String> = Vec::new();
        let candidates = preferred
            .map(str::to_string)
            .into_iter()
            .chain(current_default())
            .chain(installed().into_iter().map(|b| b.prog_id))
            .chain(["MSEdgeHTM".to_string()]);
        let mut last = None;
        for p in candidates {
            if p.starts_with("HopperURL") || p == ours || tried.contains(&p) {
                continue;
            }
            tried.push(p.clone());
            let Some(cmd) = command_of(&p) else { continue };
            match run(&cmd, url) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    log::warn!("links: {p} couldn't open it: {e:#}");
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no browser found")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_browser_commands() {
        let url = "https://example.com/a?b=c&d=e";
        assert_eq!(
            fill_command(r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --single-argument %1"#, url),
            Some((r"C:\Program Files\Google\Chrome\Application\chrome.exe".into(), format!("--single-argument {url}")))
        );
        assert_eq!(
            fill_command(r#""C:\Program Files\Mozilla Firefox\firefox.exe" -osint -url "%1""#, url),
            Some((r"C:\Program Files\Mozilla Firefox\firefox.exe".into(), format!("-osint -url \"{url}\"")))
        );
        // No placeholder: the link goes last.
        assert_eq!(fill_command(r#""C:\b\b.exe""#, url), Some((r"C:\b\b.exe".into(), format!("\"{url}\""))));
        assert_eq!(fill_command(r"C:\b\b.exe --new-tab", url), Some((r"C:\b\b.exe".into(), format!("--new-tab \"{url}\""))));
        assert_eq!(fill_command("", url), None);
        assert_eq!(fill_command("\"unclosed", url), None);
    }

    #[test]
    fn links_cannot_break_out_of_the_command() {
        assert_eq!(safe_url("https://x.com/a\" --evil \"b"), "https://x.com/a%22%20--evil%20%22b");
        assert_eq!(safe_url(" https://x.com/a\r\nb "), "https://x.com/ab");
        let (_, args) = fill_command(r#""C:\f.exe" -url "%1""#, "https://x.com/\"&calc").unwrap();
        assert_eq!(args, "-url \"https://x.com/%22&calc\"");
    }

    #[test]
    fn expands_folders_in_commands() {
        // SAFETY: tests in this module don't read this variable elsewhere.
        unsafe { std::env::set_var("HOPPER_TEST_DIR", r"C:\Apps") };
        assert_eq!(expand_env(r"%HOPPER_TEST_DIR%\b.exe"), r"C:\Apps\b.exe");
        assert_eq!(expand_env(r"%NOT_SET_HOPPER_X%\b.exe"), r"%NOT_SET_HOPPER_X%\b.exe");
        assert_eq!(expand_env("100%"), "100%");
    }

    #[test]
    fn hopper_commands_are_never_used() {
        assert!(is_ours(r#""C:\x\hopper.exe" --open-link "%1" --browser "MSEdgeHTM""#));
        assert!(!is_ours(r#""C:\Program Files\Mozilla Firefox\firefox.exe" -osint -url "%1""#));
    }
}
