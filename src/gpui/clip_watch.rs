//! Copied mod links: while it's on, a CurseForge or Modrinth project link
//! the player copies anywhere (Discord, a browser) brings up a Windows
//! notification, "Open Sodium in Hopper?", with the project's icon and
//! Open / Dismiss. Open shows Hopper with the project's panel.
//!
//! Windows tells a hidden window of ours about every clipboard change
//! (AddClipboardFormatListener), so nothing polls. It stays quiet for text
//! copied inside Hopper, the same link twice in a row, clipboard content
//! other apps mark private (password managers), and while a game or video
//! runs fullscreen.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The listener's window, while it runs.
static HWND_SLOT: Mutex<Option<isize>> = Mutex::new(None);
/// The last link offered.
static LAST: Mutex<String> = Mutex::new(String::new());
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Starts or stops listening (the setting, at start and when it changes).
pub fn set_enabled(on: bool) {
    #[cfg(windows)]
    {
        if on {
            if !RUNNING.swap(true, Ordering::SeqCst) {
                let _ = std::thread::Builder::new().name("clip-watch".into()).spawn(imp::run);
            }
        } else if let Some(h) = *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) {
            imp::stop(h);
        }
    }
    #[cfg(not(windows))]
    let _ = on;
}

/// A copied text: offer it when it's a mod link that's new.
#[cfg_attr(not(windows), allow(dead_code))]
fn copied(text: &str) {
    let Some((url, link)) = hopper::modlinks::from_text(text) else { return };
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if *last == url {
            return;
        }
        *last = url.clone();
    }
    #[cfg(windows)]
    if imp::busy() {
        log::info!("clip-watch: {url} copied while fullscreen; not offered");
        return;
    }
    // The lookup and icon take a moment; the listener stays free.
    let _ = std::thread::Builder::new().name("clip-offer".into()).spawn(move || offer(url, link));
}

