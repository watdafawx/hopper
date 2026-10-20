//! A pack compared with its rig server: what differs, and pushing, pulling
//! or removing mods. Used by the Server sync page and an instance's Server tab.

use crate::store::{Msg, Store};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashscan::ServerCrash;
use hopper::instances::Instance;
use hopper::model::{DiffEntry, DiffStatus, ModInfo};
use hopper::rigsync::{self, EnvFilter, Filters, Group, Ops, RowAct, SourceFilter, row_key};
use hopper::server::{ServerInfo, Status};
use hopper::serverpack::Annotation;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

const ROW_H: f32 = 3.5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Mods,
    Configs,
    History,
}

pub struct SyncPanel {
    store: Entity<Store>,
    section: Section,
    configs: Entity<crate::configs::ConfigsPanel>,
    history: Entity<crate::configs::HistoryPanel>,
    /// Start / stop / restart through the server's panel, beside the header.
    control: Entity<crate::server_control::ServerControl>,
    /// Inside an instance, whose header already shows the pack.
    in_instance: bool,
    server: Option<PathBuf>,
    diff: Option<Result<Vec<DiffEntry>, String>>,
    note: Option<Annotation>,
    crash: Option<ServerCrash>,
    /// The NeoForge version the server's run.bat starts, read with each
    /// comparison (run.bat may sit on a network share: not per frame).
    server_nf: Option<String>,
    filters: Filters,
    search: Entity<InputState>,
    selected: HashSet<String>,
    /// Groups showing their rows.
    open: HashSet<Group>,
    /// The list as drawn, rebuilt each frame from the comparison.
    items: Vec<Item>,
    running: bool,
    result: Option<Result<String, Vec<String>>>,
    allow_online: bool,
    /// The store's server revision this comparison is for.
    seen_rev: u64,
    scroll: UniformListScrollHandle,
    /// The Library's mod panel, opened by a mod's name or icon.
    peek: Option<Entity<crate::browse::BrowseView>>,
    _task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SyncPanel {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search mods"));
        let subs = vec![
            cx.observe(&store, |this, _, cx| {
                // The server or the pack's mods changed elsewhere: compare again.
                let rev = this.compare_key(cx);
                if rev != this.seen_rev {
                    this.seen_rev = rev;
                    this.rescan(cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filters.text = state.read(cx).value().trim().to_string();
                    cx.notify();
                }
            }),
        ];
        let seen_rev = 0;
        let configs = cx.new(|cx| crate::configs::ConfigsPanel::new(store.clone(), window, cx));
        let history = cx.new(|cx| crate::configs::HistoryPanel::new(window, cx));
        let control =
            cx.new(|cx| crate::server_control::ServerControl::new(store.clone(), window, cx));
        let mut subs = subs;
        subs.push(cx.observe(&control, |_, _, cx| cx.notify()));
        Self {
            store,
            // `--section configs|history` for scripted runs.
            section: match std::env::args()
                .skip_while(|a| a != "--section")
                .nth(1)
                .as_deref()
            {
                Some("configs") => Section::Configs,
                Some("history") => Section::History,
                _ => Section::Mods,
            },
            configs,
            history,
            control,
            in_instance: false,
            server: None,
            diff: None,
            note: None,
            crash: None,
            server_nf: None,
            filters: Filters::default(),
            search,
            selected: HashSet::new(),
            open: [Group::Add, Group::Update, Group::ServerOnly, Group::KeptOff].into(),
            items: Vec::new(),
            running: false,
            result: None,
            allow_online: false,
            seen_rev,
            scroll: UniformListScrollHandle::new(),
            peek: None,
            _task: None,
            _subscriptions: subs,
        }
    }

    pub fn set_in_instance(&mut self, on: bool) {
        self.in_instance = on;
    }

    pub fn set_peek(&mut self, peek: Entity<crate::browse::BrowseView>) {
        self.peek = Some(peek);
    }

    /// Opens (or closes, when it's the open one) a mod's page beside the list.
    fn open_peek(&mut self, project: u64, cx: &mut Context<Self>) {
        let Some(root) = self.pair_header(cx).map(|(_, i)| i.root) else {
            return;
        };
        if let Some(p) = &self.peek {
            p.update(cx, |p, cx| p.peek_project(root, Some(project), cx));
        }
    }

    pub fn server(&self) -> Option<&Path> {
        self.server.as_deref()
    }

    /// Compares `root`'s server with its pack (again when it is the same, so
    /// coming back to the tab picks up mods changed meanwhile).
    pub fn show(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let r2 = root.clone();
        self.configs.update(cx, |c, cx| c.show(r2.clone(), cx));
        self.history.update(cx, |h, cx| h.show(r2.clone(), cx));
        self.control.update(cx, |c, cx| c.show(r2, cx));
        let moved = self.server.as_ref() != Some(&root);
        if moved {
            self.server = Some(root);
            self.filters = Filters {
                text: self.filters.text.clone(),
                ..Default::default()
            };
            self.result = None;
            self.diff = None;
            self.server_nf = None;
        }
        // Same server mid-sync: the sync rescans when it ends.
        if moved || !self.running {
            self.seen_rev = self.compare_key(cx);
            self.rescan(cx);
        }
    }

    /// What the comparison depends on in the store: the server's revision and
    /// the paired pack's mod files.
    fn compare_key(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let store = self.store.read(cx);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        store.server_rev().hash(&mut h);
        let inst = self
            .server
            .as_ref()
            .and_then(|r| store.server(r))
            .and_then(|s| store.paired(s));
        if let Some(inst) = inst {
            inst.root.hash(&mut h);
            for a in &inst.addons {
                (&a.file_name, a.enabled, a.file_id).hash(&mut h);
            }
        }
        h.finish()
    }

    fn pair(&self, cx: &App) -> Option<(ServerInfo, Instance)> {
        let store = self.store.read(cx);
        let s = store.server(self.server.as_ref()?)?.clone();
        let inst = store.paired(&s)?.clone();
        Some((s, inst))
    }

    /// The server and its pack's header (no mod list), for drawing.
    fn pair_header(&self, cx: &App) -> Option<(ServerInfo, Instance)> {
        let store = self.store.read(cx);
        let s = store.server(self.server.as_ref()?)?;
        let inst = store.paired(s)?.clone_header();
        Some((s.clone(), inst))
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair(cx) else {
            cx.notify();
            return;
        };
        self.running = true;
        let anyway = self.store.read(cx).settings().sync_anyway.clone();
        self._task = Some(cx.spawn(async move |this, cx| {
            let root = server.root.clone();
            let (diff, crash, server_nf) = cx
                .background_spawn(async move {
                    (
                        rigsync::compare(&inst, &server, &anyway),
                        hopper::crashscan::last_crash(&server.root),
                        hopper::server_update::server_neoforge(&server.root),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                if this.server.as_ref() != Some(&root) {
                    return;
                }
                this.running = false;
                this.crash = crash;
                this.server_nf = server_nf;
                match diff {
                    Ok((d, note)) => {
                        let inst = this.pair(cx).map(|(_, i)| i);
                        this.store.update(cx, |s, _| {
                            for m in d.iter().filter_map(|e| e.client.as_ref()) {
                                s.want_image(&m.icon);
                            }
                            if let Some(i) = &inst {
                                s.want_categories(&i.addons);
                            }
                        });
                        // Nothing is picked for you; picks that still exist
                        // survive a rescan.
                        let keys: HashSet<String> = d.iter().map(row_key).collect();
                        this.selected.retain(|k| keys.contains(k));
                        this.note = Some(note);
                        this.diff = Some(Ok(d));
                    }
                    Err(e) => this.diff = Some(Err(e)),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Up, by ping or by its panel (which also knows a server that is still
    /// starting and doesn't answer pings yet).
    fn online(&self, cx: &App) -> bool {
        let Some(r) = self.server.as_ref() else { return false };
        // The panel's state this view polls is the freshest.
        if let Some(up) = self.control.read(cx).running() {
            return up;
        }
        let store = self.store.read(cx);
        store.status(r).is_some_and(|s| matches!(s, Status::Online { .. }))
            || store.panel_stats(r).is_some_and(|p| p.running || p.starting)
    }

    fn run(&mut self, ops: Ops, cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair(cx) else {
            return;
        };
        if ops.is_empty() {
            return;
        }
        self.running = true;
        self.result = None;
        self._task = Some(cx.spawn(async move |this, cx| {
            let results = cx
                .background_spawn(async move { rigsync::run(ops, &server.root, &inst.root) })
                .await;
            this.update(cx, |this, cx| {
                this.running = false;
                this.result = Some(rigsync::summary(&results));
                this.rescan(cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Picked rows the filters show: a pick the filters hide does nothing.
    fn selected_rows(&self, cx: &App) -> Vec<DiffEntry> {
        self.visible(cx)
            .into_iter()
            .filter(|e| self.selected.contains(&row_key(e)))
            .cloned()
            .collect()
    }

    fn confirm_update(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((server, inst)) = self.pair(cx) else {
            return;
        };
        let server_nf = hopper::server_update::server_neoforge(&server.root);
        let pack_nf = inst.loader.strip_prefix("neoforge-").map(str::to_string);
        let what = match (&pack_nf, &server_nf) {
            (Some(p), Some(sv)) if p != sv => {
                format!("install NeoForge {p}, point run.bat at it, and sync mods")
            }
            _ => "sync mods".to_string(),
        };
        let store = self.store.clone();
        let name = server.name.clone();
        let body = format!(
            "This will {what} in {}. Replaced files go to .hopper-backups.",
            server.root.display()
        );
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (store, server, inst) = (store.clone(), server.clone(), inst.clone());
            alert
                .title(format!("Update {name} to match the pack?"))
                .description(body.clone())
                .confirm()
                .ok_text("Update server")
                .on_ok(move |_, _, cx| {
                    let s = store.read(cx);
                    let host = s.settings().server_host.clone();
                    let anyway = s.settings().sync_anyway.clone();
                    let java =
                        hopper::launch::installer_java(Path::new(&s.settings().launcher_dir));
                    let panel = hopper::panel::from_settings(s.settings());
                    let (server, inst) = (server.clone(), inst.clone());
                    s.spawn_job(format!("Updating {}", server.name), move |job| {
                        let r = hopper::server_update::update_server(
                            &server, &inst, &host, &anyway, &java, panel.as_deref(), job,
                        )?;
                        job.detail(r.summary());
                        for f in &r.failed {
                            job.note(f.clone());
                        }
                        Ok(Msg::ServersChanged)
                    });
                    true
                })
        });
    }

    // ---- rendering -----------------------------------------------------

    fn header(
        &self,
        server: &ServerInfo,
        inst: &Instance,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        // Quick start / stop when a panel is set up; Servers sets one up.
        let controls = self
            .control
            .read(cx)
            .configured(cx)
            .then(|| self.control.update(cx, |c, cx| c.controls(cx)));
        let stop_where = if controls.is_some() { "stop it here" } else { "stop it in Crafty/MCSS" };
        let in_instance = self.in_instance;
        let store = self.store.read(cx);
        let theme = cx.theme();
        let icon = inst
            .icon
            .as_deref()
            .and_then(|u| store.image(u))
            .map(Path::to_path_buf);
        let online = self.online(cx);
        let pack_nf = inst.loader.strip_prefix("neoforge-").map(str::to_string);
        let nf_mismatch = match (&pack_nf, &self.server_nf) {
            (Some(p), Some(sv)) if p != sv => Some(format!("Server NeoForge {sv}, pack {p}")),
            _ => None,
        };
        // One line of facts: loader, then what the server pack said.
        let mut facts = Vec::new();
        if nf_mismatch.is_none()
            && let (Some(p), Some(_)) = (&pack_nf, &self.server_nf)
        {
            facts.push(format!("NeoForge {p} on both"));
        }
        match self.note.as_ref().map(|a| (a.server_pack_jars, &a.note, a.flagged)) {
            Some((Some(n), _, _)) => facts.push(format!("the pack's server pack lists {n} mods")),
            Some((None, Some(note), f)) => facts.push(format!("{note} {f} mods flagged.")),
            _ => {}
        }
        let facts = (!facts.is_empty()).then(|| {
            let mut t = facts.join(" · ");
            if let Some(c) = t.get(0..1) {
                t.replace_range(0..1, &c.to_uppercase());
            }
            t
        });
        let title = h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .text_lg()
            .font_semibold()
            .when(!in_instance, |r| {
                r.child(div().truncate().child(inst.name.clone())).child(
                    Icon::new(IconName::ArrowRight)
                        .small()
                        .text_color(theme.muted_foreground),
                )
            })
            .when(in_instance, |r| {
                r.child(Icon::new(IconName::Server).text_color(theme.muted_foreground))
            })
            .child(div().truncate().child(server.name.clone()));
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .when(!in_instance, |r| r.child(widgets::thumb(icon.as_deref(), rems(2.5), cx)))
                    .child(title)
                    .children(controls)
                    .child({
                        let lead = Button::new("update-server")
                            .primary()
                            .small()
                            .icon(IconName::ServerCog)
                            .label("Update server to match pack")
                            .tooltip("Installs the pack's NeoForge on the server if needed, then syncs mods. The server must be stopped.")
                            .disabled(online || self.running)
                            .on_click(cx.listener(|this, _, w, cx| this.confirm_update(w, cx)));
                        match inst.pack.as_ref().map(|p| p.project_id) {
                            Some(pid) => {
                                let (store, root) = (self.store.clone(), inst.root.clone());
                                widgets::split(
                                    widgets::split_lead(lead),
                                    "update-server-notes",
                                    widgets::Tint::Primary,
                                    gpui_kit::component::Size::Small,
                                    "Pack versions and changelogs",
                                    move |_, window, cx| {
                                        let upd = store
                                            .read(cx)
                                            .instance(&root)
                                            .and_then(|i| store.read(cx).pack_update(i).cloned());
                                        crate::versions::open(store.clone(), root.clone(), pid, true, upd, window, cx)
                                    },
                                    cx,
                                )
                                .into_any_element()
                            }
                            None => lead.into_any_element(),
                        }
                    })
                    .child(
                        Button::new("rescan")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .tooltip("Compare again")
                            .disabled(self.running)
                            .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                    ),
            )
            .when_some(nf_mismatch, |r, t| {
                r.child(
                    h_flex()
                        .gap_1p5()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(t),
                )
            })
            .when_some(facts, |r, t| r.child(widgets::caption(t, cx)))
            .when(online, |r| {
                r.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.warning)
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(format!("Server is online. Mods load at startup, so {stop_where} before syncing.")),
                )
            })
    }

    fn crash_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.crash.clone()?;
        let server_mods: Vec<ModInfo> = match &self.diff {
            Some(Ok(d)) => d.iter().filter_map(|e| e.server.clone()).collect(),
            _ => Vec::new(),
        };
        let online = self.online(cx);
        let theme = cx.theme();
        let rows = c.hits.iter().map(|h| {
            let jar = server_mods
                .iter()
                .find(|m| {
                    (!h.file_name.is_empty() && m.file_name.eq_ignore_ascii_case(&h.file_name))
                        || (!h.mod_id.is_empty() && m.mod_id.eq_ignore_ascii_case(&h.mod_id))
                })
                .map(|m| m.file_name.clone());
            let name = if h.mod_id.is_empty() {
                h.file_name.clone()
            } else {
                h.mod_id.clone()
            };
            h_flex()
                .gap_3()
                .child(div().text_sm().font_semibold().child(name))
                .child(widgets::caption(h.reason.clone(), cx).flex_1().min_w_0())
                .when_some(jar, |r, j| {
                    let server = self.server.clone();
                    r.child(
                        Button::new(ElementId::Name(format!("crash-rm:{j}").into()))
                            .xsmall()
                            .outline()
                            .label("Remove from server")
                            .tooltip(format!("Moves {j} to .hopper-backups (undo from History)"))
                            .disabled(online || self.running)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(root) = &server {
                                    let path = root.join("mods").join(&j);
                                    this.run(
                                        Ops {
                                            up: vec![hopper::model::SyncOp::Delete { path }],
                                            down: Vec::new(),
                                        },
                                        cx,
                                    );
                                }
                            })),
                    )
                })
        });
        Some(
            v_flex()
                .gap_2()
                .p_3()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.danger)
                .child(
                    h_flex()
                        .gap_2()
                        .text_color(theme.danger)
                        .font_semibold()
                        .child(Icon::new(IconName::TriangleAlert).small())
                        .child(format!("The server's last start crashed: {}", c.headline)),
                )
                .child(widgets::caption(
                    if c.client_only {
                        format!(
                            "From {}. Client-only mods on the server are the likely cause.",
                            c.source
                        )
                    } else {
                        format!("From {}.", c.source)
                    },
                    cx,
                ))
                .children(rows)
                .into_any_element(),
        )
    }

    fn group_color(g: Group, cx: &App) -> Hsla {
        let theme = cx.theme();
        match g {
            Group::Add => theme.primary,
            Group::Update => theme.info,
            Group::ServerOnly => theme.danger,
            Group::KeptOff => theme.warning,
            Group::InSync => theme.success,
        }
    }

    fn group_icon(g: Group) -> IconName {
        match g {
            Group::Add => IconName::Plus,
            Group::Update => IconName::ArrowUp,
            Group::ServerOnly => IconName::Server,
            Group::KeptOff => IconName::ShieldCheck,
            Group::InSync => IconName::Check,
        }
    }

    /// Search, which side a mod runs on, and pack or added.
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        const ENVS: [(EnvFilter, &str); 4] = [
            (EnvFilter::Any, "Any side"),
            (EnvFilter::Client, "Client only"),
            (EnvFilter::Both, "Client + server"),
            (EnvFilter::Server, "Server only"),
        ];
        const SOURCES: [(SourceFilter, &str); 2] =
            [(SourceFilter::Any, "Pack + added"), (SourceFilter::Added, "Added by me")];
        let env_ix = ENVS.iter().position(|(e, _)| *e == self.filters.env).unwrap_or(0);
        let src_ix = SOURCES
            .iter()
            .position(|(s, _)| *s == self.filters.source)
            .unwrap_or(0);
        let n_sel = self.selected_rows(cx).len();
        h_flex()
            .gap_3()
            .flex_wrap()
            .child(
                div().w_56().child(
                    Input::new(&self.search)
                        .small()
                        .prefix(Icon::new(IconName::Search).small())
                        .cleanable(true),
                ),
            )
            .child(
                TabBar::new("sync-env")
                    .segmented()
                    .small()
                    .selected_index(env_ix)
                    .children(ENVS.iter().map(|(_, l)| Tab::new().label(*l)))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.filters.env = ENVS[*ix].0;
                        cx.notify();
                    })),
            )
            .child(
                TabBar::new("sync-source")
                    .segmented()
                    .small()
                    .selected_index(src_ix)
                    .children(SOURCES.iter().map(|(_, l)| Tab::new().label(*l)))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.filters.source = SOURCES[*ix].0;
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .when(n_sel > 0, |r| {
                r.child(
                    Button::new("sel-clear")
                        .xsmall()
                        .ghost()
                        .label("Clear selection")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.selected.clear();
                            cx.notify();
                        })),
                )
            })
    }

    /// A card per group with its count; a click opens the group and
    /// scrolls to it.
    fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        // The rows the filters show, so a card matches its group's count.
        let groups: Vec<Group> = self.visible(cx).into_iter().map(Group::of).collect();
        h_flex().gap_2().children(Group::ALL.map(|g| {
            let n = groups.iter().filter(|x| **x == g).count();
            let color = Self::group_color(g, cx);
            div()
                .id(ElementId::Name(format!("card-{g:?}").into()))
                .flex_1()
                .min_w_0()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(|s| s.bg(theme.list_hover))
                .child(
                    div()
                        .text_xl()
                        .font_weight(FontWeight::BOLD)
                        .text_color(if n == 0 { theme.muted_foreground } else { color })
                        .child(n.to_string()),
                )
                .child(
                    h_flex()
                        .gap_1p5()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Icon::new(Self::group_icon(g)).xsmall())
                        .child(g.short()),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open.insert(g);
                    this.build_items(cx);
                    if let Some(ix) = this
                        .items
                        .iter()
                        .position(|i| matches!(i, Item::Header(h, _) if *h == g))
                    {
                        this.scroll.scroll_to_item(ix, ScrollStrategy::Top);
                    }
                    cx.notify();
                }))
        }))
    }

    /// What Apply will do, and the buttons for the picked rows.
    fn footer(&self, server: &ServerInfo, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let picked = self.selected_rows(cx);
        let Some((srv, inst)) = self.pair_header(cx) else {
            return div().into_any_element();
        };
        let ops = rigsync::apply_ops(&picked, &inst, &srv);
        let count = |g: Group, s: Option<DiffStatus>| {
            picked
                .iter()
                .filter(|d| Group::of(d) == g && s.is_none_or(|s| d.status == s))
                .count()
        };
        let parts: Vec<String> = [
            ("add", count(Group::Add, None)),
            ("update", count(Group::Update, None)),
            ("remove", count(Group::ServerOnly, None)),
        ]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(w, n)| format!("{w} {n}"))
        .collect();
        let online = self.online(cx);
        let blocked = self.running || (online && !self.allow_online);
        let n_sel = picked.len();
        let theme = cx.theme();
        let text = if parts.is_empty() {
            "Nothing selected. Tick mods, or use Select all on a group.".to_string()
        } else {
            format!("Apply will {} on {}", parts.join(", "), server.name)
        };
        h_flex()
            .gap_3()
            .px_4()
            .py_2p5()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(Icon::new(IconName::ListChecks).small().text_color(theme.muted_foreground))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .text_color(if parts.is_empty() {
                        theme.muted_foreground
                    } else {
                        theme.foreground
                    })
                    .child(text),
            )
            .when(self.running, |r| r.child(Spinner::new().small()))
            .when(online, |r| {
                r.child(
                    Checkbox::new("allow-online")
                        .label("Allow while online")
                        .checked(self.allow_online)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.allow_online = *v;
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("blacklist")
                    .small()
                    .outline()
                    .label(format!("Hide ({n_sel})"))
                    .tooltip("Hide the selected mods from this comparison")
                    .disabled(n_sel == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let keys: Vec<String> = this
                            .selected_rows(cx)
                            .iter()
                            .filter_map(rigsync::blacklist_key)
                            .collect();
                        this.selected.clear();
                        this.store.update(cx, |s, cx| {
                            s.update_settings(|st| st.sync_blacklist.extend(keys), cx)
                        });
                    })),
            )
            .child(
                Button::new("apply")
                    .small()
                    .primary()
                    .icon(IconName::Upload)
                    .label("Apply to server")
                    .tooltip("Replaced and removed jars are kept in .hopper-backups")
                    .disabled(ops.is_empty() || blocked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let ops = Ops {
                            up: ops.up.clone(),
                            down: ops.down.clone(),
                        };
                        this.selected.clear();
                        this.run(ops, cx)
                    })),
            )
            .into_any_element()
    }

    /// The rows the filters let through.
    fn visible(&self, cx: &App) -> Vec<&DiffEntry> {
        let Some(Ok(diff)) = &self.diff else {
            return Vec::new();
        };
        let blacklist = &self.store.read(cx).settings().sync_blacklist;
        diff.iter()
            .filter(|d| self.filters.passes(d, blacklist))
            .collect()
    }

    /// The list: each group's header, then its rows when it's open.
    fn build_items(&mut self, cx: &App) {
        let Some(Ok(diff)) = &self.diff else {
            self.items.clear();
            return;
        };
        let blacklist = &self.store.read(cx).settings().sync_blacklist;
        let mut by_group: Vec<(Group, Vec<usize>)> = Group::ALL.map(|g| (g, Vec::new())).into();
        for (i, d) in diff.iter().enumerate() {
            if self.filters.passes(d, blacklist) {
                let g = Group::of(d);
                if let Some((_, v)) = by_group.iter_mut().find(|(x, _)| *x == g) {
                    v.push(i);
                }
            }
        }
        let mut items = Vec::new();
        for (g, rows) in by_group {
            if rows.is_empty() {
                continue;
            }
            items.push(Item::Header(g, rows.len()));
            if self.open.contains(&g) {
                items.extend(rows.into_iter().map(Item::Row));
            }
        }
        self.items = items;
    }

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(Ok(diff)) = &self.diff else {
            return Vec::new();
        };
        let items: Vec<Item> = self.items[range.start.min(self.items.len())..range.end.min(self.items.len())].to_vec();
        let rows: Vec<(Item, Option<DiffEntry>)> = items
            .iter()
            .map(|i| match i {
                Item::Row(ix) => (*i, diff.get(*ix).cloned()),
                Item::Header(..) => (*i, None),
            })
            .collect();
        // Badges come from the pack's addons: read them all before drawing.
        let tags: Vec<Option<RowTags>> = {
            let store = self.store.read(cx);
            let inst = self
                .server
                .as_ref()
                .and_then(|r| store.server(r))
                .and_then(|s| store.paired(s));
            let now = hopper::time::parse_iso(&hopper::time::now_iso()).unwrap_or(0);
            rows.iter()
                .map(|(_, d)| {
                    let d = d.as_ref()?;
                    let m = d.client.as_ref().or(d.server.as_ref())?;
                    let mut t = RowTags::of(m, inst, store, now);
                    if d.client.is_none() {
                        (t.new, t.added) = (false, false);
                    }
                    Some(t)
                })
                .collect()
        };
        rows.into_iter()
            .zip(tags)
            .map(|((item, d), tags)| match (item, d) {
                (Item::Header(g, n), _) => self.group_header(g, n, cx),
                (Item::Row(_), Some(d)) => self.row(&d, tags.unwrap_or_default(), cx),
                _ => div().into_any_element(),
            })
            .collect()
    }

    fn group_header(&self, g: Group, n: usize, cx: &mut Context<Self>) -> AnyElement {
        let open = self.open.contains(&g);
        let keys: Vec<String> = if g.pickable() {
            self.visible(cx)
                .into_iter()
                .filter(|d| Group::of(d) == g)
                .map(row_key)
                .collect()
        } else {
            Vec::new()
        };
        let all = !keys.is_empty() && keys.iter().all(|k| self.selected.contains(k));
        let color = Self::group_color(g, cx);
        let theme = cx.theme();
        h_flex()
            .id(ElementId::Name(format!("group-{g:?}").into()))
            .h(rems(ROW_H))
            .w_full()
            .items_end()
            .pb_1()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded(theme.radius_lg)
                    .bg(theme.secondary)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.list_hover))
                    .child(
                        Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().size_2().rounded_full().bg(color))
                    .child(div().text_sm().font_semibold().child(g.title()))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(n.to_string()))
                    .child(
                        widgets::caption(format!("· {}", g.hint()), cx)
                            .text_xs()
                            .flex_1()
                            .min_w_0(),
                    )
                    .when(!keys.is_empty(), |r| {
                        r.child(
                            div()
                                .id(ElementId::Name(format!("all-{g:?}").into()))
                                .on_click(|_, _, cx| cx.stop_propagation())
                                .child(
                                    Checkbox::new(ElementId::Name(format!("pick-all-{g:?}").into()))
                                        .label("Select all")
                                        .checked(all)
                                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                            for k in &keys {
                                                if *v {
                                                    this.selected.insert(k.clone());
                                                } else {
                                                    this.selected.remove(k);
                                                }
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.open.remove(&g) {
                    this.open.insert(g);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn row(&self, d: &DiffEntry, tags: RowTags, cx: &mut Context<Self>) -> AnyElement {
        let g = Group::of(d);
        let key = row_key(d);
        let id = |p: &str| ElementId::Name(format!("{p}:{key}").into());
        let Some(m) = d.client.as_ref().or(d.server.as_ref()).cloned() else {
            return div().into_any_element();
        };
        let picked = g.pickable() && self.selected.contains(&key);
        let online = self.online(cx);
        let blocked = self.running || (online && !self.allow_online);
        let anyway = self.store.read(cx).settings().sync_anyway.clone();
        let pair = self.pair_header(cx);
        let icon = self.store.read(cx).image(&m.icon).map(Path::to_path_buf);
        let pid = d
            .client
            .as_ref()
            .map(|c| c.project_id)
            .filter(|p| *p != 0)
            .unwrap_or(m.project_id);
        let linked = pid != 0 && self.peek.is_some();
        // The line under the name: why it's kept off, else the file.
        let why = match g {
            Group::KeptOff => d
                .skip_reason
                .clone()
                .or_else(|| d.client.as_ref().and_then(|c| c.wont_sync.clone())),
            Group::ServerOnly if d.status == DiffStatus::ServerStale => {
                Some("An older copy left behind; the pack has this mod under a newer file".into())
            }
            Group::Update if d.status == DiffStatus::ServerNewer => {
                Some("The server has a newer version than the pack".into())
            }
            _ => None,
        };
        let version = match (g, &d.client, &d.server) {
            (Group::Update, Some(c), Some(s)) => format!("{} → {}", s.version, c.version),
            _ => m.version.clone(),
        };
        // One action per row, named for what it does.
        let act = |label: &'static str, icon: IconName, a: RowAct, danger: bool| {
            let (d2, pair2) = (d.clone(), pair.clone());
            Button::new(id(label))
                .small()
                .outline()
                .icon(icon)
                .label(label)
                .when(danger, |b| b.with_variant(ButtonVariant::Danger))
                .disabled(blocked)
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    if let Some((s, i)) = &pair2 {
                        this.run(rigsync::chip_ops(a, &d2, i, s), cx);
                    }
                }))
        };
        let theme = cx.theme().clone();
        let action: AnyElement = match g {
            Group::Add => act("Add", IconName::Plus, RowAct::Push, false).into_any_element(),
            Group::Update => {
                let lead = act(
                    if d.status == DiffStatus::ServerNewer { "Replace" } else { "Update" },
                    IconName::ArrowUp,
                    RowAct::Push,
                    false,
                );
                match pair.as_ref().filter(|_| pid != 0 && d.client.is_some()) {
                    Some((_, inst)) => {
                        let (store, root) = (self.store.clone(), inst.root.clone());
                        widgets::split(
                            widgets::split_lead(lead),
                            id("changes"),
                            widgets::Tint::Outline,
                            gpui_kit::component::Size::Small,
                            "What changed",
                            move |_, window, cx| {
                                cx.stop_propagation();
                                crate::versions::open(store.clone(), root.clone(), pid, false, None, window, cx)
                            },
                            cx,
                        )
                        .into_any_element()
                    }
                    None => lead.into_any_element(),
                }
            }
            Group::ServerOnly => h_flex()
                .gap_1()
                .when(d.status == DiffStatus::OnlyServer, |r| {
                    let (d2, pair2) = (d.clone(), pair.clone());
                    r.child(
                        Button::new(id("pull"))
                            .small()
                            .ghost()
                            .icon(IconName::Download)
                            .tooltip("Copy it into the pack")
                            .disabled(self.running)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some((s, i)) = &pair2 {
                                    this.run(rigsync::chip_ops(RowAct::Pull, &d2, i, s), cx);
                                }
                            })),
                    )
                })
                .child(act("Remove", IconName::Trash, RowAct::Remove, true))
                .into_any_element(),
            Group::KeptOff => h_flex()
                .gap_3()
                .child(act("Send once", IconName::Upload, RowAct::Push, false).tooltip(
                    "Copy it to the server this time; the next comparison still keeps it off",
                ))
                .when(pid != 0, |r| {
                    r.child(
                        Checkbox::new(id("always"))
                            .label("Always")
                            .checked(anyway.contains(&pid))
                            .tooltip("Always send this mod to the server")
                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                let on = *v;
                                this.store.update(cx, |s, cx| {
                                    s.update_settings(
                                        |st| {
                                            if on {
                                                st.sync_anyway.insert(pid);
                                            } else {
                                                st.sync_anyway.remove(&pid);
                                            }
                                        },
                                        cx,
                                    )
                                });
                                this.rescan(cx);
                            })),
                    )
                })
                .into_any_element(),
            Group::InSync => widgets::caption(
                if d.client.as_ref().zip(d.server.as_ref()).is_some_and(|(c, s)| {
                    c.file_name.eq_ignore_ascii_case(&s.file_name)
                }) {
                    "Same file"
                } else {
                    "Same version"
                },
                cx,
            )
            .text_xs()
            .into_any_element(),
        };
        let link = |el: Stateful<Div>, cx: &mut Context<Self>| {
            el.when(linked, |d| {
                d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_peek(pid, cx);
                }))
            })
        };
        let primary = theme.primary;
        let thumb = link(
            div()
                .id(id("thumb"))
                .flex_none()
                .when(linked, |d| d.hover(|s| s.opacity(0.8)))
                .child(widgets::thumb(icon.as_deref(), rems(2.), cx)),
            cx,
        );
        let name = link(
            div()
                .id(id("name"))
                .min_w_0()
                .text_sm()
                .font_medium()
                .truncate()
                .when(linked, |d| {
                    d.hover(move |s| s.text_color(primary)).tooltip(|w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new("Details").build(w, cx)
                    })
                })
                .child(m.mod_name.clone()),
            cx,
        );
        let sub = why.clone().unwrap_or_else(|| m.file_name.clone());
        let k2 = key.clone();
        let pickable = g.pickable();
        h_flex()
            .id(id("sync-row"))
            .w_full()
            .h(rems(ROW_H))
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border.opacity(0.4))
            .when(picked, |r| r.bg(theme.primary.opacity(0.06)))
            .hover(|s| s.bg(theme.list_hover))
            .when(pickable, |r| {
                r.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    if !this.selected.remove(&k2) {
                        this.selected.insert(k2.clone());
                    }
                    cx.notify();
                }))
            })
            .child(div().w_6().flex_none().when(pickable, |c| {
                let pick_id = id("pick");
                let key = key.clone();
                c.child(
                    Checkbox::new(pick_id)
                        .checked(picked)
                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                            if *v {
                                this.selected.insert(key.clone());
                            } else {
                                this.selected.remove(&key);
                            }
                            cx.notify();
                        })),
                )
            }))
            .child(thumb)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(name)
                    .child(
                        widgets::caption(sub, cx)
                            .text_xs()
                            .truncate()
                            .when(why.is_none(), |c| c.opacity(0.75)),
                    ),
            )
            .child(
                div().flex_1().min_w_0().child(widgets::mod_tags(
                    widgets::ModTags {
                        key: &m.file_name,
                        new: tags.new,
                        added: tags.added,
                        // The line under the name already says client-only.
                        env: if why.as_deref().is_some_and(|w| w.to_lowercase().contains("client-only")) {
                            ""
                        } else {
                            &tags.env
                        },
                        categories: &tags.categories,
                        modrinth: tags.modrinth,
                        ..Default::default()
                    },
                    cx,
                )),
            )
            .child(
                widgets::caption(version, cx)
                    .w(rems(10.))
                    .flex_none()
                    .text_xs()
                    .truncate(),
            )
            .child(h_flex().w(rems(14.)).flex_none().justify_end().child(action))
            .into_any_element()
    }
}

