//! Hopper's themes. Hopper Dark is hand-tuned (theme.json); the rest come
//! from small palettes, filled out into every color the components use.
//! Glass themes are translucent over a blurred (acrylic) or Mica window.

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeSet};
use gpui_kit::*;
use serde_json::{Map, Value, json};
use std::rc::Rc;

/// The base font size at 100% zoom; every rem-sized thing follows it.
const BASE: f32 = 15.0;

/// Sets the zoom (percent) by scaling the base font, which is the rem.
pub fn set_zoom(zoom: u16, cx: &mut App) {
    let size = BASE * zoom.clamp(50, 200) as f32 / 100.0;
    Theme::update(cx, |t| t.font_size = px(size));
    // Pages are cached views; a refresh re-renders them with the new theme.
    cx.refresh_windows();
}

/// The corner radius every theme sets ("radius" in theme.json), for
/// helpers that have no `cx`.
pub const RADIUS: f32 = 6.0;

/// Brand, site and loader colors, defined once.
pub mod palette {
    use gpui_kit::Hsla;

    /// HSL from degrees, saturation and lightness (all but hue 0-1).
    const fn hs(deg: f32, s: f32, l: f32) -> Hsla {
        Hsla { h: deg / 360.0, s, l, a: 1.0 }
    }

    pub const CURSEFORGE: Hsla = hs(14.0, 0.87, 0.58);
    pub const MODRINTH: Hsla = hs(145.0, 0.78, 0.48);
    pub const NEOFORGE: Hsla = hs(20.0, 0.75, 0.6);
    pub const FORGE: Hsla = hs(215.0, 0.35, 0.65);
    pub const FABRIC: Hsla = hs(38.0, 0.45, 0.7);
    pub const QUILT: Hsla = hs(270.0, 0.55, 0.7);
}

/// What sits behind a theme's colors. Only Opaque is in use: the glass
/// themes were taken out (Windows' blur didn't hold up), the rest stays for
/// when they come back.
#[derive(Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Backdrop {
    Opaque,
    /// The desktop blurred, the way Windows 10 did it (works on both).
    Blur,
    /// Windows 11's own Acrylic, as its menus and flyouts use.
    Acrylic,
    Mica,
    /// Mica tinted more strongly, as tabbed windows use.
    MicaAlt,
}

/// A theme as a few colors (#rrggbb or #rrggbbaa).
pub struct Palette {
    pub name: &'static str,
    pub dark: bool,
    pub backdrop: Backdrop,
    /// The page.
    bg: &'static str,
    /// Title bar and side bars.
    panel: &'static str,
    /// Inputs, segmented controls, muted fills.
    surface: &'static str,
    /// Hovered and picked fills.
    raised: &'static str,
    border: &'static str,
    /// Menus and dialogs; glass themes keep these solid enough to read.
    pop: Option<&'static str>,
    fg: &'static str,
    muted: &'static str,
    accent: &'static str,
    accent_fg: &'static str,
    success: &'static str,
    warning: &'static str,
    danger: &'static str,
    info: &'static str,
}

const fn dark(name: &'static str) -> Palette {
    Palette {
        name,
        dark: true,
        backdrop: Backdrop::Opaque,
        bg: "",
        panel: "",
        surface: "",
        raised: "",
        border: "",
        pop: None,
        fg: "",
        muted: "",
        accent: "",
        accent_fg: "",
        success: "",
        warning: "",
        danger: "",
        info: "",
    }
}

const fn light(name: &'static str) -> Palette {
    Palette { dark: false, ..dark(name) }
}

/// The name of the hand-tuned default dark theme.
pub const HOPPER_DARK: &str = "Hopper Dark";
pub const HOPPER_LIGHT: &str = "Hopper Light";

