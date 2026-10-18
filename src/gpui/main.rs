//! Hopper on GPUI: the window, theme and top-level navigation.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod accounts;
mod addons;
mod addons_extra;
mod backups_view;
mod image_viewer;
mod anim;
mod app;
mod browse;
mod configs;
mod content;
mod datapacks;
mod dialogs;
mod fetch_window;
mod crash_help;
mod host_dialog;
mod hopper_versions;
mod inst_settings;
mod java_form;
mod launchers_list;
mod library;
mod logs;
mod mcp;
mod nav;
mod pack_update;
mod palette;
mod peers;
mod perf_view;
mod rigsync;
mod server_control;
mod server_settings;
mod servers;
mod settings_page;
mod setup_wizard;
mod side_card;
mod skin_viewer;
mod skins;
mod store;
mod theme;
mod tasks;
mod tray;
mod versions;
mod video;
#[cfg(windows)]
mod video_window;
mod whats_new;
mod widgets;

use gpui_kit::component::TitleBar;
use gpui_kit::*;

fn main() {
    load_env();
    env_logger::init();
    // Windows' Installed apps → Uninstall runs this.
    if std::env::args().any(|a| a == "--uninstall") {
        uninstall();
        return;
    }
    // The window that fetches website-only files: `--fetch DIR URL...`.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--fetch") && args.len() > 2 {
        std::process::exit(fetch_window::run(args[2].clone().into(), args[3..].to_vec()));
    }
    // The window that plays a description's video: `--video URL [--owner HWND]`.
    #[cfg(windows)]
    if args.get(1).is_some_and(|a| a == "--video") && args.len() > 2 {
        let owner = (args.get(3).map(String::as_str) == Some("--owner"))
            .then(|| args.get(4)?.parse().ok())
            .flatten();
        std::process::exit(video_window::run(&args[2], owner));
    }
    // A curseforge:// link while Hopper is already open: hand it over.
    if let Some(url) = std::env::args().skip(1).find(|a| hopper::links::is_link(a))
        && hopper::links::forward(&url)
    {
        return;
    }
    gpui_kit::application()
        .with_assets(gpui_kit::assets::AllAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit::component::set_locale("en");
            app::bind_keys(cx);
            addons::bind_keys(cx);
            palette::bind_keys(cx);
            let saved = hopper::settings::Settings::load();
            let (name, mode, zoom) = saved
                .as_ref()
                .map_or((String::new(), 0, 100), |s| (s.theme_name.clone(), s.theme, s.zoom));
            theme::apply(&name, mode, zoom, cx);
            let options = WindowOptions {
                window_background: theme::appearance(theme::backdrop(&name, mode, cx)),
                window_bounds: Some(match dev_size() {
                    Some(s) => WindowBounds::Windowed(Bounds::centered(None, s, cx)),
                    None => app::start_bounds(saved.and_then(|s| s.window), cx),
                }),
                window_min_size: Some(size(px(960.), px(600.))),
                ..TitleBar::window_options()
            };
            gpui_kit::open_window(options, cx, move |window, cx| {
                window.set_window_title("Hopper");
                let dark = gpui_kit::component::ActiveTheme::theme(cx).mode.is_dark();
                theme::set_backdrop(window, theme::backdrop(&name, mode, cx), dark);
                cx.new(|cx| app::AppView::new(window, cx))
            })
            .expect("open the main window");
        });
}

/// `--size 1100x700`: a window of that size for this run (screenshots),
/// whose place isn't remembered.
pub(crate) fn dev_size() -> Option<Size<Pixels>> {
    let args: Vec<String> = std::env::args().collect();
    let v = args.iter().position(|a| a == "--size").and_then(|i| args.get(i + 1))?;
    let (w, h) = v.split_once('x')?;
    Some(size(px(w.trim().parse().ok()?), px(h.trim().parse().ok()?)))
}

/// CF_API_KEY and friends: `.env` next to the exe, the working dir, then
/// the repo's (for dev builds started from elsewhere).
fn load_env() {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    {
        hopper::env::load(&dir.join(".env"));
    }
    hopper::env::load(std::path::Path::new(".env"));
    hopper::env::load(std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.env"
    )));
}

/// Asks, then removes Hopper's shortcuts, its Installed apps entry and its
/// folder. Settings and saved keys stay, for a reinstall.
fn uninstall() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO, MessageBoxW,
        };
        use windows::core::HSTRING;
        let ask = |text: &str, style| unsafe {
            MessageBoxW(None, &HSTRING::from(text), &HSTRING::from("Uninstall Hopper"), style)
        };
        let sure = ask(
            "Remove Hopper from this PC?\n\nYour instances, servers, settings and saved keys stay.",
            MB_YESNO | MB_ICONQUESTION,
        );
        if sure != IDYES {
            return;
        }
        if let Err(e) = hopper::setup::uninstall() {
            ask(&format!("Hopper couldn't be removed:\n{e:#}"), MB_OK | MB_ICONERROR);
        }
    }
}