/// One line of the changeset list.
#[derive(Clone, Copy)]
enum Item {
    /// A group's header and how many rows it has.
    Header(Group, usize),
    /// An index into the comparison.
    Row(usize),
}

/// What one side of a row shows as badges, read from the pack's addon.
#[derive(Clone, Default)]
struct RowTags {
    env: String,
    categories: Vec<String>,
    new: bool,
    added: bool,
    modrinth: bool,
}

impl RowTags {
    fn of(m: &ModInfo, inst: Option<&Instance>, store: &Store, now: i64) -> Self {
        let a = inst.and_then(|i| i.addon_by_file(&m.file_name));
        RowTags {
            // The jar's own word first, then the site's tags.
            env: if m.side.is_empty() {
                a.map(|a| a.env.clone()).unwrap_or_default()
            } else {
                m.side.clone()
            },
            categories: a.map(|a| store.categories(a).to_vec()).unwrap_or_default(),
            new: a
                .and_then(|a| hopper::time::parse_iso(&a.date_added))
                .is_some_and(|t| now - t < 3 * 86_400),
            added: inst.is_some_and(|i| i.pack.is_some()) && a.is_some_and(|a| !a.from_pack),
            modrinth: a.is_some_and(|a| !a.mr_project.is_empty()),
        }
    }
}