/// Every theme but Hopper Dark (which is theme.json), dark ones first.
pub const PALETTES: &[Palette] = &[
    Palette { bg: "#1e1e2e", panel: "#181825", surface: "#262637", raised: "#313244", border: "#313244", fg: "#cdd6f4", muted: "#a6adc8", accent: "#cba6f7", accent_fg: "#1e1e2e", success: "#a6e3a1", warning: "#f9e2af", danger: "#f38ba8", info: "#89dceb", ..dark("Catppuccin Mocha") },
    Palette { bg: "#24273a", panel: "#1e2030", surface: "#2b2e43", raised: "#363a4f", border: "#363a4f", fg: "#cad3f5", muted: "#a5adcb", accent: "#c6a0f6", accent_fg: "#181926", success: "#a6da95", warning: "#eed49f", danger: "#ed8796", info: "#91d7e3", ..dark("Catppuccin Macchiato") },
    Palette { bg: "#191724", panel: "#16141f", surface: "#1f1d2e", raised: "#26233a", border: "#26233a", fg: "#e0def4", muted: "#908caa", accent: "#ebbcba", accent_fg: "#191724", success: "#9ccfd8", warning: "#f6c177", danger: "#eb6f92", info: "#c4a7e7", ..dark("Rosé Pine") },
    Palette { bg: "#232136", panel: "#1d1b2e", surface: "#2a273f", raised: "#393552", border: "#393552", fg: "#e0def4", muted: "#908caa", accent: "#ea9a97", accent_fg: "#232136", success: "#9ccfd8", warning: "#f6c177", danger: "#eb6f92", info: "#c4a7e7", ..dark("Rosé Pine Moon") },
    Palette { bg: "#1a1b26", panel: "#16161e", surface: "#1f2335", raised: "#292e42", border: "#292e42", fg: "#c0caf5", muted: "#9aa5ce", accent: "#7aa2f7", accent_fg: "#1a1b26", success: "#9ece6a", warning: "#e0af68", danger: "#f7768e", info: "#7dcfff", ..dark("Tokyo Night") },
    Palette { bg: "#282a36", panel: "#21222c", surface: "#303241", raised: "#44475a", border: "#44475a", fg: "#f8f8f2", muted: "#a3a7c2", accent: "#bd93f9", accent_fg: "#282a36", success: "#50fa7b", warning: "#f1fa8c", danger: "#ff5555", info: "#8be9fd", ..dark("Dracula") },
    Palette { bg: "#2e3440", panel: "#272c36", surface: "#353c4a", raised: "#434c5e", border: "#3b4252", fg: "#eceff4", muted: "#a3acbf", accent: "#88c0d0", accent_fg: "#2e3440", success: "#a3be8c", warning: "#ebcb8b", danger: "#bf616a", info: "#81a1c1", ..dark("Nord") },
    Palette { bg: "#282828", panel: "#1d2021", surface: "#32302f", raised: "#3c3836", border: "#3c3836", fg: "#ebdbb2", muted: "#a89984", accent: "#fabd2f", accent_fg: "#282828", success: "#b8bb26", warning: "#fe8019", danger: "#fb4934", info: "#83a598", ..dark("Gruvbox Dark") },
    Palette { bg: "#2d353b", panel: "#272e33", surface: "#343f44", raised: "#3d484d", border: "#3d484d", fg: "#d3c6aa", muted: "#9da9a0", accent: "#a7c080", accent_fg: "#2d353b", success: "#a7c080", warning: "#dbbc7f", danger: "#e67e80", info: "#7fbbb3", ..dark("Everforest") },
    Palette { bg: "#282c34", panel: "#21252b", surface: "#2c313a", raised: "#3a3f4b", border: "#3a3f4b", fg: "#abb2bf", muted: "#8b92a0", accent: "#61afef", accent_fg: "#1e2127", success: "#98c379", warning: "#e5c07b", danger: "#e06c75", info: "#56b6c2", ..dark("One Dark") },
    Palette { bg: "#2c2525", panel: "#211c1c", surface: "#352e2e", raised: "#403838", border: "#403838", fg: "#fff1f3", muted: "#c3b7b8", accent: "#f9cc6c", accent_fg: "#2c2525", success: "#adda78", warning: "#f38d70", danger: "#fd6883", info: "#85dacc", ..dark("Ristretto") },
    Palette { bg: "#1f1f28", panel: "#16161d", surface: "#2a2a37", raised: "#363646", border: "#363646", fg: "#dcd7ba", muted: "#a09c8c", accent: "#7e9cd8", accent_fg: "#1f1f28", success: "#98bb6c", warning: "#e6c384", danger: "#e46876", info: "#7fb4ca", ..dark("Kanagawa") },
    Palette { bg: "#fafafa", panel: "#f1f1f3", surface: "#f4f4f5", raised: "#e7e7ea", border: "#e4e4e7", fg: "#18181b", muted: "#6b6b75", accent: "#ea580c", accent_fg: "#ffffff", success: "#16a34a", warning: "#ca8a04", danger: "#dc2626", info: "#0284c7", ..light(HOPPER_LIGHT) },
    Palette { bg: "#eff1f5", panel: "#e6e9ef", surface: "#e6e9ef", raised: "#ccd0da", border: "#ccd0da", fg: "#4c4f69", muted: "#6c6f85", accent: "#8839ef", accent_fg: "#eff1f5", success: "#40a02b", warning: "#df8e1d", danger: "#d20f39", info: "#04a5e5", ..light("Catppuccin Latte") },
    Palette { bg: "#faf4ed", panel: "#f2e9e1", surface: "#fffaf3", raised: "#dfdad9", border: "#dfdad9", fg: "#575279", muted: "#797593", accent: "#d7827e", accent_fg: "#faf4ed", success: "#56949f", warning: "#ea9d34", danger: "#b4637a", info: "#907aa9", ..light("Rosé Pine Dawn") },
    Palette { bg: "#e1e2e7", panel: "#d0d5e3", surface: "#e9e9ed", raised: "#c4c8da", border: "#c4c8da", fg: "#3760bf", muted: "#6172b0", accent: "#2e7de9", accent_fg: "#ffffff", success: "#587539", warning: "#8c6c3e", danger: "#f52a65", info: "#007197", ..light("Tokyo Night Day") },
    Palette { bg: "#fbf1c7", panel: "#f2e5bc", surface: "#f9f5d7", raised: "#ebdbb2", border: "#d5c4a1", fg: "#3c3836", muted: "#7c6f64", accent: "#af3a03", accent_fg: "#fbf1c7", success: "#79740e", warning: "#b57614", danger: "#9d0006", info: "#076678", ..light("Gruvbox Light") },
    Palette { bg: "#fdf6e3", panel: "#eee8d5", surface: "#f5efdc", raised: "#e4ddc8", border: "#e0d8c0", fg: "#475b62", muted: "#657b83", accent: "#268bd2", accent_fg: "#fdf6e3", success: "#859900", warning: "#b58900", danger: "#dc322f", info: "#2aa198", ..light("Solarized Light") },
];

