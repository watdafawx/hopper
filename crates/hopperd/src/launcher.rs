//! What double-clicking hopperd.exe does on Windows.
//!
//! - First time: a dialog asks how to run it. **Just run it** and **Start
//!   with Windows** run it portable, as you, with a tray icon (the second
//!   also adds it to your sign-in, `HKCU\…\Run`, no admin). **Install as a
//!   service** copies it to `C:\Program Files\Hopper Server` and registers
//!   the Windows service (one UAC prompt), then opens the panel.
//! - When it runs already (any mode): Open panel / Update (a newer exe) /
//!   Uninstall, instead of a second copy.
//! - From a terminal, or with any argument (`--portable`), none of this:
//!   hopperd runs as it always has.
//!
//! Portable mode is two processes: this one keeps the tray icon and starts
//! the panel as a child (`hopperd --portable`, no window, log in
//! `hopperd.log` in the data folder). The panel restarts itself by exiting
//! (Settings › Restart, a new panel folder): the child is started again,
//! as the service manager does for the service. The two talk through
//! `control` (a token only this tray knows).

use crate::control::{self, Running};
use crate::service;
use crate::tray::{self, Cmd, MenuState};
use anyhow::{Context as _, bail};
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::Win32::System::Threading::{GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_NOTIFICATIONS, TASKDIALOGCONFIG, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_USE_COMMAND_LINKS,
    TDF_USE_HICON_MAIN, TDN_CREATED,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, HRESULT, HSTRING, PCWSTR, s, w};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const OUR_KEY: PCWSTR = w!(r"Software\Hopper Server");

// ---- start-up --------------------------------------------------------------------

/// Started by a double-click: this process owns its console alone (from a
/// terminal, the shell is attached too).
pub fn double_clicked() -> bool {
    let mut ids = [0u32; 4];
    // SAFETY: fills our buffer.
    unsafe { GetConsoleProcessList(&mut ids) == 1 }
}

fn hide_console() {
    // SAFETY: detaches from the console window Windows made for us.
    unsafe {
        let _ = FreeConsole();
    }
}

fn boot_dir() -> PathBuf {
    crate::config::data_dir()
}

/// Logs to `hopperd.log` in the start-up data folder (it stays there when
/// the panel folder is set).
fn init_log() {
    let dir = boot_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("hopperd.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 20 << 20) {
        let _ = std::fs::rename(&path, dir.join("hopperd.log.old"));
    }
    let mut log = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        log.target(env_logger::Target::Pipe(Box::new(f)));
    }
    let _ = log.try_init();
}

/// Double-clicked.
pub fn start() -> anyhow::Result<()> {
    hide_console();
    init_log();
    if let Err(e) = first_steps() {
        log::error!("{e:#}");
        error_box(&format!("{e:#}"));
    }
    Ok(())
}

/// `hopperd --tray` (Start with Windows): the tray, without the dialog.
/// With the service installed, the service's tray (its state, start/stop).
pub fn tray_only() -> anyhow::Result<()> {
    hide_console();
    init_log();
    // Started by the tray of the copy just updated: it closes first.
    if let Some(pid) = std::env::var("HOPPERD_WAIT_PID").ok().and_then(|p| p.parse::<u32>().ok()) {
        if !wait_exit(pid, Duration::from_secs(30)) {
            log::warn!("the previous tray (pid {pid}) didn't close");
        }
    }
    if let Some(s) = service::installed() {
        return run_service_tray(s.data.unwrap_or_else(boot_dir));
    }
    if find_running().is_some() {
        log::info!("already running; not starting a second copy");
        return Ok(());
    }
    run_tray(saved_mode().filter(|m| m == "portable" || m == "startup").unwrap_or_else(|| "startup".into()), false)
}

fn first_steps() -> anyhow::Result<()> {
    if let Some(r) = find_running() {
        return already_running(r);
    }
    match saved_mode().as_deref() {
        Some(m @ ("portable" | "startup")) => return run_tray(m.into(), true),
        _ => {}
    }
    match first_run_dialog() {
        Choice::Portable => {
            save_mode("portable");
            run_tray("portable".into(), true)
        }
        Choice::Startup => {
            set_startup(true)?;
            run_tray("startup".into(), true)
        }
        Choice::Service => {
            install_service()?;
            Ok(())
        }
        Choice::Cancel => Ok(()),
    }
}

// ---- what runs already -----------------------------------------------------------

enum Found {
    Service { running: bool, boot: PathBuf },
    Tray(Running),
}

fn find_running() -> Option<Found> {
    if let Some(s) = service::installed() {
        return Some(Found::Service { running: s.running, boot: s.data.unwrap_or_else(boot_dir) });
    }
    let r = control::read_running()?;
    if alive(r.pid) || r.tray_pid.is_some_and(alive) {
        return Some(Found::Tray(r));
    }
    let _ = std::fs::remove_file(control::running_file());
    None
}

fn alive(pid: u32) -> bool {
    // SAFETY: opens and closes a handle to look at a process.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code).is_ok() && code == 259; // STILL_ACTIVE
        let _ = CloseHandle(h);
        ok
    }
}

fn wait_exit(pid: u32, limit: Duration) -> bool {
    // SAFETY: waits on a process handle we open and close.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return true };
        let r = WaitForSingleObject(h, limit.as_millis() as u32);
        let _ = CloseHandle(h);
        r.0 == 0
    }
}

/// "0.10.0" > "0.9.1".
fn newer(a: &str, b: &str) -> bool {
    let v = |s: &str| s.split(['.', '-']).take(3).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    v(a) > v(b)
}

fn hello(port: u16) -> Option<serde_json::Value> {
    let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(2))).build().new_agent();
    agent.get(&format!("http://127.0.0.1:{port}/api/v1/hello")).call().ok()?.body_mut().read_json().ok()
}

fn wait_for_panel(port: u16, limit: Duration) -> bool {
    let t = Instant::now();
    while t.elapsed() < limit {
        if hello(port).is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(400));
    }
    false
}