impl Render for SyncPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.server.clone() else {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(widgets::caption(
                    "Pick a server to compare it with its pack.",
                    cx,
                ))
                .into_any_element();
        };
        let Some((server, inst)) = self.pair_header(cx) else {
            let name = self
                .store
                .read(cx)
                .server(&root)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .child(widgets::caption(
                    format!("{name} has no paired pack yet. Pick one in its list entry."),
                    cx,
                ))
                .into_any_element();
        };
        let header = self.header(&server, &inst, cx);
        let crash = self.crash_banner(cx);
        let section_ix = match self.section {
            Section::Mods => 0,
            Section::Configs => 1,
            Section::History => 2,
        };
        let sections = TabBar::new("sync-section")
            .underline()
            .selected_index(section_ix)
            .child(Tab::new().prefix(Icon::new(IconName::Puzzle).small()).label("Mods"))
            .child(Tab::new().prefix(Icon::new(IconName::Settings).small()).label("Configs"))
            .child(Tab::new().prefix(Icon::new(IconName::Clock).small()).label("History"))
            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                this.section = [Section::Mods, Section::Configs, Section::History][*ix];
                cx.notify();
            }));
        self.build_items(cx);
        let body: AnyElement = match (self.section, &self.diff) {
            (Section::Configs, _) => self.configs.clone().into_any_element(),
            (Section::History, _) => self.history.clone().into_any_element(),
            (Section::Mods, d) => match d {
            None => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Scanning mod folders", cx))
                .into_any_element(),
            Some(Err(e)) => div()
                .text_sm()
                .text_color(cx.theme().danger)
                .child(e.clone())
                .into_any_element(),
            Some(Ok(_)) => {
                let count = self.items.len();
                let toolbar = self.toolbar(cx);
                let summary = self.summary(cx);
                let footer = self.footer(&server, cx);
                let theme = cx.theme();
                let result: Option<AnyElement> = self.result.as_ref().map(|r| match r {
                    Ok(line) => h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.success)
                        .child(Icon::new(IconName::CircleCheck).small())
                        .child(line.clone())
                        .into_any_element(),
                    Err(failed) => v_flex()
                        .text_sm()
                        .text_color(theme.danger)
                        .children(failed.iter().cloned())
                        .into_any_element(),
                });
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .gap_3()
                    .child(summary)
                    .child(toolbar)
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .child(
                                uniform_list(
                                    "sync-rows",
                                    count,
                                    cx.processor(|this, range: Range<usize>, _, cx| {
                                        this.render_rows(range, cx)
                                    }),
                                )
                                .track_scroll(&self.scroll)
                                .size_full(),
                            )
                            .vertical_scrollbar(&self.scroll),
                    )
                    .child(footer)
                    .children(result)
                    .into_any_element()
            }
            },
        };
        v_flex()
            .size_full()
            .px_6()
            .py_5()
            .gap_4()
            .child(header)
            .children(crash)
            .child(sections)
            .child(v_flex().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}
