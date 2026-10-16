//! Ctrl+K (or Ctrl+P): the command palette. A search box over pages,
//! instances (and their tabs), servers, the picked instance's mods, themes
//! and actions, fuzzy-matched and grouped by section, with recent picks on
//! top. Nothing destructive is offered here: deleting, stopping and pack
//! updates keep their own confirmations on their pages.

use crate::app::{AppView, Page};
use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::kbd::Kbd;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::fuzzy::{self, Candidate, Hit, Query};
use std::path::PathBuf;

gpui_kit::actions!(palette, [SelectPrev, SelectNext, SelectFirst, SelectLast, Confirm, Dismiss]);

const CONTEXT: &str = "CommandPalette";

/// How many recent picks are kept.
const RECENT: usize = 8;

/// The palette's own keys, while it is open.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, ctx),
        KeyBinding::new("down", SelectNext, ctx),
        KeyBinding::new("ctrl-p", SelectPrev, ctx),
        KeyBinding::new("ctrl-n", SelectNext, ctx),
        KeyBinding::new("pageup", SelectFirst, ctx),
        KeyBinding::new("pagedown", SelectLast, ctx),
        KeyBinding::new("enter", Confirm, ctx),
        KeyBinding::new("escape", Dismiss, ctx),
        // Ctrl+K again closes it.
        KeyBinding::new("ctrl-k", Dismiss, ctx),
    ]);
}

// ---- what can be picked -------------------------------------------------------

/// Result groups, in their order when nothing is typed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
enum Section {
    Recent,
    Actions,
    Navigation,
    Instances,
    Servers,
    Mods,
    Themes,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Recent => "Recent",
            Section::Actions => "Actions",
            Section::Navigation => "Navigation",
            Section::Instances => "Instances",
            Section::Servers => "Servers",
            Section::Mods => "Mods",
            Section::Themes => "Themes",
        }
    }

    fn from_u8(n: u8) -> Self {
        match n {
            0 => Section::Recent,
            1 => Section::Actions,
            2 => Section::Navigation,
            3 => Section::Instances,
            4 => Section::Servers,
            5 => Section::Mods,
            _ => Section::Themes,
        }
    }

    /// Most hits shown per section while searching.
    fn cap(n: u8) -> usize {
        match Section::from_u8(n) {
            Section::Mods => 12,
            Section::Instances => 12,
            Section::Themes => 8,
            _ => 10,
        }
    }
}

/// Instance actions, on the instance picked in the Library.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InstAct {
    Play,
    Folder,
    CheckUpdates,
    Logs,
    Host,
    Share,
}

/// App-wide actions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    Create,
    Import,
    Redeem,
    ToggleTheme,
    PackUpdates,
    SelfUpdate,
    Refresh,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Setup,
}

/// What a pick does.
#[derive(Clone, PartialEq)]
pub enum Target {
    Page(Page),
    /// An instance, and one of its tabs (start of the tab's label).
    Instance(PathBuf, Option<&'static str>),
    Server(PathBuf),
    Compare(PathBuf),
    /// The instance's Mods tab, filtered to a mod.
    Mod(PathBuf, String),
    Theme(String),
    Inst(PathBuf, InstAct),
    Act(Act),
    /// Browse, searching CurseForge for the query.
    Search(String),
    /// A category of Settings.
    Settings(crate::settings_page::Category),
}

impl Target {
    /// A stable name, for remembering recent picks.
    fn id(&self) -> String {
        match self {
            Target::Page(p) => format!("page:{}", p.label()),
            Target::Instance(r, t) => format!("inst:{}:{}", r.display(), t.unwrap_or("")),
            Target::Server(r) => format!("server:{}", r.display()),
            Target::Compare(r) => format!("compare:{}", r.display()),
            Target::Mod(r, n) => format!("mod:{}:{n}", r.display()),
            Target::Theme(n) => format!("theme:{n}"),
            // Instance actions follow the picked instance.
            Target::Inst(_, a) => format!("do:{a:?}"),
            Target::Act(a) => format!("act:{a:?}"),
            Target::Search(_) => String::new(),
            Target::Settings(c) => format!("settings:{}", c.label()),
        }
    }
}

struct Item {
    section: Section,
    icon: IconName,
    title: SharedString,
    /// Other words it answers to.
    keywords: &'static str,
    /// Muted text at the trailing edge.
    hint: Option<SharedString>,
    key: Option<Keystroke>,
    target: Target,
    /// Listed before anything is typed.
    browse: bool,
}

impl Item {
    fn new(section: Section, icon: IconName, title: impl Into<SharedString>, target: Target) -> Self {
        Self {
            section,
            icon,
            title: title.into(),
            keywords: "",
            hint: None,
            key: None,
            target,
            browse: false,
        }
    }

