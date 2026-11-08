//! "Changes in the files" on a changelog card (design/changelog-from-files.md).
//! A mod version whose author wrote no changelog shows, instead of "No
//! changelog for this version", either a notice offering to compare the
//! files (setting off) or what Hopper found in them (setting on). The
//! comparison itself is hopper-core's `changelog_files`; the web UI's
//! `VersionCard` shows the same words.

use crate::store::Store;
use crate::widgets;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use hopper::cf;
use hopper::changelog_files as cf_files;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

/// Where a version's scan stands.
#[derive(Clone)]
pub enum Scan {
    Running,
    /// The result as markdown.
    Done(SharedString),
    Failed(String),
    /// The oldest version Hopper knows of: nothing before it to compare with.
    Nothing,
}

/// By (project, file): lives as long as the app, a few KB each.
static STATE: LazyLock<Mutex<HashMap<(u64, u64), Scan>>> = LazyLock::new(Default::default);

fn state(project: u64, file: u64) -> Option<Scan> {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).get(&(project, file)).cloned()
}

fn set(project: u64, file: u64, s: Scan) {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).insert((project, file), s);
}

/// What a card needs to scan its version.
#[derive(Clone)]
pub struct Ctx {
    pub store: Entity<Store>,
    pub project_id: u64,
    pub file: cf::File,
    /// The project's listed versions (any order); empty: look them up.
    pub all: Vec<cf::File>,
    pub root: Option<PathBuf>,
    /// The installed file's id and path in the instance, when it is one.
    pub installed: Option<(u64, PathBuf)>,
}

/// Starts the comparison for `ctx` once, when the setting is on. Call it
/// while rendering a card with no changelog; `true` when this card shows a
/// result (at most `MAX_PER_VIEW` per `used` count of the caller), or, with
/// the setting off, the offer (the first card only).
pub fn ensure<V: 'static>(ctx: &Ctx, used: &mut usize, cx: &mut Context<V>) -> bool {
    if !ctx.store.read(cx).settings().files_changes {
        // Off: the offer shows once, on the first card with no changelog.
        *used += 1;
        return *used == 1;
    }
    if state(ctx.project_id, ctx.file.id).is_some() {
        return true;
    }
    if *used >= cf_files::MAX_PER_VIEW {
        return false;
    }
    *used += 1;
    set(ctx.project_id, ctx.file.id, Scan::Running);
    let c = ctx.clone();
    cx.spawn(async move |this, cx| {
        let r = cx.background_spawn(async move { run(&c) }).await;
        let (p, f, scan) = r;
        set(p, f, scan);
        this.update(cx, |_, cx| cx.notify()).ok();
    })
    .detach();
    true
}

/// The comparison, on a background thread.
fn run(c: &Ctx) -> (u64, u64, Scan) {
    let done = |s| (c.project_id, c.file.id, s);
    let all: Vec<cf::File> = if c.all.is_empty() {
        let gv = c
            .file
            .game_versions
            .iter()
            .find(|g| g.starts_with(|ch: char| ch.is_ascii_digit()) && g.contains('.'))
            .cloned()
            .unwrap_or_default();
        hopper::source::files(c.project_id, &gv, hopper::cf::Loader::Any).unwrap_or_default()
    } else {
        c.all.clone()
    };
    let Some(old) = cf_files::previous_of(&all, &c.file) else {
        return done(Scan::Nothing);
    };
    let installed = c.installed.as_ref().map(|(id, p)| (*id, p.as_path()));
    match cf_files::scan_versions(c.root.as_deref(), c.project_id, installed, old, &c.file) {
        Ok(ch) => done(Scan::Done(ch.to_markdown().into())),
        Err(e) => done(Scan::Failed(format!("{e:#}"))),
    }
}

/// The card's body for a version with no changelog. `shown`: `ensure`'s
/// answer. `id` names the text element.
pub fn card(ctx: &Ctx, shown: bool, id: ElementId, cx: &App) -> AnyElement {
    let plain = || widgets::caption(hopper::changelog_trim::NO_CHANGELOG, cx).italic().into_any_element();
    let s = ctx.store.read(cx).settings();
    if s.files_changes {
        if !shown {
            return plain();
        }
        return match state(ctx.project_id, ctx.file.id) {
            None | Some(Scan::Running) => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Comparing the files", cx))
                .into_any_element(),
            Some(Scan::Nothing) => plain(),
            Some(Scan::Failed(e)) => v_flex()
                .gap_1()
                .child(plain())
                .child(widgets::caption(format!("Hopper couldn't compare the files. {e}"), cx).text_xs())
                .into_any_element(),
            Some(Scan::Done(md)) => v_flex()
                .gap_2()
                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(cf_files::RESULT_LABEL))
                .child(widgets::changelog_text(id, &md, cx))
                .child(widgets::caption(cf_files::RESULT_FOOTER, cx).text_xs())
                .into_any_element(),
        };
    }
    if !shown || s.notice_hidden(&cf_files::dismiss_key(ctx.project_id)) {
        return plain();
    }
    let (on, later) = (ctx.store.clone(), ctx.store.clone());
    let pid = ctx.project_id;
    let key = format!("files-notice:{}", ctx.file.id);
    v_flex()
        .gap_2()
        .child(div().text_sm().text_color(cx.theme().muted_foreground).child(cf_files::NOTICE))
        .child(
            h_flex()
                .gap_2()
                .child(
                    Button::new(SharedString::from(format!("{key}:on")))
                        .small()
                        .outline()
                        .label("Turn on")
                        .tooltip(cf_files::SETTING_HELP)
                        .on_click(move |_, window, cx| {
                            on.update(cx, |s, cx| s.update_settings(|st| st.files_changes = true, cx));
                            window.refresh();
                        }),
                )
                .child(
                    Button::new(SharedString::from(format!("{key}:later")))
                        .small()
                        .ghost()
                        .label("Not now")
                        .on_click(move |_, window, cx| {
                            later.update(cx, |s, cx| s.update_settings(|st| st.hide_notice(&cf_files::dismiss_key(pid)), cx));
                            window.refresh();
                        }),
                ),
        )
        .into_any_element()
}
