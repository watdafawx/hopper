//! The modpack update preview: what the update will do to mods and config
//! files, with a choice per file where both you and the pack changed it.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
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
use hopper::cf;
use hopper::packdiff::{FileAction, ModAction, UpdatePlan};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Collapsible groups of the preview.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    Mods(ModAction),
    Files(FileAction),
}

pub struct UpdatePreview {
    store: Entity<Store>,
    root: PathBuf,
    from: String,
    plan: Option<Result<UpdatePlan, String>>,
    /// Conflicting files where the pack's copy wins.
    take: HashSet<String>,
    open: HashSet<Group>,
    /// Diffs shown under conflicting files (`None` while not text).
    diffs: HashMap<String, Option<String>>,
    /// (Minecraft now, Minecraft after) when the update changes it.
    mc_change: Option<(String, String)>,
    /// Back up every world before updating (on when Minecraft changes).
    backup_worlds: bool,
}

impl UpdatePreview {
    fn plan(&mut self, file: cf::File, cx: &mut Context<Self>) {
        let Some(inst) = self.store.read(cx).instance(&self.root).cloned() else {
            return;
        };
        let job = self
            .store
            .read(cx)
            .jobs()
            .start(format!("Comparing {} with {}", inst.name, file.display_name));
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let r = hopper::packdiff::plan(&inst, &file, &job);
                    job.finish(&r.as_ref().map(|_| ()).map_err(|e| anyhow::anyhow!("{e:#}")));
                    if r.is_ok() {
                        job.detail("Ready to review");
                    }
                    r.map_err(|e| format!("{e:#}"))
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(p) = &r {
                    this.take = p.default_take();
                    if p.count(FileAction::Conflict) > 0 {
                        this.open.insert(Group::Files(FileAction::Conflict));
                    }
                }
                this.plan = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle(&mut self, g: Group, cx: &mut Context<Self>) {
        if !self.open.remove(&g) {
            self.open.insert(g);
        }
        cx.notify();
    }

    fn toggle_diff(&mut self, rel: String, cx: &mut Context<Self>) {
        if self.diffs.remove(&rel).is_some() {
            cx.notify();
            return;
        }
        let d = match &self.plan {
            Some(Ok(p)) => p.diff(&self.root, &rel).unwrap_or(None),
            _ => None,
        };
        self.diffs.insert(rel, d);
        cx.notify();
    }

    fn apply(view: &Entity<Self>, cx: &mut App) -> bool {
        let v = view.read(cx);
        let Some(Ok(plan)) = &v.plan else {
            return false;
        };
        let overrides = plan.decisions(&v.take);
        let (store, root, file) = (v.store.clone(), v.root.clone(), plan.file.clone());
        let backup = v.backup_worlds;
        store.update(cx, |s, cx| {
            s.update_pack(&root, file, Some(overrides), backup);
            cx.notify();
        });
        true
    }

    fn group_header(
        &self,
        g: Group,
        tag: Tag,
        label: String,
        n: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let open = self.open.contains(&g);
        let theme = cx.theme();
        h_flex()
            .id(ElementId::Name(format!("group:{label}").into()))
            .gap_2()
            .py_1()
            .cursor_pointer()
            .when(n > 0, |r| r.hover(|s| s.text_color(theme.primary)))
            .when(n == 0, |r| r.opacity(0.5))
            .child(
                Icon::new(if open && n > 0 {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .small()
                .text_color(theme.muted_foreground),
            )
            .child(div().w_8().text_sm().font_semibold().child(n.to_string()))
            .child(tag)
            .child(div().text_sm().child(label))
            .on_click(cx.listener(move |this, _, _, cx| {
                if n > 0 {
                    this.toggle(g, cx)
                }
            }))
    }

    fn mods_group(&self, plan: &UpdatePlan, a: ModAction, cx: &mut Context<Self>) -> AnyElement {
        let (tag, label) = match a {
            ModAction::Add => (Tag::success().xsmall().child("Added"), "mods the pack adds"),
            ModAction::Update => (Tag::info().xsmall().child("Updated"), "mods updated"),
            ModAction::Remove => (
                Tag::danger().xsmall().child("Removed"),
                "mods the pack drops (moved to backups)",
            ),
        };
        let g = Group::Mods(a);
        let rows: Vec<_> = plan.mods.iter().filter(|m| m.action == a).cloned().collect();
        let header = self.group_header(g, tag, label.to_string(), rows.len(), cx);
        let open = self.open.contains(&g);
        v_flex()
            .child(header)
            .when(open, |c| {
                c.child(
                    v_flex().pl(rems(4.25)).pb_2().children(rows.into_iter().map(|m| {
                        let detail = match a {
                            ModAction::Update => format!("{}  →  {}", m.from, m.to),
                            ModAction::Add => m.to.clone(),
                            ModAction::Remove => m.from.clone(),
                        };
                        h_flex()
                            .gap_3()
                            .py_0p5()
                            .child(div().w_56().flex_none().text_sm().truncate().child(m.name))
                            .child(widgets::caption(detail, cx).text_xs().min_w_0())
                    })),
                )
            })
            .into_any_element()
    }

    fn files_group(&self, plan: &UpdatePlan, a: FileAction, cx: &mut Context<Self>) -> AnyElement {
        let (tag, label) = match a {
            FileAction::Add => (Tag::success().xsmall().child("New"), "new files from the pack"),
            FileAction::Update => (
                Tag::info().xsmall().child("Updated"),
                "files the pack changed and you didn't",
            ),
            FileAction::Merge => (
                Tag::info().xsmall().child("Merged"),
                "files both changed in different places: combined",
            ),
            FileAction::KeepYours => (
                Tag::secondary().xsmall().child("Yours"),
                "files only you changed: kept",
            ),
            FileAction::Remove => (
                Tag::danger().xsmall().child("Removed"),
                "files the pack dropped (moved to backups)",
            ),
            FileAction::KeepDropped => (
                Tag::secondary().xsmall().child("Kept"),
                "files the pack dropped but you changed: kept",
            ),
            FileAction::Conflict => (
                Tag::warning().xsmall().child("Both changed"),
                "files you and the pack both changed",
            ),
        };
        let g = Group::Files(a);
        let rows: Vec<String> = plan
            .files
            .iter()
            .filter(|f| f.action == a)
            .map(|f| f.rel.clone())
            .collect();
        let header = self.group_header(g, tag, label.to_string(), rows.len(), cx);
        let open = self.open.contains(&g);
        let body: Option<AnyElement> = open.then(|| {
            if a == FileAction::Conflict {
                self.conflicts(rows, cx)
            } else {
                v_flex()
                    .pl(rems(4.25))
                    .pb_2()
                    .children(rows.into_iter().map(|r| {
                        widgets::caption(r, cx).text_xs().py_0p5()
                    }))
                    .into_any_element()
            }
        });
        v_flex().child(header).children(body).into_any_element()
    }

    fn conflicts(&self, rows: Vec<String>, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let all = rows.clone();
        let all2 = rows.clone();
        v_flex()
            .pl(rems(4.25))
            .pb_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_2()
                    .pb_1()
                    .child(widgets::caption(
                        "Pack content folders (quests, scripts) default to the pack's copy; settings default to yours. Replaced files go to .hopper-backups.",
                        cx,
                    ).text_xs().flex_1())
                    .child(
                        Button::new("keep-all")
                            .xsmall()
                            .ghost()
                            .label("Keep all mine")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                for r in &all {
                                    this.take.remove(r);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("take-all")
                            .xsmall()
                            .ghost()
                            .label("Take all the pack's")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.take.extend(all2.iter().cloned());
                                cx.notify();
                            })),
                    ),
            )
            .children(rows.into_iter().map(|rel| {
                let theirs = self.take.contains(&rel);
                let diff = self.diffs.get(&rel).cloned();
                let (r1, r2) = (rel.clone(), rel.clone());
                v_flex()
                    .child(
                        h_flex()
                            .gap_2()
                            .py_0p5()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .truncate()
                                    .child(rel.clone()),
                            )
                            .child(
                                Button::new(SharedString::from(format!("diff:{rel}")))
                                    .xsmall()
                                    .ghost()
                                    .label(if diff.is_some() { "Hide changes" } else { "Changes" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_diff(r1.clone(), cx)
                                    })),
                            )
                            .child(
                                TabBar::new(SharedString::from(format!("pick:{rel}")))
                                    .segmented()
                                    .xsmall()
                                    .selected_index(usize::from(theirs))
                                    .child(Tab::new().label("Mine"))
                                    .child(Tab::new().label("Pack's"))
                                    .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                                        if *ix == 1 {
                                            this.take.insert(r2.clone());
                                        } else {
                                            this.take.remove(&r2);
                                        }
                                        cx.notify();
                                    })),
                            ),
                    )
                    .when_some(diff, |c, d| {
                        let lines: Vec<String> = d
                            .unwrap_or("Not a text file.".into())
                            .lines()
                            .take(400)
                            .map(str::to_string)
                            .collect();
                        c.child(
                            v_flex()
                                .my_1()
                                .p_3()
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .font_family(theme.mono_font_family.clone())
                                .text_xs()
                                .children(lines.into_iter().map(|l| {
                                    let color = if l.starts_with('+') && !l.starts_with("+++") {
                                        theme.success
                                    } else if l.starts_with('-') && !l.starts_with("---") {
                                        theme.danger
                                    } else if l.starts_with("@@") {
                                        theme.info
                                    } else {
                                        theme.muted_foreground
                                    };
                                    div()
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .text_color(color)
                                        .child(l.replace('\t', "    "))
                                })),
                        )
                    })
            }))
            .into_any_element()
    }
}