/// Every theme's name, in menu order: Hopper's, the built-in ones, then
/// the user's own theme files.
pub fn names() -> Vec<String> {
    std::iter::once(HOPPER_DARK.to_string())
        .chain(PALETTES.iter().map(|p| p.name.to_string()))
        .chain(custom().iter().map(|c| c.name.to_string()))
        .collect()
}

/// A theme's look in four colors: page, panel, accent, text.
pub fn swatch(name: &str) -> [Hsla; 4] {
    let c = |hex: &str| {
        let Rgba(r, g, b, a) = parse(hex);
        Hsla::from(gpui_kit::Rgba { r, g, b, a: a.max(0.85) })
    };
    if let Some(p) = PALETTES.iter().find(|p| p.name == name) {
        return [c(p.bg), c(p.panel), c(p.accent), c(p.fg)];
    }
    if let Some(t) = custom().iter().find(|t| t.name.as_ref() == name) {
        let get = |v: &Option<SharedString>, or: &str| c(v.as_deref().unwrap_or(or));
        return [
            get(&t.colors.background, "#202020"),
            get(&t.colors.sidebar, "#181818"),
            get(&t.colors.primary, "#888888"),
            get(&t.colors.foreground, "#eeeeee"),
        ];
    }
    [c("#121214"), c("#0e0e10"), c("#f97316"), c("#ededef")]
}

