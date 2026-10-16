//! Server sync's Configs and History sections: review a folder file by file
//! and push or pull what you pick, set how a folder syncs (mirror or live
//! link), and undo any sync from the change log.

use crate::store::{Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::configdiff::{self, ConfigDiff, FileStatus, Line, LineKind, MergedField};
use hopper::folderrules::{self as fr, Mode, Rule};
use hopper::instances::Instance;
use hopper::model::SyncOp;
use hopper::server::ServerInfo;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const MODES: [Mode; 4] = [Mode::Review, Mode::MirrorExcept, Mode::MirrorOnly, Mode::Link];

/// A preview line's height, in rem.
const LINE_H: f32 = 1.25;

fn kb(n: u64) -> String {
    format!("{:.1} kB", n as f64 / 1024.0)
}

/// A file opened under its row: the side-by-side diff and its settings.
struct Preview {
    rel: String,
    rows: Result<Vec<Line>, String>,
    fields: Vec<MergedField>,
}

fn load_preview(rel: &str, status: FileStatus, c_dir: &Path, s_dir: &Path) -> Preview {
    let client = (status != FileStatus::OnlyServer).then(|| configdiff::read_text(&c_dir.join(rel)));
    let server = (status != FileStatus::OnlyClient).then(|| configdiff::read_text(&s_dir.join(rel)));
    let (ct, st) = match (client, server) {
        (Some(Err(e)), _) | (_, Some(Err(e))) => {
            return Preview {
                rel: rel.to_string(),
                rows: Err(e),
                fields: Vec::new(),
            };
        }
        (c, s) => (
            c.and_then(Result::ok).unwrap_or_default(),
            s.and_then(Result::ok).unwrap_or_default(),
        ),
    };
    Preview {
        rel: rel.to_string(),
        rows: Ok(configdiff::line_diff(&st, &ct)),
        fields: configdiff::merge_fields(&ct, &st),
    }
}

/// A row of the file list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A status's heading, with how many files it has.
    Head(FileStatus, usize),
    /// A file, by its index in the comparison.
    File(usize),
    /// The open file's preview.
    Preview,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Push,
    Pull,
    Delete,
}

