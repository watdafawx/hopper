//! The Configs tab's "Compare with server": the picked mod's settings
//! files beside the linked server's live copies, setting by setting. Pick
//! → (this PC's value to the server) or ← (the server's to this PC), then
//! Apply: only those settings change, not whole files, and the old copies
//! go to Backups on both sides.

use crate::config_view;
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configcompare::{self, Pair};
use hopper::configedit::{self, Edit, Format};
use hopper::configindex::Side;
use hopper::server::ServerInfo;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

/// Which way a picked setting goes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dir {
    ToServer,
    ToPc,
}

/// One file of this PC beside the server's copy.
struct Cmp {
    /// The server's copy (relative to its folder), when it has one.
    srel: Option<String>,
    client: String,
    server: String,
    pairs: Result<Vec<Pair>, String>,
}

/// Reads a file of the server: through its panel when it's on a Hopper
/// Server, else from its folder.
fn read_server(server: &ServerInfo, rel: &str) -> Option<String> {
    let path = server.root.join(rel);
    match hopper::hopperd::locate(&path) {
        Some(Ok((c, id, r))) => c.read_text(&id, &r).ok(),
        Some(Err(_)) => None,
        None => std::fs::read_to_string(&path).ok(),
    }
}

/// Writes `edits` into the server's `rel` (opened as `base`): through the
/// panel's batch route (it checks them, backs up and writes), or straight
/// to a local server's folder.
fn write_server(server: &ServerInfo, rel: &str, base: &str, edits: &[Edit]) -> anyhow::Result<()> {
    let path = server.root.join(rel);
    match hopper::hopperd::locate(&path) {
        Some(Ok((c, id, r))) => {
            let body = serde_json::json!({
                "rel": r,
                "base": base,
                "edits": edits.iter().map(|e| serde_json::json!({ "path": e.path, "key": e.key, "value": e.value.text() })).collect::<Vec<_>>(),
            });
            let v = c.post(&hopper::hopperd::Client::server_path(&id, "/configs/batch"), body)?;
            if v["ok"].as_bool() == Some(false) {
                let why: Vec<String> = v["errors"].as_array().into_iter().flatten().filter_map(|e| e["error"].as_str().map(str::to_string)).collect();
                anyhow::bail!("{}", if why.is_empty() { v["error"].as_str().unwrap_or("The server refused it.").to_string() } else { why.join(" ") });
            }
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => configedit::save_edits(&server.root, rel, base, edits).map(|_| ()),
    }
}

pub struct CompareView {
    store: Entity<Store>,
    root: PathBuf,
    server: ServerInfo,
    /// The files shown (the picked mod's).
    only: Option<Vec<String>>,
    files: HashMap<String, Cmp>,
    loading: bool,
    all: bool,
    picks: BTreeMap<(String, Vec<String>, String), Dir>,
    applying: bool,
    /// Files paired by hand (Sync › Configs: the same path on both sides),
    /// instead of looking for the server's live copy.
    fixed: HashMap<String, String>,
    /// Drawn inside another list (Sync): no server chip, a set height.
    embedded: bool,
}

/// Settings were applied (Sync compares its files again).
pub struct CompareApplied;

impl EventEmitter<CompareApplied> for CompareView {}

impl CompareView {
    pub fn new(store: Entity<Store>, root: PathBuf, server: ServerInfo, _cx: &mut Context<Self>) -> Self {
        Self { store, root, server, only: None, files: HashMap::new(), loading: false, all: false, picks: BTreeMap::new(), applying: false, fixed: HashMap::new(), embedded: false }
    }

    /// One file of Sync › Configs: `rel` on this PC beside `srel` on the
    /// server, its differing settings with → / ← and Apply.
    pub fn for_file(store: Entity<Store>, root: PathBuf, server: ServerInfo, rel: String, srel: String, cx: &mut Context<Self>) -> Self {
        let mut v = Self::new(store, root, server, cx);
        v.fixed.insert(rel.clone(), srel);
        v.embedded = true;
        v.set_files(Some(vec![rel]), cx);
        v
    }

    /// Shows these files (the picked mod's); reads the ones not read yet.
    pub fn set_files(&mut self, only: Option<Vec<String>>, cx: &mut Context<Self>) {
        if self.only == only {
            return;
        }
        self.only = only;
        self.load(cx);
    }

    fn level(&self) -> String {
        self.server.properties.get("level-name").cloned().filter(|l| configedit::editable(&format!("{l}/serverconfig/x")) ).unwrap_or_else(|| "world".into())
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let want: Vec<String> = self.only.clone().unwrap_or_default().into_iter().filter(|r| !self.files.contains_key(r)).collect();
        if want.is_empty() {
            cx.notify();
            return;
        }
        self.loading = true;
        let (root, server, level, fixed) = (self.root.clone(), self.server.clone(), self.level(), self.fixed.clone());
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move {
                    want.into_iter()
                        .map(|rel| {
                            let client = std::fs::read_to_string(root.join(&rel)).unwrap_or_default();
                            let tries = match fixed.get(&rel) {
                                Some(s) => vec![s.clone()],
                                None => configcompare::server_tries(&rel, &level),
                            };
                            let found = tries.into_iter().find_map(|r| read_server(&server, &r).map(|t| (r, t)));
                            let cmp = match (fixed.contains_key(&rel) || configcompare::compared(&rel), found, Format::of(&rel)) {
                                (false, _, _) => Cmp { srel: None, client, server: String::new(), pairs: Err("client".into()) },
                                (true, None, _) => Cmp { srel: None, client, server: String::new(), pairs: Err("The server has no copy yet.".into()) },
                                (true, Some((srel, text)), Some(f)) => {
                                    let pairs = configcompare::pairs(f, &client, &text);
                                    Cmp { srel: Some(srel), client, server: text, pairs }
                                }
                                (true, Some((srel, text)), None) => Cmp { srel: Some(srel), client, server: text, pairs: Err(String::new()) },
                            };
                            (rel, cmp)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.files.extend(read);
                this.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picks.is_empty() || self.applying {
            return;
        }
        // Edits per file and side.
        let mut to_pc: BTreeMap<String, Vec<Edit>> = BTreeMap::new();
        let mut to_server: BTreeMap<String, (String, String, Vec<Edit>)> = BTreeMap::new();
        for ((rel, path, key), dir) in &self.picks {
            let Some(c) = self.files.get(rel) else { continue };
            let Ok(pairs) = &c.pairs else { continue };
            let Some(p) = pairs.iter().find(|p| p.entry.path == *path && p.entry.key == *key) else { continue };
            match dir {
                Dir::ToPc => to_pc.entry(rel.clone()).or_default().push(Edit { path: path.clone(), key: key.clone(), value: p.server.clone() }),
                Dir::ToServer => {
                    let Some(srel) = &c.srel else { continue };
                    to_server.entry(rel.clone()).or_insert_with(|| (srel.clone(), c.server.clone(), Vec::new())).2.push(Edit { path: path.clone(), key: key.clone(), value: p.client.clone() })
                }
            }
        }
        let bases: HashMap<String, String> = self.files.iter().map(|(r, c)| (r.clone(), c.client.clone())).collect();
        self.applying = true;
        cx.notify();
        let (root, server) = (self.root.clone(), self.server.clone());
        let name = server.name.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (done, failed) = cx
                .background_spawn(async move {
                    let (mut done, mut failed) = (0usize, Vec::new());
                    for (rel, edits) in &to_pc {
                        match configedit::save_edits(&root, rel, bases.get(rel).map_or("", |s| s.as_str()), edits) {
                            Ok(_) => done += edits.len(),
                            Err(e) => failed.push(format!("{rel}: {e:#}")),
                        }
                    }
                    for (srel, base, edits) in to_server.values() {
                        match write_server(&server, srel, base, edits) {
                            Ok(()) => done += edits.len(),
                            Err(e) => failed.push(format!("{srel} on the server: {e:#}")),
                        }
                    }
                    (done, failed)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.applying = false;
                this.picks.clear();
                // Read both sides again.
                let shown = this.only.clone();
                this.files.clear();
                this.only = None;
                this.set_files(shown, cx);
                config_view::invalidate(&this.root, cx);
                cx.emit(CompareApplied);
                if done > 0 {
                    window.push_notification(
                        Notification::success(format!("Applied {done} setting{}. The old copies are in Backups here and on {name}; the server uses them on its next start.", if done == 1 { "" } else { "s" })),
                        cx,
                    );
                }
                for f in failed {
                    window.push_notification(Notification::error(format!("Not applied: {f}")), cx);
                }
            });
        })
        .detach();
    }

    fn file_block(&self, rel: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let name = rel.rsplit('/').next().unwrap_or(rel).to_string();
        let side = hopper::configindex::side_of(rel);
        let c = self.files.get(rel);
        let (chip, why): (AnyElement, &str) = match side {
            Side::Client => (widgets::chip(theme.muted_foreground).child("This PC only").into_any_element(), "Client settings never go to the server."),
            Side::Server => (
                widgets::chip(theme.warning).child(h_flex().gap_1().child(Icon::new(IconName::Lock).xsmall()).child("Server decides")).into_any_element(),
                "The server sends these to players when they join. Your copy only matters in single player.",
            ),
            _ => (widgets::chip(theme.info).child("Both load it").into_any_element(), "Each side reads its own copy. Some mods refuse to join when these differ; keep them the same."),
        };
        let mut col = v_flex().gap_0p5();
        let head = h_flex()
            .gap_2()
            .px_3()
            .pt_3()
            .child(div().font_family("Consolas").text_sm().font_semibold().child(name))
            .child(chip)
            .child(div().flex_1())
            .children(c.and_then(|c| c.srel.clone()).map(|s| widgets::caption(format!("server: {s}"), cx).text_xs()));
        col = col.child(head);
        let Some(c) = c else {
            return col.child(div().px_3().child(widgets::caption("Reading…", cx).text_xs())).into_any_element();
        };
        let pairs = match &c.pairs {
            Err(e) if e == "client" => return col.child(div().px_3().child(widgets::caption("Not compared. Shown under This PC.", cx).text_xs())).into_any_element(),
            Err(e) if e.is_empty() => return col.child(div().px_3().child(widgets::caption("Can't be compared setting by setting. Sync › Configs compares it as a whole file.", cx).text_xs())).into_any_element(),
            Err(e) => return col.child(div().px_3().child(widgets::caption(e.clone(), cx).text_xs())).into_any_element(),
            Ok(p) => p,
        };
        let shown: Vec<&Pair> = pairs.iter().filter(|p| self.all || p.differs()).collect();
        let differ = pairs.iter().filter(|p| p.differs()).count();
        col = col.child(
            h_flex()
                .mx_3()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .bg(theme.foreground.opacity(0.04))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().whitespace_normal().child(why))
                .child(format!("{differ} differ")),
        );
        if shown.is_empty() {
            col = col.child(div().px_3().child(widgets::caption("The same on both sides.", cx).text_xs()));
        }
        for p in shown {
            let k = (rel.to_string(), p.entry.path.clone(), p.entry.key.clone());
            let pick = self.picks.get(&k).copied();
            let cell = |v: &configedit::Value, on: bool, dim: bool| {
                div()
                    .w(rems(10.))
                    .flex_none()
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(if on { theme.primary.opacity(0.6) } else { theme.border })
                    .when(dim, |d| d.text_color(theme.muted_foreground))
                    .font_family("Consolas")
                    .text_xs()
                    .truncate()
                    .child(v.text())
            };
            let arrow = |id: String, dir: Dir, icon: IconName, tip: &str, k: (String, Vec<String>, String)| {
                Button::new(SharedString::from(id))
                    .xsmall()
                    .map(|b| if pick == Some(dir) { b.primary() } else { b.ghost() })
                    .icon(icon)
                    .tooltip(tip.to_string())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.picks.get(&k) == Some(&dir) {
                            this.picks.remove(&k);
                        } else {
                            this.picks.insert(k.clone(), dir);
                        }
                        cx.notify();
                    }))
            };
            let id = format!("cmp:{rel}:{}:{}", p.entry.path.join("."), p.entry.key);
            let server_dims = side == Side::Server;
            let row = h_flex()
                .gap_3()
                .px_3()
                .py_1p5()
                .rounded(theme.radius)
                .when(p.differs(), |d| d.bg(theme.primary.opacity(0.05)))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_semibold().truncate().child(p.entry.label()))
                        .child(div().text_xs().font_family("Consolas").text_color(theme.muted_foreground).truncate().child(p.entry.key.clone()))
                        .when(!p.entry.comment.is_empty(), |d| d.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(p.entry.comment.clone()))),
                )
                .child(cell(&p.client, pick == Some(Dir::ToServer), server_dims))
                .child(
                    h_flex()
                        .w(rems(4.))
                        .flex_none()
                        .gap_0p5()
                        .justify_center()
                        .when(p.differs(), |d| {
                            d.child(arrow(format!("{id}:s"), Dir::ToServer, IconName::ArrowRight, "Copy this PC's value to the server", k.clone()))
                                .child(arrow(format!("{id}:c"), Dir::ToPc, IconName::ArrowLeft, "Copy the server's value to this PC", k.clone()))
                        }),
                )
                .child(cell(&p.server, pick == Some(Dir::ToPc), false));
            col = col.child(row);
        }
        col.into_any_element()
    }
}