/// Whether a theme shows the desktop through the window.
pub fn is_glass(name: &str) -> bool {
    PALETTES.iter().any(|p| p.name == name && p.backdrop != Backdrop::Opaque)
}

// ---- the user's own themes ---------------------------------------------------

/// Where theme files go: gpui-component theme JSON, one or more per file.
pub fn custom_dir() -> Option<std::path::PathBuf> {
    hopper::paths::dirs().map(|d| d.data_dir().join("themes"))
}

thread_local! {
    /// The user's themes, read once (UI thread only; configs are Rc).
    static CUSTOM: std::cell::RefCell<Option<Vec<Rc<ThemeConfig>>>> =
        const { std::cell::RefCell::new(None) };
}

fn custom() -> Vec<Rc<ThemeConfig>> {
    CUSTOM.with(|c| c.borrow_mut().get_or_insert_with(load_custom).clone())
}

/// Reads the theme folder again; returns how many themes it has.
pub fn reload_custom() -> usize {
    let fresh = load_custom();
    let n = fresh.len();
    CUSTOM.with(|c| *c.borrow_mut() = Some(fresh));
    n
}

fn load_custom() -> Vec<Rc<ThemeConfig>> {
    let Some(dir) = custom_dir() else { return Vec::new() };
    let mut out = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")))
        .collect();
    files.sort();
    for f in files {
        let text = match std::fs::read_to_string(&f) {
            Ok(t) => t,
            Err(_) => continue,
        };
        match serde_json::from_str::<ThemeSet>(&text) {
            Ok(set) => out.extend(set.themes.into_iter().map(Rc::new)),
            Err(e) => log::warn!("theme file {}: {e}", f.display()),
        }
    }
    out
}

/// The theme to use: the picked one, else Hopper's light or dark to match
/// Windows (the old 0/1/2 setting still counts).
/// Glass themes that were taken out; a saved one follows Windows instead.
const RETIRED: [&str; 7] = [
    "Smoked Acrylic",
    "Aurora Glass",
    "Acrylic Dark",
    "Mica Alt Dark",
    "Mica Dark",
    "Acrylic Light",
    "Clear Glass",
];

fn resolve<'a>(name: &'a str, mode: u8, cx: &App) -> &'a str {
    if !name.is_empty() && !RETIRED.contains(&name) {
        return name;
    }
    let dark = match mode {
        1 => true,
        2 => false,
        _ => matches!(
            cx.window_appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        ),
    };
    if dark { HOPPER_DARK } else { HOPPER_LIGHT }
}

/// What goes behind the window for a theme name.
pub fn backdrop(name: &str, mode: u8, cx: &App) -> Backdrop {
    let name = resolve(name, mode, cx);
    PALETTES
        .iter()
        .find(|p| p.name == name)
        .map_or(Backdrop::Opaque, |p| p.backdrop)
}

/// What GPUI is told. For Windows 11's Acrylic it is told Mica (so frames
/// are drawn see-through), then [`set_backdrop`] switches the kind.
pub fn appearance(b: Backdrop) -> WindowBackgroundAppearance {
    match b {
        Backdrop::Opaque => WindowBackgroundAppearance::Opaque,
        Backdrop::Blur => WindowBackgroundAppearance::Blurred,
        Backdrop::Acrylic | Backdrop::Mica => WindowBackgroundAppearance::MicaBackdrop,
        Backdrop::MicaAlt => WindowBackgroundAppearance::MicaAltBackdrop,
    }
}

/// Puts `b` behind the window; `dark` says whether the theme is dark.
pub fn set_backdrop(window: &Window, b: Backdrop, dark: bool) {
    window.set_background_appearance(appearance(b));
    #[cfg(windows)]
    system_backdrop(window, b, dark);
}