    fn browse(mut self) -> Self {
        self.browse = true;
        self
    }

    fn key(mut self, k: &str) -> Self {
        self.key = (!k.is_empty()).then(|| Keystroke::parse(k).ok()).flatten();
        self
    }

    fn hint(mut self, h: impl Into<SharedString>) -> Self {
        self.hint = Some(h.into());
        self
    }

    fn words(mut self, w: &'static str) -> Self {
        self.keywords = w;
        self
    }
}

/// An instance's tabs: label start (what `LibraryView::navigate` takes),
/// shown name and icon.
const TABS: [(&str, &str, IconName); 10] = [
    ("mods", "Mods", IconName::Puzzle),
    ("resource", "Resource packs", IconName::Paintbrush),
    ("shaders", "Shaders", IconName::Sun),
    ("data", "Data packs", IconName::Database),
    ("worlds", "Worlds", IconName::Globe),
    ("screenshots", "Screenshots", IconName::Image),
    ("server", "Server", IconName::HardDrive),
    ("peers", "Peers", IconName::Users),
    ("settings", "Settings", IconName::SlidersHorizontal),
    ("logs", "Logs & crashes", IconName::Stethoscope),
];

/// Everything the palette offers right now. Built once when it opens.
fn build(app: &AppView, cx: &App) -> (Vec<Item>, Option<SharedString>) {
    let store = app.store.read(cx);
    let picked = app
        .library
        .read(cx)
        .selected()
        .and_then(|r| store.instance(r));
    let mut v = Vec::with_capacity(64 + picked.map_or(0, |i| i.addons.len()));
    use Section as S;

    // Actions on the picked instance.
    if let Some(i) = picked {
        let r = || i.root.clone();
        let name = i.name.as_str();
        // Play isn't listed before typing, so Ctrl+K, Enter can't launch.
        v.push(Item::new(S::Actions, IconName::Play, format!("Play {name}"), Target::Inst(r(), InstAct::Play)).words("launch start run game"));
        v.push(Item::new(S::Actions, IconName::FolderOpen, "Open instance folder", Target::Inst(r(), InstAct::Folder)).words("explorer files").browse());
        v.push(Item::new(S::Actions, IconName::CircleArrowUp, "Check for mod updates", Target::Inst(r(), InstAct::CheckUpdates)).words("upgrade").browse());
        v.push(Item::new(S::Actions, IconName::ScrollText, "Open logs", Target::Inst(r(), InstAct::Logs)).words("latest.log crash reports crash help crash assistant"));
        v.push(Item::new(S::Actions, IconName::Server, "Host a server", Target::Inst(r(), InstAct::Host)).words("create server multiplayer"));
        v.push(Item::new(S::Actions, IconName::Share2, "Share code", Target::Inst(r(), InstAct::Share)).words("share friend send code copy"));
    }
    // App-wide actions.
    v.push(Item::new(S::Actions, IconName::Plus, "Create instance", Target::Act(Act::Create)).words("new").browse());
    v.push(Item::new(S::Actions, IconName::Import, "Import a modpack", Target::Act(Act::Import)).words("zip curseforge add").browse());
    v.push(Item::new(S::Actions, IconName::Link, "Install from a share code", Target::Act(Act::Redeem)).words("code friend paste join pack redeem").browse());
    v.push(Item::new(S::Actions, IconName::SunMoon, "Toggle light / dark theme", Target::Act(Act::ToggleTheme)).words("dark light mode appearance").browse());
    v.push(Item::new(S::Actions, IconName::Package, "Check for modpack updates", Target::Act(Act::PackUpdates)).words("upgrade packs"));
    v.push(Item::new(S::Actions, IconName::Download, "Check for a Hopper update", Target::Act(Act::SelfUpdate)).words("upgrade version"));
    v.push(Item::new(S::Actions, IconName::Sparkles, "Run setup again", Target::Act(Act::Setup)).words("first run wizard welcome onboarding curseforge api key install"));
    v.push(Item::new(S::Actions, IconName::RefreshCw, "Re-read instances and servers", Target::Act(Act::Refresh)).words("refresh reload rescan").key("f5"));
    v.push(Item::new(S::Actions, IconName::ZoomIn, "Zoom in", Target::Act(Act::ZoomIn)).words("bigger larger scale").key("ctrl-="));
    v.push(Item::new(S::Actions, IconName::ZoomOut, "Zoom out", Target::Act(Act::ZoomOut)).words("smaller scale").key("ctrl--"));
    v.push(Item::new(S::Actions, IconName::RotateCcw, "Reset zoom", Target::Act(Act::ZoomReset)).words("100% scale").key("ctrl-0"));

    // Pages.
    for (p, key) in [
        (Page::Library, "ctrl-1"),
        (Page::Browse, "ctrl-2"),
        (Page::Servers, "ctrl-3"),
        (Page::Sync, "ctrl-4"),
        (Page::Settings, "ctrl-,"),
    ] {
        let icon = match p {
            Page::Library => IconName::LibraryBig,
            Page::Browse => IconName::Compass,
            Page::Servers => IconName::Server,
            Page::Sync => IconName::ArrowLeftRight,
            Page::Settings => IconName::Settings,
        };
        let words = match p {
            Page::Browse => "curseforge modrinth find download",
            Page::Settings => "preferences options",
            Page::Sync => "rig compare",
            _ => "",
        };
        v.push(Item::new(S::Navigation, icon, p.label(), Target::Page(p)).key(key).words(words).browse());
    }
    // Settings, by category: "Settings › Playing & Java" answers "memory".
    for c in crate::settings_page::Category::ALL {
        v.push(
            Item::new(S::Navigation, c.icon(), format!("Settings › {}", c.label()), Target::Settings(c))
                .words(c.keywords()),
        );
    }

    // Instances, and each one's tabs.
    for i in store.instances() {
        let hint = format!("{} · {}", i.game_version, crate::store::loader_label(&i.loader));
        v.push(Item::new(S::Instances, IconName::Box, i.name.clone(), Target::Instance(i.root.clone(), None)).hint(hint).browse());
        for (tab, label, icon) in TABS {
            v.push(Item::new(
                S::Instances,
                icon,
                format!("{} › {label}", i.name),
                Target::Instance(i.root.clone(), Some(tab)),
            ));
        }
    }

    // Servers.
    for s in store.servers() {
        v.push(Item::new(S::Servers, IconName::Server, format!("Servers › {}", s.name), Target::Server(s.root.clone())).hint(s.loader.clone().unwrap_or_default()));
        v.push(Item::new(S::Servers, IconName::GitCompare, format!("{} › Compare with pack", s.name), Target::Compare(s.root.clone())).words("server sync rig sync diff"));
    }

    // The picked instance's mods.
    if let Some(i) = picked {
        for a in i.addons.iter().filter(|a| crate::addons::in_class(a, hopper::cf::CLASS_MODS)) {
            let name = if a.name.is_empty() { a.file_name.clone() } else { a.name.clone() };
            let hint = if a.enabled { a.author.clone() } else { "disabled".to_string() };
            v.push(Item::new(S::Mods, IconName::Puzzle, name.clone(), Target::Mod(i.root.clone(), name)).hint(hint));
        }
    }

    // Themes.
    for name in crate::theme::names() {
        v.push(Item::new(S::Themes, IconName::Palette, format!("Theme: {name}"), Target::Theme(name)).words("appearance colors"));
    }

    (v, picked.map(|i| SharedString::from(i.name.clone())))
}

// ---- recent picks ---------------------------------------------------------------

fn recent_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.data_dir().join("palette_recent.json"))
}