pub struct ConfigsPanel {
    store: Entity<Store>,
    server: Option<PathBuf>,
    folder: String,
    /// Config folders found on either side (None while they're looked
    /// for). Read once per server: the server sits on a network share,
    /// too slow to ask on every frame.
    present: Option<Vec<String>>,
    /// Whether the server was up as of the last store change.
    was_online: bool,
    diff: Option<Arc<ConfigDiff>>,
    running: bool,
    progress: Arc<(AtomicUsize, AtomicUsize)>,
    selected: HashSet<String>,
    filter: Entity<InputState>,
    query: String,
    new_folder: Entity<InputState>,
    adding: bool,
    preview: Option<Preview>,
    preview_scroll: UniformListScrollHandle,
    values: bool,
    show_all: bool,
    mirror: Option<fr::MirrorPlan>,
    message: Option<Result<String, String>>,
    /// The file list's rows and its scroll state, and the comparison they
    /// were built from.
    items: Vec<Item>,
    list: ListState,
    items_for: Option<Arc<ConfigDiff>>,
    _ticker: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ConfigsPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
        let new_folder = cx.new(|cx| InputState::new(window, cx).placeholder("Folder, like world or scripts"));
        let subs = vec![
            // Only the online warning depends on the store's frequent
            // changes: redraw when it flips, not on every tick.
            cx.observe(&store, |this, _, cx| {
                let on = this.online(cx);
                if on != this.was_online {
                    this.was_online = on;
                    cx.notify();
                }
            }),
            cx.subscribe(&filter, |this, state, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
            cx.subscribe(&new_folder, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.add_folder(cx);
                }
            }),
        ];
        Self {
            store,
            server: None,
            folder: String::new(),
            present: None,
            was_online: false,
            diff: None,
            running: false,
            progress: Arc::new((AtomicUsize::new(0), AtomicUsize::new(0))),
            selected: HashSet::new(),
            filter,
            query: String::new(),
            new_folder,
            adding: false,
            preview: None,
            preview_scroll: UniformListScrollHandle::new(),
            values: false,
            show_all: false,
            mirror: None,
            message: None,
            items: Vec::new(),
            list: ListState::new(0, ListAlignment::Top, px(400.)),
            items_for: None,
            _ticker: None,
            _subscriptions: subs,
        }
    }

    fn pair(&self, cx: &App) -> Option<(ServerInfo, Instance)> {
        let store = self.store.read(cx);
        let s = store.server(self.server.as_ref()?)?.clone();
        // Only the roots are used: skip copying the mod list.
        let inst = store.paired(&s)?.clone_header();
        Some((s, inst))
    }

    fn key(&self) -> String {
        self.server
            .as_ref()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Config folders on either side, plus folders added by hand.
    fn folders(&self, cx: &App) -> Vec<String> {
        let mut folders = self.present.clone().unwrap_or_default();
        for r in self
            .store
            .read(cx)
            .settings()
            .folder_rules
            .get(&self.key())
            .into_iter()
            .flatten()
        {
            if !folders.contains(&r.folder) {
                folders.push(r.folder.clone());
            }
        }
        folders
    }

    pub fn show(&mut self, server: PathBuf, cx: &mut Context<Self>) {
        if self.server.as_ref() == Some(&server) && self.diff.is_some() {
            return;
        }
        let fresh = self.server.as_ref() != Some(&server);
        self.server = Some(server.clone());
        self.was_online = self.online(cx);
        if fresh {
            self.present = None;
            self.diff = None;
        }
        let Some((s, inst)) = self.pair(cx) else {
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let present = cx
                .background_spawn(async move { configdiff::present(&inst.root, &s.root) })
                .await;
            this.update(cx, |this, cx| {
                if this.server.as_ref() != Some(&server) {
                    return;
                }
                this.present = Some(present);
                let folders = this.folders(cx);
                if !folders.contains(&this.folder) {
                    this.folder = folders.first().cloned().unwrap_or_default();
                }
                this.reset(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        self.diff = None;
        self.selected.clear();
        self.message = None;
        self.mirror = None;
        self.preview = None;
        if !self.folder.is_empty() {
            self.compare(false, cx);
        }
        cx.notify();
    }

    fn compare(&mut self, deep: bool, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        self.running = true;
        self.preview = None;
        self.progress.0.store(0, Ordering::Relaxed);
        self.progress.1.store(0, Ordering::Relaxed);
        let (client, server, folder) = (inst.root.clone(), s.root.clone(), self.folder.clone());
        let progress = self.progress.clone();
        // Redraw while the content check counts up.
        self._ticker = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        }));
        cx.spawn(async move |this, cx| {
            let f = folder.clone();
            let r = cx
                .background_spawn(async move {
                    configdiff::compare(&client, &server, &f, deep, &|d, t| {
                        progress.0.store(d, Ordering::Relaxed);
                        progress.1.store(t, Ordering::Relaxed);
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                if this.folder == folder {
                    this.running = false;
                    this._ticker = None;
                    this.diff = Some(Arc::new(r));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn rule(&self, cx: &App) -> Rule {
        self.store
            .read(cx)
            .settings()
            .folder_rules
            .get(&self.key())
            .and_then(|v| v.iter().find(|r| r.folder == self.folder))
            .cloned()
            .unwrap_or_else(|| Rule::new(&self.folder))
    }

    fn set_rule(&mut self, rule: Rule, cx: &mut Context<Self>) {
        let key = self.key();
        self.mirror = None;
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    let rules = st.folder_rules.entry(key).or_default();
                    match rules.iter_mut().find(|r| r.folder == rule.folder) {
                        Some(r) => *r = rule,
                        None => rules.push(rule),
                    }
                },
                cx,
            )
        });
        cx.notify();
    }

    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let name = self
            .new_folder
            .read(cx)
            .value()
            .trim()
            .trim_matches(['/', '\\'])
            .to_string();
        if !configdiff::safe_rel(&name) || name.is_empty() {
            return;
        }
        self.adding = false;
        self.folder = name.clone();
        let rule = self.rule(cx);
        self.set_rule(rule, cx);
        self.reset(cx);
    }

    fn run(&mut self, act: Act, files: Vec<String>, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let folder = self.folder.clone();
        let (c_dir, s_dir) = (inst.root.join(&folder), s.root.join(&folder));
        let ops: Vec<SyncOp> = files
            .iter()
            .filter(|r| configdiff::safe_rel(r))
            .map(|r| match act {
                Act::Push => SyncOp::Copy {
                    src: c_dir.join(r),
                    dst: s_dir.join(r),
                },
                Act::Pull => SyncOp::Copy {
                    src: s_dir.join(r),
                    dst: c_dir.join(r),
                },
                Act::Delete => SyncOp::Delete {
                    path: s_dir.join(r),
                },
            })
            .collect();
        let (server, client) = (s.root.clone(), inst.root.clone());
        let deep = self.diff.as_ref().is_some_and(|d| d.deep);
        self.running = true;
        self.selected.clear();
        cx.spawn(async move |this, cx| {
            let message = cx
                .background_spawn(async move {
                    let (side, root) = if act == Act::Pull {
                        ("client", client.clone())
                    } else {
                        ("server", server.clone())
                    };
                    let results = hopper::sync::execute(ops, &root);
                    hopper::history::record(&server, side, &root, &results);
                    configdiff::forget_walk(&c_dir);
                    configdiff::forget_walk(&s_dir);
                    let failed: Vec<String> = results
                        .iter()
                        .filter(|r| !r.ok)
                        .map(|r| r.error.clone().unwrap_or_default())
                        .collect();
                    let verb = match act {
                        Act::Push => "Pushed",
                        Act::Pull => "Pulled",
                        Act::Delete => "Deleted",
                    };
                    if failed.is_empty() {
                        Ok(format!("{verb} {} files. Undo from History.", results.len()))
                    } else {
                        Err(format!("{} failed: {}", failed.len(), failed[0]))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.message = Some(message);
                this.compare(deep, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn confirm(&mut self, act: Act, files: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        let n = files.len();
        let what = match act {
            Act::Push => format!("Overwrite {n} server files with the client's?"),
            Act::Pull => format!("Overwrite {n} client files with the server's?"),
            Act::Delete => format!("Delete {n} files from the server?"),
        };
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, files) = (view.clone(), files.clone());
            alert
                .title(what.clone())
                .description("Every replaced file is backed up; undo from History.")
                .confirm()
                .ok_text(match act {
                    Act::Push => "Push",
                    Act::Pull => "Pull",
                    Act::Delete => "Delete",
                })
                .ok_variant(if act == Act::Delete {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Primary
                })
                .on_ok(move |_, _, cx| {
                    view.update(cx, |this, cx| this.run(act, files.clone(), cx));
                    true
                })
        });
    }

    fn mirror_plan(&mut self, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let rule = self.rule(cx);
        self.running = true;
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_spawn(async move { fr::plan_mirror(&rule, &inst.root, &s.root) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.mirror = Some(plan);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn mirror_now(&mut self, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let Some(plan) = self.mirror.take() else { return };
        let rule = self.rule(cx);
        self.running = true;
        cx.spawn(async move |this, cx| {
            let message = cx
                .background_spawn(async move {
                    let (side, root) = if rule.pull {
                        ("client", inst.root.clone())
                    } else {
                        ("server", s.root.clone())
                    };
                    let results = hopper::sync::execute(plan.ops, &root);
                    hopper::history::record(&s.root, side, &root, &results);
                    let failed: Vec<String> = results
                        .iter()
                        .filter(|r| !r.ok)
                        .map(|r| r.error.clone().unwrap_or_default())
                        .collect();
                    if failed.is_empty() {
                        Ok(format!("Mirrored {} changes. Undo from History.", results.len()))
                    } else {
                        Err(format!("{} failed: {}", failed.len(), failed[0]))
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.message = Some(message);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn link(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((s, inst)) = self.pair(cx) else { return };
        let mut rule = self.rule(cx);
        let (from, to) = rule.ends(&inst.root, &s.root);
        let r = if on {
            let backup = if rule.pull { inst.root.clone() } else { s.root.clone() };
            fr::link(&from, &to, &backup).map(|d| rule.displaced = d)
        } else {
            fr::unlink(&to, rule.displaced.as_deref()).map(|()| rule.displaced = None)
        };
        match r {
            Ok(()) => self.set_rule(rule, cx),
            Err(e) => window.push_notification(
                gpui_kit::component::notification::Notification::error(format!("{e:#}")),
                cx,
            ),
        }
    }

    fn online(&self, cx: &App) -> bool {
        self.server
            .as_ref()
            .and_then(|r| self.store.read(cx).status(r))
            .is_some_and(|s| matches!(s, hopper::server::Status::Online { .. }))
    }

    // ---- rendering -----------------------------------------------------

    fn rule_row(&self, cx: &mut Context<Self>) -> (AnyElement, bool) {
        let rule = self.rule(cx);
        let theme = cx.theme();
        let rem = theme.font_size;
        let view = cx.entity();
        let r2 = rule.clone();
        let pick_mode = std::rc::Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
            let mut r = r2.clone();
            r.mode = MODES[ix];
            view.update(cx, |this, cx| this.set_rule(r, cx));
        });
        let help = match rule.mode {
            Mode::Review => "Nothing syncs by itself: compare file by file below and push or pull what you pick.",
            Mode::MirrorExcept => "Mirror makes the destination match the source, except files matching the patterns, which stay as they are. Everything replaced or removed is backed up (undo from History).",
            Mode::MirrorOnly => "Mirror copies only files matching the patterns and removes everything else at the destination. Everything replaced or removed is backed up (undo from History).",
            Mode::Link => "The destination folder becomes an NTFS junction to the source: always identical, no second copy. What was there is moved to backups and put back on unlink.",
        };
        let mut hide = false;
        let extra: Option<AnyElement> = match rule.mode {
            Mode::Review => None,
            Mode::MirrorExcept | Mode::MirrorOnly => {
                hide = true;
                let plan = self.mirror.clone();
                Some(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("mirror-preview")
                                        .small()
                                        .outline()
                                        .label("Preview mirror")
                                        .disabled(self.running)
                                        .on_click(cx.listener(|this, _, _, cx| this.mirror_plan(cx))),
                                )
                                .when_some(plan.as_ref(), |r, p| {
                                    r.child(widgets::caption(
                                        format!("{} to copy, {} to delete", p.copy.len(), p.delete.len()),
                                        cx,
                                    ))
                                    .child(
                                        Button::new("mirror-now")
                                            .small()
                                            .primary()
                                            .label("Mirror now")
                                            .disabled(self.running || p.ops.is_empty())
                                            .on_click(cx.listener(|this, _, _, cx| this.mirror_now(cx))),
                                    )
                                })
                                .when(self.running, |r| r.child(Spinner::new().small())),
                        )
                        .when_some(plan, |c, p| {
                            c.child(
                                v_flex()
                                    .id("mirror-plan")
                                    // A set height once long: a max height alone doesn't scroll.
                                    .when(p.copy.len() + p.delete.len() > 16, |l| l.h(rems(20.)))
                                    .p_3()
                                    .rounded(theme.radius)
                                    .bg(theme.muted)
                                    .font_family(theme.mono_font_family.clone())
                                    .text_xs()
                                    .children(p.copy.iter().map(|f| div().child(format!("copy    {f}"))))
                                    .children(p.delete.iter().map(|f| {
                                        div().text_color(theme.danger).child(format!("delete  {f}"))
                                    }))
                                    .overflow_y_scrollbar(),
                            )
                        })
                        .into_any_element(),
                )
            }
            Mode::Link => {
                hide = true;
                let pair = self.pair(cx);
                let online = self.online(cx);
                Some(match pair {
                    None => div().into_any_element(),
                    Some((s, inst)) => {
                        let (from, to) = rule.ends(&inst.root, &s.root);
                        if fr::on_network(&inst.root) || fr::on_network(&s.root) {
                            div()
                                .text_sm()
                                .text_color(theme.warning)
                                .child("Not possible here: the server is on a network drive, and a junction can only point within one PC. Use a mirror mode instead.")
                                .into_any_element()
                        } else if fr::is_link(&to) {
                            h_flex()
                                .gap_2()
                                .child(div().text_sm().text_color(theme.success).child(format!(
                                    "Live-linked: {} points at {}",
                                    to.display(),
                                    from.display()
                                )))
                                .child(
                                    Button::new("unlink")
                                        .small()
                                        .outline()
                                        .label("Unlink")
                                        .disabled(online)
                                        .on_click(cx.listener(|this, _, w, cx| this.link(false, w, cx))),
                                )
                                .into_any_element()
                        } else {
                            Button::new("link")
                                .small()
                                .primary()
                                .label("Link")
                                .disabled(online)
                                .on_click(cx.listener(|this, _, w, cx| this.link(true, w, cx)))
                                .into_any_element()
                        }
                    }
                })
            }
        };
        let r_dir = rule.clone();
        let patterns = rule.patterns.clone();
        let el = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(div().text_sm().font_semibold().child(format!("{}/ syncs by", rule.folder)))
                    .child(div().w_56().child(widgets::picker(
                        "rule-mode",
                        rule.mode.label().to_string(),
                        MODES.iter().map(|m| (m.label().to_string(), *m == rule.mode)).collect(),
                        rem,
                        pick_mode,
                    )))
                    .child(
                        TabBar::new("rule-dir")
                            .segmented()
                            .small()
                            .selected_index(usize::from(rule.pull))
                            .child(Tab::new().label("Client → server"))
                            .child(Tab::new().label("Server → client"))
                            .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                let mut r = r_dir.clone();
                                r.pull = *ix == 1;
                                this.set_rule(r, cx);
                            })),
                    )
                    .when(matches!(rule.mode, Mode::MirrorExcept | Mode::MirrorOnly), |r| {
                        r.child(widgets::caption(
                            if patterns.is_empty() {
                                "Patterns: none".to_string()
                            } else {
                                format!("Patterns: {patterns}")
                            },
                            cx,
                        ))
                    }),
            )
            .child(widgets::caption(help, cx).text_xs())
            .children(extra)
            .into_any_element();
        (el, hide)
    }

    fn preview_el(&self, p: &Preview, c_dir: &Path, s_dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let rows = match &p.rows {
            Ok(r) => r,
            Err(e) => {
                return div().text_sm().text_color(theme.warning).child(e.clone()).into_any_element();
            }
        };
        if self.values {
            let changed = p.fields.iter().filter(|f| f.changed()).count();
            let shown: Vec<&MergedField> =
                p.fields.iter().filter(|f| self.show_all || f.changed()).collect();
            return v_flex()
                .gap_1()
                .p_3()
                .rounded(theme.radius)
                .bg(theme.muted)
                .child(
                    h_flex()
                        .gap_2()
                        .child(widgets::caption(
                            format!("{changed} changed of {} settings", p.fields.len()),
                            cx,
                        ))
                        .child(
                            Button::new("show-all")
                                .xsmall()
                                .ghost()
                                .label(if self.show_all { "Changed only" } else { "Show all" })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_all = !this.show_all;
                                    cx.notify();
                                })),
                        ),
                )
                .when(p.fields.is_empty(), |c| {
                    c.child(widgets::caption("No key = value settings in this file; use the Diff view.", cx))
                })
                .children(shown.into_iter().take(400).map(|f| {
                    let key = if f.section.is_empty() {
                        f.key.clone()
                    } else {
                        format!("[{}] {}", f.section, f.key)
                    };
                    let value: AnyElement = match (&f.client, &f.server) {
                        (Some(c), Some(s)) if c != s => h_flex()
                            .gap_2()
                            .child(div().text_color(theme.success).child(c.clone()))
                            .child(widgets::caption("was", cx).text_xs())
                            .child(div().text_color(theme.danger).line_through().child(s.clone()))
                            .into_any_element(),
                        (Some(c), None) => h_flex()
                            .gap_2()
                            .child(div().text_color(theme.success).child(c.clone()))
                            .child(widgets::caption("new on client", cx).text_xs())
                            .into_any_element(),
                        (None, Some(s)) => h_flex()
                            .gap_2()
                            .child(div().text_color(theme.muted_foreground).child(s.clone()))
                            .child(widgets::caption("only on server", cx).text_xs())
                            .into_any_element(),
                        (Some(c), _) => div().text_color(theme.muted_foreground).child(c.clone()).into_any_element(),
                        (None, None) => div().into_any_element(),
                    };
                    h_flex()
                        .gap_3()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .child(div().w_64().flex_none().truncate().child(key))
                        .child(value)
                }))
                .into_any_element();
        }
        let (a, d, ch) = configdiff::stats(rows);
        let (c_file, s_file) = (c_dir.join(&p.rel), s_dir.join(&p.rel));
        let n = rows.len();
        v_flex()
            .gap_1()
            .p_3()
            .rounded(theme.radius)
            .bg(theme.muted)
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().text_sm().font_semibold().child("Client"))
                    .child(div().flex_1().text_sm().font_semibold().child("Server"))
                    .child(widgets::caption(format!("+{a}  -{d}  ~{ch}"), cx).text_xs())
                    .child(
                        Button::new("open-client-file")
                            .xsmall()
                            .ghost()
                            .label("Open client")
                            .on_click(move |_, _, _| open_path(&c_file)),
                    )
                    .child(
                        Button::new("open-server-file")
                            .xsmall()
                            .ghost()
                            .label("Open server")
                            .on_click(move |_, _, _| open_path(&s_file)),
                    ),
            )
            .child(
                // Only the lines in view are built: a big file has
                // thousands, too many to lay out on every frame.
                div()
                    .relative()
                    .h(rems(LINE_H * n.min(20) as f32))
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .child(
                        uniform_list(
                            "cfg-preview",
                            n,
                            cx.processor(|this, range: Range<usize>, _, cx| this.preview_lines(range, cx)),
                        )
                        .track_scroll(&self.preview_scroll)
                        .size_full(),
                    )
                    .vertical_scrollbar(&self.preview_scroll),
            )
            .into_any_element()
    }

    /// Lines `range` of the open file's side-by-side diff.
    fn preview_lines(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(Ok(rows)) = self.preview.as_ref().map(|p| &p.rows) else {
            return Vec::new();
        };
        let theme = cx.theme();
        let side = |no: Option<usize>, text: &Option<String>, fill: Option<Hsla>| {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .when_some(fill, |r, f| r.bg(f.opacity(0.18)))
                .child(
                    div()
                        .w_10()
                        .flex_none()
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child(no.map(|n| (n + 1).to_string()).unwrap_or_default()),
                )
                .child(
                    div()
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .child(text.clone().unwrap_or_default().replace('\t', "    ")),
                )
        };
        rows[range.start.min(rows.len())..range.end.min(rows.len())]
            .iter()
            .map(|r| {
                let (lc, rc) = match r.kind {
                    LineKind::Same => (None, None),
                    LineKind::Add => (Some(theme.success), None),
                    LineKind::Del => (None, Some(theme.danger)),
                    LineKind::Change => (Some(theme.warning), Some(theme.warning)),
                };
                h_flex()
                    .w_full()
                    .h(rems(LINE_H))
                    .gap_2()
                    .child(side(r.client_no, &r.client, lc))
                    .child(side(r.server_no, &r.server, rc))
                    .into_any_element()
            })
            .collect()
    }
}

impl Render for ConfigsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(_) = self.pair(cx) else {
            return div().into_any_element();
        };
        let folders = self.folders(cx);
        let online = self.online(cx);
        let folder_ix = folders.iter().position(|f| *f == self.folder).unwrap_or(0);
        let fl = folders.clone();
        let header = h_flex()
            .gap_2()
            .flex_wrap()
            .when(!folders.is_empty(), |r| {
                r.child(
                    TabBar::new("cfg-folders")
                        .segmented()
                        .small()
                        .selected_index(folder_ix)
                        .children(folders.iter().map(|f| Tab::new().label(format!("{f}/"))))
                        .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                            if let Some(f) = fl.get(*ix) {
                                this.folder = f.clone();
                                this.reset(cx);
                            }
                        })),
                )
            })
            .map(|r| {
                if self.adding {
                    r.child(div().w_56().child(Input::new(&self.new_folder).small()))
                        .child(
                            Button::new("folder-add")
                                .small()
                                .outline()
                                .label("Add")
                                .on_click(cx.listener(|this, _, _, cx| this.add_folder(cx))),
                        )
                        .child(
                            Button::new("folder-cancel")
                                .small()
                                .ghost()
                                .label("Cancel")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.adding = false;
                                    cx.notify();
                                })),
                        )
                } else {
                    r.child(
                        Button::new("folder-new")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .label("Folder")
                            .tooltip("Sync another folder, like world, resourcepacks or scripts")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.adding = true;
                                cx.notify();
                            })),
                    )
                }
            })
            .child(div().flex_1())
            .child(
                TabBar::new("cfg-view")
                    .segmented()
                    .small()
                    .selected_index(usize::from(self.values))
                    .child(Tab::new().label("Diff"))
                    .child(Tab::new().label("Values"))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.values = *ix == 1;
                        cx.notify();
                    })),
            );
        if folders.is_empty() {
            return v_flex()
                .gap_3()
                .child(header)
                .child(if self.present.is_none() {
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child(widgets::caption("Looking for config folders", cx))
                        .into_any_element()
                } else {
                    widgets::caption("Neither side has a config folder yet.", cx).into_any_element()
                })
                .into_any_element();
        }
        let (rule_row, hide) = self.rule_row(cx);
        let theme = cx.theme().clone();
        let message: Option<AnyElement> = self.message.as_ref().map(|m| match m {
            Ok(t) => div().text_sm().text_color(theme.success).child(t.clone()).into_any_element(),
            Err(t) => div().text_sm().text_color(theme.danger).child(t.clone()).into_any_element(),
        });
        let mut body = v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(header)
            .when(online, |c| {
                c.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child("Server is online: most mods only read their config at startup, so restart it after pushing."),
                )
            })
            .child(rule_row)
            .children(message);
        if hide {
            return body.into_any_element();
        }
        if self.running {
            let (d, t) = (
                self.progress.0.load(Ordering::Relaxed),
                self.progress.1.load(Ordering::Relaxed),
            );
            body = body.child(
                h_flex()
                    .gap_2()
                    .child(Spinner::new().small())
                    .child(widgets::caption(
                        if t > 0 {
                            format!("Checking contents {d}/{t}")
                        } else {
                            "Scanning folders (the server disk can be slow)".into()
                        },
                        cx,
                    )),
            );
        }
        let Some(diff) = self.diff.clone() else {
            return body.into_any_element();
        };
        let status_of = |rel: &str| diff.files.iter().find(|f| f.rel == rel).map(|f| f.status);
        let sel = |ok: &dyn Fn(FileStatus) -> bool| -> Vec<String> {
            self.selected
                .iter()
                .filter(|r| status_of(r).is_some_and(|s| ok(s)))
                .cloned()
                .collect()
        };
        let push = sel(&|s| s != FileStatus::OnlyServer);
        let pull = sel(&|s| s != FileStatus::OnlyClient);
        let delete = sel(&|s| s == FileStatus::OnlyServer);
        let modified: Vec<String> = diff
            .files
            .iter()
            .filter(|f| f.status == FileStatus::Modified)
            .map(|f| f.rel.clone())
            .collect();
        let (np, nl, nd, nm) = (push.len(), pull.len(), delete.len(), modified.len());
        let deep = diff.deep;
        body = body
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(widgets::caption(
                        format!("{} client / {} server files compared", diff.client_files, diff.server_files),
                        cx,
                    ))
                    .child(div().w_48().child(Input::new(&self.filter).small().cleanable(true)))
                    .map(|r| {
                        if deep {
                            r.child(widgets::caption(format!("{} identical (contents checked)", diff.same), cx))
                        } else if diff.same > 0 {
                            r.child(
                                Button::new("deep")
                                    .small()
                                    .ghost()
                                    .label(format!("Deep compare ({})", diff.same))
                                    .tooltip("Also compare the contents of files whose size matches. Slower: reads each one off the server disk. Cached afterwards.")
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _, cx| this.compare(true, cx))),
                            )
                        } else {
                            r
                        }
                    })
                    .child(
                        Button::new("cfg-rescan")
                            .small()
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .tooltip("Rescan")
                            .disabled(self.running)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some((s, inst)) = this.pair(cx) {
                                    configdiff::forget_walk(&inst.root.join(&this.folder));
                                    configdiff::forget_walk(&s.root.join(&this.folder));
                                }
                                this.compare(deep, cx)
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Button::new("sel-changed-cfg")
                            .xsmall()
                            .ghost()
                            .label(format!("Select changed ({nm})"))
                            .disabled(nm == 0)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected.extend(modified.iter().cloned());
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("clear-cfg")
                            .xsmall()
                            .ghost()
                            .label("Clear")
                            .disabled(self.selected.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected.clear();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("cfg-delete")
                            .small()
                            .outline()
                            .with_variant(ButtonVariant::Danger)
                            .label(format!("Delete server-only ({nd})"))
                            .disabled(nd == 0 || self.running)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.confirm(Act::Delete, delete.clone(), w, cx)
                            })),
                    )
                    .child(
                        Button::new("cfg-pull")
                            .small()
                            .outline()
                            .label(format!("Pull to client ({nl})"))
                            .tooltip("Copy the server's file over the client's. The client's copy is backed up first.")
                            .disabled(nl == 0 || self.running)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.confirm(Act::Pull, pull.clone(), w, cx)
                            })),
                    )
                    .child(
                        Button::new("cfg-push")
                            .small()
                            .primary()
                            .label(format!("Push to server ({np})"))
                            .tooltip("Copy the client's file over the server's. The server's copy is backed up first.")
                            .disabled(np == 0 || self.running)
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.confirm(Act::Push, push.clone(), w, cx)
                            })),
                    ),
            );
        if diff.files.is_empty() {
            body = body.child(widgets::caption(
                if deep {
                    "Client and server match for this folder.".to_string()
                } else {
                    format!("No differences by size. {} files match by size; Deep compare checks their contents.", diff.same)
                },
                cx,
            ));
        }
        // Only the rows in view are built: a folder can differ in hundreds
        // of files, too many to lay out on every scroll frame.
        let items = self.build_items(&diff);
        let fresh = !self.items_for.as_ref().is_some_and(|d| Arc::ptr_eq(d, &diff));
        if fresh {
            self.list.reset(items.len());
            self.items_for = Some(diff.clone());
        } else if items != self.items {
            // Splice just what changed, so the list keeps its scroll spot.
            let old = &self.items;
            let pre = old.iter().zip(&items).take_while(|(a, b)| a == b).count();
            let max_suf = old.len().min(items.len()) - pre;
            let suf = old
                .iter()
                .rev()
                .zip(items.iter().rev())
                .take(max_suf)
                .take_while(|(a, b)| a == b)
                .count();
            self.list.splice(pre..old.len() - suf, items.len() - pre - suf);
        }
        self.items = items;
        body.child(
            div()
                .flex_1()
                .min_h_0()
                .relative()
                .child(
                    list(
                        self.list.clone(),
                        cx.processor(|this, ix: usize, _, cx| this.render_item(ix, cx)),
                    )
                    .size_full(),
                )
                .vertical_scrollbar(&self.list),
        )
        .into_any_element()
    }
}