impl Render for CompareView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let files = self.only.clone().unwrap_or_default();
        let differ: usize = files.iter().filter_map(|r| self.files.get(r)).filter_map(|c| c.pairs.as_ref().ok()).map(|p| p.iter().filter(|p| p.differs()).count()).sum();
        let total: usize = files.iter().filter_map(|r| self.files.get(r)).filter_map(|c| c.pairs.as_ref().ok()).map(Vec::len).sum();
        let running = self.store.read(cx).running(&self.root).is_some();
        let _ = running;
        let head = h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .when(!self.embedded, |d| d.child(widgets::chip(theme.success).child(format!("{} · server", self.server.name))))
            .child(div().flex_1())
            .child(widgets::toggle_chip("cmp-differs", format!("Differs {differ}"), !self.all, cx).on_click(cx.listener(|this, _, _, cx| {
                this.all = false;
                cx.notify();
            })))
            .child(widgets::toggle_chip("cmp-all", format!("All {total}"), self.all, cx).on_click(cx.listener(|this, _, _, cx| {
                this.all = true;
                cx.notify();
            })));
        let cols = h_flex()
            .gap_3()
            .px_3()
            .py_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().child("Setting"))
            .child(h_flex().w(rems(10.)).gap_1().child(Icon::new(IconName::Monitor).xsmall()).child("This PC"))
            .child(div().w(rems(4.)))
            .child(h_flex().w(rems(10.)).gap_1().child(Icon::new(IconName::Server).xsmall()).child("Server"));
        let mut body = v_flex().gap_1().pb_4();
        // Server-read files first: they're the ones that matter.
        let mut order = files.clone();
        order.sort_by_key(|r| match hopper::configindex::side_of(r) {
            Side::Common => 0,
            Side::Server => 1,
            Side::Unknown | Side::Startup => 2,
            Side::Client => 3,
        });
        for r in &order {
            body = body.child(self.file_block(r, cx));
        }
        if files.is_empty() {
            body = body.child(widgets::empty_state(IconName::GitCompare, "Pick a mod to compare", None, cx));
        }
        let (to_s, to_c) = (self.picks.values().filter(|d| **d == Dir::ToServer).count(), self.picks.values().filter(|d| **d == Dir::ToPc).count());
        let n = self.picks.len();
        let bar = h_flex()
            .gap_2()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .child(div().text_sm().child(if n == 0 { "Pick → or ← on a setting".to_string() } else { format!("{n} picked:") }))
            .when(to_s > 0, |d| d.child(widgets::chip(theme.muted_foreground).child(format!("{to_s} → server"))))
            .when(to_c > 0, |d| d.child(widgets::chip(theme.muted_foreground).child(format!("{to_c} → this PC"))))
            .child(widgets::caption("Only the picked settings change. Old copies go to Backups on both sides.", cx).text_xs())
            .child(div().flex_1())
            .child(Button::new("cmp-clear").ghost().small().label("Clear").disabled(n == 0).on_click(cx.listener(|this, _, _, cx| {
                this.picks.clear();
                cx.notify();
            })))
            .child(
                Button::new("cmp-apply")
                    .primary()
                    .small()
                    .label(if self.applying { "Applying…".to_string() } else { format!("Apply {n}") })
                    .disabled(n == 0 || self.applying)
                    .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
            );
        v_flex()
            .size_full()
            .child(head)
            .child(cols)
            .child(div().id("cmp-body").flex_1().min_h_0().overflow_y_scroll().child(body).when(self.loading, |d| d.child(div().px_3().child(widgets::caption("Reading the server's copies…", cx)))))
            .child(bar)
    }
}