fn load_recent() -> Vec<String> {
    recent_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Puts `id` first and saves, off the UI thread.
fn remember(id: String, cx: &App) {
    if id.is_empty() {
        return;
    }
    cx.background_spawn(async move {
        let mut list = load_recent();
        list.retain(|x| *x != id);
        list.insert(0, id);
        list.truncate(RECENT);
        if let (Some(p), Ok(json)) = (recent_path(), serde_json::to_vec(&list)) {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, json);
        }
    })
    .detach();
}

// ---- the view -------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    Header(Section),
    Item(usize),
    /// "Search CurseForge for …"
    Search,
}

pub enum PaletteEvent {
    Dismiss,
    Run(Target),
}

impl EventEmitter<PaletteEvent> for Palette {}

pub struct Palette {
    focus: FocusHandle,
    input: Entity<InputState>,
    items: Vec<Item>,
    /// Recent picks still on offer, most recent first.
    recent: Vec<usize>,
    /// The instance the actions act on.
    context: Option<SharedString>,
    text: SharedString,
    query: Query,
    hits: Vec<Hit>,
    rows: Vec<Row>,
    selected: usize,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

impl Palette {
    fn new(items: Vec<Item>, context: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search or run a command…"));
        let subs = vec![cx.subscribe(&input, |this: &mut Self, state, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                let text = state.read(cx).value();
                this.set_query(text, cx);
            }
        })];
        let ids = load_recent();
        let recent = ids
            .iter()
            .filter_map(|id| items.iter().position(|i| !id.is_empty() && i.target.id() == *id))
            .collect();
        let mut this = Self {
            focus: cx.focus_handle(),
            input,
            items,
            recent,
            context,
            text: SharedString::default(),
            query: Query::default(),
            hits: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            scroll: ScrollHandle::new(),
            _subscriptions: subs,
        };
        this.refilter();
        this
    }

    fn set_query(&mut self, text: SharedString, cx: &mut Context<Self>) {
        if text == self.text {
            return;
        }
        self.query.set(&text);
        self.text = text;
        self.refilter();
        cx.notify();
    }

    /// Rebuilds the rows for the query. Reuses its buffers: per keystroke
    /// only the scoring runs.
    fn refilter(&mut self) {
        self.rows.clear();
        if self.query.is_empty() {
            if !self.recent.is_empty() {
                self.rows.push(Row::Header(Section::Recent));
                self.rows.extend(self.recent.iter().map(|&i| Row::Item(i)));
            }
            let mut last = None;
            for (i, item) in self.items.iter().enumerate() {
                if !item.browse || self.recent.contains(&i) {
                    continue;
                }
                if last != Some(item.section) {
                    self.rows.push(Row::Header(item.section));
                    last = Some(item.section);
                }
                self.rows.push(Row::Item(i));
            }
        } else {
            let recent = &self.recent;
            fuzzy::rank(
                &self.query,
                self.items.iter().enumerate().map(|(i, it)| Candidate {
                    section: it.section as u8,
                    title: &it.title,
                    keywords: it.keywords,
                    // Recent picks, then the app's own commands, lead ties.
                    boost: match recent.iter().position(|&r| r == i) {
                        Some(n) => 24 - 2 * n as i32,
                        None => 0,
                    } + match it.section {
                        Section::Actions | Section::Navigation => 5,
                        Section::Instances | Section::Servers => 3,
                        _ => 0,
                    },
                }),
                Section::cap,
                &mut self.hits,
            );
            let mut last = None;
            for h in &self.hits {
                if last != Some(h.section) {
                    self.rows.push(Row::Header(Section::from_u8(h.section)));
                    last = Some(h.section);
                }
                self.rows.push(Row::Item(h.item as usize));
            }
            self.rows.push(Row::Search);
        }
        self.selected = self.rows.iter().position(|r| !matches!(r, Row::Header(_))).unwrap_or(0);
        self.scroll.set_offset(point(px(0.), px(0.)));
    }

    fn select(&mut self, delta: isize, cx: &mut Context<Self>) {
        let n = self.rows.len() as isize;
        if n == 0 {
            return;
        }
        let mut at = self.selected as isize;
        loop {
            at += delta;
            if at < 0 || at >= n {
                return;
            }
            if !matches!(self.rows[at as usize], Row::Header(_)) {
                break;
            }
        }
        self.show(at as usize, cx);
    }

    fn show(&mut self, row: usize, cx: &mut Context<Self>) {
        self.selected = row;
        // A section's first item brings its heading into view too.
        let top = if row > 0 && matches!(self.rows[row - 1], Row::Header(_)) { row - 1 } else { row };
        self.scroll.scroll_to_item(top);
        self.scroll.scroll_to_item(row);
        cx.notify();
    }

    fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.select(-1, cx);
    }

    fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.select(1, cx);
    }

    fn select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.rows.iter().position(|r| !matches!(r, Row::Header(_))) {
            self.show(r, cx);
        }
    }

    fn select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.rows.iter().rposition(|r| !matches!(r, Row::Header(_))) {
            self.show(r, cx);
        }
    }

    fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        self.pick(self.selected, cx);
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PaletteEvent::Dismiss);
    }

    fn pick(&mut self, row: usize, cx: &mut Context<Self>) {
        let target = match self.rows.get(row) {
            Some(Row::Item(i)) => self.items[*i].target.clone(),
            Some(Row::Search) => Target::Search(self.text.trim().to_string()),
            _ => return,
        };
        remember(target.id(), cx);
        cx.emit(PaletteEvent::Run(target));
    }

    fn row(&self, ix: usize, row: Row, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (fg, muted, accent) = (theme.popover_foreground, theme.muted_foreground, theme.primary);
        let on = ix == self.selected;
        match row {
            Row::Header(s) => div()
                .px_2p5()
                .pt_2()
                .pb_1()
                .text_xs()
                .font_medium()
                .text_color(muted)
                .child(s.label())
                .into_any_element(),
            Row::Item(_) | Row::Search => {
                let (icon, title, hint, key): (IconName, SharedString, Option<SharedString>, Option<Keystroke>) = match row {
                    Row::Item(i) => {
                        let it = &self.items[i];
                        (it.icon, it.title.clone(), it.hint.clone(), it.key.clone())
                    }
                    _ => (
                        IconName::Compass,
                        format!("Search CurseForge for “{}”", self.text.trim()).into(),
                        Some("Browse".into()),
                        None,
                    ),
                };
                // The matched letters, in the accent.
                let mut at = Vec::new();
                if matches!(row, Row::Item(_)) {
                    fuzzy::positions(&self.query, &title, &mut at);
                }
                let marks = at.iter().map(|&b| {
                    let len = title[b..].chars().next().map_or(1, char::len_utf8);
                    (
                        b..b + len,
                        HighlightStyle {
                            color: Some(accent),
                            font_weight: Some(FontWeight::SEMIBOLD),
                            ..Default::default()
                        },
                    )
                });
                let text = StyledText::new(title.clone()).with_highlights(marks.collect::<Vec<_>>());
                let hover = theme.list_hover;
                h_flex()
                    .id(("palette-row", ix))
                    .h(rems(2.25))
                    .px_2p5()
                    .gap_2p5()
                    .rounded(theme.radius)
                    .cursor_pointer()
                    .text_sm()
                    .text_color(if on { fg } else { muted })
                    .when(on, |r| r.bg(hover))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.selected != ix {
                            this.selected = ix;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(ix, cx)))
                    .child(Icon::new(icon).small().flex_none())
                    .child(div().flex_1().min_w_0().truncate().text_color(fg).child(text))
                    .when_some(hint, |r, h| {
                        r.child(div().flex_none().max_w(rems(16.)).truncate().text_xs().text_color(muted).child(h))
                    })
                    .when_some(key, |r, k| r.child(Kbd::new(k)))
                    .when(on, |r| r.child(Icon::new(IconName::CornerDownLeft).xsmall().text_color(muted)))
                    .into_any_element()
            }
        }
    }
}

