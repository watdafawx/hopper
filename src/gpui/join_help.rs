//! Failures to join a server or open a world (`hopper::joinfail`): the
//! card on Crash help after the game ends, and the warning under an
//! instance's header when its paired server would turn it away
//! (`hopper::rigsync::join_check`). They read like the crash card
//! (`crash_view`), in amber: nothing crashed.

use crate::crash_help::callout;
use crate::crash_view::{sentence, words};
use crate::store::{Msg, Store, StoreEvent};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::joinfail::{JoinKind, JoinProblem};
use hopper::rigsync::JoinCheck;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a comparison with the server counts as current.
const FRESH: Duration = Duration::from_secs(90);

/// The paired server, compared with the pack before it plays.
#[derive(Clone, Debug)]
pub struct PreJoin {
    pub server_root: PathBuf,
    pub server: String,
    pub check: JoinCheck,
}

#[derive(Default)]
pub struct Joins {
    problems: HashMap<PathBuf, Option<JoinProblem>>,
    checks: HashMap<PathBuf, (Instant, Option<PreJoin>)>,
    pending: HashSet<PathBuf>,
}

impl Store {
    /// The instance's last join problem, until a game ends without one.
    pub fn join_problem(&mut self, root: &Path) -> Option<&JoinProblem> {
        self.joins.problems.entry(root.to_path_buf()).or_insert_with(|| hopper::joinfail::load(root)).as_ref()
    }

    pub fn dismiss_join(&mut self, root: &Path) {
        hopper::joinfail::clear(root);
        self.joins.problems.insert(root.to_path_buf(), None);
    }

    /// What the game that just ended says (from `Msg::JoinChecked`).
    pub fn join_checked(&mut self, root: PathBuf, p: Option<JoinProblem>, cx: &mut Context<Self>) {
        match &p {
            Some(p) => {
                hopper::joinfail::save(&root, p);
                if let Some(i) = self.instance(&root) {
                    cx.emit(StoreEvent::JoinFailed(i.name.clone(), p.title(), p.why.clone()));
                }
            }
            None => hopper::joinfail::clear(&root),
        }
        self.joins.problems.insert(root.clone(), p);
        // The server may have changed too: compare again when next shown.
        self.joins.checks.remove(&root);
        cx.notify();
    }

    /// The pack compared with its paired server, when it was compared
    /// lately and something differs.
    pub fn pre_join(&self, root: &Path) -> Option<&PreJoin> {
        self.joins.checks.get(root)?.1.as_ref().filter(|p| !p.check.is_empty())
    }

    /// Forgets the comparison, so the next look compares again (after a
    /// sync or a loader change).
    pub fn forget_pre_join(&mut self, root: &Path) {
        self.joins.checks.remove(root);
    }

    /// Compares the pack with its paired server in the background, unless
    /// that was done lately.
    pub fn want_pre_join(&mut self, root: &Path) {
        if !self.server_tools() || self.joins.pending.contains(root) {
            return;
        }
        if self.joins.checks.get(root).is_some_and(|(t, _)| t.elapsed() < FRESH) {
            return;
        }
        let Some(server) = self.server_of(root).cloned() else { return };
        let Some(inst) = self.instance(root).cloned() else { return };
        self.joins.pending.insert(root.to_path_buf());
        let anyway = self.settings().sync_anyway.clone();
        let hidden = self.settings().sync_blacklist.clone();
        let root = root.to_path_buf();
        self.spawn(move || {
            // An unreachable server says nothing here: its row has a dot for that.
            let found = hopper::rigsync::compare(&inst, &server, &anyway).ok().map(|(diff, _)| {
                let server_nf = hopper::server_update::server_neoforge(&server.root);
                let pack_nf = inst.loader.strip_prefix("neoforge-");
                let check = hopper::rigsync::join_check(&diff, &hidden, pack_nf, server_nf.as_deref());
                PreJoin { server_root: server.root.clone(), server: server.name.clone(), check }
            });
            Msg::PreJoin(root, found)
        });
    }

    pub fn pre_join_done(&mut self, root: PathBuf, p: Option<PreJoin>, cx: &mut Context<Self>) {
        self.joins.pending.remove(&root);
        self.joins.checks.insert(root, (Instant::now(), p));
        cx.notify();
    }
}

