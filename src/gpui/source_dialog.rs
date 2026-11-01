//! Mods from GitHub and other links: Add content › "From GitHub or a
//! link…", and a mod's "Follow a source…". One dialog: paste a link,
//! Hopper finds the file for the instance (or lists the files when it
//! can't tell), says who publishes it the first time, then adds it.
//! The work is in `hopper::sources`.

use crate::dialogs::footer;
use crate::store::{Msg, Store};
use crate::widgets::{self, Clickable as _};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::sources::{self, Fit, Resolved};
use std::path::PathBuf;

/// What the dialog is doing.
enum Stage {
    Idle,
    Looking,
    Found(Box<Resolved>),
    /// Following a source with the jar already there: how it matches.
    Adopt(Box<sources::Adoption>),
    Failed(String),
}

pub struct SourceForm {
    store: Entity<Store>,
    root: PathBuf,
    /// Following a source for this installed mod (file name, mod name).
    follow: Option<(String, String)>,
    link: Entity<InputState>,
    pre: bool,
    stage: Stage,
    /// The file picked (index into the candidates).
    chosen: Option<usize>,
    /// The file list is open (ambiguous, nothing fits, or "Other file").
    picking: bool,
    _subs: Vec<Subscription>,
}

impl SourceForm {
    fn new(store: Entity<Store>, root: PathBuf, follow: Option<(String, String)>, link: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link_in = cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://github.com/owner/repo")
        });
        if !link.is_empty() {
            link_in.update(cx, |s, cx| s.set_value(link.to_string(), window, cx));
        }
        let subs = vec![cx.subscribe_in(&link_in, window, |this: &mut Self, _, e: &InputEvent, _, cx| match e {
            InputEvent::PressEnter { .. } => this.look_up(cx),
            InputEvent::Change => {
                // A changed link starts over.
                if !matches!(this.stage, Stage::Idle | Stage::Looking) {
                    this.stage = Stage::Idle;
                    this.chosen = None;
                    this.picking = false;
                    cx.notify();
                }
            }
            _ => {}
        })];
        link_in.update(cx, |s, cx| s.focus(window, cx));
        let mut this = SourceForm {
            store,
            root,
            follow,
            link: link_in,
            pre: false,
            stage: Stage::Idle,
            chosen: None,
            picking: false,
            _subs: subs,
        };
        if !link.is_empty() {
            this.look_up(cx);
        }
        this
    }

    fn look_up(&mut self, cx: &mut Context<Self>) {
        let link = self.link.read(cx).value().trim().to_string();
        if link.is_empty() || matches!(self.stage, Stage::Looking) {
            return;
        }
        let Some(inst) = self.store.read(cx).instance(&self.root).cloned() else { return };
        let (mc, loader, pre) = (inst.game_version.clone(), hopper::install::loader_of(&inst), self.pre);
        self.stage = Stage::Looking;
        self.chosen = None;
        self.picking = false;
        cx.notify();
        // An installed jar: matched where it is, nothing downloads.
        if let Some((file, _)) = self.follow.clone() {
            cx.spawn(async move |this, cx| {
                let r = cx
                    .background_spawn(async move { sources::plan_adopt(&inst, &file, &link, pre).map_err(|e| format!("{e:#}")) })
                    .await;
                this.update(cx, |this, cx| {
                    this.stage = match r {
                        Ok(a) => Stage::Adopt(Box::new(a)),
                        Err(e) => Stage::Failed(e),
                    };
                    cx.notify();
                })
                .ok();
            })
            .detach();
            return;
        }
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { sources::resolve(&link, &mc, loader, pre).map_err(|e| format!("{e:#}")) })
                .await;
            this.update(cx, |this, cx| {
                match r {
                    Ok(r) => {
                        this.chosen = r.pick;
                        this.picking = r.ambiguous || r.pick.is_none();
                        this.stage = Stage::Found(Box::new(r));
                    }
                    Err(e) => this.stage = Stage::Failed(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn resolved(&self) -> Option<&Resolved> {
        match &self.stage {
            Stage::Found(r) => Some(r),
            _ => None,
        }
    }

    fn ready(&self) -> bool {
        if matches!(self.stage, Stage::Adopt(_)) {
            return true;
        }
        self.resolved().zip(self.chosen).is_some_and(|(r, i)| i < r.candidates.len())
    }

    fn title(&self) -> String {
        match &self.follow {
            Some((_, name)) => format!("Follow a source for {name}"),
            None => "Add from GitHub or a link".into(),
        }
    }

    fn footer(&self) -> DialogFooter {
        let trusted = match &self.stage {
            Stage::Adopt(a) => sources::trusted(&a.source),
            _ => self.resolved().is_none_or(|r| sources::trusted(&r.source)),
        };
        let action = match (&self.follow, trusted) {
            (Some(_), true) => "Follow",
            (Some(_), false) => "Trust and follow",
            (None, true) => "Add",
            (None, false) => "Trust and add",
        };
        if matches!(self.stage, Stage::Idle | Stage::Failed(_)) {
            return DialogFooter::new()
                .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
                .child(DialogAction::new().child(Button::new("dialog-ok").primary().label("Look up")));
        }
        footer(action, self.ready())
    }

    /// The dialog's OK: looks the link up, or adds the chosen file.
    fn ok(form: &Entity<Self>, window: &mut Window, cx: &mut App) -> bool {
        if matches!(form.read(cx).stage, Stage::Idle | Stage::Failed(_)) {
            form.update(cx, |this, cx| this.look_up(cx));
            return false;
        }
        let this = form.read(cx);
        if let (Stage::Adopt(a), Some((file, name))) = (&this.stage, this.follow.clone()) {
            let (a, store, root) = ((**a).clone(), this.store.clone(), this.root.clone());
            if let Err(e) = sources::trust(&a.source) {
                window.push_notification(gpui_kit::component::notification::Notification::error(format!("{e:#}")), cx);
            }
            store.update(cx, |s, _| {
                s.spawn_instance_job(&root.clone(), format!("{name} follows {}", a.source.publisher), move |job| {
                    let mut inst = hopper::instances::load(&root).ok_or_else(|| anyhow::anyhow!("the instance is gone"))?;
                    sources::adopt(&mut inst, &file, &a)?;
                    job.detail(match &a.newest {
                        Some(c) => format!("{name} follows {}. {} is out: it's in Updates.", a.source.publisher, c.version),
                        None => format!("{name} follows {}. It's up to date.", a.source.publisher),
                    });
                    Ok(Msg::InstanceChanged(root))
                });
            });
            return true;
        }
        let (Some(r), Some(i)) = (this.resolved().cloned(), this.chosen) else { return false };
        let Some(inst) = this.store.read(cx).instance(&this.root).cloned() else { return true };
        let follow = this.follow.clone();
        let store = this.store.clone();
        if let Err(e) = sources::trust(&r.source) {
            window.push_notification(gpui_kit::component::notification::Notification::error(format!("{e:#}")), cx);
        }
        let file = r.candidates[i].file_name.clone();
        let what = match &follow {
            Some((_, name)) => format!("Following {} for {name}", r.source.publisher),
            None => format!("Adding {file}"),
        };
        store.update(cx, |s, _| {
            let root = inst.root.clone();
            s.spawn_instance_job(&root, what, move |job| {
                let mut inst = inst;
                let done = match &follow {
                    Some((f, _)) => sources::follow(&mut inst, f, &r, i, job)?,
                    None => sources::install(&mut inst, &r, i, job)?,
                };
                job.detail(format!("{done} is in. Updates come from {}.", r.source.publisher));
                Ok(Msg::InstanceChanged(inst.root.clone()))
            });
        });
        true
    }

    fn picker(&self, r: &Resolved, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .gap_0p5()
            .children(r.candidates.iter().enumerate().map(|(i, c)| {
                let chosen = self.chosen == Some(i);
                let tag_color = match c.fit {
                    Fit::Good => theme.success,
                    Fit::Unknown => theme.info,
                    Fit::Extra(_) => theme.muted_foreground,
                    _ => theme.danger,
                };
                let row = h_flex()
                    .id(("src-file", i))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(px(crate::theme::RADIUS))
                    .clickable(cx)
                    .child(Icon::new(if chosen { IconName::CircleCheck } else { IconName::Circle }).small())
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(c.file_name.clone()))
                    .child(widgets::chip(tag_color).xsmall().child(c.fit.tag()))
                    .child(
                        div()
                            .w(rems(4.5))
                            .text_xs()
                            .text_right()
                            .text_color(theme.muted_foreground)
                            .child(size(c.size)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.chosen = Some(i);
                        cx.notify();
                    }));
                if chosen { widgets::row_selected(row, cx) } else { widgets::row_hover(row, cx) }.into_any_element()
            }))
            .into_any_element()
    }
}

/// The once-per-source note on who publishes a file.
fn trust_note(publisher: &str, kind: &str, author: Option<String>, theme: &gpui_kit::component::theme::Theme) -> AnyElement {
    let who = match author {
        Some(a) => format!("Published by {publisher}, release by {a}."),
        None => format!("Published by {publisher}."),
    };
    v_flex()
        .gap_1()
        .p_3()
        .rounded(px(crate::theme::RADIUS_LG))
        .bg(theme.warning.opacity(widgets::alpha::TINT))
        .child(div().text_sm().font_semibold().child(format!("First file from {publisher}")))
        .child(div().text_sm().child(format!(
            "{who} Files from {kind} aren't checked by CurseForge or Modrinth. Add them only from people you trust. Hopper never runs anything from the download."
        )))
        .into_any_element()
}

fn size(n: u64) -> String {
    match n {
        0 => String::new(),
        n if n < 1 << 20 => format!("{} KB", n.div_ceil(1024)),
        n => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
    }
}

impl Render for SourceForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let mut body = v_flex().gap_3();
        // A CurseForge or Modrinth mod switched to a source: say what changes.
        let site = self.follow.as_ref().and_then(|(file, _)| {
            let store = self.store.read(cx);
            let a = store.instance(&self.root)?.addon_by_file(file)?;
            (a.project_id != 0 && a.source.is_none()).then(|| if a.mr_project.is_empty() { "CurseForge" } else { "Modrinth" })
        });
        if let Some(site) = site {
            body = body.child(
                widgets::notice_bar(theme.info, IconName::Info, cx).child(div().flex_1().min_w_0().text_sm().child(format!(
                    "This mod comes from {site} now. Once it follows a source, its updates come from there instead of {site}. Stop following puts it back."
                ))),
            );
        }
        body = body.child(
            v_flex()
                .gap_1()
                .child(widgets::caption("A GitHub repo or release, a Maven repo, a Jenkins job, or a link to a .jar", cx))
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.link).small()))
                        .child(
                            Button::new("src-look")
                                .small()
                                .outline()
                                .label("Look up")
                                .loading(matches!(self.stage, Stage::Looking))
                                .on_click(cx.listener(|this, _, _, cx| this.look_up(cx))),
                        ),
                ),
        );
        body = body.child(
            Checkbox::new("src-pre")
                .small()
                .label("Include pre-releases")
                .checked(self.pre)
                .on_click(cx.listener(|this, v: &bool, _, cx| {
                    this.pre = *v;
                    if matches!(this.stage, Stage::Found(_)) {
                        this.stage = Stage::Idle;
                        this.look_up(cx);
                    }
                    cx.notify();
                })),
        );
        match &self.stage {
            Stage::Idle => {}
            Stage::Looking => body = body.child(widgets::skeleton_rows(2, rems(2.5), cx)),
            Stage::Failed(e) => {
                body = body.child(
                    h_flex()
                        .gap_2()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(Icon::new(IconName::CircleX).small())
                        .child(div().flex_1().child(e.clone())),
                )
            }
            Stage::Adopt(a) => {
                let what = match &a.matched {
                    sources::Matched::Exact { tag } => format!("Your jar is the file from release {tag}, byte for byte."),
                    sources::Matched::Baseline { version, tag } if tag.is_empty() => {
                        format!("Your jar says it's version {version}. {} has no release that old, so every release there counts as newer.", a.source.publisher)
                    }
                    sources::Matched::Baseline { version, tag } => {
                        format!("Your jar says it's version {version}; the nearest release is {tag}. Only later releases count as updates.")
                    }
                    sources::Matched::Unknown => "Your jar doesn't say its version, so the newest release counts as an update.".into(),
                };
                let next = match &a.newest {
                    Some(c) => format!("{} is out ({}). It shows in Updates; nothing changes until you update.", c.version, c.file_name),
                    None => "It's up to date. Later releases will show in Updates.".into(),
                };
                body = body.child(
                    v_flex()
                        .gap_1()
                        .p_3()
                        .rounded(px(crate::theme::RADIUS_LG))
                        .bg(theme.muted)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().text_sm().font_semibold().child(a.name.clone()))
                                .child(widgets::chip(theme.info).xsmall().child(a.source.kind.label())),
                        )
                        .child(div().text_sm().child(what))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(next)),
                );
                if !sources::trusted(&a.source) {
                    body = body.child(trust_note(&a.source.publisher, a.source.kind.label(), None, &theme));
                }
                body = body.child(widgets::caption(
                    "Your jar stays as it is. Stop following puts the mod back to where it came from.",
                    cx,
                ));
            }
            Stage::Found(r) => {
                let r = (**r).clone();
                let chosen = self.chosen.and_then(|i| r.candidates.get(i)).cloned();
                if r.candidates.is_empty() {
                    body = body.child(widgets::caption("This source has no jar files for Hopper to add.", cx));
                } else if self.picking {
                    let why = if r.pick.is_none() {
                        "None of these say they fit this instance. Pick one only if you know it does; Hopper reads the jar after download and stops if it doesn't."
                    } else {
                        "Several files fit. Pick the one you want; updates keep to the same kind of file."
                    };
                    body = body.child(widgets::caption(why, cx)).child(self.picker(&r, cx));
                } else if let Some(c) = &chosen {
                    body = body.child(
                        h_flex()
                            .gap_3()
                            .p_3()
                            .rounded(px(crate::theme::RADIUS_LG))
                            .bg(theme.muted)
                            .child(Icon::new(IconName::Package))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(div().text_sm().font_semibold().truncate().child(c.file_name.clone()))
                                            .child(widgets::chip(theme.info).xsmall().child(r.source.kind.label())),
                                    )
                                    .child(widgets::caption(
                                        [
                                            c.title.clone(),
                                            size(c.size),
                                            match c.fit {
                                                Fit::Good => "fits this instance".into(),
                                                _ => "Hopper checks the jar after download".into(),
                                            },
                                        ]
                                        .into_iter()
                                        .filter(|s| !s.is_empty())
                                        .collect::<Vec<_>>()
                                        .join(" · "),
                                        cx,
                                    )),
                            )
                            .when(r.candidates.len() > 1, |d| {
                                d.child(
                                    Button::new("src-other")
                                        .ghost()
                                        .small()
                                        .label("Other file…")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.picking = true;
                                            cx.notify();
                                        })),
                                )
                            }),
                    );
                }
                if !sources::trusted(&r.source) && chosen.is_some() {
                    let by = chosen.as_ref().map(|c| c.author.clone()).filter(|a| !a.is_empty());
                    body = body.child(trust_note(&r.source.publisher, r.source.kind.label(), by, &theme));
                }

            }
        }
        body
    }
}