impl ConfigsPanel {
    /// The file list's rows: per status a heading, then its files, with
    /// the open file's preview under its row.
    fn build_items(&self, diff: &ConfigDiff) -> Vec<Item> {
        let q = &self.query;
        let mut items = Vec::new();
        for status in [FileStatus::Modified, FileStatus::OnlyClient, FileStatus::OnlyServer] {
            let files: Vec<usize> = diff
                .files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.status == status && (q.is_empty() || f.rel.to_lowercase().contains(q)))
                .map(|(i, _)| i)
                .collect();
            if files.is_empty() {
                continue;
            }
            items.push(Item::Head(status, files.len()));
            for i in files {
                items.push(Item::File(i));
                if self.preview.as_ref().is_some_and(|p| p.rel == diff.files[i].rel) {
                    items.push(Item::Preview);
                }
            }
        }
        items
    }

    fn render_item(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let (Some(item), Some(diff)) = (self.items.get(ix).cloned(), self.diff.clone()) else {
            return div().into_any_element();
        };
        let Some((s, inst)) = self.pair(cx) else {
            return div().into_any_element();
        };
        let (c_dir, s_dir) = (inst.root.join(&diff.folder), s.root.join(&diff.folder));
        let theme = cx.theme().clone();
        match item {
            Item::Head(status, n) => {
                let (tag, label) = match status {
                    FileStatus::Modified => (Tag::info().xsmall().child("Changed"), "changed"),
                    FileStatus::OnlyClient => (Tag::warning().xsmall().child("Client only"), "only on the client"),
                    FileStatus::OnlyServer => (Tag::primary().xsmall().child("Server only"), "only on the server"),
                };
                let q = self.query.clone();
                h_flex()
                    .gap_2()
                    .pt_3()
                    .pb_1()
                    .child(tag)
                    .child(div().text_sm().font_semibold().child(format!("{n} {label}")))
                    .when(n > 1, |r| {
                        r.child(
                            Button::new(SharedString::from(format!("selall:{label}")))
                                .xsmall()
                                .ghost()
                                .label("Select all")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(d) = &this.diff {
                                        this.selected.extend(
                                            d.files
                                                .iter()
                                                .filter(|f| {
                                                    f.status == status
                                                        && (q.is_empty() || f.rel.to_lowercase().contains(&q))
                                                })
                                                .map(|f| f.rel.clone()),
                                        );
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .into_any_element()
            }
            Item::Preview => match &self.preview {
                Some(p) => div().w_full().pb_2().child(self.preview_el(p, &c_dir, &s_dir, cx)).into_any_element(),
                None => div().into_any_element(),
            },
            Item::File(i) => {
                let Some(f) = diff.files.get(i).cloned() else {
                    return div().into_any_element();
                };
                let picked = self.selected.contains(&f.rel);
                let open = self.preview.as_ref().is_some_and(|p| p.rel == f.rel);
                let (r1, r2, st) = (f.rel.clone(), f.rel.clone(), f.status);
                let size = match f.status {
                    FileStatus::Modified => format!("{} → {}", kb(f.server_size), kb(f.client_size)),
                    FileStatus::OnlyClient => kb(f.client_size),
                    FileStatus::OnlyServer => kb(f.server_size),
                };
                h_flex()
                    .id(SharedString::from(format!("cfgrow:{}", f.rel)))
                    .w_full()
                    .gap_3()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .hover(|s| s.bg(theme.list_hover))
                    .child(
                        Checkbox::new(SharedString::from(format!("cfgsel:{}", f.rel)))
                            .checked(picked)
                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                if *v {
                                    this.selected.insert(r1.clone());
                                } else {
                                    this.selected.remove(&r1);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .font_family(theme.mono_font_family.clone())
                            .truncate()
                            .child(f.rel.clone()),
                    )
                    .child(widgets::caption(size, cx).text_xs().flex_none())
                    .child(
                        Button::new(SharedString::from(format!("cfgview:{}", f.rel)))
                            .xsmall()
                            .ghost()
                            .label(if open { "Hide" } else { "View" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.preview.as_ref().is_some_and(|p| p.rel == r2) {
                                    this.preview = None;
                                } else {
                                    this.preview = Some(load_preview(&r2, st, &c_dir, &s_dir));
                                    this.preview_scroll = UniformListScrollHandle::new();
                                }
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
        }
    }
}

// ---- History -------------------------------------------------------------

pub struct HistoryPanel {
    server: Option<PathBuf>,
    running: bool,
    errors: Vec<String>,
    open: HashSet<u64>,
}

impl HistoryPanel {
    pub fn new(_: &mut Window, _: &mut Context<Self>) -> Self {
        Self {
            server: None,
            running: false,
            errors: Vec::new(),
            open: HashSet::new(),
        }
    }

    pub fn show(&mut self, server: PathBuf, cx: &mut Context<Self>) {
        self.server = Some(server);
        cx.notify();
    }

    fn apply(&mut self, ids: Vec<u64>, undo: bool, cx: &mut Context<Self>) {
        self.running = true;
        self.errors.clear();
        cx.spawn(async move |this, cx| {
            let errors = cx
                .background_spawn(async move { hopper::history::apply(&ids, undo) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.errors = errors;
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }
}

impl Render for HistoryPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(server) = self.server.clone() else {
            return div().into_any_element();
        };
        let theme = cx.theme();
        let changes = hopper::history::for_server(&server);
        let mut batches: Vec<(u64, Vec<hopper::history::Change>)> = Vec::new();
        for c in changes {
            match batches.last_mut() {
                Some((b, list)) if *b == c.batch => list.push(c),
                _ => batches.push((c.batch, vec![c])),
            }
        }
        let empty = batches.is_empty();
        let cards = batches.into_iter().map(|(batch, list)| {
            let first = list[0].clone();
            let all_undone = list.iter().all(|c| c.undone);
            let any_live = list.iter().any(|c| !c.undone);
            let ids: Vec<u64> = list.iter().map(|c| c.id).collect();
            let ids2 = ids.clone();
            let open = self.open.contains(&batch) || list.len() <= 6;
            v_flex()
                .gap_2()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().text_sm().font_semibold().child(hopper::time::relative(&first.time)))
                        .child(widgets::caption(format!("{} files on the {}", list.len(), first.side), cx))
                        .child(div().flex_1())
                        .when(list.len() > 6, |r| {
                            r.child(
                                Button::new(SharedString::from(format!("files:{batch}")))
                                    .xsmall()
                                    .ghost()
                                    .label(if open { "Hide files" } else { "Show files" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.open.remove(&batch) {
                                            this.open.insert(batch);
                                        }
                                        cx.notify();
                                    })),
                            )
                        })
                        .when(any_live, |r| {
                            r.child(
                                Button::new(SharedString::from(format!("undoall:{batch}")))
                                    .xsmall()
                                    .outline()
                                    .label("Undo all")
                                    .disabled(self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.apply(ids.clone(), true, cx)
                                    })),
                            )
                        })
                        .when(all_undone, |r| {
                            r.child(
                                Button::new(SharedString::from(format!("redoall:{batch}")))
                                    .xsmall()
                                    .outline()
                                    .label("Redo all")
                                    .disabled(self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.apply(ids2.clone(), false, cx)
                                    })),
                            )
                        }),
                )
                .when(open, |c| {
                    c.children(list.into_iter().map(|ch| {
                        let tag = match ch.action {
                            hopper::history::Action::Copied if ch.backup.is_some() => {
                                Tag::info().xsmall().child("Replaced")
                            }
                            hopper::history::Action::Copied => Tag::success().xsmall().child("Added"),
                            hopper::history::Action::Removed => Tag::danger().xsmall().child("Removed"),
                        };
                        let can_undo = ch.action == hopper::history::Action::Copied || ch.backup.is_some();
                        let id = ch.id;
                        h_flex()
                            .gap_3()
                            .child(div().w_20().flex_none().child(tag))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_family(theme.mono_font_family.clone())
                                    .truncate()
                                    .when(ch.undone, |d| d.line_through().text_color(theme.muted_foreground))
                                    .child(ch.label.clone()),
                            )
                            .child(if ch.undone {
                                Button::new(SharedString::from(format!("redo:{id}")))
                                    .xsmall()
                                    .ghost()
                                    .label("Redo")
                                    .disabled(self.running)
                                    .on_click(cx.listener(move |this, _, _, cx| this.apply(vec![id], false, cx)))
                            } else {
                                Button::new(SharedString::from(format!("undo:{id}")))
                                    .xsmall()
                                    .ghost()
                                    .label("Undo")
                                    .tooltip(if can_undo { "Put back what was there" } else { "No backup was kept" })
                                    .disabled(self.running || !can_undo)
                                    .on_click(cx.listener(move |this, _, _, cx| this.apply(vec![id], true, cx)))
                            })
                    }))
                })
        });
        let s2 = server.clone();
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_3()
            .child(
                h_flex()
                    .gap_3()
                    .child(widgets::caption(
                        "Every file Hopper copied or removed for this server. Undo puts back what was there before, from the backup taken at the time.",
                        cx,
                    ).flex_1())
                    .when(self.running, |r| r.child(Spinner::new().small()))
                    .when(!empty, |r| {
                        r.child(
                            Button::new("clear-history")
                                .xsmall()
                                .ghost()
                                .label("Clear history")
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    let s = s2.clone();
                                    window.open_alert_dialog(cx, move |alert, _, _| {
                                        let s = s.clone();
                                        alert
                                            .title("Forget this server's history?")
                                            .description("Backups stay on disk.")
                                            .confirm()
                                            .ok_text("Forget")
                                            .on_ok(move |_, _, _| {
                                                hopper::history::clear(&s);
                                                true
                                            })
                                    });
                                })),
                        )
                    }),
            )
            .children(self.errors.iter().map(|e| div().text_sm().text_color(theme.danger).child(e.clone())))
            .when(empty, |c| c.child(widgets::caption("Nothing synced yet.", cx)))
            .child(
                v_flex()
                    .id("history")
                    .flex_1()
                    .min_h_0()
                    .gap_2()
                    .children(cards)
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }
}
