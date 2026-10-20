//! The GPUI side of Hopper's MCP server (`--mcp`): answers tool calls on
//! the UI thread. Input goes through GPUI's own event dispatch, so an agent
//! can click, hover and type in this window without desktop control.

use crate::app::{AppView, Page};
use gpui_kit::*;
use hopper::mcp::{self, Command, Reply, Request, text};
use serde_json::{Value, json};
use std::sync::mpsc::Receiver;
use std::time::Duration;

/// Starts the server when Hopper runs with `--mcp` (or `HOPPER_MCP=1`).
pub fn start(window: &mut Window, cx: &mut Context<AppView>) {
    let wanted =
        std::env::args().any(|a| a == "--mcp") || std::env::var_os("HOPPER_MCP").is_some();
    if !wanted {
        return;
    }
    // HOPPER_MCP_PORT runs a second copy beside one already listening.
    let port = std::env::var("HOPPER_MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(mcp::DEFAULT_PORT);
    let rx: Receiver<Request> = match mcp::start(port, || {}) {
        Ok(rx) => rx,
        Err(e) => {
            log::error!("{e:#}");
            return;
        }
    };
    cx.spawn_in(window, async move |this, cx| {
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(30))
                .await;
            let requests: Vec<Request> = rx.try_iter().collect();
            if requests.is_empty() {
                continue;
            }
            for (cmd, reply) in requests {
                // Input goes through the window, outside any view's update,
                // since handlers update the views themselves.
                let r = match cmd {
                    Command::State | Command::Navigate(_) => {
                        this.update_in(cx, |this, window, cx| answer(this, cmd, window, cx))
                    }
                    cmd => cx.update(|window, cx| input(cmd, window, cx)),
                };
                let Ok(r) = r else { return };
                let _ = reply.send(r);
            }
        }
    })
    .detach();
}

fn answer(app: &mut AppView, cmd: Command, window: &mut Window, cx: &mut Context<AppView>) -> Reply {
    match cmd {
        Command::State => Ok(vec![text(
            serde_json::to_string_pretty(&state(app, window, cx)).unwrap_or_default(),
        )]),
        Command::Navigate(v) => {
            navigate(app, &v, window, cx);
            Ok(vec![text("ok")])
        }
        _ => Err("not a view command".into()),
    }
}