/// Asks to bring the server in step with the pack (Sync to server).
fn update_server(store: &Entity<Store>, root: &Path, server_root: &Path, window: &mut Window, cx: &mut App) {
    let s = store.read(cx);
    let (Some(server), Some(inst)) = (s.server(server_root).cloned(), s.instance(root).cloned()) else { return };
    crate::rigsync::confirm_update_server(store, server, inst, window, cx);
    store.update(cx, |s, _| s.forget_pre_join(root));
}

/// Switches the pack to the server's NeoForge build.
fn match_server(store: &Entity<Store>, root: &Path, nf: &str, window: &mut Window, cx: &mut App) {
    if crate::inst_settings::switch_loader(store, root, format!("neoforge-{nf}"), window, cx) {
        store.update(cx, |s, _| s.forget_pre_join(root));
    }
}

/// Whether the address the game tried (`host` or `host:port`) is this
/// server: the same port, and its host is the server's (the address
/// Hopper pings it at, this PC, or its Hopper Server's machine). An
/// unknown address counts: the game may not have logged it.
fn is_server(target: &str, server: &hopper::server::ServerInfo, server_host: &str) -> bool {
    if target.is_empty() {
        return true;
    }
    let (host, port) = match target.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() => (h, p.parse::<u16>().unwrap_or(25565)),
        _ => (target, 25565),
    };
    if port != server.port() {
        return false;
    }
    let host = host.to_ascii_lowercase();
    let mut known = vec!["localhost".to_string(), "127.0.0.1".to_string(), server_host.to_ascii_lowercase()];
    if let Some(r) = &server.remote {
        known.push(hopper::hopperd::host_name(&r.base).to_ascii_lowercase());
    }
    known.iter().any(|k| !k.is_empty() && *k == host)
}

/// A mod by id: its icon and name opening the mod peek when the pack has
/// it, else just its id.
fn mod_chip<V: 'static>(store: &Entity<Store>, root: &Path, id: &str, jar: &str, key: usize, cx: &Context<V>) -> AnyElement {
    let s = store.read(cx);
    let addon = (!jar.is_empty())
        .then(|| s.instance(root).and_then(|i| i.addons.iter().find(|a| a.file_name.eq_ignore_ascii_case(jar.trim_end_matches(".disabled")))))
        .flatten();
    let Some(a) = addon else {
        return div().text_sm().font_semibold().child(id.to_string()).into_any_element();
    };
    let name = if a.name.is_empty() { id.to_string() } else { a.name.clone() };
    let icon = s.image(&a.icon).map(Path::to_path_buf);
    let (store2, root2) = (store.clone(), root.to_path_buf());
    widgets::mod_link(
        ElementId::Name(format!("join-mod:{key}").into()),
        name,
        icon.as_deref(),
        a.project_id,
        move |project, _, cx| {
            let root = root2.clone();
            store2.update(cx, |_, cx| cx.emit(StoreEvent::Peek { root, project }));
        },
        cx,
    )
    .flex_none()
    .text_sm()
    .into_any_element()
}

