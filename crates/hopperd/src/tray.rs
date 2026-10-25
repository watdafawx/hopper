//! Hopper Server's icon in the notification area (portable mode, Windows).
//!
//! Runs the message loop on the calling thread; menu picks go to the
//! launcher as [`Cmd`]s on a channel (so slow work, like stopping servers,
//! never freezes the icon). The menu reads [`MenuState`], which the
//! launcher keeps current.

use std::sync::mpsc::Sender;
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

/// A menu pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Open,
    CopyAddress,
    OpenFolder,
    ToggleStartup,
    InstallService,
    Quit,
    /// Windows is signing out or shutting down.
    EndSession,
}

/// What the menu shows.
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    /// "Running · 2 servers up · port 8790".
    pub status: String,
    /// The address other devices use, when there is one.
    pub lan: Option<String>,
    pub startup: bool,
    /// Busy (stopping servers, installing): only the status line.
    pub busy: bool,
}

static STATE: Mutex<Option<MenuState>> = Mutex::new(None);
static CMDS: OnceLock<Mutex<Sender<Cmd>>> = OnceLock::new();
static HWND_SLOT: Mutex<Option<isize>> = Mutex::new(None);

const WM_TRAY: u32 = WM_APP + 1;
const WM_REFRESH: u32 = WM_APP + 2;

const ID_OPEN: usize = 1;
const ID_COPY: usize = 2;
const ID_FOLDER: usize = 3;
const ID_STARTUP: usize = 4;
const ID_SERVICE: usize = 5;
const ID_QUIT: usize = 6;

pub fn set_state(s: MenuState) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
    post(WM_REFRESH);
}

pub fn state() -> MenuState {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn send(c: Cmd) {
    if let Some(tx) = CMDS.get() {
        let _ = tx.lock().unwrap_or_else(|e| e.into_inner()).send(c);
    }
}

fn post(msg: u32) {
    if let Some(h) = *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) {
        // SAFETY: posting to our own window; a closed one just fails.
        unsafe {
            let _ = PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// Ends [`run`]'s loop (from any thread); the icon goes with it.
pub fn close() {
    post(WM_CLOSE);
}

fn copy(dst: &mut [u16], s: &str) {
    let n = dst.len() - 1;
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(n).chain(std::iter::once(0))) {
        *d = c;
    }
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: hwnd, uID: 1, ..Default::default() }
}

fn tip() -> String {
    let s = state();
    if s.status.is_empty() { crate::service::display() } else { format!("{}\n{}", crate::service::display(), s.status) }
}

/// Adds the icon and runs the message loop until [`close`].
pub fn run(cmds: Sender<Cmd>) {
    let _ = CMDS.set(Mutex::new(cmds));
    // SAFETY: plain Win32 on this thread's own window; the class name and
    // window procedure live for the process.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let class = w!("HopperServerTray");
        let wc = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: module.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        // A hidden top-level window (not message-only): it gets
        // TaskbarCreated when Explorer restarts, and the end-of-session
        // messages.
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("Hopper Server"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(module.into()),
            None,
        ) else {
            return;
        };
        *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd.0 as isize);
        add_icon(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &data(hwnd));
        *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

unsafe fn add_icon(hwnd: HWND) {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let mut nid = data(hwnd);
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_TRAY;
        // The exe's own icon (resource 1, from build.rs).
        nid.hIcon = LoadIconW(Some(module.into()), PCWSTR(1 as _)).or_else(|_| LoadIconW(None, IDI_APPLICATION)).unwrap_or_default();
        copy(&mut nid.szTip, &tip());
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            log::warn!("tray: Windows refused the icon");
        }
    }
}

fn taskbar_created() -> u32 {
    static MSG: OnceLock<u32> = OnceLock::new();
    // SAFETY: registers (or looks up) a named message.
    *MSG.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONDBLCLK | WM_LBUTTONUP => {
                    if !state().busy {
                        send(Cmd::Open);
                    }
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { menu(hwnd) },
                _ => {}
            }
            LRESULT(0)
        }
        WM_REFRESH => {
            let mut nid = data(hwnd);
            nid.uFlags = NIF_TIP;
            copy(&mut nid.szTip, &tip());
            // SAFETY: our own icon.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            }
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                send(Cmd::EndSession);
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: our own window, from its own thread.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m == taskbar_created() => {
            // Explorer restarted: the icon has to be added again.
            unsafe { add_icon(hwnd) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn menu(hwnd: HWND) {
    let s = state();
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let item = |flags: MENU_ITEM_FLAGS, id: usize, text: &str| {
            let _ = AppendMenuW(m, flags, id, &HSTRING::from(text));
        };
        if !s.status.is_empty() {
            item(MF_STRING | MF_GRAYED, 0, &s.status);
        }
        if !s.busy {
            item(MF_STRING, ID_OPEN, "Open panel");
            let _ = SetMenuDefaultItem(m, ID_OPEN as u32, 0);
            if let Some(lan) = &s.lan {
                item(MF_STRING, ID_COPY, &format!("Copy address for other devices\t{}", lan.trim_start_matches("http://")));
            }
            item(MF_STRING, ID_FOLDER, "Open panel folder");
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
            item(if s.startup { MF_STRING | MF_CHECKED } else { MF_STRING }, ID_STARTUP, "Start with Windows");
            item(MF_STRING, ID_SERVICE, "Install as a service…");
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
            item(MF_STRING, ID_QUIT, "Quit");
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // The menu closes on a click elsewhere only if its window is in
        // front first.
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_BOTTOMALIGN, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(m);
        match picked.0 as usize {
            ID_OPEN => send(Cmd::Open),
            ID_COPY => send(Cmd::CopyAddress),
            ID_FOLDER => send(Cmd::OpenFolder),
            ID_STARTUP => send(Cmd::ToggleStartup),
            ID_SERVICE => send(Cmd::InstallService),
            ID_QUIT => send(Cmd::Quit),
            _ => {}
        }
    }
}

/// A Windows notification from the icon.
pub fn notify(title: &str, body: &str, error: bool) {
    let Some(h) = *HWND_SLOT.lock().unwrap_or_else(|e| e.into_inner()) else { return };
    let mut nid = data(HWND(h as *mut _));
    nid.uFlags = NIF_INFO;
    nid.dwInfoFlags = if error { NIIF_ERROR } else { NIIF_INFO };
    copy(&mut nid.szInfoTitle, title);
    copy(&mut nid.szInfo, body);
    // SAFETY: the icon's window outlives the call; the struct is ours.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// Puts `text` on the clipboard.
pub fn clipboard(text: &str) -> bool {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the usual clipboard dance; the memory belongs to the
    // clipboard once SetClipboardData succeeds.
    unsafe {
        if OpenClipboard(None).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = (|| {
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            // CF_UNICODETEXT
            SetClipboardData(13, Some(HANDLE(mem.0))).ok()
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}