impl Focusable for Palette {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<AnyElement> = (0..self.rows.len()).map(|ix| self.row(ix, self.rows[ix], cx)).collect();
        let theme = cx.theme();
        let empty = self.rows.is_empty();
        let (cap_bg, cap_border) = (theme.muted, theme.border);
        let foot = move |keys: &[&'static str], what: &'static str| {
            h_flex()
                .gap_1()
                .children(keys.iter().map(|k| {
                    div()
                        .min_w(rems(1.125))
                        .px_1()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(cap_border)
                        .bg(cap_bg)
                        .text_center()
                        .child(*k)
                }))
                .child(div().ml_0p5().child(what))
        };
        div()
            .id("palette-scrim")
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_first))
            .on_action(cx.listener(Self::select_last))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::dismiss))
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(0.3))
            .flex()
            .justify_center()
            .items_start()
            .pt(rems(6.5))
            .child(
                v_flex()
                    .id("palette")
                    .w(rems(40.))
                    .max_w_4_5()
                    .h_auto()
                    .max_h(rems(34.))
                    .bg(theme.popover)
                    .text_color(theme.popover_foreground)
                    .border_1()
                    .border_color(theme.border)
                    .rounded(theme.radius_lg)
                    .shadow_xl()
                    .overflow_hidden()
                    .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(PaletteEvent::Dismiss)))
                    .child(
                        h_flex()
                            .h(rems(3.25))
                            .px_4()
                            .gap_2p5()
                            .flex_none()
                            .border_b_1()
                            .border_color(theme.border)
                            .child(Icon::new(IconName::Search).text_color(theme.muted_foreground))
                            .when_some(self.context.clone(), |r, c| {
                                r.child(
                                    div()
                                        .flex_none()
                                        .max_w(rems(14.))
                                        .truncate()
                                        .px_2()
                                        .py_0p5()
                                        .rounded(theme.radius)
                                        .bg(theme.muted)
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(c),
                                )
                            })
                            .child(div().flex_1().child(Input::new(&self.input).appearance(false).cleanable(false))),
                    )
                    .child(
                        div()
                            .id("palette-rows")
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .p_1p5()
                            .when(empty, |r| {
                                r.child(div().p_4().text_sm().text_color(theme.muted_foreground).child("Nothing matches."))
                            })
                            .children(rows),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .h(rems(2.25))
                            .px_3p5()
                            .gap_3p5()
                            .border_t_1()
                            .border_color(theme.border)
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(foot(&["↑", "↓"], "navigate"))
                            .child(foot(&["↵"], "run"))
                            .child(div().flex_1())
                            .child(foot(&["Esc"], "close")),
                    ),
            )
    }
}