/// The join-problem card on Crash help.
pub fn card<V: 'static>(store: &Entity<Store>, root: &Path, p: &JoinProblem, cx: &mut Context<V>) -> AnyElement {
    let theme = cx.theme().clone();
    let color = theme.warning;
    let (pack_nf, paired) = {
        let s = store.read(cx);
        let nf = s.instance(root).and_then(|i| i.loader.strip_prefix("neoforge-").map(str::to_string));
        // Only when the game failed to join that very server.
        let paired = s
            .server_of(root)
            .filter(|sv| s.server_tools() && !p.world && is_server(&p.target, sv, &s.settings().server_host))
            .map(|sv| (sv.root.clone(), sv.name.clone()));
        (nf, paired)
    };
    // The server's NeoForge, as the message says it.
    let server_nf = p.versions.as_ref().map(|v| v.1.clone()).filter(|v| p.kind == JoinKind::Loader && !v.is_empty());
    let versions = match (p.kind, &p.versions) {
        (JoinKind::Loader, Some((_, sv))) if !sv.is_empty() => Some(sentence(vec![
            widgets::caption("This game:", cx).into_any_element(),
            div().font_semibold().child(pack_nf.clone().map_or("NeoForge (unknown build)".into(), |v| format!("NeoForge {v}"))).into_any_element(),
            widgets::caption("· the server:", cx).into_any_element(),
            div().font_semibold().child(format!("NeoForge {sv}")).into_any_element(),
        ])),
        (JoinKind::GameVersion, Some((_, sv))) if !sv.is_empty() => Some(sentence(vec![
            widgets::caption("The server's Minecraft:", cx).into_any_element(),
            div().font_semibold().child(sv.clone()).into_any_element(),
        ])),
        (JoinKind::Mods, Some((mine, theirs))) => Some(sentence(vec![widgets::caption(
            format!("Network version here {mine}, on the server {theirs}."),
            cx,
        )
        .into_any_element()])),
        _ => None,
    };
    let mods: Vec<AnyElement> = p
        .mods
        .iter()
        .enumerate()
        .take(8)
        .map(|(i, id)| mod_chip(store, root, id, p.jars.get(i).map_or("", String::as_str), i, cx))
        .collect();
    let more = p.mods.len().saturating_sub(8);
    // The fixes Hopper can do: only for the server it knows.
    let mut actions: Vec<AnyElement> = Vec::new();
    if let Some((sroot, sname)) = &paired {
        if matches!(p.kind, JoinKind::Loader | JoinKind::Mods | JoinKind::Registry) {
            let (st, r, sr) = (store.clone(), root.to_path_buf(), sroot.clone());
            actions.push(
                Button::new("join-update-server")
                    .small()
                    .primary()
                    .icon(IconName::ServerCog)
                    .label(format!("Update {sname}"))
                    .tooltip("Gives the server this pack's NeoForge and mods (it asks first)")
                    .on_click(move |_, window, cx| update_server(&st, &r, &sr, window, cx))
                    .into_any_element(),
            );
            let (st, sr) = (store.clone(), sroot.clone());
            actions.push(
                Button::new("join-compare")
                    .small()
                    .outline()
                    .label("Compare with server")
                    .on_click(move |_, _, cx| {
                        let root = sr.clone();
                        st.update(cx, |_, cx| cx.emit(StoreEvent::ShowServer { root, compare: true }));
                    })
                    .into_any_element(),
            );
        }
    }
    if let (Some(nf), Some(_)) = (&server_nf, &pack_nf)
        && pack_nf.as_deref() != Some(nf.as_str())
    {
        let (st, r, nf2) = (store.clone(), root.to_path_buf(), nf.clone());
        actions.push(
            Button::new("join-match-server")
                .small()
                .outline()
                .label(format!("Use NeoForge {nf}"))
                .tooltip("Installs the server's NeoForge build in this pack")
                .on_click(move |_, window, cx| match_server(&st, &r, &nf2, window, cx))
                .into_any_element(),
        );
    }
    if p.kind == JoinKind::Session {
        actions.push(
            Button::new("join-accounts")
                .small()
                .outline()
                .icon(IconName::User)
                .label("Accounts")
                .on_click({
                    let st = store.clone();
                    move |_, window, cx| crate::dialogs::open_sign_in(st.clone(), window, cx)
                })
                .into_any_element(),
        );
    }
    let log = root.join("logs").join("latest.log");
    let root_d = root.to_path_buf();
    let store_d = store.clone();
    let view = cx.entity_id();
    callout(color, cx)
        .child(
            h_flex()
                .gap_2()
                .child(Icon::new(IconName::TriangleAlert).small().text_color(color))
                .child(div().font_semibold().text_color(color).child(p.title()))
                .child(widgets::caption(hopper::time::relative_secs(p.when), cx))
                .child(div().flex_1())
                .when(log.is_file(), |r| {
                    let l = log.clone();
                    r.child(
                        Button::new("join-open-log")
                            .small()
                            .outline()
                            .icon(IconName::FileText)
                            .label("Open log")
                            .tooltip("Opens the game log this was read from")
                            .on_click(move |_, _, _| crate::store::open_path(&l)),
                    )
                })
                .child(
                    Button::new("join-dismiss")
                        .small()
                        .ghost()
                        .icon(IconName::X)
                        .tooltip("Dismiss")
                        .on_click(move |_, _, cx| {
                            store_d.update(cx, |s, cx| {
                                s.dismiss_join(&root_d);
                                cx.notify();
                            });
                            cx.notify(view);
                        }),
                ),
        )
        .child(div().text_sm().font_medium().child(p.why.clone()))
        .children(versions.map(|v| v.text_sm()))
        .when(!mods.is_empty(), |d| {
            let mut parts = vec![widgets::caption(if p.mods.len() == 1 { "The message names:" } else { "The message names these mods:" }, cx).into_any_element()];
            parts.extend(mods);
            if more > 0 {
                parts.push(words(format!("and {more} more")));
            }
            d.child(sentence(parts))
        })
        .child(
            crate::crash_view::fix_box(cx)
                .child(div().text_sm().font_semibold().child("What to do"))
                .child(div().text_sm().child(p.fix.clone()))
                .when(!actions.is_empty(), |d| d.child(h_flex().flex_wrap().gap_2().children(actions))),
        )
        .when(!p.line.is_empty(), |d| {
            d.child(
                div()
                    .font_family(theme.mono_font_family.clone())
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .whitespace_normal()
                    .child(p.line.clone()),
            )
        })
        .into_any_element()
}