fn input(cmd: Command, window: &mut Window, cx: &mut App) -> Reply {
    let scale = window.scale_factor();
    let at = |x: f32, y: f32| point(px(x / scale), px(y / scale));
    let done = || Ok(vec![text("done")]);
    match cmd {
        Command::Screenshot => {
            let (w, h, rgba) = capture(window)?;
            let png = encode_png(w, h, rgba)?;
            Ok(vec![
                mcp::image_png(png),
                text(format!("{w}x{h} px, {scale} px per point")),
            ])
        }
        Command::State | Command::Navigate(_) => Err("not an input command".into()),
        Command::Hover { x, y } => {
            move_to(at(x, y), window, cx);
            done()
        }
        Command::Click { x, y, double, button } => {
            let button = match button.as_str() {
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                "back" => MouseButton::Navigate(NavigationDirection::Back),
                "forward" => MouseButton::Navigate(NavigationDirection::Forward),
                _ => MouseButton::Left,
            };
            let position = at(x, y);
            move_to(position, window, cx);
            for click_count in 1..=if double { 2 } else { 1 } {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button,
                        position,
                        click_count,
                        // Held through `modifiers`, as for a Shift+click.
                        modifiers: window.modifiers(),
                        ..Default::default()
                    }),
                    cx,
                );
                window.dispatch_event(
                    PlatformInput::MouseUp(MouseUpEvent {
                        button,
                        position,
                        click_count,
                        // Held through `modifiers`, as for a Shift+click.
                        modifiers: window.modifiers(),
                        ..Default::default()
                    }),
                    cx,
                );
            }
            done()
        }
        Command::Drag { from, to } => {
            let start = at(from.0, from.1);
            move_to(start, window, cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position: start,
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            const STEPS: usize = 8;
            let mut position = start;
            for i in 1..=STEPS {
                let t = i as f32 / STEPS as f32;
                position = at(from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t);
                window.dispatch_event(
                    PlatformInput::MouseMove(MouseMoveEvent {
                        position,
                        pressed_button: Some(MouseButton::Left),
                        ..Default::default()
                    }),
                    cx,
                );
            }
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    click_count: 1,
                    ..Default::default()
                }),
                cx,
            );
            done()
        }
        Command::Scroll { x, y, dy } => {
            let position = at(x, y);
            move_to(position, window, cx);
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position,
                    delta: ScrollDelta::Pixels(point(px(0.), px(-dy / scale))),
                    ..Default::default()
                }),
                cx,
            );
            done()
        }
        Command::Type(s) => {
            for c in s.chars() {
                let key = if c == ' ' { "space".to_string() } else { c.to_string() };
                let mut k = Keystroke::parse(&key).map_err(|e| e.to_string())?;
                k.key_char = Some(c.to_string());
                window.dispatch_keystroke(k, cx);
            }
            done()
        }
        Command::Key(name) => {
            // "ctrl+down": modifiers, then the key.
            let mut parts: Vec<&str> = name.split('+').collect();
            let last = parts.pop().unwrap_or_default();
            let key = key_name(last).ok_or(format!("unknown key {name}"))?;
            let mods: Vec<String> = parts.iter().map(|m| m.trim().to_lowercase()).collect();
            let combo = mods.into_iter().chain([key.to_string()]).collect::<Vec<_>>().join("-");
            let k = Keystroke::parse(&combo).map_err(|e| e.to_string())?;
            window.dispatch_keystroke(k, cx);
            done()
        }
        Command::Modifiers { shift, ctrl, alt } => {
            window.dispatch_event(
                PlatformInput::ModifiersChanged(ModifiersChangedEvent {
                    modifiers: Modifiers { shift, control: ctrl, alt, ..Default::default() },
                    ..Default::default()
                }),
                cx,
            );
            done()
        }
    }
}

fn move_to(position: Point<Pixels>, window: &mut Window, cx: &mut App) {
    window.dispatch_event(
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            modifiers: window.modifiers(),
            ..Default::default()
        }),
        cx,
    );
}

/// MCP key names in GPUI's keystroke syntax.
fn key_name(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => "enter",
        "escape" | "esc" => "escape",
        "tab" => "tab",
        "backspace" => "backspace",
        "delete" => "delete",
        "arrowup" | "up" => "up",
        "arrowdown" | "down" => "down",
        "arrowleft" | "left" => "left",
        "arrowright" | "right" => "right",
        "pageup" => "pageup",
        "pagedown" => "pagedown",
        "home" => "home",
        "space" => "space",
        "end" => "end",
        "," | "comma" => ",",
        _ => return None,
    })
}

/// `{tab, instance, view, search, server}`, like the dev arguments.
fn navigate(app: &mut AppView, v: &Value, window: &mut Window, cx: &mut Context<AppView>) {
    let s = |k: &str| v[k].as_str().map(str::to_string);
    if let Some(tab) = s("tab") {
        let page = std::iter::once(Page::Settings)
            .chain(Page::ALL)
            .into_iter()
            .find(|p| p.label().to_lowercase().replace(' ', "").starts_with(&tab.to_lowercase()));
        if let Some(p) = page {
            app.go(p, cx);
        }
    }
    // {"settings": "java"}: a category of Settings, or a setting's id.
    if let Some(w) = s("settings") {
        match crate::settings_page::entry_id(&w) {
            Some(id) => app.open_settings_at(id, cx),
            None => {
                if let Some(c) = crate::settings_page::Category::find(&w) {
                    app.settings_category(c, cx);
                }
            }
        }
    }
    if s("instance").is_some() || s("view").is_some() {
        app.go(Page::Library, cx);
        let (inst, view) = (s("instance"), s("view"));
        app.library
            .update(cx, |l, cx| l.navigate(inst.as_deref(), view.as_deref(), cx));
    }
    if s("dialog").as_deref() == Some("accounts") {
        crate::accounts::open(app.store.clone(), window, cx);
    }
    // A preview: it never marks the setup done.
    if s("dialog").as_deref() == Some("setup") {
        crate::setup_wizard::open(app.store.clone(), cx.entity().downgrade(), true, window, cx);
    }
    if let Some(q) = s("search") {
        app.go(Page::Browse, cx);
        app.browse.update(cx, |b, cx| b.set_query(&q, window, cx));
    }
    if let Some(name) = s("server") {
        let name = name.to_lowercase();
        let root = app
            .store
            .read(cx)
            .servers()
            .iter()
            .find(|s| s.name.to_lowercase().contains(&name))
            .map(|s| s.root.clone());
        if let Some(root) = root {
            app.sync.update(cx, |p, cx| p.compare(root, cx));
            app.go(Page::Sync, cx);
        }
    }
}

