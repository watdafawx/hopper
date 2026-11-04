//! The instance's Configs tab and the side panel a mod's config button
//! opens. Both show the shared config view; the tab adds the list of mods
//! with config files ("Changed by you", "All mods", "Not matched to a
//! mod"), a search across every setting, filters, and (for an instance
//! linked to a server) a compare with the server's copies.

use crate::config_view::{self, ConfigView, ConfigViewEvent, Host, InstIndex};
use crate::store::Store;
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configindex::Side;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// A mod's name and icon from the instance, by its jar's file name.
fn mod_info(store: &Store, root: &Path, key: &str) -> (String, Option<PathBuf>, u64) {
    let a = store.instance(root).and_then(|i| i.addons.iter().find(|a| a.file_name == key));
    match a {
        Some(a) => (a.name.clone(), store.image(&a.icon).map(Path::to_path_buf), a.project_id),
        None => (key.trim_end_matches(".jar").to_string(), None, 0),
    }
}

/// Who a list entry is: a mod, or a file no mod matched.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Pick {
    Mod(String),
    File(String),
}

// ---- the side panel ---------------------------------------------------------------------

const PANEL: widgets::PanelSpec = widgets::PanelSpec::new("config.panel", 30., 24., 48.).trailing();

pub enum ConfigPanelEvent {
    Close,
    /// Open the Configs tab at this mod.
    OpenInTab(String),
}

impl EventEmitter<ConfigPanelEvent> for ConfigPanel {}

/// One mod's config beside the Mods tab, from its config button.
pub struct ConfigPanel {
    store: Entity<Store>,
    view: Entity<ConfigView>,
    panel: widgets::SidePanel,
    /// The mod (its jar's file name).
    pub key: String,
    _sub: Subscription,
}

impl ConfigPanel {
    #[allow(clippy::too_many_arguments)]
    pub fn new(store: Entity<Store>, root: PathBuf, key: String, name: String, icon: Option<PathBuf>, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let view = cx.new(|cx| ConfigView::new(store.clone(), root, Host::Panel, name, icon, files, None, None, window, cx));
        let k = key.clone();
        let sub = cx.subscribe(&view, move |_, _, e: &ConfigViewEvent, cx| match e {
            ConfigViewEvent::Close => cx.emit(ConfigPanelEvent::Close),
            ConfigViewEvent::OpenInTab => cx.emit(ConfigPanelEvent::OpenInTab(k.clone())),
            ConfigViewEvent::Changed => cx.notify(),
        });
        Self { store, view, panel: widgets::SidePanel::new(PANEL), key, _sub: sub }
    }

    pub fn unsaved(&self, cx: &App) -> usize {
        self.view.read(cx).unsaved()
    }

    pub fn root(&self, cx: &App) -> PathBuf {
        self.view.read(cx).root().to_path_buf()
    }
}

impl Render for ConfigPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let content = div().size_full().bg(theme.background).border_l_1().border_color(theme.border).child(self.view.clone());
        widgets::side_panel("cfg-panel-edge", &self.panel, |t: &mut Self| &mut t.panel, &self.store, content, window, cx)
    }
}

// ---- the tab ---------------------------------------------------------------------------

/// The filter chips over the list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Changed,
    Unsaved,
}

pub struct ConfigsTab {
    store: Entity<Store>,
    root: Option<PathBuf>,
    index: Option<Arc<InstIndex>>,
    search: Entity<InputState>,
    query: String,
    desc: bool,
    filter: Filter,
    side: Option<Side>,
    /// Search hits per entry of the list, and the files they're in.
    hits: Option<HashMap<Pick, (usize, Vec<String>)>>,
    search_serial: u64,
    picked: Option<Pick>,
    /// Open views by entry, so edits survive picking another mod.
    views: HashMap<Pick, Entity<ConfigView>>,
    /// The compare with the linked server, when it's on.
    compare: Option<Entity<crate::config_compare::CompareView>>,
    compare_on: bool,
    _subs: Vec<Subscription>,
}