/// Windows 11's backdrops (DWMWA_SYSTEMBACKDROP_TYPE), with the frame
/// stretched over the whole window so the backdrop shows behind the app
/// and not only the border. Older Windows ignores it.
#[cfg(windows)]
fn system_backdrop(window: &Window, b: Backdrop, dark: bool) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{
        DWM_SYSTEMBACKDROP_TYPE, DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TABBEDWINDOW,
        DWMSBT_TRANSIENTWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
        DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
    };
    use windows::Win32::UI::Controls::MARGINS;

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(h.hwnd.get() as *mut _);
    let (kind, system): (DWM_SYSTEMBACKDROP_TYPE, bool) = match b {
        Backdrop::Acrylic => (DWMSBT_TRANSIENTWINDOW, true),
        Backdrop::Mica => (DWMSBT_MAINWINDOW, true),
        Backdrop::MicaAlt => (DWMSBT_TABBEDWINDOW, true),
        Backdrop::Opaque | Backdrop::Blur => (DWMSBT_NONE, false),
    };
    let edge = if system { -1 } else { 0 };
    let margins = MARGINS {
        cxLeftWidth: edge,
        cxRightWidth: edge,
        cyTopHeight: edge,
        cyBottomHeight: edge,
    };
    // SAFETY: attribute calls on our own live window with values that
    // outlive the calls.
    unsafe {
        // Dark or light, as the theme is: Mica and Acrylic tint by it, and
        // while the window isn't focused Windows swaps the blur for a plain
        // fill in this mode (light grey under a dark theme without it).
        let dark: i32 = dark.into();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of_val(&dark) as u32,
        );
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &kind as *const _ as *const _,
            std::mem::size_of_val(&kind) as u32,
        );
    }
}

/// Applies a theme by name ("" follows Windows) and the zoom.
pub fn apply(name: &str, mode: u8, zoom: u16, cx: &mut App) {
    let name = resolve(name, mode, cx).to_string();
    let own = custom().into_iter().find(|t| t.name.as_ref() == name);
    let (dark, config, back) = match (PALETTES.iter().find(|p| p.name == name), own) {
        (Some(p), _) => (p.dark, config(p), p.backdrop),
        (None, Some(t)) => (t.mode.is_dark(), Some((*t).clone()), Backdrop::Opaque),
        (None, None) => (true, hopper_dark(), Backdrop::Opaque),
    };
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    if let Some(c) = config {
        Theme::update(cx, |t| t.apply_config(&Rc::new(c)));
    }
    // Code editors (Java arguments, config files) paint their line numbers
    // over text scrolled sideways; the see-through input colour let that
    // text show through, so the numbers get a solid fill.
    Theme::update(cx, |t| {
        use gpui_kit::component::theme::Colorize as _;
        let mut gutter = t.input.mix_oklab(t.background, 0.3);
        gutter.a = 1.;
        std::sync::Arc::make_mut(&mut t.highlight_theme).style.editor_gutter_background = Some(gutter);
    });
    for w in cx.windows() {
        let _ = w.update(cx, |_, window, _| set_backdrop(window, back, dark));
    }
    set_zoom(zoom, cx);
}

fn hopper_dark() -> Option<ThemeConfig> {
    match serde_json::from_str::<ThemeSet>(include_str!("theme.json")) {
        Ok(set) => set.themes.into_iter().next(),
        Err(e) => {
            log::error!("theme.json: {e}");
            None
        }
    }
}

// ---- colors -----------------------------------------------------------------

#[derive(Clone, Copy)]
struct Rgba(f32, f32, f32, f32);

fn parse(hex: &str) -> Rgba {
    let h = hex.trim_start_matches('#');
    let c = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("ff"), 16).unwrap_or(0) as f32 / 255.0;
    Rgba(c(0), c(2), c(4), if h.len() >= 8 { c(6) } else { 1.0 })
}

fn hex(c: Rgba) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2), b(c.3))
}

/// `a` moved `t` of the way to `b` (color and alpha).
fn mix(a: &str, b: &str, t: f32) -> String {
    let (a, b) = (parse(a), parse(b));
    let m = |x: f32, y: f32| x + (y - x) * t;
    hex(Rgba(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2), m(a.3, b.3)))
}