/// The panel's address on this PC; with the setup code in the link while
/// nobody has set it up (the panel reads it and skips that field).
fn panel_url(port: u16, setup_code: Option<&str>) -> String {
    match setup_code {
        Some(c) => format!("http://localhost:{port}/#setup={c}"),
        None => format!("http://localhost:{port}/"),
    }
}

/// The service's setup code, from its data folder, while it has no owner.
fn service_setup_code(boot: &Path) -> Option<String> {
    let (data, _) = service::real_data(boot);
    if service::has_owner(&data) {
        return None;
    }
    std::fs::read_to_string(data.join(crate::SETUP_FILE)).ok().map(|c| c.trim().to_string()).filter(|c| !c.is_empty())
}

fn open_service_panel(boot: &Path) {
    let port = service::real_data(boot).1.port;
    wait_for_panel(port, Duration::from_secs(30));
    // The code file is written as the panel starts.
    open_url(&panel_url(port, service_setup_code(boot).as_deref()));
}

fn already_running(found: Found) -> anyhow::Result<()> {
    let me = env!("CARGO_PKG_VERSION");
    let (how, port, version) = match &found {
        Found::Service { running, boot, .. } => {
            let port = service::real_data(boot).1.port;
            let v = hello(port).and_then(|h| h["version"].as_str().map(String::from));
            (if *running { "As a Windows service" } else { "As a Windows service (stopped)" }, port, v)
        }
        Found::Tray(r) => (if r.mode == "startup" { "Starts with Windows" } else { "Portable" }, r.port, Some(r.version.clone())),
    };
    let update = version.as_deref().is_some_and(|v| newer(me, v));
    let elevated = matches!(found, Found::Service { .. });
    let mut content = format!("{how} · v{} · port {port}.", version.as_deref().unwrap_or("?"));
    if update {
        content.push_str(&format!(" This file is v{me}."));
    }
    let mut buttons = vec![(100, "Open panel\nhttp://localhost:".to_string() + &port.to_string(), false)];
    if update {
        buttons.push((
            101,
            format!("Update to v{me}\nReplaces the running copy with this one and restarts it. Your servers keep running and the panel is back in a few seconds."),
            elevated,
        ));
    }
    if let Found::Service { .. } = &found {
        buttons.push((103, "Show tray icon\nShows whether the service is up, and starts or stops it from the icon. Also at sign-in.".into(), false));
    }
    buttons.push((102, "Uninstall…\nRemoves the service or the startup entry. Your servers and data stay where they are.".into(), elevated));
    let picked = task_dialog(&format!("{} is already running", service::display()), &content, &buttons, None);
    match (picked, found) {
        (Some(100), Found::Service { running, boot, .. }) => {
            if !running {
                elevate(&["service", "start"])?;
            }
            open_service_panel(&boot);
        }
        (Some(100), Found::Tray(r)) => {
            let code = tray_status(&r).and_then(|s| s["setup_code"].as_str().map(String::from));
            open_url(&panel_url(r.port, code.as_deref()));
        }
        (Some(101), Found::Service { boot, .. }) => {
            elevate(&["service", "update"])?;
            info_box(&format!("Updated to v{me}."));
            open_service_panel(&boot);
        }
        (Some(103), Found::Service { .. }) => {
            set_service_tray_startup(true);
            start_service_tray();
        }
        (Some(101), Found::Tray(r)) => {
            quit_running(&r)?;
            let exe = if r.exe.as_os_str().is_empty() { std::env::current_exe()? } else { r.exe.clone() };
            let me_exe = std::env::current_exe()?;
            if crate::absolute(&exe) != crate::absolute(&me_exe) {
                service::copy_retrying(&me_exe, &exe)?;
            }
            spawn_tray(&exe).with_context(|| format!("starting {}", exe.display()))?;
            if wait_for_panel(r.port, Duration::from_secs(30)) {
                open_url(&panel_url(r.port, None));
            }
        }
        (Some(102), found) => {
            let folder = match &found {
                Found::Service { boot, .. } => service::real_data(boot).0,
                Found::Tray(r) => tray_status(r).and_then(|s| s["data"].as_str().map(PathBuf::from)).unwrap_or_else(boot_dir),
            };
            let q = format!("Stop {} and remove it?\n\nYour servers and the panel's data stay in {}.", service::display(), folder.display());
            if !confirm(&q) {
                return Ok(());
            }
            match found {
                Found::Service { .. } => elevate(&["service", "uninstall", "--remove-files"])?,
                Found::Tray(r) => quit_running(&r)?,
            }
            set_startup(false)?;
            clear_mode();
            info_box(&format!("{} is removed. Your servers and data are still in {}.", service::display(), folder.display()));
        }
        _ => {}
    }
    Ok(())
}

fn control_agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(3))).http_status_as_error(false).build().new_agent()
}

fn tray_status(r: &Running) -> Option<serde_json::Value> {
    let mut res = control_agent().get(&format!("http://127.0.0.1:{}/api/v1/local/status", r.port)).header(control::HEADER, &r.token).call().ok()?;
    if res.status() != 200 {
        return None;
    }
    res.body_mut().read_json().ok()
}

/// Asks a running tray's panel to quit, tray and all, and waits for both.
fn quit_running(r: &Running) -> anyhow::Result<()> {
    let _ = control_agent()
        .post(&format!("http://127.0.0.1:{}/api/v1/local/quit", r.port))
        .header(control::HEADER, &r.token)
        .send_json(serde_json::json!({ "tray": true }));
    let ok = wait_exit(r.pid, Duration::from_secs(30)) && r.tray_pid.is_none_or(|p| wait_exit(p, Duration::from_secs(15)));
    if !ok {
        bail!("{} didn't quit. Quit it from its tray icon and try again.", service::display());
    }
    Ok(())
}

// ---- the first-run dialog --------------------------------------------------------

enum Choice {
    Portable,
    Startup,
    Service,
    Cancel,
}

