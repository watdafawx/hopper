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
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::crashscan::ServerCrash;
use hopper::instances::Instance;
use hopper::model::{DiffEntry, DiffStatus, ModInfo};
use hopper::rigsync::{
    self, EnvFilter, Filters, Ops, SourceFilter, StatusFilter, is_wont, row_key,
};
use hopper::server::{ServerInfo, Status};
use hopper::serverpack::Annotation;
use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

const ROW_H: f32 = 3.75;
/// The Status column's width, in rem.
const STATUS_W: f32 = 13.;

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
                        this.store.update(cx, |s, _| {
                            for m in d.iter().filter_map(|e| e.client.as_ref()) {
                                s.want_image(&m.icon);
                            }
                        });
                        this.selected = rigsync::default_selection(&d);
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

    fn selected_rows(&self) -> Vec<DiffEntry> {
        match &self.diff {
            Some(Ok(d)) => d
                .iter()
                .filter(|e| self.selected.contains(&row_key(e)))
                .cloned()
                .collect(),
            _ => Vec::new(),
        }
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
            Some((Some(n), _, f)) => facts.push(format!(
                "server pack lists {n} mods · {f} the author keeps off servers are skipped by default"
            )),
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
                    .child(
                        Button::new("update-server")
                            .primary()
                            .small()
                            .icon(IconName::ServerCog)
                            .label("Update server to match pack")
                            .tooltip("Installs the pack's NeoForge on the server if needed, then syncs mods. The server must be stopped.")
                            .disabled(online || self.running)
                            .on_click(cx.listener(|this, _, w, cx| this.confirm_update(w, cx))),
                    )
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

    fn chip(on: bool, label: &'static str, id: &'static str) -> Button {
        let b = Button::new(id).xsmall().label(label);
        if on { b.primary() } else { b.ghost() }
    }

    fn toolbar(&self, diff: &[DiffEntry], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = |f: &dyn Fn(&DiffEntry) -> bool| diff.iter().filter(|d| f(d)).count();
        let status_ix = StatusFilter::ALL
            .iter()
            .position(|s| *s == self.filters.status)
            .unwrap_or(0);
        let wont = count(&is_wont);
        let f = &self.filters;
        let env = |e: EnvFilter, label: &'static str, id: &'static str| {
            Self::chip(f.env == e, label, id).on_click(cx.listener(move |this, _, _, cx| {
                this.filters.env = e;
                cx.notify();
            }))
        };
        let source = |s: SourceFilter, label: &'static str, id: &'static str| {
            Self::chip(f.source == s, label, id).on_click(cx.listener(move |this, _, _, cx| {
                this.filters.source = s;
                cx.notify();
            }))
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div().w_56().child(
                            Input::new(&self.search)
                                .small()
                                .prefix(Icon::new(IconName::Search).small())
                                .cleanable(true),
                        ),
                    )
                    .child(
                        TabBar::new("sync-status")
                            .segmented()
                            .small()
                            .selected_index(status_ix)
                            .children(StatusFilter::ALL.iter().map(|s| {
                                let n = count(&|d| s.matches(d.status));
                                Tab::new().label(format!("{} {n}", s.label()))
                            }))
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.filters.status = StatusFilter::ALL[*ix];
                                cx.notify();
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .child(env(EnvFilter::Any, "Any side", "env-any"))
                    .child(env(EnvFilter::Client, "Client only", "env-client"))
                    .child(env(EnvFilter::Server, "Server only", "env-server"))
                    .child(env(EnvFilter::Both, "Both", "env-both"))
                    .child(div().w_px().h_4().mx_2().bg(cx.theme().border))
                    .child(source(SourceFilter::Any, "Any source", "src-any"))
                    .child(source(SourceFilter::Pack, "Pack", "src-pack"))
                    .child(source(SourceFilter::Added, "Added", "src-added"))
                    .when(wont > 0, |r| {
                        let (only, hide) = (f.wont_only, f.hide_wont);
                        r.child(div().w_px().h_4().mx_2().bg(cx.theme().border))
                            .child(
                                Button::new("wont-only")
                                    .xsmall()
                                    .label(format!("Won't sync {wont}"))
                                    .map(|b| if only { b.danger() } else { b.ghost() })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.filters.wont_only = !this.filters.wont_only;
                                        if this.filters.wont_only {
                                            this.filters.hide_wont = false;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(!only, |r| {
                                r.child(
                                    Checkbox::new("hide-wont")
                                        .label("Hide them")
                                        .checked(hide)
                                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                                            this.filters.hide_wont = *v;
                                            cx.notify();
                                        })),
                                )
                            })
                    }),
            )
    }

    fn actions(&self, diff: &[DiffEntry], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let picked = self.selected_rows();
        let Some((server, inst)) = self.pair_header(cx) else {
            return div().into_any_element();
        };
        let push = rigsync::push(&picked, &inst, &server);
        let pull = rigsync::pull(&picked, &inst, &server);
        let remove = rigsync::remove(&picked, &server);
        let online = self.online(cx);
        let blocked = self.running || (online && !self.allow_online);
        let n = push.len() + pull.len();
        let label = match (push.is_empty(), pull.is_empty()) {
            (false, false) => format!("Push and pull ({n})"),
            (true, false) => format!("Pull to client ({n})"),
            _ => format!("Push to server ({n})"),
        };
        let n_remove = remove.len();
        let client_only: Vec<String> = diff
            .iter()
            .filter(|d| d.status == DiffStatus::OnlyClient && !is_wont(d))
            .map(row_key)
            .collect();
        let mismatched: Vec<String> = diff
            .iter()
            .filter(|d| matches!(d.status, DiffStatus::ClientNewer | DiffStatus::ServerNewer))
            .map(row_key)
            .collect();
        let n_sel = self.selected.len();
        h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("sel-client")
                    .xsmall()
                    .ghost()
                    .label("Select client-only")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected.extend(client_only.iter().cloned());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("sel-mismatch")
                    .xsmall()
                    .ghost()
                    .label("Select mismatches")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected.extend(mismatched.iter().cloned());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("sel-clear")
                    .xsmall()
                    .ghost()
                    .label("Clear")
                    .disabled(n_sel == 0)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected.clear();
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
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
                            .selected_rows()
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
                Button::new("remove-server")
                    .small()
                    .outline()
                    .with_variant(ButtonVariant::Danger)
                    .label(format!("Remove ({n_remove})"))
                    .tooltip("Delete the selected server-only mods (kept in .hopper-backups)")
                    .disabled(n_remove == 0 || blocked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run(
                            Ops {
                                up: remove.clone(),
                                down: Vec::new(),
                            },
                            cx,
                        )
                    })),
            )
            .child(
                Button::new("push")
                    .small()
                    .primary()
                    .icon(IconName::ArrowLeftRight)
                    .label(label)
                    .disabled(n == 0 || blocked)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run(
                            Ops {
                                up: push.clone(),
                                down: pull.clone(),
                            },
                            cx,
                        )
                    })),
            )
            .into_any_element()
    }

    /// One side of a row: the mod (icon and name open its page), or a
    /// muted note where there is none.
    fn side_card(
        &self,
        id: ElementId,
        m: Option<&ModInfo>,
        show_file: bool,
        empty: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(m) = m else {
            return Self::note_cell(Icon::new(IconName::Minus).small(), empty, cx);
        };
        let icon = self.store.read(cx).image(&m.icon).map(Path::to_path_buf);
        let pid = m.project_id;
        let linked = pid != 0 && self.peek.is_some();
        // The icon and name open the mod's page (when Hopper knows its
        // project); the rest of the row still selects it.
        let link = |el: Stateful<Div>, cx: &mut Context<Self>| {
            el.when(linked, |d| {
                d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_peek(pid, cx);
                }))
            })
        };
        let thumb = link(
            div()
                .id(ElementId::NamedChild(std::sync::Arc::new(id.clone()), "thumb".into()))
                .flex_none()
                .when(linked, |d| d.hover(|s| s.opacity(0.8)))
                .child(widgets::thumb(icon.as_deref(), rems(2.), cx)),
            cx,
        );
        let primary = cx.theme().primary;
        let name = link(
            div()
                .id(ElementId::NamedChild(std::sync::Arc::new(id), "name".into()))
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
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(thumb)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(name)
                            .when(!m.version.is_empty(), |r| {
                                r.child(
                                    widgets::caption(m.version.clone(), cx)
                                        .text_xs()
                                        .flex_none(),
                                )
                            }),
                    )
                    .when(show_file, |c| {
                        c.child(
                            widgets::caption(m.file_name.clone(), cx)
                                .text_xs()
                                .truncate()
                                .opacity(0.75),
                        )
                    }),
            )
            .into_any_element()
    }

    /// A side with no mod to show: an icon where the thumbnail goes, and a note.
    fn note_cell(icon: Icon, text: &'static str, cx: &App) -> AnyElement {
        let theme = cx.theme();
        h_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .size(rems(2.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon),
            )
            .child(text)
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

    fn render_rows(&mut self, range: Range<usize>, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let anyway = self.store.read(cx).settings().sync_anyway.clone();
        let rows: Vec<DiffEntry> = {
            let all = self.visible(cx);
            all[range.start.min(all.len())..range.end.min(all.len())]
                .iter()
                .map(|d| (*d).clone())
                .collect()
        };
        let pair = self.pair_header(cx);
        rows.iter()
            .map(|d| {
                let key = row_key(d);
                let wont = is_wont(d);
                let picked = self.selected.contains(&key);
                let tip = if wont {
                    d.skip_reason.clone().unwrap_or_default()
                } else {
                    rigsync::explain(d)
                };
                let label = if wont {
                    "Won't sync"
                } else {
                    rigsync::status_label(d.status)
                };
                let tag = match (wont, d.status) {
                    (true, _) => Tag::secondary(),
                    (_, DiffStatus::OnlyClient) => Tag::warning(),
                    (_, DiffStatus::OnlyServer) => Tag::info(),
                    (_, DiffStatus::ServerStale) => Tag::danger(),
                    (_, DiffStatus::ClientNewer) => Tag::primary(),
                    (_, DiffStatus::ServerNewer) => Tag::warning(),
                    (_, DiffStatus::Same) => Tag::success(),
                }
                .outline()
                .xsmall()
                .child(label);
                let act = if wont {
                    None
                } else {
                    rigsync::row_action(d.status)
                };
                let id = |p: &str| ElementId::Name(format!("{p}:{key}").into());
                let delta = match (&d.client, &d.server) {
                    (Some(c), Some(s))
                        if matches!(
                            d.status,
                            DiffStatus::ClientNewer | DiffStatus::ServerNewer
                        ) =>
                    {
                        Some(format!("{} → {}", s.version, c.version))
                    }
                    _ => None,
                };
                let chip = {
                    let (d2, pair2) = (d.clone(), pair.clone());
                    div()
                        .id(id("chip"))
                        .flex_none()
                        .when(act.is_some() && !self.running, |c| {
                            c.cursor_pointer().hover(|s| s.opacity(0.8))
                        })
                        .tooltip(move |w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                        })
                        .child(tag)
                        .when_some(act.filter(|_| !self.running), |c, a| {
                            c.on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some((s, i)) = &pair2 {
                                    this.run(rigsync::chip_ops(a, &d2, i, s), cx);
                                }
                            }))
                        })
                };
                let anyway_box = d
                    .client
                    .as_ref()
                    .filter(|c| {
                        c.project_id != 0
                            && (c.wont_sync.is_some() || anyway.contains(&c.project_id))
                    })
                    .map(|c| {
                        let pid = c.project_id;
                        Checkbox::new(id("anyway"))
                            .xsmall()
                            .label("Send anyway")
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
                            }))
                    });
                let server_card = d.server.as_ref().map(|m| {
                    let mut m = m.clone();
                    if let Some(c) = &d.client {
                        if m.icon.is_empty() {
                            m.icon = c.icon.clone();
                        }
                        if m.project_id == 0 {
                            m.project_id = c.project_id;
                        }
                    }
                    m
                });
                // The same jar on both sides: say so rather than repeat it.
                let same_file = d.status == DiffStatus::Same
                    && d.client
                        .as_ref()
                        .zip(d.server.as_ref())
                        .is_some_and(|(c, s)| c.file_name.eq_ignore_ascii_case(&s.file_name));
                let client =
                    self.side_card(id("client"), d.client.as_ref(), true, "Not in the pack", cx);
                let server = if same_file {
                    let ok = cx.theme().success;
                    Self::note_cell(Icon::new(IconName::Check).small().text_color(ok), "Same file", cx)
                } else {
                    let file_differs = d
                        .client
                        .as_ref()
                        .zip(server_card.as_ref())
                        .is_none_or(|(c, s)| !c.file_name.eq_ignore_ascii_case(&s.file_name));
                    self.side_card(
                        id("server"),
                        server_card.as_ref(),
                        file_differs,
                        if wont { "Kept off the server" } else { "Not on the server" },
                        cx,
                    )
                };
                let theme = cx.theme();
                let k2 = key.clone();
                h_flex()
                    .id(id("sync-row"))
                    .w_full()
                    .h(rems(ROW_H))
                    .px_3()
                    .gap_4()
                    .border_b_1()
                    .border_color(theme.border.opacity(0.5))
                    .cursor_pointer()
                    .when(picked, |r| r.bg(theme.primary.opacity(0.06)))
                    .hover(|s| s.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.selected.remove(&k2) {
                            this.selected.insert(k2.clone());
                        }
                        cx.notify();
                    }))
                    .child(
                        div().w_6().flex_none().child(
                            Checkbox::new(id("pick"))
                                .checked(picked)
                                .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                    if *v {
                                        this.selected.insert(key.clone());
                                    } else {
                                        this.selected.remove(&key);
                                    }
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(client)
                    .child(
                        v_flex()
                            .w(rems(STATUS_W))
                            .flex_none()
                            .gap_1()
                            .child(h_flex().gap_2().child(chip).when_some(delta, |r, t| {
                                r.child(widgets::caption(t, cx).text_xs().min_w_0().truncate())
                            }))
                            .children(anyway_box),
                    )
                    .child(server)
                    .into_any_element()
            })
            .collect()
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
            Some(Ok(diff)) => {
                let shown: Vec<String> = self.visible(cx).into_iter().map(row_key).collect();
                let count = shown.len();
                let all_picked = count > 0 && shown.iter().all(|k| self.selected.contains(k));
                let toolbar = self.toolbar(diff, cx);
                let actions = self.actions(diff, cx);
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
                    .child(toolbar)
                    .child(actions)
                    .child(
                        h_flex()
                            .px_3()
                            .pb_2()
                            .gap_4()
                            .border_b_1()
                            .border_color(theme.border)
                            .text_xs()
                            .font_medium()
                            .text_color(theme.muted_foreground)
                            .child(
                                div().w_6().flex_none().child(
                                    Checkbox::new("pick-all")
                                        .checked(all_picked)
                                        .tooltip("Select every row shown")
                                        .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                            for k in &shown {
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
                            .child(div().flex_1().pl(rems(2.75)).child("In the pack"))
                            .child(div().w(rems(STATUS_W)).flex_none().child("Status"))
                            .child(div().flex_1().pl(rems(2.75)).child("On the server")),
                    )
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
                    .child(
                        widgets::caption(
                            "Click a row to select it · a mod's name or icon opens its page · a status does just that row",
                            cx,
                        )
                        .text_xs(),
                    )
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