// ---- opening, closing and running -----------------------------------------------

/// An open palette, held by the [`AppView`].
pub struct Open {
    view: Entity<Palette>,
    /// Focus goes back here on close.
    restore: FocusHandle,
    _sub: Subscription,
}

impl Open {
    pub fn view(&self) -> AnyView {
        self.view.clone().into()
    }

    pub fn close(self, window: &mut Window, cx: &mut App) {
        window.focus(&self.restore, cx);
    }
}

/// Opens the palette over `app`; focus returns to what had it, or to
/// `fallback`.
pub fn open(app: &AppView, fallback: FocusHandle, window: &mut Window, cx: &mut Context<AppView>) -> Open {
    let restore = window.focused(cx).unwrap_or(fallback);
    let (items, context) = build(app, cx);
    let view = cx.new(|cx| Palette::new(items, context, window, cx));
    let input = view.read(cx).input.clone();
    input.update(cx, |s, cx| s.focus(window, cx));
    let sub = cx.subscribe_in(&view, window, |app: &mut AppView, _, e: &PaletteEvent, window, cx| {
        if let Some(open) = app.palette.take() {
            open.close(window, cx);
        }
        if let PaletteEvent::Run(t) = e {
            run(app, t.clone(), window, cx);
        }
        cx.notify();
    });
    Open {
        view,
        restore,
        _sub: sub,
    }
}