/// The warning under an instance's header: its paired server would turn
/// it away (or ask for mods it lacks). Closed per set of differences.
pub fn pre_join_bar<V: 'static>(store: &Entity<Store>, root: &Path, cx: &mut Context<V>) -> Option<AnyElement> {
    store.update(cx, |s, _| s.want_pre_join(root));
    let s = store.read(cx);
    let p = s.pre_join(root)?.clone();
    let pack_nf = s.instance(root).and_then(|i| i.loader.strip_prefix("neoforge-").map(str::to_string));
    let theme = cx.theme().clone();
    let color = theme.warning;
    let key = hopper::settings::notice_key(&root.to_string_lossy(), "pre-join", &p.check.signature());
    let title = if p.check.loader.is_some() {
        format!("{} will turn this pack away", p.server)
    } else {
        format!("{} has other mods than this pack", p.server)
    };
    let detail = {
        let c = &p.check;
        let list = |v: &[String]| {
            let shown: Vec<&str> = v.iter().take(4).map(String::as_str).collect();
            let more = v.len().saturating_sub(4);
            if more > 0 { format!("{} and {more} more", shown.join(", ")) } else { shown.join(", ") }
        };
        let mut lines = Vec::new();
        if let Some((mine, theirs)) = &c.loader {
            lines.push(format!("NeoForge: {mine} in this pack, {theirs} on the server"));
        }
        if !c.only_pack.is_empty() {
            lines.push(format!("Only in the pack: {}", list(&c.only_pack)));
        }
        if !c.only_server.is_empty() {
            lines.push(format!("Only on the server: {}", list(&c.only_server)));
        }
        if !c.versions.is_empty() {
            let names: Vec<String> = c.versions.iter().map(|(n, a, b)| format!("{n} ({a} here, {b} there)")).collect();
            lines.push(format!("Other versions: {}", list(&names)));
        }
        lines
    };
    let (st, r, sr) = (store.clone(), root.to_path_buf(), p.server_root.clone());
    let update = Button::new("prejoin-update")
        .small()
        .primary()
        .icon(IconName::ServerCog)
        .label("Update server")
        .tooltip("Gives the server this pack's NeoForge and mods (it asks first)")
        .on_click(move |_, window, cx| update_server(&st, &r, &sr, window, cx));
    let matcher = p.check.loader.as_ref().filter(|_| pack_nf.is_some()).map(|(_, snf)| {
        let (st, r, nf) = (store.clone(), root.to_path_buf(), snf.clone());
        Button::new("prejoin-match")
            .small()
            .outline()
            .label("Match server")
            .tooltip(format!("Installs NeoForge {snf} in this pack, the server's build"))
            .on_click(move |_, window, cx| match_server(&st, &r, &nf, window, cx))
    });
    let (st, sr) = (store.clone(), p.server_root.clone());
    let details = Button::new("prejoin-details")
        .small()
        .outline()
        .label("Compare")
        .tooltip("Opens the server's mod comparison")
        .on_click(move |_, _, cx| {
            let root = sr.clone();
            st.update(cx, |_, cx| cx.emit(StoreEvent::ShowServer { root, compare: true }));
        });
    let body = widgets::notice_bar(color, IconName::TriangleAlert, cx)
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().font_medium().child(title.clone()))
                .children(detail.into_iter().map(|l| widgets::caption(l, cx).text_xs().whitespace_normal())),
        )
        .child(h_flex().flex_none().gap_2().child(update).children(matcher).child(details));
    widgets::closable_notice(body, key, widgets::Closed::Chip, title, color, store, cx).map(|e| {
        // A closed notice's chip needs the same room as the bar.
        div().mx_6().mt_3().child(e).into_any_element()
    })
}