/// `c` at opacity `alpha` (0-1).
fn alpha(c: &str, alpha: f32) -> String {
    let c = parse(c);
    hex(Rgba(c.0, c.1, c.2, alpha))
}

fn lum(c: Rgba) -> f32 {
    let f = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * f(c.0) + 0.7152 * f(c.1) + 0.0722 * f(c.2)
}

/// WCAG contrast ratio of two colors.
fn contrast(a: &str, b: &str) -> f32 {
    let (x, y) = (lum(parse(a)), lum(parse(b)));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `c` moved toward `to` just far enough to reach `ratio` against `bg`.
fn reach(c: &str, to: &str, bg: &str, ratio: f32) -> String {
    let mut out = c.to_string();
    for i in 0..=20 {
        out = mix(c, to, i as f32 * 0.05);
        if contrast(&out, bg) >= ratio {
            break;
        }
    }
    out
}

/// Dark or light text that reads on `bg`.
fn on(bg: &str) -> &'static str {
    let c = parse(bg);
    let lum = 0.2126 * c.0 + 0.7152 * c.1 + 0.0722 * c.2;
    if lum > 0.55 { "#141414" } else { "#ffffff" }
}

/// The full color set the components read, from a palette.
fn config(p: &Palette) -> Option<ThemeConfig> {
    let (white, black) = ("#ffffff", "#000000");
    // On glass only the window's own background is painted; everything that
    // would repaint the page color is clear, or the coats add up to opaque.
    let glass = p.backdrop != Backdrop::Opaque;
    let page = if glass { "#00000000" } else { p.bg };
    // Lighter on dark themes, darker on light ones.
    let lift = if p.dark { white } else { black };
    let pop = p.pop.map(str::to_string).unwrap_or_else(|| mix(p.bg, p.surface, 0.5));
    // Fill, hover, active and text; the fill is pushed until its text
    // reads at 4.5:1, and hover/active move further the same way.
    let status = |c: &str| {
        let text = on(c);
        let to = if text == "#ffffff" { black } else { white };
        let fill = reach(c, to, text, 4.5);
        (fill.clone(), mix(&fill, to, 0.08), mix(&fill, to, 0.16), text.to_string())
    };
    // Glass reads as frosted layers: fills a little stronger so rows and
    // picked items stand out over any backdrop, edges lit, and the accent
    // brightening on hover like a glow.
    let (hover_a, active_a) = if glass { (0.075, 0.2) } else { (crate::widgets::alpha::HOVER, crate::widgets::alpha::SELECTED) };
    let primary_hover = if glass { mix(p.accent, white, 0.22) } else { mix(p.accent, lift, 0.12) };
    let edge = if glass { alpha(p.fg, if p.dark { 0.14 } else { 0.12 }) } else { p.border.to_string() };
    let (ok, ok_h, ok_a, ok_f) = status(p.success);
    let (bad, bad_h, bad_a, bad_f) = status(p.danger);
    let colors: Vec<(&str, String)> = vec![
        ("background", p.bg.into()),
        ("foreground", p.fg.into()),
        ("border", p.border.into()),
        ("input.border", if glass { edge.clone() } else { reach(p.border, p.fg, p.bg, 3.0) }),
        ("muted.background", p.surface.into()),
        ("muted.foreground", p.muted.into()),
        ("accent.background", p.raised.into()),
        ("accent.foreground", p.fg.into()),
        ("popover.background", pop.clone()),
        ("popover.foreground", p.fg.into()),
        ("primary.background", p.accent.into()),
        ("primary.hover.background", primary_hover),
        ("primary.active.background", mix(p.accent, if p.dark { black } else { white }, 0.1)),
        ("primary.foreground", p.accent_fg.into()),
        ("secondary.background", p.surface.into()),
        ("secondary.hover.background", p.raised.into()),
        ("secondary.active.background", mix(p.raised, p.fg, 0.06)),
        ("secondary.foreground", p.fg.into()),
        ("ring", p.accent.into()),
        ("selection.background", alpha(p.accent, 0.35)),
        ("caret", p.accent.into()),
        ("link.foreground", mix(p.accent, lift, 0.1)),
        ("link.hover.foreground", mix(p.accent, lift, 0.3)),
        ("link.active.foreground", p.accent.into()),
        ("list.background", page.into()),
        ("list.hover.background", alpha(p.fg, hover_a)),
        ("list.active.background", alpha(p.accent, active_a)),
        ("list.active.border", p.accent.into()),
        ("list.even.background", alpha(p.fg, 0.02)),
        ("list.head.background", p.panel.into()),
        ("sidebar.background", p.panel.into()),
        ("sidebar.foreground", p.fg.into()),
        ("sidebar.border", p.border.into()),
        ("sidebar.accent.background", p.raised.into()),
        ("sidebar.accent.foreground", p.fg.into()),
        ("sidebar.primary.background", p.accent.into()),
        ("sidebar.primary.foreground", p.accent_fg.into()),
        ("title_bar.background", p.panel.into()),
        ("title_bar.border", p.border.into()),
        ("status_bar.background", p.panel.into()),
        ("status_bar.border", p.border.into()),
        ("tab_bar.background", "#00000000".into()),
        ("tab_bar.segmented.background", p.surface.into()),
        ("tab.background", "#00000000".into()),
        ("tab.foreground", p.muted.into()),
        ("tab.active.background", if glass { alpha(p.fg, 0.1) } else { reach(p.raised, p.fg, p.surface, 3.0) }),
        ("tab.active.foreground", p.fg.into()),
        ("table.background", page.into()),
        ("table.head.foreground", p.muted.into()),
        ("table.row.border", alpha(p.fg, 0.05)),
        ("switch.background", reach(p.raised, p.fg, p.bg, 3.0)),
        ("progress_bar.background", p.accent.into()),
        ("slider.bar.background", p.accent.into()),
        ("slider.thumb.background", if p.dark { p.fg.into() } else { white.into() }),
        ("skeleton.background", alpha(p.fg, 0.1)),
        ("scrollbar.background", "#00000000".into()),
        ("scrollbar.thumb.background", alpha(p.fg, 0.15)),
        ("scrollbar.thumb.hover.background", alpha(p.fg, 0.25)),
        ("success.background", ok.clone()),
        ("success.hover.background", ok_h.clone()),
        ("success.active.background", ok_a.clone()),
        ("success.foreground", ok_f.clone()),
        ("button.success.background", ok),
        ("button.success.hover.background", ok_h),
        ("button.success.active.background", ok_a),
        ("button.success.foreground", ok_f),
        ("button.danger.background", bad.clone()),
        ("button.danger.hover.background", bad_h),
        ("button.danger.active.background", bad_a),
        ("button.danger.foreground", bad_f.clone()),
        ("warning.background", p.warning.into()),
        ("warning.foreground", on(p.warning).into()),
        ("danger.background", bad),
        ("danger.foreground", bad_f),
        ("info.background", p.info.into()),
        ("info.foreground", on(p.info).into()),
        ("overlay", if p.dark { "#0000008c".into() } else { "#00000059".into() }),
        ("window.border", edge),
    ];
    let colors: Map<String, Value> = colors
        .into_iter()
        .map(|(k, v)| (k.to_string(), Value::String(v)))
        .collect();
    let v = json!({
        "name": p.name,
        "mode": if p.dark { "dark" } else { "light" },
        "font.size": 15,
        "radius": RADIUS as usize,
        "radius.lg": 10,
        "colors": colors,
    });
    serde_json::from_value(v)
        .inspect_err(|e| log::error!("theme {}: {e}", p.name))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn every_palette_makes_a_theme() {
        for p in PALETTES {
            assert!(config(p).is_some(), "{}", p.name);
        }
        assert!(hopper_dark().is_some());
    }

    #[::core::prelude::v1::test]
    fn colors_mix() {
        assert_eq!(mix("#000000", "#ffffff", 0.5), "#808080ff");
        assert_eq!(alpha("#ff0000", 0.5), "#ff000080");
        assert_eq!(on("#ffffff"), "#141414");
        assert_eq!(on("#000000"), "#ffffff");
    }
}
