//! The link opener. Once the player picks Hopper for links in Windows'
//! Default apps, Windows starts `hopper --open-link <url> --browser <class>`
//! for every link they click anywhere. This runs before any window: a
//! CurseForge or Modrinth project goes to the Hopper that's open (or starts
//! one), and every other link goes straight on to the browser the player
//! chose, so it costs them no more than a moment.
//!
//! A link Hopper itself opens (a project's "Open on CurseForge") comes back
//! here too, since Hopper is the default; those go to the browser, or
//! they'd land back in Hopper.

/// What `main` does after [`route`].
pub enum Route {
    /// The link went somewhere; this process is done.
    Done,
    /// No Hopper is open: start one, which opens the link.
    StartHopper,
}

/// None when this isn't a link launch.
pub fn route(args: &[String]) -> Option<Route> {
    let i = args.iter().position(|a| a == "--open-link")?;
    let url = args.get(i + 1).cloned().unwrap_or_default();
    let browser = args
        .iter()
        .position(|a| a == "--browser")
        .and_then(|j| args.get(j + 1))
        .cloned();
    let mod_link = hopper::modlinks::parse(&url).is_some();
    if mod_link && !from_hopper() {
        allow_front();
        if hopper::links::forward(&url) {
            return Some(Route::Done);
        }
        return Some(Route::StartHopper);
    }
    if let Err(e) = hopper::browser::open_in_browser(browser.as_deref(), &url) {
        failed(&url, &e);
    }
    Some(Route::Done)
}

/// The URL a link launch carries, for the Hopper it starts.
pub fn url_arg(args: &[String]) -> Option<String> {
    let i = args.iter().position(|a| a == "--open-link")?;
    args.get(i + 1).cloned()
}

/// Whether the program that opened the link is Hopper (the same exe name),
/// so Hopper's own website buttons still reach the browser.
#[cfg(windows)]
fn from_hopper() -> bool {
    let Some(parent) = parent_exe() else { return false };
    let Ok(me) = std::env::current_exe() else { return false };
    let name = |p: &std::path::Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase());
    parent == me || (name(&parent).is_some() && name(&parent) == name(&me))
}

#[cfg(not(windows))]
fn from_hopper() -> bool {
    false
}

/// The exe of the process that started this one.
#[cfg(windows)]
fn parent_exe() -> Option<std::path::PathBuf> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    // SAFETY: a process snapshot and a query of one process, both closed
    // before returning; the buffers are ours.
    unsafe {
        let me = GetCurrentProcessId();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()?;
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut parent = None;
        let mut more = Process32FirstW(snap, &mut e).is_ok();
        while more {
            if e.th32ProcessID == me {
                parent = Some(e.th32ParentProcessID);
                break;
            }
            more = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, parent?).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]).into())
    }
}

/// Lets the open Hopper come to the front: this process was started by
/// the player's click, so Windows lets it pass that on.
fn allow_front() {
    #[cfg(windows)]
    // SAFETY: a plain call with no pointers.
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(
            windows::Win32::UI::WindowsAndMessaging::ASFW_ANY,
        );
    }
}

/// No browser would take the link: say so, with the link, rather than
/// dropping it. (Ctrl+C in the message copies its text.)
fn failed(url: &str, e: &anyhow::Error) {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
        use windows::core::HSTRING;
        let text = format!(
            "Hopper couldn't find a browser to open this link:\n\n{url}\n\nPick another one in Hopper's Settings › Mod sources. ({e:#})"
        );
        // SAFETY: both strings outlive the call.
        unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("Open link"), MB_OK | MB_ICONWARNING) };
    }
    #[cfg(not(windows))]
    let _ = (url, e);
}