fn state(app: &AppView, window: &Window, cx: &App) -> Value {
    let store = app.store.read(cx);
    json!({
        "page": app.page.label(),
        "theme": theme_state(window, cx),
        "zoom": store.settings().zoom,
        "scale": window.scale_factor(),
        "selected_instance": app.library.read(cx).selected().and_then(|r| store.instance(r)).map(|i| i.name.clone()),
        "instances": store.instances().iter().map(|i| json!({
            "name": i.name,
            "minecraft": i.game_version,
            "loader": i.loader,
            "pack_version": i.pack.as_ref().map(|p| p.version.clone()),
            "addons": i.addons.len(),
        })).collect::<Vec<_>>(),
        "servers": store.servers().iter().map(|s| json!({
            "name": s.name,
            "port": s.port(),
            "loader": s.loader,
        })).collect::<Vec<_>>(),
        "jobs": store.jobs().snapshot().iter().map(|j| json!({
            "title": j.title, "detail": j.detail, "done": j.done, "error": j.error,
        })).collect::<Vec<_>>(),
    })
}

fn theme_state(_: &Window, cx: &App) -> Value {
    let t = gpui_kit::component::Theme::global(cx);
    json!({
        "name": t.theme_name().to_string(),
        "background": format!("{:?}", t.colors.background),
        "token_background": format!("{:?}", t.tokens.background),
    })
}

fn encode_png(w: u32, h: u32, rgba: Vec<u8>) -> Result<String, String> {
    use base64::Engine;
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("bad screenshot size")?;
    let mut png = std::io::Cursor::new(Vec::new());
    img.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(png.into_inner()))
}

/// The window's client area as RGBA, drawn by the window itself (so it
/// works behind other windows).
#[cfg(windows)]
fn capture(window: &Window) -> Result<(u32, u32, Vec<u8>), String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};

    let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return Err("not a Win32 window".into());
    };
    let hwnd = HWND(h.hwnd.get() as *mut _);
    // SAFETY: plain GDI calls on our own live window; every object made here
    // is released before returning.
    unsafe {
        let mut r = RECT::default();
        windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut r)
            .map_err(|e| e.to_string())?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        if w <= 0 || h <= 0 {
            return Err("the window is minimized".into());
        }
        let wdc = GetDC(Some(hwnd));
        let mdc = CreateCompatibleDC(Some(wdc));
        let bmp = CreateCompatibleBitmap(wdc, w, h);
        let old = SelectObject(mdc, bmp.into());
        // Client area only, with DirectComposition content.
        let ok = PrintWindow(hwnd, mdc, PRINT_WINDOW_FLAGS(1 | 2)).as_bool();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(
            mdc,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        SelectObject(mdc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mdc);
        ReleaseDC(Some(hwnd), wdc);
        if !ok || lines == 0 {
            return Err("could not capture the window".into());
        }
        for p in buf.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        Ok((w as u32, h as u32, buf))
    }
}

#[cfg(not(windows))]
fn capture(_: &Window) -> Result<(u32, u32, Vec<u8>), String> {
    Err("screenshots are only available on Windows".into())
}