fn notify(text: impl Into<SharedString>, window: &mut Window, cx: &mut App) {
    window.push_notification(
        gpui_kit::component::notification::Notification::info(text.into()),
        cx,
    );
}

fn set_theme(store: &Entity<Store>, name: String, cx: &mut App) {
    store.update(cx, |s, cx| {
        s.update_settings(
            |st| {
                st.theme_name = name.clone();
                st.theme = 0;
            },
            cx,
        )
    });
    let zoom = store.read(cx).settings().zoom;
    crate::theme::apply(&name, 0, zoom, cx);
}

fn open_instance(app: &mut AppView, root: PathBuf, tab: Option<&str>, cx: &mut Context<AppView>) {
    app.go(Page::Library, cx);
    app.library.update(cx, |l, cx| {
        l.open(root, cx);
        if tab.is_some() {
            l.navigate(None, tab, cx);
        }
    });
}

/// Does what was picked.
fn run(app: &mut AppView, target: Target, window: &mut Window, cx: &mut Context<AppView>) {
    let store = app.store.clone();
    match target {
        Target::Page(p) => app.nav(p, window, cx),
        Target::Instance(root, tab) => open_instance(app, root, tab, cx),
        Target::Server(root) => app.open_server(root, cx),
        Target::Compare(root) => {
            app.sync.update(cx, |p, cx| p.compare(root, cx));
            app.go(Page::Sync, cx);
        }
        Target::Mod(root, name) => {
            open_instance(app, root, Some("mods"), cx);
            let addons = app.library.read(cx).addons().clone();
            addons.update(cx, |a, cx| a.set_search(&name, window, cx));
        }
        Target::Theme(name) => set_theme(&store, name, cx),
        Target::Settings(c) => app.settings_category(c, cx),
        Target::Search(q) => {
            app.go(Page::Browse, cx);
            app.browse.update(cx, |b, cx| b.set_query(&q, window, cx));
        }
        Target::Inst(root, act) => match act {
            InstAct::Play => {
                // As the Library's Play button: sign in first when Hopper
                // launches the game itself.
                let native = store.read(cx).settings().launch_native;
                if native && !hopper::msauth::has_saved_login() {
                    crate::dialogs::open_sign_in(store, window, cx);
                } else if let Err(e) = store.update(cx, |s, _| s.play(&root)) {
                    window.push_notification(gpui_kit::component::notification::Notification::warning(e), cx);
                }
            }
            InstAct::Folder => crate::store::open_path(&root),
            InstAct::CheckUpdates => {
                store.update(cx, |s, cx| {
                    s.check_updates(&root, true);
                    cx.notify();
                });
                open_instance(app, root, Some("mods"), cx);
            }
            InstAct::Logs => open_instance(app, root, Some("logs"), cx),
            InstAct::Host => crate::host_dialog::open(store, Some(root), window, cx),
            InstAct::Share => crate::dialogs::open_share(store, root, window, cx),
        },
        Target::Act(act) => match act {
            Act::Create => crate::dialogs::open_create(store, window, cx),
            Act::Import => crate::dialogs::import_pack(store, cx),
            Act::Redeem => crate::dialogs::open_redeem(store, window, cx),
            Act::ToggleTheme => {
                let name = if cx.theme().is_dark() {
                    crate::theme::HOPPER_LIGHT
                } else {
                    crate::theme::HOPPER_DARK
                };
                set_theme(&store, name.to_string(), cx);
            }
            Act::PackUpdates => {
                store.read(cx).check_pack_updates();
                notify("Checking for modpack updates…", window, cx);
            }
            Act::SelfUpdate => {
                store.update(cx, |s, cx| {
                    s.check_self_update();
                    cx.notify();
                });
                notify("Checking for a Hopper update…", window, cx);
            }
            // The app's own key handlers, once focus is back on the page.
            Act::Refresh => window.defer(cx, |w, cx| w.dispatch_action(Box::new(crate::app::Refresh), cx)),
            Act::ZoomIn => window.defer(cx, |w, cx| w.dispatch_action(Box::new(crate::app::ZoomIn), cx)),
            Act::ZoomOut => window.defer(cx, |w, cx| w.dispatch_action(Box::new(crate::app::ZoomOut), cx)),
            Act::ZoomReset => window.defer(cx, |w, cx| w.dispatch_action(Box::new(crate::app::ZoomReset), cx)),
            Act::Setup => crate::setup_wizard::open(store, cx.entity().downgrade(), false, window, cx),
        },
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: GPUI's own `test` attribute would shadow the std one.
    use super::{InstAct, Target};
    use crate::app::Page;
    use std::path::PathBuf;

    #[test]
    fn recent_ids_are_stable_and_follow_the_instance_for_actions() {
        let a = Target::Inst(PathBuf::from("a"), InstAct::Play);
        let b = Target::Inst(PathBuf::from("b"), InstAct::Play);
        assert_eq!(a.id(), b.id());
        assert_ne!(Target::Page(Page::Library).id(), Target::Page(Page::Browse).id());
        assert!(Target::Search("x".into()).id().is_empty());
    }
}