impl ConfigsTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search settings in every mod"));
        let idx = config_view::indexes(cx);
        let subs = vec![
            cx.subscribe(&search, |this, s, e: &InputEvent, cx| {
                if let InputEvent::Change = e {
                    this.query = s.read(cx).value().to_string();
                    this.run_search(cx);
                }
            }),
            cx.observe(&idx, |this, _, cx| this.refresh_index(cx)),
        ];
        Self {
            store,
            root: None,
            index: None,
            search,
            query: String::new(),
            desc: false,
            filter: Filter::All,
            side: None,
            hits: None,
            search_serial: 0,
            picked: None,
            views: HashMap::new(),
            compare: None,
            compare_on: false,
            _subs: subs,
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            // Unsaved edits of the old instance stay with its views only while it's shown.
            self.views.clear();
            self.picked = None;
            self.hits = None;
            self.compare = None;
            self.compare_on = false;
            self.index = None;
        }
        self.root = Some(root);
        self.refresh_index(cx);
    }

    /// Opens a mod (from its panel's "Open in Configs tab").
    pub fn pick_mod(&mut self, key: String, cx: &mut Context<Self>) {
        self.picked = Some(Pick::Mod(key));
        cx.notify();
    }

    fn refresh_index(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.clone();
        let idx = config_view::indexes(cx).update(cx, |i, cx| i.get(&root, &store, cx));
        if idx.is_some() {
            self.index = idx;
            if !self.query.trim().is_empty() {
                self.run_search(cx);
            }
        }
        cx.notify();
    }

    /// Searches every file off the UI thread, a moment after typing stops.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        self.search_serial += 1;
        let serial = self.search_serial;
        let q = self.query.trim().to_string();
        for v in self.views.values() {
            let q = q.clone();
            v.update(cx, |v, cx| v.set_query(&q, cx));
        }
        if q.is_empty() {
            self.hits = None;
            cx.notify();
            return;
        }
        let (Some(root), Some(idx)) = (self.root.clone(), self.index.clone()) else { return };
        let desc = self.desc;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(180)).await;
            if this.update(cx, |t, _| t.search_serial != serial).unwrap_or(true) {
                return;
            }
            let hits = cx
                .background_spawn(async move {
                    let rels: Vec<String> = idx.files.iter().map(|f| f.rel.clone()).collect();
                    let owner: HashMap<&str, &str> = idx.files.iter().filter_map(|f| Some((f.rel.as_str(), f.owner.as_deref()?))).collect();
                    let mut out: HashMap<Pick, (usize, Vec<String>)> = HashMap::new();
                    for h in hopper::configindex::search(&root, &rels, &q, desc, 20_000) {
                        let pick = match owner.get(h.rel.as_str()) {
                            Some(o) => Pick::Mod(o.to_string()),
                            None => Pick::File(h.rel.clone()),
                        };
                        let e = out.entry(pick).or_default();
                        e.0 += 1;
                        if !e.1.contains(&h.rel) {
                            e.1.push(h.rel);
                        }
                    }
                    out
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if t.search_serial == serial {
                    t.hits = Some(hits);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn files_of(&self, p: &Pick) -> Vec<String> {
        match (p, &self.index) {
            (Pick::Mod(k), Some(i)) => i.mod_files(k),
            (Pick::File(r), _) => vec![r.clone()],
            _ => Vec::new(),
        }
    }

    fn changed_of(&self, p: &Pick) -> usize {
        match (p, &self.index) {
            (Pick::Mod(k), Some(i)) => i.mod_changed(k),
            (Pick::File(r), Some(i)) => i.changed.get(r).copied().unwrap_or(0),
            _ => 0,
        }
    }

    fn unsaved_of(&self, p: &Pick, cx: &App) -> usize {
        self.views.get(p).map_or(0, |v| v.read(cx).unsaved())
    }

    /// The view for `p`, made the first time it's shown.
    fn view_for(&mut self, p: &Pick, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<ConfigView>> {
        if let Some(v) = self.views.get(p) {
            return Some(v.clone());
        }
        let root = self.root.clone()?;
        let files = self.files_of(p);
        let (title, icon) = match p {
            Pick::Mod(k) => {
                let (n, i, _) = mod_info(self.store.read(cx), &root, k);
                (n, i)
            }
            Pick::File(r) => (r.rsplit('/').next().unwrap_or(r).to_string(), None),
        };
        // A search hit opens the file it's in.
        let first = self.hits.as_ref().and_then(|h| h.get(p)).and_then(|(_, f)| f.first().cloned());
        let (q, desc, changed, side) = (self.query.trim().to_string(), self.desc, self.filter == Filter::Changed, self.side);
        let view = cx.new(|cx| {
            let mut v = ConfigView::new(self.store.clone(), root, Host::Tab, title, icon, files, first, None, window, cx);
            v.set_query(&q, cx);
            v.desc = desc;
            v.changed_only = changed;
            v.side = side;
            v
        });
        let sub = cx.subscribe(&view, |this, _, e: &ConfigViewEvent, cx| {
            if let ConfigViewEvent::Changed = e {
                cx.notify();
            }
            let _ = this;
        });
        self._subs.push(sub);
        self.views.insert(p.clone(), view.clone());
        Some(view)
    }

    fn set_filter(&mut self, f: Filter, cx: &mut Context<Self>) {
        self.filter = f;
        let changed = f == Filter::Changed;
        for v in self.views.values() {
            v.update(cx, |v, cx| {
                v.changed_only = changed;
                cx.notify();
            });
        }
        cx.notify();
    }

    fn set_side(&mut self, s: Option<Side>, cx: &mut Context<Self>) {
        self.side = s;
        for v in self.views.values() {
            v.update(cx, |v, cx| {
                v.side = s;
                cx.notify();
            });
        }
        cx.notify();
    }

    /// The entries of the list, grouped, as the filters and search leave them.
    fn groups(&self, cx: &App) -> Vec<(&'static str, Vec<Pick>)> {
        let Some(idx) = &self.index else { return Vec::new() };
        let root = self.root.as_deref().unwrap_or(Path::new(""));
        let store = self.store.read(cx);
        let side_ok = |files: &[String]| self.side.is_none_or(|s| files.iter().any(|f| hopper::configindex::side_of(f) == s));
        let keep = |p: &Pick| {
            let files = self.files_of(p);
            if !side_ok(&files) {
                return false;
            }
            if let Some(h) = &self.hits
                && !h.contains_key(p)
            {
                return false;
            }
            match self.filter {
                Filter::All => true,
                Filter::Changed => self.changed_of(p) > 0 || self.unsaved_of(p, cx) > 0,
                Filter::Unsaved => self.unsaved_of(p, cx) > 0,
            }
        };
        let mut mods: Vec<Pick> = idx.by_owner.keys().map(|k| Pick::Mod(k.clone())).filter(keep).collect();
        mods.sort_by_cached_key(|p| match p {
            Pick::Mod(k) => mod_info(store, root, k).0.to_lowercase(),
            Pick::File(r) => r.clone(),
        });
        let (changed, rest): (Vec<Pick>, Vec<Pick>) = mods.into_iter().partition(|p| self.changed_of(p) > 0 || self.unsaved_of(p, cx) > 0);
        let unmatched: Vec<Pick> = idx.unmatched().into_iter().map(Pick::File).filter(keep).collect();
        vec![("Changed", changed), ("All mods", rest), ("Not matched to a mod", unmatched)].into_iter().filter(|(_, v)| !v.is_empty()).collect()
    }

    fn list_row(&self, p: &Pick, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let root = self.root.clone().unwrap_or_default();
        let (name, icon, unmatched) = match p {
            Pick::Mod(k) => {
                let (n, i, _) = mod_info(self.store.read(cx), &root, k);
                (n, i, false)
            }
            Pick::File(r) => (r.trim_start_matches("config/").to_string(), None, true),
        };
        let files = self.files_of(p);
        let (changed, unsaved) = (self.changed_of(p), self.unsaved_of(p, cx));
        let hits = self.hits.as_ref().and_then(|h| h.get(p)).map(|h| h.0);
        let right = match (hits, unsaved, changed) {
            (Some(h), _, _) => format!("{h} match{}", if h == 1 { "" } else { "es" }),
            (_, u, _) if u > 0 => format!("{u} unsaved"),
            (_, _, c) if c > 0 => c.to_string(),
            _ if files.len() == 1 => files[0].rsplit('.').next().unwrap_or("").to_string(),
            _ => format!("{} files", files.len()),
        };
        let selected = self.picked.as_ref() == Some(p);
        let pick = p.clone();
        let row = h_flex()
            .id(SharedString::from(format!("cfg-mod:{p:?}")))
            .gap_2()
            .px_2()
            .py_1()
            .rounded(theme.radius)
            .text_sm()
            .clickable(cx)
            .child(if unmatched {
                div().size(rems(1.25)).flex_none().rounded(theme.radius).bg(theme.secondary).flex().items_center().justify_center().text_xs().text_color(theme.muted_foreground).child("?").into_any_element()
            } else {
                widgets::thumb(icon.as_deref(), rems(1.25), cx)
            })
            .child(div().flex_1().min_w_0().truncate().when(unmatched, |d| d.text_color(theme.muted_foreground)).child(name))
            .when(changed > 0 || unsaved > 0, |d| d.child(div().size(px(6.)).flex_none().rounded_full().bg(theme.primary)))
            .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(right))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.picked = Some(pick.clone());
                cx.notify();
            }));
        let row = if selected { widgets::row_selected(row, cx) } else { row.hover(|s| widgets::row_hover(s, cx)) };
        row.into_any_element()
    }
}