impl Render for UpdatePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Window-tall once loaded; the list takes the rest and scrolls (a
        // max height on the scroll area alone doesn't scroll).
        let total_h = (window.viewport_size().height - window.rem_size() * 8.).max(window.rem_size() * 16.);
        let loaded = matches!(self.plan, Some(Ok(_)));
        let theme = cx.theme();
        let body: AnyElement = match &self.plan {
            None => h_flex()
                .gap_2()
                .py_4()
                .child(Spinner::new().small())
                .child(widgets::caption(
                    "Downloading both pack versions and comparing them with your instance",
                    cx,
                ))
                .into_any_element(),
            Some(Err(e)) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            Some(Ok(plan)) => {
                let plan = plan.clone();
                let mods: Vec<AnyElement> = [ModAction::Add, ModAction::Update, ModAction::Remove]
                    .into_iter()
                    .map(|a| self.mods_group(&plan, a, cx))
                    .collect();
                let files: Vec<AnyElement> = [
                    FileAction::Conflict,
                    FileAction::Merge,
                    FileAction::Update,
                    FileAction::Add,
                    FileAction::KeepYours,
                    FileAction::Remove,
                    FileAction::KeepDropped,
                ]
                .into_iter()
                .map(|a| self.files_group(&plan, a, cx))
                .collect();
                let theme = cx.theme();
                let mc_change = self.mc_change.clone();
                let backup = self.backup_worlds;
                v_flex()
                    .gap_4()
                    .when_some(mc_change, |c, (from, to)| {
                        c.child(
                            v_flex()
                                .gap_2()
                                .px_3()
                                .py_2p5()
                                .rounded(theme.radius)
                                .border_1()
                                .border_color(theme.warning.opacity(0.5))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_sm()
                                        .text_color(theme.warning)
                                        .child(Icon::new(IconName::TriangleAlert).small())
                                        .child(format!("Minecraft {from} → {to}: worlds are converted and can't go back.")),
                                )
                                .child(
                                    gpui_kit::component::checkbox::Checkbox::new("backup-worlds")
                                        .label("Back up every world first")
                                        .checked(backup)
                                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                                            this.backup_worlds = *v;
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
                    .when(!plan.base_known, |c| {
                        c.child(
                            h_flex()
                                .gap_2()
                                .text_sm()
                                .text_color(theme.warning)
                                .child(Icon::new(IconName::TriangleAlert).small())
                                .child("The installed pack version couldn't be downloaded, so every file that differs counts as both changed."),
                        )
                    })
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Mods"))
                            .children(mods)
                            .child(widgets::caption(
                                format!("{} mods you added stay as they are.", plan.kept_mods),
                                cx,
                            ).text_xs().pl(rems(1.5))),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(div().font_semibold().child("Config and other files"))
                            .children(files),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .when(loaded, |c| c.h(total_h))
            .gap_3()
            .child(widgets::caption(
                match &self.plan {
                    Some(Ok(p)) => format!("{}  →  {}", self.from, p.file.file_name),
                    _ => self.from.clone(),
                },
                cx,
            ))
            .child(
                div()
                    .id("update-preview")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body),
            )
    }
}

/// Previews updating an instance's modpack to `file`, then updates it.
pub fn open(store: Entity<Store>, root: PathBuf, file: cf::File, window: &mut Window, cx: &mut App) {
    let Some(inst) = store.read(cx).instance(&root).cloned() else {
        return;
    };
    let from = inst
        .pack
        .as_ref()
        .map(|p| if p.file_name.is_empty() { p.version.clone() } else { p.file_name.clone() })
        .unwrap_or_default();
    // Minecraft versions look like 1.21.1 or 26.3; the file's list also
    // names loaders.
    let to_mc = file
        .game_versions
        .iter()
        .find(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .cloned();
    let mc_change = to_mc
        .filter(|to| !inst.game_version.is_empty() && *to != inst.game_version)
        .map(|to| (inst.game_version.clone(), to));
    let backup_worlds = mc_change.is_some();
    let view = cx.new(|cx| {
        let mut v = UpdatePreview {
            store,
            root,
            from,
            plan: None,
            take: HashSet::new(),
            open: HashSet::new(),
            diffs: HashMap::new(),
            mc_change,
            backup_worlds,
        };
        v.plan(file, cx);
        v
    });
    let title = format!("Update {}", inst.name);
    window.open_dialog(cx, move |dialog, window, cx| {
        let ready = matches!(view.read(cx).plan, Some(Ok(_)));
        let v2 = view.clone();
        dialog
            .title(title.clone())
            .w(rems(54.).to_pixels(window.rem_size()))
            .child(view.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("upd-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(
                        Button::new("upd-go")
                            .primary()
                            .icon(IconName::Download)
                            .label("Update")
                            .disabled(!ready),
                    )),
            )
            .on_ok(move |_, _, cx| UpdatePreview::apply(&v2, cx))
    });
}