fn offer(url: String, link: hopper::modlinks::ModLink) {
    let m = match hopper::source::mod_from_link(&link.project) {
        Ok(m) => m,
        // Not a project (a typo, a deleted one): nothing to offer.
        Err(e) => {
            log::info!("clip-watch: {url}: {e:#}");
            return;
        }
    };
    // The icon gets two seconds; a slow image site doesn't hold up the
    // question, which then shows Hopper's logo.
    let icon = m.icon().map(str::to_string).and_then(|u| {
        let (tx, rx) = std::sync::mpsc::channel();
        let id = m.id;
        std::thread::spawn(move || {
            let _ = tx.send(icon_png(id, &u).map_err(|e| log::info!("clip-watch: icon: {e:#}")).ok());
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).ok().flatten()
    });
    #[cfg(windows)]
    if let Err(e) = imp::toast(&url, &m.name, link.site(), icon.as_deref()) {
        log::warn!("clip-watch: notification: {e:#}");
    }
    #[cfg(not(windows))]
    let _ = icon;
}

/// The project's icon as a small PNG for the notification (which takes
/// PNG or JPEG files, not the WebP the sites often send).
fn icon_png(id: u64, url: &str) -> anyhow::Result<std::path::PathBuf> {
    let src = hopper::imgcache::fetch(url)?;
    let dir = std::env::temp_dir().join("hopper-link-icons");
    std::fs::create_dir_all(&dir)?;
    let out = dir.join(format!("{id}.png"));
    if !out.is_file() {
        image::open(&src)?.thumbnail(128, 128).save(&out)?;
    }
    Ok(out)
}

/// Open on the notification: the same road a clicked link takes, so the
/// open Hopper comes to the front with the project.
#[cfg_attr(not(windows), allow(dead_code))]
fn open(url: &str) {
    if !hopper::links::forward(url) {
        log::warn!("clip-watch: Hopper's link hand-off isn't listening");
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
    use windows::Win32::System::DataExchange::{
        AddClipboardFormatListener, CloseClipboard, GetClipboardData, GetClipboardOwner, IsClipboardFormatAvailable,
        OpenClipboard, RegisterClipboardFormatW, RemoveClipboardFormatListener,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    use windows::Win32::System::Ole::CF_UNICODETEXT;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::Shell::{
        QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::w;

    pub fn run() {
        // SAFETY: plain Win32 on this thread's own window; the class name
        // and window procedure live for the process.
        unsafe {
            let Ok(module) = GetModuleHandleW(None) else {
                RUNNING.store(false, Ordering::SeqCst);
                return;
            };
            let class = w!("HopperClipWatch");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: module.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let made = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                w!("Hopper clipboard"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(module.into()),
                None,
            );
            let Ok(hwnd) = made else {
                RUNNING.store(false, Ordering::SeqCst);
                return;
            };
            if AddClipboardFormatListener(hwnd).is_err() {
                log::warn!("clip-watch: Windows refused the listener");
                let _ = DestroyWindow(hwnd);
                RUNNING.store(false, Ordering::SeqCst);
                return;
            }
            log::info!("clip-watch: listening");
            *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                DispatchMessageW(&msg);
            }
            *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
            LAST.lock().unwrap_or_else(|e| e.into_inner()).clear();
            RUNNING.store(false, Ordering::SeqCst);
            log::info!("clip-watch: stopped");
        }
    }

    pub fn stop(h: isize) {
        // SAFETY: posting to our own window; it closes itself on its thread.
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_CLIPBOARDUPDATE => {
                if let Some(text) = unsafe { read_text(hwnd) } {
                    copied(&text);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                unsafe {
                    let _ = RemoveClipboardFormatListener(hwnd);
                    let _ = DestroyWindow(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// The clipboard's text, unless Hopper put it there or the app that did
    /// asked clipboard watchers to leave it alone.
    unsafe fn read_text(hwnd: HWND) -> Option<String> {
        unsafe {
            if let Ok(owner) = GetClipboardOwner()
                && !owner.is_invalid()
            {
                let mut pid = 0u32;
                GetWindowThreadProcessId(owner, Some(&mut pid));
                if pid == GetCurrentProcessId() {
                    return None;
                }
            }
            let private = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
            if private != 0 && IsClipboardFormatAvailable(private).is_ok() {
                return None;
            }
            IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32).ok()?;
            // The app that copied may still hold it for a moment.
            let mut open = false;
            for _ in 0..5 {
                if OpenClipboard(Some(hwnd)).is_ok() {
                    open = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if !open {
                return None;
            }
            let text = (|| {
                let h: HANDLE = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
                let g = HGLOBAL(h.0);
                // Only short texts can be a link; a long one isn't read.
                let units = GlobalSize(g) / 2;
                if units == 0 || units > 4096 {
                    return None;
                }
                let p = GlobalLock(g) as *const u16;
                if p.is_null() {
                    return None;
                }
                let s = std::slice::from_raw_parts(p, units);
                let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
                let out = String::from_utf16_lossy(&s[..n]);
                let _ = GlobalUnlock(g);
                Some(out)
            })();
            let _ = CloseClipboard();
            text
        }
    }

    /// A game, video or presentation fills the screen: Windows' own
    /// "busy" states, or the window in front covering its whole monitor
    /// (borderless fullscreen, which Windows doesn't count).
    pub fn busy() -> bool {
        // SAFETY: plain queries; the structs are ours.
        unsafe {
            if let Ok(s) = SHQueryUserNotificationState()
                && [QUNS_BUSY, QUNS_RUNNING_D3D_FULL_SCREEN, QUNS_PRESENTATION_MODE].contains(&s)
            {
                return true;
            }
            let fg = GetForegroundWindow();
            if fg.is_invalid() || fg == GetShellWindow() || fg == GetDesktopWindow() {
                return false;
            }
            let mut class = [0u16; 32];
            let n = GetClassNameW(fg, &mut class) as usize;
            let class = String::from_utf16_lossy(&class[..n]);
            if class == "WorkerW" || class == "Progman" {
                return false;
            }
            // A maximized window (a browser, Discord) also covers the
            // screen, but has a title bar or says it's maximized; a game
            // in borderless fullscreen has neither.
            let style = GetWindowLongW(fg, GWL_STYLE) as u32;
            if IsZoomed(fg).as_bool() || style & WS_CAPTION.0 == WS_CAPTION.0 {
                return false;
            }
            let mut r = RECT::default();
            if GetWindowRect(fg, &mut r).is_err() {
                return false;
            }
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
                return false;
            }
            let m = mi.rcMonitor;
            r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
        }
    }

    // ── the notification ──────────────────────────────────────────────

    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::TypedEventHandler;
    use windows::UI::Notifications::{ToastActivatedEventArgs, ToastNotification, ToastNotificationManager};
    use windows::core::{HSTRING, IInspectable, Interface as _};

    /// The notification on screen, kept so its Open still works and a new
    /// one can replace it.
    static SHOWN: Mutex<Option<ToastNotification>> = Mutex::new(None);

    /// Hopper's name on its notifications. An app that isn't installed from
    /// the Store needs it registered (per user) for Windows to show them.
    fn app_id() -> String {
        let n = hopper::browser::names();
        let tag: String = n.prog_id.chars().filter(char::is_ascii_alphanumeric).collect();
        let id = format!("Hopper.ModLinks.{tag}");
        let key = format!(r"Software\Classes\AppUserModelId\{id}");
        if let Ok(k) = windows_registry::CURRENT_USER.create(&key) {
            let _ = k.set_string("DisplayName", &n.app);
            if let Some(icon) = brand_icon() {
                let _ = k.set_string("IconUri", icon.display().to_string());
            }
        }
        id
    }

    /// Hopper's logo as a file, for the notification's header.
    fn brand_icon() -> Option<std::path::PathBuf> {
        let out = std::env::temp_dir().join("hopper-link-icons").join("hopper.png");
        if out.is_file() {
            return Some(out);
        }
        std::fs::create_dir_all(out.parent()?).ok()?;
        std::fs::write(&out, include_bytes!("../../assets/hopper-256.png")).ok()?;
        Some(out)
    }

    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    pub fn toast(url: &str, name: &str, site: &str, icon: Option<&std::path::Path>) -> anyhow::Result<()> {
        let image = icon
            .map(|p| format!(r#"<image placement="appLogoOverride" src="{}"/>"#, esc(&format!("file:///{}", p.display().to_string().replace('\\', "/")))))
            .unwrap_or_default();
        let xml = format!(
            r#"<toast launch="open"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text>{image}</binding></visual><actions><action content="Open" arguments="open"/><action content="Dismiss" arguments="dismiss" activationType="system"/></actions><audio silent="true"/></toast>"#,
            esc(&format!("Open {name} in Hopper?")),
            esc(&format!("You copied a {site} link.")),
        );
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml))?;
        let t = ToastNotification::CreateToastNotification(&doc)?;
        t.SetExpiresOnReboot(true)?;
        let url = url.to_string();
        t.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(move |_, args| {
            let arg = args
                .as_ref()
                .and_then(|a| a.cast::<ToastActivatedEventArgs>().ok())
                .and_then(|a| a.Arguments().ok())
                .map(|a| a.to_string())
                .unwrap_or_default();
            if arg != "dismiss" {
                open(&url);
            }
            Ok(())
        }))?;
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id()))?;
        let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(prev) = shown.take() {
            let _ = notifier.Hide(&prev);
        }
        notifier.Show(&t)?;
        *shown = Some(t);
        log::info!("clip-watch: offered {name}");
        Ok(())
    }
}