fn first_run_dialog() -> Choice {
    let (data, cfg) = service::real_data(&boot_dir());
    let folder = cfg.home.clone().unwrap_or(data);
    let buttons = [
        (1, "Just run it\nRuns from this folder as you. Stops when you quit or sign out. Tray icon while it runs.".to_string(), false),
        (2, "Start with Windows\nSame as above, and starts again when you sign in. No admin needed.".to_string(), false),
        (
            3,
            format!(
                "Install as a service\nCopies itself to {} and runs in the background even when nobody is signed in. Asks for admin once.",
                service::install_dir().display()
            ),
            true,
        ),
    ];
    let footer = format!("Panel data: {}", folder.display());
    match task_dialog("How should Hopper Server run?", "You can change this later from the tray icon.", &buttons, Some(&footer)) {
        Some(1) => Choice::Portable,
        Some(2) => Choice::Startup,
        Some(3) => Choice::Service,
        _ => Choice::Cancel,
    }
}

/// Copies hopperd to Program Files and installs the service there (one UAC
/// prompt), then opens the panel.
fn install_service() -> anyhow::Result<()> {
    let boot = boot_dir();
    let data = boot.to_string_lossy().into_owned();
    elevate(&["service", "install", "--copy", "--data", &data])?;
    set_startup(false)?;
    save_mode("service");
    set_service_tray_startup(true);
    start_service_tray();
    open_service_panel(&boot);
    Ok(())
}

// ---- dialogs ---------------------------------------------------------------------

static SHIELDS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

unsafe extern "system" fn dialog_events(hwnd: HWND, msg: TASKDIALOG_NOTIFICATIONS, _w: WPARAM, _l: LPARAM, _data: isize) -> HRESULT {
    if msg == TDN_CREATED {
        // TDM_SET_BUTTON_ELEVATION_REQUIRED_STATE: the UAC shield.
        for id in SHIELDS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            // SAFETY: a message to the dialog Windows just made.
            unsafe { SendMessageW(hwnd, WM_USER + 115, Some(WPARAM(*id as usize)), Some(LPARAM(1))) };
        }
    }
    HRESULT(0)
}

type TaskDialogIndirectFn = unsafe extern "system" fn(*const TASKDIALOGCONFIG, *mut i32, *mut i32, *mut BOOL) -> HRESULT;

/// A Windows dialog with big "command link" buttons (`"Title\nWhat it
/// does"`); returns the picked id, `None` on Cancel. Falls back to message
/// boxes where task dialogs aren't available.
fn task_dialog(title: &str, content: &str, buttons: &[(i32, String, bool)], footer: Option<&str>) -> Option<i32> {
    *SHIELDS.lock().unwrap_or_else(|e| e.into_inner()) = buttons.iter().filter(|b| b.2).map(|b| b.0).collect();
    // Loaded at run time: the import would stop hopperd from starting
    // where common controls 6 aren't active (tests, odd copies).
    // SAFETY: the function pointer has TaskDialogIndirect's signature.
    let f: Option<TaskDialogIndirectFn> = unsafe {
        LoadLibraryW(w!("comctl32.dll")).ok().and_then(|m| GetProcAddress(m, s!("TaskDialogIndirect"))).map(|p| std::mem::transmute(p))
    };
    let Some(f) = f else {
        for (id, text, _) in buttons {
            let (head, rest) = text.split_once('\n').unwrap_or((text, ""));
            if confirm(&format!("{title}\n\n{head}?\n{rest}")) {
                return Some(*id);
            }
        }
        return None;
    };
    let texts: Vec<HSTRING> = buttons.iter().map(|b| HSTRING::from(b.1.as_str())).collect();
    let tb: Vec<TASKDIALOG_BUTTON> =
        buttons.iter().zip(&texts).map(|(b, t)| TASKDIALOG_BUTTON { nButtonID: b.0, pszButtonText: PCWSTR(t.as_ptr()) }).collect();
    let (wt, mi, ct) = (HSTRING::from(service::display()), HSTRING::from(title), HSTRING::from(content));
    let ft = HSTRING::from(footer.unwrap_or(""));
    let mut cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        dwFlags: TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION,
        dwCommonButtons: TDCBF_CANCEL_BUTTON,
        pszWindowTitle: PCWSTR(wt.as_ptr()),
        pszMainInstruction: PCWSTR(mi.as_ptr()),
        pszContent: PCWSTR(ct.as_ptr()),
        cButtons: tb.len() as u32,
        pButtons: tb.as_ptr(),
        nDefaultButton: buttons.first().map_or(0, |b| b.0),
        pfCallback: Some(dialog_events),
        ..Default::default()
    };
    if footer.is_some() {
        cfg.pszFooter = PCWSTR(ft.as_ptr());
    }
    // SAFETY: loading our own icon resource.
    if let Ok(module) = unsafe { GetModuleHandleW(None) }
        && let Ok(icon) = unsafe { LoadIconW(Some(module.into()), PCWSTR(1 as _)) }
    {
        cfg.dwFlags = TDF_USE_COMMAND_LINKS | TDF_ALLOW_DIALOG_CANCELLATION | TDF_USE_HICON_MAIN;
        cfg.Anonymous1.hMainIcon = icon;
    }
    let mut picked = 0i32;
    // SAFETY: every pointer in cfg outlives the call.
    let hr = unsafe { f(&cfg, &mut picked, std::ptr::null_mut(), std::ptr::null_mut()) };
    if hr.is_err() {
        log::warn!("task dialog: {hr:?}");
        return None;
    }
    (picked != 2).then_some(picked) // IDCANCEL
}

fn message(text: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    // SAFETY: a modal message box with our strings.
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(service::display()), style | MB_SETFOREGROUND | MB_TOPMOST) }
}

fn error_box(text: &str) {
    message(text, MB_OK | MB_ICONERROR);
}

fn info_box(text: &str) {
    message(text, MB_OK | MB_ICONINFORMATION);
}

fn confirm(text: &str) -> bool {
    message(text, MB_OKCANCEL | MB_ICONQUESTION) == IDOK
}