fn open(store: Entity<Store>, root: PathBuf, follow: Option<(String, String)>, link: String, window: &mut Window, cx: &mut App) {
    let form = cx.new(|cx| SourceForm::new(store, root, follow, &link, window, cx));
    window.open_dialog(cx, move |dialog, window, cx| {
        let f2 = form.clone();
        let (title, foot) = {
            let f = form.read(cx);
            (f.title(), f.footer())
        };
        dialog
            .title(title)
            .w(rems(38.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(form.clone())
            .footer(foot)
            .on_ok(move |_, window, cx| SourceForm::ok(&f2, window, cx))
    });
}

/// Add content › "From GitHub or a link…". Asks to turn the setting on
/// first when it's off.
pub fn open_add(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    with_opt_in(store, window, cx, move |store, window, cx| open(store, root.clone(), None, String::new(), window, cx));
}

/// A mod's "Follow a source…" (also "Change source…").
pub fn open_follow(store: Entity<Store>, root: PathBuf, file: String, name: String, link: String, window: &mut Window, cx: &mut App) {
    // The jar's own links (its sources or issues page) suggest the repo.
    let link = if link.is_empty() {
        store
            .read(cx)
            .instance(&root)
            .and_then(|i| sources::suggest(i, &file))
            .unwrap_or_default()
    } else {
        link
    };
    with_opt_in(store, window, cx, move |store, window, cx| {
        open(store, root.clone(), Some((file.clone(), name.clone())), link.clone(), window, cx)
    });
}

/// "Stop following": the mod goes back to where it came from.
pub fn unfollow(store: Entity<Store>, root: PathBuf, file: String, cx: &mut App) {
    store.update(cx, |s, _| {
        s.spawn_instance_job(&root.clone(), format!("Stop following a source for {file}"), move |job| {
            let mut inst = hopper::instances::load(&root).ok_or_else(|| anyhow::anyhow!("the instance is gone"))?;
            hopper::sources::unfollow(&mut inst, &file)?;
            job.detail(format!("{file} no longer follows its source."));
            Ok(Msg::InstanceChanged(root))
        });
    });
}

/// Runs `then` when mods from links are allowed; otherwise explains and
/// offers to turn it on (Settings › Mod sources).
fn with_opt_in(
    store: Entity<Store>,
    window: &mut Window,
    cx: &mut App,
    then: impl Fn(Entity<Store>, &mut Window, &mut App) + 'static,
) {
    if store.read(cx).settings().allow_sources {
        then(store, window, cx);
        return;
    }
    let then = std::rc::Rc::new(then);
    window.open_dialog(cx, move |dialog, window, _| {
        let (s, t) = (store.clone(), then.clone());
        dialog
            .title("Allow mods from GitHub and other links?")
            .w(rems(30.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_2()
                    .text_sm()
                    .child("Hopper can add mods from a GitHub release, a Maven repo, a Jenkins job or a download link, and keep them updated from there.")
                    .child("These files skip CurseForge's and Modrinth's checks. Hopper shows who publishes each one before it adds it, and never runs anything from a download.")
                    .child("You can turn this off again in Settings › Mod sources."),
            )
            .footer(footer("Turn on", true))
            .on_ok(move |_, window, cx| {
                s.update(cx, |st, cx| st.update_settings(|x| x.allow_sources = true, cx));
                let (s, t) = (s.clone(), t.clone());
                // The next dialog opens once this one has closed.
                window.defer(cx, move |window, cx| t(s, window, cx));
                true
            })
    });
}

/// "Turn on updates" for a mod whose source came with a pack, a share code
/// or a sync: the same once-per-source confirm as adding one.
pub fn confirm_trust(store: Entity<Store>, root: PathBuf, src: sources::ModSource, window: &mut Window, cx: &mut App) {
    with_opt_in(store, window, cx, move |store, window, cx| {
        let (src, root) = (src.clone(), root.clone());
        window.open_dialog(cx, move |dialog, window, cx| {
            let (s2, st, r2) = (src.clone(), store.clone(), root.clone());
            let theme = cx.theme().clone();
            dialog
                .title(format!("Get updates from {}?", src.publisher))
                .w(rems(32.).to_pixels(window.rem_size()))
                .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(format!(
                            "This mod came with a pack or a sync, pinned to the file its sender had. Hopper checks {} for updates once you say so.",
                            src.page()
                        )))
                        .child(trust_note(&src.publisher, src.kind.label(), None, &theme)),
                )
                .footer(footer("Trust and check for updates", true))
                .on_ok(move |_, window, cx| {
                    if let Err(e) = sources::trust(&s2) {
                        window.push_notification(gpui_kit::component::notification::Notification::error(format!("{e:#}")), cx);
                    }
                    st.update(cx, |s, cx| {
                        s.check_updates(&r2, true);
                        cx.notify();
                    });
                    true
                })
        });
    });
}

/// "Find sources for N unknown mods": each unknown jar with the repo its
/// own metadata links to, ticked when there is one, followed in one go.
pub struct FindForm {
    store: Entity<Store>,
    root: PathBuf,
    rows: Option<Vec<(sources::Unknown, bool)>>,
}

impl FindForm {
    fn new(store: Entity<Store>, root: PathBuf, cx: &mut Context<Self>) -> Self {
        if let Some(inst) = store.read(cx).instance(&root).cloned() {
            cx.spawn(async move |this, cx| {
                let found = cx.background_spawn(async move { sources::unknown_mods(&inst) }).await;
                this.update(cx, |this, cx| {
                    this.rows = Some(found.into_iter().map(|u| {
                        let on = u.link.is_some();
                        (u, on)
                    }).collect());
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        FindForm { store, root, rows: None }
    }

    fn picked(&self) -> Vec<(String, String)> {
        self.rows
            .iter()
            .flatten()
            .filter(|(u, on)| *on && u.link.is_some())
            .map(|(u, _)| (u.file_name.clone(), u.link.clone().unwrap_or_default()))
            .collect()
    }

    fn ok(form: &Entity<Self>, cx: &mut App) -> bool {
        let this = form.read(cx);
        let picked = this.picked();
        if picked.is_empty() {
            return false;
        }
        let (store, root) = (this.store.clone(), this.root.clone());
        let n = picked.len();
        store.update(cx, |s, _| {
            s.spawn_instance_job(&root.clone(), format!("Finding sources for {n} mods"), move |job| {
                let mut inst = hopper::instances::load(&root).ok_or_else(|| anyhow::anyhow!("the instance is gone"))?;
                let mut done = 0;
                for (i, (file, link)) in picked.iter().enumerate() {
                    job.check_cancelled()?;
                    job.progress(i, n);
                    job.detail(format!("{file}: {link}"));
                    // Each source is asked once; answers are cached, so a
                    // batch stays inside GitHub's limits.
                    match sources::plan_adopt(&inst, file, link, false) {
                        Ok(a) => {
                            sources::trust(&a.source)?;
                            sources::adopt(&mut inst, file, &a)?;
                            done += 1;
                        }
                        Err(e) => job.note(format!("{file}: {e:#}")),
                    }
                }
                job.detail(format!("{done} of {n} mods follow a source now."));
                Ok(Msg::InstanceChanged(root))
            });
        });
        true
    }
}

impl Render for FindForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let Some(rows) = &self.rows else {
            return v_flex().gap_2().child(widgets::skeleton_rows(3, rems(2.5), cx));
        };
        if rows.is_empty() {
            return v_flex().child(widgets::caption("Every mod here is from CurseForge, Modrinth or a source already.", cx));
        }
        let with = rows.iter().filter(|(u, _)| u.link.is_some()).count();
        v_flex()
            .gap_2()
            .child(div().text_sm().text_color(theme.muted_foreground).child(
                format!(
                    "{with} of {} jars link to their GitHub repo in their own files. Tick the ones to follow; each jar stays as it is, and later releases show in Updates.",
                    rows.len()
                ),
            ))
            .child(v_flex().gap_0p5().children(rows.iter().enumerate().map(|(i, (u, on))| {
                let has = u.link.is_some();
                let row = h_flex()
                    .id(("find-src", i))
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(px(crate::theme::RADIUS))
                    .child(
                        Checkbox::new(("find-ck", i))
                            .small()
                            .checked(*on && has)
                            .on_click(cx.listener(move |this, v: &bool, _, cx| {
                                // A jar with no link has nothing to follow.
                                if let Some(r) = this.rows.as_mut().and_then(|r| r.get_mut(i))
                                    && r.0.link.is_some()
                                {
                                    r.1 = *v;
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(u.name.clone()))
                            .child(div().text_xs().truncate().text_color(theme.muted_foreground).child(u.file_name.clone())),
                    )
                    .child(match &u.link {
                        Some(l) => div()
                            .text_xs()
                            .truncate()
                            .max_w(rems(16.))
                            .child(l.trim_start_matches("https://github.com/").to_string())
                            .into_any_element(),
                        None => div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("No link in the jar")
                            .into_any_element(),
                    });
                widgets::row_hover(row, cx).into_any_element()
            })))
    }
}

/// Opens "Find sources for N unknown mods".
pub fn open_find(store: Entity<Store>, root: PathBuf, window: &mut Window, cx: &mut App) {
    with_opt_in(store, window, cx, move |store, window, cx| {
        let form = cx.new(|cx| FindForm::new(store.clone(), root.clone(), cx));
        window.open_dialog(cx, move |dialog, window, cx| {
            let f2 = form.clone();
            let n = form.read(cx).picked().len();
            dialog
                .title("Find sources for unknown mods")
                .w(rems(38.).to_pixels(window.rem_size()))
                .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
                .child(form.clone())
                .footer(footer(if n == 1 { "Follow 1 source" } else { "Follow sources" }, n > 0))
                .on_ok(move |_, _, cx| FindForm::ok(&f2, cx))
        });
    });
}