impl Render for ConfigsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(root) = self.root.clone() else { return div().into_any_element() };
        let Some(idx) = self.index.clone() else {
            return v_flex()
                .size_full()
                .p_6()
                .gap_2()
                .child(widgets::caption("Reading the config files…", cx))
                .child(widgets::skeleton_rows(8, rems(2.5), cx))
                .into_any_element();
        };
        let groups = self.groups(cx);
        // The first entry when nothing is picked (or the pick is filtered out).
        let all: Vec<Pick> = groups.iter().flat_map(|(_, v)| v.clone()).collect();
        if self.picked.as_ref().is_none_or(|p| !all.contains(p) && self.hits.is_some()) {
            self.picked = all.first().cloned();
        }
        let picked = self.picked.clone();
        let view = picked.as_ref().and_then(|p| self.view_for(p, window, cx));
        let total = idx.total_settings();
        let changed_n: usize = idx.changed.values().sum();
        let unsaved_n: usize = self.views.values().map(|v| v.read(cx).unsaved()).sum();
        self.search.update(cx, |s, cx| s.set_placeholder(format!("Search {} settings in every mod", fmt_n(total)), window, cx));
        let linked = self.store.read(cx).server_of(&root).cloned();

        let chips = h_flex()
            .gap_1p5()
            .child(widgets::toggle_chip("cfg-f-all", "All", self.filter == Filter::All, cx).on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::All, cx))))
            .child(widgets::toggle_chip("cfg-f-changed", format!("Changed {}", fmt_n(changed_n)), self.filter == Filter::Changed, cx).tooltip("Settings that aren't at the default their file states").on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::Changed, cx))))
            .child(widgets::toggle_chip("cfg-f-unsaved", format!("Unsaved {unsaved_n}"), self.filter == Filter::Unsaved, cx).on_click(cx.listener(|this, _, _, cx| this.set_filter(Filter::Unsaved, cx))));
        let sides: Vec<(Option<Side>, &str)> = vec![(None, "All"), (Some(Side::Client), "This PC"), (Some(Side::Common), "Both"), (Some(Side::Server), "Server decides")];
        let side_label = sides.iter().find(|(s, _)| *s == self.side).map_or("All", |(_, l)| l);
        let view_e = cx.entity();
        let side_pick = div().w(rems(11.)).child(widgets::picker(
            "cfg-side",
            format!("Side: {side_label}"),
            sides.iter().map(|(s, l)| (l.to_string(), *s == self.side)).collect(),
            window.rem_size(),
            Rc::new(move |i, _, cx| {
                let s = [None, Some(Side::Client), Some(Side::Common), Some(Side::Server)][i.min(3)];
                view_e.update(cx, |this, cx| this.set_side(s, cx));
            }),
        ));
        let toolbar = h_flex()
            .gap_2()
            .px_6()
            .py_2()
            .flex_wrap()
            .child(div().w(rems(24.)).child(widgets::search_input(&self.search).small().cleanable(true)))
            .child(chips)
            .child(div().flex_1())
            .child(
                Checkbox::new("cfg-desc").label("Search descriptions").checked(self.desc).on_click(cx.listener(|this, v: &bool, _, cx| {
                    this.desc = *v;
                    let d = *v;
                    for view in this.views.values() {
                        view.update(cx, |view, cx| {
                            view.desc = d;
                            cx.notify();
                        });
                    }
                    this.run_search(cx);
                })),
            )
            .child(side_pick)
            .when_some(linked.clone(), |d, s| {
                d.child(
                    widgets::toggle_chip("cfg-compare", "Compare with server", self.compare_on, cx)
                        .tooltip(format!("Settings that differ from {}'s copies, side by side", s.name))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.compare_on = !this.compare_on;
                            cx.notify();
                        })),
                )
            });

        let mut list = v_flex().gap_0p5().p_2();
        let empty = groups.is_empty();
        for (title, picks) in &groups {
            list = list.child(div().px_2().pt_2().pb_1().text_xs().font_semibold().text_color(theme.muted_foreground).child(format!("{} · {}", title.to_uppercase(), picks.len())));
            for p in picks.iter().take(1500) {
                list = list.child(self.list_row(p, cx));
            }
        }
        if empty {
            list = list.child(widgets::empty_state(
                IconName::FileCog,
                if self.hits.is_some() { "No settings match" } else if self.filter != Filter::All { "Nothing here" } else { "No config files yet" },
                (self.hits.is_none() && self.filter == Filter::All).then(|| "Most mods write theirs on the first launch.".into()),
                cx,
            ));
        }

        let right: AnyElement = if self.compare_on
            && let Some(server) = linked
        {
            let c = match &self.compare {
                Some(c) => c.clone(),
                None => {
                    let c = cx.new(|cx| crate::config_compare::CompareView::new(self.store.clone(), root.clone(), server.clone(), cx));
                    self.compare = Some(c.clone());
                    c
                }
            };
            let only = picked.as_ref().map(|p| self.files_of(p));
            c.update(cx, |c, cx| c.set_files(only, cx));
            c.into_any_element()
        } else {
            match view {
                Some(v) => v.into_any_element(),
                None => div().into_any_element(),
            }
        };

        v_flex()
            .size_full()
            .child(toolbar)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().id("cfg-mods").w(rems(17.)).flex_none().h_full().border_r_1().border_color(theme.border).overflow_y_scroll().child(list))
                    .child(div().flex_1().min_w_0().h_full().child(right)),
            )
            .into_any_element()
    }
}

fn fmt_n(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}