fn open_url(url: &str) {
    log::info!("opening {}", url.split('#').next().unwrap_or(url));
    // SAFETY: hands a URL or folder to the shell.
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(url), None, None, SW_SHOWNORMAL);
    }
}

/// Runs `hopperd <args>` as admin (one UAC prompt) and waits for it.
fn elevate(args: &[&str]) -> anyhow::Result<()> {
    elevate_with(&std::env::current_exe()?, args)
}

/// [`elevate`] with another hopperd.exe (a downloaded update).
fn elevate_with(exe: &Path, args: &[&str]) -> anyhow::Result<()> {
    let report = std::env::temp_dir().join(format!("hopperd-elevated-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&report);
    let quote = |a: &str| if a.contains(' ') || a.is_empty() { format!("\"{a}\"") } else { a.to_string() };
    let mut params: Vec<String> = args.iter().map(|a| quote(a)).collect();
    // The elevated copy doesn't get our environment: pass the test names on.
    if let Ok(n) = std::env::var("HOPPERD_SERVICE") {
        params.extend(["--name".into(), quote(&n)]);
    }
    if let Ok(d) = std::env::var("HOPPERD_INSTALL_DIR") {
        params.extend(["--install-dir".into(), quote(&d)]);
    }
    params.extend(["--report".into(), quote(&report.to_string_lossy())]);
    let (file, params) = (HSTRING::from(exe.as_os_str()), HSTRING::from(params.join(" ")));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    log::info!("asking for admin: hopperd {}", args.join(" "));
    // SAFETY: the strings outlive the call; we close the process handle.
    unsafe {
        if ShellExecuteExW(&mut info).is_err() {
            bail!("Windows didn't give admin rights (cancelled).");
        }
        let _ = WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        let _ = GetExitCodeProcess(info.hProcess, &mut code);
        let _ = CloseHandle(info.hProcess);
        if code != 0 {
            let why = std::fs::read_to_string(&report).unwrap_or_else(|_| format!("exit code {code}"));
            let _ = std::fs::remove_file(&report);
            bail!("{why}");
        }
    }
    let _ = std::fs::remove_file(&report);
    Ok(())
}

// ---- remembered choice, Start with Windows ---------------------------------------------

fn suffixed(base: &str) -> String {
    match service::name().as_str() {
        "hopperd" => base.to_string(),
        n => format!("{base} ({n})"),
    }
}

fn reg_get(key: PCWSTR, value: &str) -> Option<String> {
    let v = HSTRING::from(value);
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: reads into our buffer, its size in bytes.
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, key, &v, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len)) };
    if r.is_err() {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..n]))
}

fn reg_set(key: PCWSTR, value: &str, data: &str) -> anyhow::Result<()> {
    let v = HSTRING::from(value);
    let wide: Vec<u16> = data.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: writes a string value from our buffer.
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key, &v, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32) }
        .ok()
        .with_context(|| format!("saving {value} in the registry"))
}

fn reg_delete(key: PCWSTR, value: &str) {
    let v = HSTRING::from(value);
    // SAFETY: deletes one value; missing is fine.
    let _ = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key, &v) };
}

/// "portable", "startup" or "service": what was picked last time.
fn saved_mode() -> Option<String> {
    reg_get(OUR_KEY, &suffixed("Mode"))
}

fn save_mode(m: &str) {
    if let Err(e) = reg_set(OUR_KEY, &suffixed("Mode"), m) {
        log::warn!("{e:#}");
    }
}

fn clear_mode() {
    reg_delete(OUR_KEY, &suffixed("Mode"));
}

fn startup_on() -> bool {
    reg_get(RUN_KEY, &service::display()).is_some()
}

/// Starts the tray at sign-in (or not). No admin: the user's own Run key.
fn set_startup(on: bool) -> anyhow::Result<()> {
    if on {
        let exe = std::env::current_exe()?;
        reg_set(RUN_KEY, &service::display(), &format!("\"{}\" --tray", exe.display()))?;
        save_mode("startup");
    } else {
        reg_delete(RUN_KEY, &service::display());
        if saved_mode().as_deref() == Some("startup") {
            save_mode("portable");
        }
    }
    Ok(())
}

/// Shows the service's tray at sign-in (or not): the same Run entry as
/// Start with Windows, pointing at the service's exe.
fn set_service_tray_startup(on: bool) {
    if !on {
        reg_delete(RUN_KEY, &service::display());
        return;
    }
    let Some(exe) = service::installed().map(|s| s.exe).filter(|e| e.is_file()) else { return };
    if let Err(e) = reg_set(RUN_KEY, &service::display(), &format!("\"{}\" --tray", exe.display())) {
        log::warn!("{e:#}");
    }
}

/// Starts the service's tray (it quits at once if one runs already).
fn start_service_tray() {
    let exe = service::installed().map(|s| s.exe).filter(|e| e.is_file());
    let Some(exe) = exe.or_else(|| std::env::current_exe().ok()) else { return };
    if let Err(e) = spawn_tray(&exe) {
        log::warn!("starting the tray: {e}");
    }
}

/// Starts the tray of the updated `exe` (it waits for this one to close),
/// so the icon comes back on the new version.
fn reopen_tray(exe: &Path) {
    let r = Command::new(exe)
        .arg("--tray")
        .env("HOPPERD_WAIT_PID", std::process::id().to_string())
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match r {
        Ok(_) => log::info!("reopening the tray from {}", exe.display()),
        Err(e) => log::warn!("reopening the tray from {}: {e}", exe.display()),
    }
}

