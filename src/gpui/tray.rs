//! Hopper's icon in the notification area: a click brings Hopper back, its
//! menu shows it or quits, and it pops Windows notifications (balloons,
//! which Windows 10 and 11 show as toasts) for things that finish while
//! Hopper isn't in front.
//!
//! It runs on its own thread with its own hidden window and message loop,
//! so GPUI's loop never has to know about it.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, OnceLock};

/// What the tray asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    Show,
    Quit,
}

/// The tray's window, for notifications from any thread.
static HWND_SLOT: Mutex<Option<isize>> = Mutex::new(None);
static EVENTS: OnceLock<Mutex<Sender<TrayEvent>>> = OnceLock::new();

/// Adds the icon; events arrive on the returned receiver.
pub fn start() -> Receiver<TrayEvent> {
    let (tx, rx) = channel();
    let _ = EVENTS.set(Mutex::new(tx));
    #[cfg(windows)]
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(imp::run)
        .ok();
    rx
}

/// A Windows notification from Hopper's icon. Quiet if there is no icon.
pub fn notify(title: &str, body: &str, error: bool) {
    #[cfg(windows)]
    imp::notify(title, body, error);
    #[cfg(not(windows))]
    let _ = (title, body, error);
}

/// Takes the icon away (on quit, so no dead icon lingers).
pub fn remove() {
    #[cfg(windows)]
    imp::remove();
}

fn send(e: TrayEvent) {
    if let Some(tx) = EVENTS.get()
        && let Ok(tx) = tx.lock()
    {
        let _ = tx.send(e);
    }
}

#[cfg(windows)]
mod imp {
    use super::{HWND_SLOT, TrayEvent, send};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIM_ADD, NIM_DELETE,
        NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::{PCWSTR, w};

    const WM_TRAY: u32 = WM_APP + 1;
    const ID_SHOW: usize = 1;
    const ID_QUIT: usize = 2;

    fn copy(dst: &mut [u16], s: &str) {
        let n = dst.len() - 1;
        for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(n).chain(std::iter::once(0))) {
            *d = c;
        }
    }

    fn data(hwnd: HWND) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            ..Default::default()
        }
    }

    pub fn run() {
        // SAFETY: plain Win32 on this thread's own window; the class name
        // and window procedure live for the process.
        unsafe {
            let Ok(module) = GetModuleHandleW(None) else { return };
            let class = w!("HopperTray");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc),
                hInstance: module.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let Ok(hwnd) = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                w!("Hopper tray"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(module.into()),
                None,
            ) else {
                return;
            };
            let mut nid = data(hwnd);
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY;
            // The exe's own icon (resource 1, from build.rs).
            nid.hIcon = LoadIconW(Some(module.into()), PCWSTR(1 as _))
                .or_else(|_| LoadIconW(None, IDI_APPLICATION))
                .unwrap_or_default();
            copy(&mut nid.szTip, "Hopper");
            if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
                log::warn!("tray: Windows refused the icon");
                return;
            }
            log::info!("tray: icon added");
            *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
        }
    }

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_TRAY {
            match lparam.0 as u32 {
                WM_LBUTTONUP | NIN_BALLOONUSERCLICK => send(TrayEvent::Show),
                WM_RBUTTONUP => unsafe { menu(hwnd) },
                _ => {}
            }
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// NIN_BALLOONUSERCLICK (WM_USER + 5): the notification was clicked.
    const NIN_BALLOONUSERCLICK: u32 = WM_USER + 5;

    unsafe fn menu(hwnd: HWND) {
        unsafe {
            let Ok(m) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(m, MF_STRING, ID_SHOW, w!("Show Hopper"));
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(m, MF_STRING, ID_QUIT, w!("Quit Hopper"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            // The menu closes on a click elsewhere only if its window is
            // in front first.
            let _ = SetForegroundWindow(hwnd);
            let picked = TrackPopupMenu(
                m,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                pt.x,
                pt.y,
                Some(0),
                hwnd,
                None,
            );
            let _ = DestroyMenu(m);
            match picked.0 as usize {
                ID_SHOW => send(TrayEvent::Show),
                ID_QUIT => send(TrayEvent::Quit),
                _ => {}
            }
        }
    }

    pub fn remove() {
        let Some(h) = HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return;
        };
        // SAFETY: deleting our own icon by its window and id.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data(HWND(h as *mut _)));
        }
    }

    pub fn notify(title: &str, body: &str, error: bool) {
        let Some(h) = *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) else {
            return;
        };
        let mut nid = data(HWND(h as *mut _));
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = if error { NIIF_ERROR } else { NIIF_INFO };
        copy(&mut nid.szInfoTitle, title);
        copy(&mut nid.szInfo, body);
        // SAFETY: the icon's window outlives the call; the struct is ours.
        let shown = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool();
        log::info!("tray: notification {}", if shown { "shown" } else { "refused" });
    }
}