/// `exe --tray`, detached. Stdio must be null: after `hide_console` our
/// inherited console handles are dead, and Windows refuses to start a
/// child with them ("The request is not supported", os error 50).
fn spawn_tray(exe: &Path) -> std::io::Result<Child> {
    Command::new(exe)
        .arg("--tray")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

// ---- the service's tray -----------------------------------------------------------------

/// The tray for the installed service: a dot on the icon shows its state,
/// and the menu starts, stops or restarts it (each asks for admin).
fn run_service_tray(boot: PathBuf) -> anyhow::Result<()> {
    if tray::already_open() {
        log::info!("the service's tray is open already");
        return Ok(());
    }
    tray::set_state(MenuState {
        status: "Checking the service…".into(),
        startup: startup_on(),
        service: Some(tray::Service::Starting),
        ..Default::default()
    });
    let (tx, rx) = channel();
    // Set while we stop the service ourselves (no "it stopped" alert).
    let expected_stop = Arc::new(AtomicBool::new(false));
    {
        let (boot, expected_stop) = (boot.clone(), expected_stop.clone());
        std::thread::Builder::new().name("watch-service".into()).spawn(move || watch_service(boot, expected_stop))?;
    }
    {
        let boot = boot.clone();
        std::thread::Builder::new().name("update-check".into()).spawn(move || {
            std::thread::sleep(first_check());
            loop {
                service_check(&boot, false);
                std::thread::sleep(UPDATE_EVERY);
            }
        })?;
    }
    std::thread::Builder::new().name("tray-cmds".into()).spawn(move || {
        for cmd in rx {
            if !handle_service(&boot, &expected_stop, cmd) {
                tray::close();
                break;
            }
        }
    })?;
    tray::run(tx);
    log::info!("service tray closed");
    Ok(())
}

/// Every 2 seconds: the service's state and whether the panel answers.
/// Alerts when it stops without being asked to.
fn watch_service(boot: PathBuf, expected_stop: Arc<AtomicBool>) {
    let mut last: Option<tray::Service> = None;
    loop {
        let mut ms = tray::state();
        if ms.busy {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let Some(inst) = service::installed() else {
            log::info!("the service was removed; closing its tray");
            tray::close();
            return;
        };
        let port = service::real_data(&boot).1.port;
        let h = if inst.running { hello(port) } else { None };
        let state = match (&h, inst.running, inst.pending) {
            (Some(_), true, _) => tray::Service::Running,
            (_, _, true) | (None, true, _) => tray::Service::Starting,
            _ => tray::Service::Stopped,
        };
        ms.status = match (state, &h) {
            (tray::Service::Running, Some(h)) => format!("Service running · v{} · port {port}", h["version"].as_str().unwrap_or("?")),
            (tray::Service::Starting, _) if inst.running => "Service running, panel not answering yet".into(),
            (tray::Service::Starting, _) => "Service starting…".into(),
            _ => "Service stopped".into(),
        };
        // The service was updated (by itself or by "Update"): this tray
        // reopens on the new exe, so its icon keeps showing.
        if let Some(v) = h.as_ref().and_then(|h| h["version"].as_str())
            && newer(v, env!("CARGO_PKG_VERSION"))
        {
            log::info!("the service is on v{v}; reopening its tray");
            reopen_tray(&inst.exe);
            tray::close();
            return;
        }
        // The first address is the LAN one (lan_addresses ranks it first).
        ms.lan = h.as_ref().and_then(|h| h["addresses"].as_array()?.first()?.as_str().map(String::from));
        ms.startup = startup_on();
        ms.service = Some(state);
        // The service may have updated itself past what this tray found.
        let svc_version = h.as_ref().and_then(|h| h["version"].as_str());
        ms.update = SVC_UPDATE.lock().unwrap().clone().filter(|v| svc_version.is_none_or(|s| newer(v, s)));
        ms.checking = SVC_CHECKING.load(Ordering::SeqCst);
        // Restarting into an update: the service manager starts it again.
        let updating = std::fs::metadata(boot.join(crate::autoupdate::MARKER))
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().unwrap_or_default() < Duration::from_secs(180));
        if state == tray::Service::Stopped
            && last.is_some_and(|l| l != tray::Service::Stopped)
            && !updating
            && !expected_stop.swap(false, Ordering::SeqCst)
        {
            tray::notify(&format!("{} stopped", service::display()), "The service isn't running. Click the icon to start it.", true);
        }
        if state == tray::Service::Running {
            expected_stop.store(false, Ordering::SeqCst);
        }
        last = Some(state);
        tray::set_state(ms);
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn handle_service(boot: &Path, expected_stop: &AtomicBool, cmd: Cmd) -> bool {
    let run = |args: &[&str], busy: &str| {
        set_busy(busy);
        let r = elevate(args);
        set_busy("");
        if let Err(e) = r {
            log::warn!("service {}: {e:#}", args[1]);
            error_box(&format!("{e:#}"));
        }
    };
    match cmd {
        Cmd::Open => {
            let port = service::real_data(boot).1.port;
            open_url(&panel_url(port, service_setup_code(boot).as_deref()));
        }
        Cmd::CopyAddress => {
            if let Some(a) = tray::state().lan
                && tray::clipboard(&a)
            {
                tray::notify("Address copied", &format!("{a}: Hopper on another PC connects here."), false);
            }
        }
        Cmd::OpenFolder => {
            let (data, cfg) = service::real_data(boot);
            open_url(&cfg.home.unwrap_or(data).to_string_lossy());
        }
        Cmd::OpenLog => open_url(&boot.join("hopperd.log").to_string_lossy()),
        Cmd::ToggleStartup => {
            set_service_tray_startup(!startup_on());
            let mut ms = tray::state();
            ms.startup = startup_on();
            tray::set_state(ms);
        }
        Cmd::StartService => run(&["service", "start"], "Starting the service…"),
        Cmd::StopService => {
            expected_stop.store(true, Ordering::SeqCst);
            run(&["service", "stop"], "Stopping the service…");
        }
        Cmd::RestartService => {
            expected_stop.store(true, Ordering::SeqCst);
            run(&["service", "restart"], "Restarting the service…");
        }
        Cmd::CheckUpdates => service_check(boot, true),
        Cmd::Update => update_service(expected_stop),
        Cmd::Quit | Cmd::EndSession => return false,
        Cmd::InstallService => {}
    }
    true
}

// ---- updates in the tray ------------------------------------------------------------------

/// How often the service's tray looks for a new version (the panel itself
/// checks as often; see `autoupdate`).
const UPDATE_EVERY: Duration = Duration::from_secs(30 * 60);

/// The first look, a while after start (sooner when testing an update).
fn first_check() -> Duration {
    Duration::from_secs(if std::env::var_os("HOPPERD_TEST_VERSION").is_some() { 15 } else { 120 })
}

/// The last version a notification announced: each one is announced once.
static ANNOUNCED: Mutex<Option<String>> = Mutex::new(None);
/// The service's tray: the newer version it found, and whether it's looking.
static SVC_UPDATE: Mutex<Option<String>> = Mutex::new(None);
static SVC_CHECKING: AtomicBool = AtomicBool::new(false);

/// "Hopper Server X.Y.Z is available"; clicking it updates.
fn show_available(v: &str, service: bool) {
    *ANNOUNCED.lock().unwrap() = Some(v.to_string());
    let body = if service {
        "Click to update (Windows asks for admin). Your servers keep running."
    } else {
        "Click to update. Your servers keep running; the panel is back in a few seconds."
    };
    tray::notify_then(&format!("Hopper Server {v} is available"), body, false, Some(Cmd::Update));
}

/// [`show_available`] the first time `v` is seen.
fn announce_once(v: &str, service: bool) {
    if ANNOUNCED.lock().unwrap().as_deref() != Some(v) {
        show_available(v, service);
    }
}

/// The result of a "Check for updates" the player asked for.
fn show_check_result(r: Result<Option<String>, String>, current: &str, service: bool) {
    match r {
        Ok(Some(v)) => show_available(&v, service),
        Ok(None) => tray::notify("Hopper Server is up to date", &format!("{current} is the newest version."), false),
        Err(e) => tray::notify("Couldn't check for updates", &e, true),
    }
}

fn set_checking(on: bool) {
    let mut ms = tray::state();
    ms.checking = on;
    tray::set_state(ms);
}

/// The service's tray looks itself (the service's own state needs a
/// sign-in to read). Background looks announce a new version once, unless
/// the service installs it by itself.
fn service_check(boot: &Path, asked: bool) {
    if SVC_CHECKING.swap(true, Ordering::SeqCst) {
        return;
    }
    set_checking(true);
    let r = crate::autoupdate::latest().map_err(|e| format!("{e:#}"));
    if let Ok(v) = &r {
        *SVC_UPDATE.lock().unwrap() = v.clone();
    }
    SVC_CHECKING.store(false, Ordering::SeqCst);
    let mut ms = tray::state();
    ms.checking = false;
    if let Ok(v) = &r {
        ms.update = v.clone();
    }
    tray::set_state(ms);
    if asked {
        show_check_result(r, env!("CARGO_PKG_VERSION"), true);
    } else if let Ok(Some(v)) = &r
        && !service::real_data(boot).1.auto_update
    {
        announce_once(v, true);
    }
}

/// Downloads the newer version and installs it into the service (asks for
/// admin): the new exe's own `service update`, as when a newer copy is run.
fn update_service(expected_stop: &AtomicBool) {
    let Some(v) = tray::state().update else { return };
    set_busy(&format!("Downloading {v}…"));
    let dir = std::env::temp_dir().join(format!("hopperd-update-{}", std::process::id()));
    let r = crate::autoupdate::download_newest(&dir).and_then(|(v, exe)| {
        set_busy(&format!("Installing {v}…"));
        expected_stop.store(true, Ordering::SeqCst);
        elevate_with(&exe, &["service", "update"])
    });
    let _ = std::fs::remove_dir_all(&dir);
    set_busy("");
    if let Err(e) = r {
        expected_stop.store(false, Ordering::SeqCst);
        log::warn!("updating the service: {e:#}");
        error_box(&format!("The service wasn't updated: {e:#}"));
    }
}

/// The panel's update state (from `control`'s status) into the menu.
/// Announces a new version once, unless the panel installs it by itself.
fn apply_panel_update(ms: &mut MenuState, u: &serde_json::Value) {
    let state = u["state"].as_str().unwrap_or("");
    let latest = u["latest"].as_str().map(String::from);
    let v = latest.clone().unwrap_or_else(|| "the update".into());
    ms.updating = match state {
        "downloading" => Some(format!("Downloading {v}…")),
        "waiting" => Some(format!("Installing {v} once the running job finishes")),
        "restarting" => Some(format!("Restarting into {v}…")),
        _ => None,
    };
    ms.update = if ms.updating.is_none() { latest } else { None };
    ms.checking = state == "checking";
    let installs_itself = u["auto"].as_bool().unwrap_or(false) && u["why_not"].is_null();
    if let Some(v) = &ms.update
        && !installs_itself
    {
        announce_once(v, false);
    }
}

// ---- the tray and the panel it runs ---------------------------------------------------

struct Supervisor {
    token: String,
    /// The exe this tray started from (an update puts the new one there).
    exe: PathBuf,
    mode: Mutex<String>,
    child: Mutex<Option<Child>>,
    /// Keep the panel running (false while quitting or installing).
    keep: AtomicBool,
    /// The child's latest status (from `control`).
    status: Mutex<Option<serde_json::Value>>,
}

impl Supervisor {
    fn port(&self) -> u16 {
        let pid = self.child.lock().unwrap().as_ref().map(Child::id);
        control::read_running().filter(|r| Some(r.pid) == pid).map_or_else(|| service::real_data(&boot_dir()).1.port, |r| r.port)
    }

    fn spawn(&self) -> anyhow::Result<Child> {
        let exe = self.exe.clone();
        let log = boot_dir().join("hopperd.log");
        let out = std::fs::OpenOptions::new().create(true).append(true).open(&log)?;
        let err = out.try_clone()?;
        let child = Command::new(&exe)
            .arg("--portable")
            .env("HOPPERD_CONTROL_TOKEN", &self.token)
            .env("HOPPERD_TRAY_PID", std::process::id().to_string())
            .env("HOPPERD_TRAY_EXE", &exe)
            .env("HOPPERD_MODE", self.mode.lock().unwrap().as_str())
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .context("starting the panel")?;
        log::info!("panel started (pid {})", child.id());
        Ok(child)
    }

    fn request(&self, method: &str, path: &str) -> Option<serde_json::Value> {
        let url = format!("http://127.0.0.1:{}/api/v1/local/{path}", self.port());
        let a = control_agent();
        let mut res = if method == "GET" {
            a.get(&url).header(control::HEADER, &self.token).call().ok()?
        } else {
            a.post(&url).header(control::HEADER, &self.token).send_json(serde_json::json!({})).ok()?
        };
        if res.status() != 200 {
            return None;
        }
        res.body_mut().read_json().ok()
    }

    /// Asks the panel to check for an update (waits for the answer) or to
    /// install it (returns at once).
    fn update(&self, install: bool) -> Option<serde_json::Value> {
        let url = format!("http://127.0.0.1:{}/api/v1/local/update", self.port());
        let a = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(120))).http_status_as_error(false).build().new_agent();
        let mut res = a.post(&url).header(control::HEADER, &self.token).send_json(serde_json::json!({ "install": install })).ok()?;
        if res.status() != 200 {
            return None;
        }
        res.body_mut().read_json().ok()
    }

    /// Stops the panel (servers keep running unless stopped first).
    fn stop(&self) {
        self.keep.store(false, Ordering::SeqCst);
        let _ = self.request("POST", "quit");
        let mut slot = self.child.lock().unwrap();
        if let Some(c) = slot.as_mut() {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(20) {
                if matches!(c.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if matches!(c.try_wait(), Ok(None)) {
                log::warn!("the panel didn't quit; ending it");
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        *slot = None;
    }

    fn servers_up(&self) -> Vec<String> {
        self.status
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s["servers_up"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()))
            .unwrap_or_default()
    }
}

/// Keeps the panel running: starts it again after it restarts itself (exit
/// 75) or crashes; gives up after three quick crashes.
fn supervise(sup: Arc<Supervisor>) {
    let mut quick_fails = 0;
    while sup.keep.load(Ordering::SeqCst) {
        let started = Instant::now();
        match sup.spawn() {
            Ok(c) => *sup.child.lock().unwrap() = Some(c),
            Err(e) => {
                error_box(&format!("{e:#}"));
                tray::close();
                return;
            }
        }
        // Wait without holding the lock (stop() needs it).
        let code = loop {
            std::thread::sleep(Duration::from_millis(300));
            let mut slot = sup.child.lock().unwrap();
            match slot.as_mut().map(|c| c.try_wait()) {
                Some(Ok(Some(st))) => break st.code(),
                Some(Ok(None)) => {}
                _ => break None,
            }
        };
        if !sup.keep.load(Ordering::SeqCst) {
            return;
        }
        match code {
            Some(control::EXIT_TRAY_TOO) => {
                log::info!("another copy took over; quitting");
                sup.keep.store(false, Ordering::SeqCst);
                tray::close();
                return;
            }
            Some(75) => {
                log::info!("panel restarting");
                quick_fails = 0;
                continue;
            }
            Some(control::EXIT_UPDATED) => {
                log::info!("panel updated; reopening the tray on the new version");
                sup.keep.store(false, Ordering::SeqCst);
                reopen_tray(&sup.exe);
                tray::close();
                return;
            }
            Some(0) => {}
            other => log::warn!("panel stopped ({other:?})"),
        }
        if started.elapsed() < Duration::from_secs(15) {
            quick_fails += 1;
        } else {
            quick_fails = 0;
        }
        if quick_fails >= 3 {
            let tail = std::fs::read_to_string(boot_dir().join("hopperd.log")).unwrap_or_default();
            let tail: Vec<&str> = tail.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect();
            error_box(&format!(
                "{} keeps stopping right after it starts. Is another copy using port {}?\n\n{}",
                service::display(),
                sup.port(),
                tail.join("\n")
            ));
            sup.keep.store(false, Ordering::SeqCst);
            tray::close();
            return;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Refreshes the status line every 2 seconds; opens the panel the first
/// time it answers when `open` is set.
fn watch(sup: Arc<Supervisor>, mut open: bool) {
    loop {
        if !sup.keep.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let st = sup.request("GET", "status");
        let mut ms = tray::state();
        if ms.busy {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        match &st {
            Some(s) => {
                let up = s["servers_up"].as_array().map_or(0, Vec::len);
                ms.status = format!("Running · {up} server{} up · port {}", if up == 1 { "" } else { "s" }, s["port"]);
                ms.lan = s["lan"].as_str().map(String::from);
                apply_panel_update(&mut ms, &s["update"]);
                if open {
                    open = false;
                    open_url(&panel_url(s["port"].as_u64().unwrap_or(8790) as u16, s["setup_code"].as_str()));
                }
            }
            None => ms.status = "Starting…".into(),
        }
        ms.startup = startup_on();
        *sup.status.lock().unwrap() = st;
        tray::set_state(ms);
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn set_busy(text: &str) {
    let mut ms = tray::state();
    ms.busy = !text.is_empty();
    ms.status = text.to_string();
    tray::set_state(ms);
}

fn handle(sup: &Arc<Supervisor>, cmd: Cmd) -> bool {
    let st = sup.status.lock().unwrap().clone();
    match cmd {
        Cmd::Open => {
            let code = st.as_ref().and_then(|s| s["setup_code"].as_str().map(String::from));
            open_url(&panel_url(sup.port(), code.as_deref()));
        }
        Cmd::CopyAddress => {
            if let Some(lan) = st.as_ref().and_then(|s| s["lan"].as_str()) {
                if tray::clipboard(lan) {
                    tray::notify("Address copied", &format!("{lan}: open it in a browser on your phone or laptop (same network)."), false);
                }
            }
        }
        Cmd::OpenFolder => {
            let folder = st
                .as_ref()
                .and_then(|s| s["home"].as_str().or(s["data"].as_str()).map(PathBuf::from))
                .unwrap_or_else(|| service::real_data(&boot_dir()).0);
            open_url(&folder.to_string_lossy());
        }
        Cmd::ToggleStartup => {
            let on = !startup_on();
            if let Err(e) = set_startup(on) {
                error_box(&format!("{e:#}"));
            }
            *sup.mode.lock().unwrap() = if on { "startup" } else { "portable" }.into();
            let mut ms = tray::state();
            ms.startup = startup_on();
            tray::set_state(ms);
        }
        Cmd::InstallService => {
            let q = format!(
                "{} will run as a Windows service: it starts with the PC, even when nobody is signed in, and keeps running when you sign out.\n\nIt's copied to {}. Windows asks for admin next.",
                service::display(),
                service::install_dir().display()
            );
            if !confirm(&q) {
                return true;
            }
            set_busy("Installing the service…");
            sup.stop();
            match install_service() {
                Ok(()) => return false,
                Err(e) => {
                    log::warn!("service install: {e:#}");
                    error_box(&format!("The service wasn't installed: {e:#}\n\nHopper Server keeps running from here."));
                    set_busy("");
                    sup.keep.store(true, Ordering::SeqCst);
                    let s = sup.clone();
                    std::thread::spawn(move || supervise(s));
                }
            }
        }
        Cmd::Quit => {
            let up = sup.servers_up();
            if !up.is_empty() {
                let q = format!(
                    "{} server{} running: {}.\n\nStop {} before quitting?\nYes stops {} cleanly first. No quits and leaves {} running (Hopper Server finds {} again next time).",
                    up.len(),
                    if up.len() == 1 { " is" } else { "s are" },
                    up.join(", "),
                    if up.len() == 1 { "it" } else { "them" },
                    if up.len() == 1 { "it" } else { "them" },
                    if up.len() == 1 { "it" } else { "them" },
                    if up.len() == 1 { "it" } else { "them" },
                );
                match message(&q, MB_YESNOCANCEL | MB_ICONQUESTION) {
                    IDYES => {
                        set_busy("Stopping servers…");
                        let _ = sup.request("POST", "stop-servers");
                        let t = Instant::now();
                        while t.elapsed() < Duration::from_secs(180) {
                            std::thread::sleep(Duration::from_secs(1));
                            let left = sup.request("GET", "status").and_then(|s| s["servers_up"].as_array().map(Vec::len)).unwrap_or(0);
                            if left == 0 {
                                break;
                            }
                            set_busy(&format!("Stopping servers… {left} left"));
                        }
                    }
                    IDNO => {}
                    _ => return true,
                }
            }
            sup.stop();
            return false;
        }
        Cmd::EndSession => {
            sup.stop();
            return false;
        }
        Cmd::CheckUpdates => {
            set_checking(true);
            let mut r = sup.update(false).map(|r| r["update"].clone());
            // Another check was running: wait for it.
            let t = Instant::now();
            while r.as_ref().is_some_and(|u| u["state"] == "checking") && t.elapsed() < Duration::from_secs(90) {
                std::thread::sleep(Duration::from_secs(1));
                r = sup.request("GET", "status").map(|s| s["update"].clone());
            }
            set_checking(false);
            let result = match &r {
                None => Err("The panel isn't answering.".to_string()),
                Some(u) if u["state"] == "error" => Err(u["error"].as_str().unwrap_or("unknown error").to_string()),
                Some(u) => Ok(u["latest"].as_str().map(String::from)),
            };
            let current = r.as_ref().and_then(|u| u["current"].as_str().map(String::from)).unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
            show_check_result(result, &current, false);
        }
        Cmd::Update => match sup.update(true) {
            Some(r) if r["ok"] == true => {
                let mut ms = tray::state();
                ms.updating = Some(format!("Downloading {}…", ms.update.take().unwrap_or_else(|| "the update".into())));
                tray::set_state(ms);
            }
            Some(r) => error_box(r["error"].as_str().unwrap_or("The update didn't start.")),
            None => error_box("The panel isn't answering, so the update didn't start."),
        },
        // Service menu only.
        Cmd::StartService | Cmd::StopService | Cmd::RestartService | Cmd::OpenLog => {}
    }
    true
}

fn run_tray(mode: String, open: bool) -> anyhow::Result<()> {
    if mode == "portable" {
        save_mode("portable");
    }
    let sup = Arc::new(Supervisor {
        token: crate::auth::random_hex(24),
        exe: std::env::current_exe()?,
        mode: Mutex::new(mode),
        child: Mutex::new(None),
        keep: AtomicBool::new(true),
        status: Mutex::new(None),
    });
    tray::set_state(MenuState { status: "Starting…".into(), startup: startup_on(), ..Default::default() });
    let (tx, rx) = channel();
    {
        let s = sup.clone();
        std::thread::Builder::new().name("supervise".into()).spawn(move || supervise(s))?;
        let s = sup.clone();
        std::thread::Builder::new().name("watch".into()).spawn(move || watch(s, open))?;
        let s = sup.clone();
        std::thread::Builder::new().name("tray-cmds".into()).spawn(move || {
            for cmd in rx {
                if !handle(&s, cmd) {
                    tray::close();
                    break;
                }
            }
        })?;
    }
    tray::run(tx);
    // The icon is gone: make sure the panel is too (it may have quit already).
    if sup.keep.load(Ordering::SeqCst) {
        sup.stop();
    }
    log::info!("tray closed");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::newer("0.10.0", "0.9.1"));
        assert!(!super::newer("0.9.1", "0.9.1"));
        assert!(!super::newer("0.9.0", "0.9.1"));
    }

    #[test]
    fn setup_links() {
        assert_eq!(super::panel_url(8790, Some("Q7K2M9XP")), "http://localhost:8790/#setup=Q7K2M9XP");
        assert_eq!(super::panel_url(8796, None), "http://localhost:8796/");
    }
}
