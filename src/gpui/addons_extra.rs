//! Extras around the addon table: global resource pack folders (Paxi and
//! friends), required mods that are missing, shader notes and mod profiles.

use crate::addons::{AddonsEvent, AddonsView};
use crate::crash_help::callout;
use crate::store::open_path;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::deps::Missing;
use hopper::instance_ops::PackFile;
use hopper::instances::Instance;
use std::path::PathBuf;

/// Global resource pack folders and the one shown.
#[derive(Default)]
pub struct GlobalPacks {
    /// (folder relative to the instance, loader name)
    pub dirs: Vec<(&'static str, &'static str)>,
    /// `None` shows the instance's own packs.
    pub picked: Option<&'static str>,
    /// Each folder's packs, read in the background (a big Paxi folder
    /// takes a few hundred ms of zip reading), so switching tabs is instant.
    pub packs: std::collections::HashMap<&'static str, Vec<PackFile>>,
    /// How many packs each of `dirs` holds.
    pub counts: Vec<usize>,
    /// The instance `packs` was read for.
    pub root: Option<PathBuf>,
}

/// Hopper's own working files in an instance folder (backups, staging,
/// trash, pending installs), never shown as content.
pub fn is_hopper_file(path: &std::path::Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().starts_with(".hopper-"))
}

/// A folder's packs, without Hopper's own files.
pub fn pack_files(dir: &std::path::Path) -> Vec<PackFile> {
    let mut v = hopper::instance_ops::pack_files(dir);
    v.retain(|p| !is_hopper_file(&p.path));
    v
}

/// A folder's data packs, without Hopper's own files.
pub fn datapacks(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut v = hopper::instance_ops::datapacks(dir);
    v.retain(|p| !is_hopper_file(p));
    v
}

/// Required mods that are missing, for the mods folder as of `mtime`.
#[derive(Default)]
pub struct MissingDeps {
    pub root: Option<PathBuf>,
    pub mtime: Option<std::time::SystemTime>,
    pub list: Vec<Missing>,
    /// Jars that are the same mod twice.
    pub dups: Vec<hopper::install::Duplicate>,
    /// Old files a running game holds, leaving when it closes.
    pub queued: usize,
}

impl AddonsView {
    /// Reads which global pack folders an instance has.
    pub(crate) fn load_globals(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        let dirs = hopper::instance_ops::global_resourcepack_dirs(inst);
        let picked = self
            .globals
            .picked
            .filter(|p| dirs.iter().any(|(d, _)| d == p));
        let counts = dirs
            .iter()
            .map(|(d, _)| datapacks(&inst.root.join(d)).len())
            .collect();
        // Same instance: the old lists stay up until the new ones are read.
        let packs = if self.globals.root.as_ref() == Some(&inst.root) {
            std::mem::take(&mut self.globals.packs)
        } else {
            Default::default()
        };
        self.globals = GlobalPacks {
            dirs: dirs.clone(),
            picked,
            packs,
            counts,
            root: Some(inst.root.clone()),
        };
        if dirs.is_empty() || self.class != cf::CLASS_RESOURCE_PACKS {
            return;
        }
        let root = inst.root.clone();
        let inst = inst.clone();
        cx.spawn(async move |this, cx| {
            let r2 = root.clone();
            let read = cx
                .background_spawn(async move {
                    dirs.iter()
                        .map(|(d, _)| (*d, pack_files(&r2.join(d))))
                        .collect::<std::collections::HashMap<_, _>>()
                })
                .await;
            this.update(cx, |this, cx| {
                if this.globals.root.as_ref() != Some(&root) {
                    return;
                }
                for (i, (d, _)) in this.globals.dirs.iter().enumerate() {
                    if let (Some(v), Some(n)) = (read.get(d), this.globals.counts.get_mut(i)) {
                        *n = v.len();
                    }
                }
                this.globals.packs = read;
                cx.notify();
            })
            .ok();
            // Then which projects they are, their versions and updates.
            let all: Vec<PackFile> = this
                .update(cx, |this, _| this.globals.packs.values().flatten().cloned().collect())
                .unwrap_or_default();
            let all2 = all.clone();
            let urls = cx
                .background_spawn(async move { crate::pack_lookup::look_up(&all2, cf::CLASS_RESOURCE_PACKS) })
                .await;
            this.update(cx, |this, cx| {
                this.store.update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                cx.notify();
            })
            .ok();
            if cx.background_spawn(async move { crate::pack_lookup::check_versions(&all, &inst) }).await {
                this.update(cx, |_, cx| cx.notify()).ok();
            }
        })
        .detach();
    }

    /// The packs of the global folder shown.
    fn picked_packs(&self) -> &[PackFile] {
        self.globals
            .picked
            .and_then(|p| self.globals.packs.get(p))
            .map_or(&[], Vec::as_slice)
    }

    /// Looks for missing required mods when the mods folder changed.
    pub(crate) fn want_missing(&mut self, inst: &Instance, cx: &mut Context<Self>) {
        let dir = inst.mods_dir();
        let mtime = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
        if self.missing.root.as_ref() == Some(&inst.root) && self.missing.mtime == mtime {
            return;
        }
        // Same instance: the old list stays up while the folder is read
        // again, so the banner doesn't flash on every update.
        let same = self.missing.root.as_ref() == Some(&inst.root);
        self.missing = MissingDeps {
            root: Some(inst.root.clone()),
            mtime,
            list: if same { std::mem::take(&mut self.missing.list) } else { Vec::new() },
            dups: if same { std::mem::take(&mut self.missing.dups) } else { Vec::new() },
            queued: hopper::locked::list(&inst.root).len(),
        };
        let root = inst.root.clone();
        let snapshot = inst.clone();
        cx.spawn(async move |this, cx| {
            let (list, dups) = cx
                .background_spawn(async move {
                    (hopper::deps::missing(&dir), hopper::install::duplicates(&snapshot))
                })
                .await;
            this.update(cx, |this, cx| {
                if this.missing.root.as_ref() == Some(&root) && this.missing.mtime == mtime {
                    this.missing.dups = dups;
                    // Wrong versions are warned about when an update causes
                    // them; packs often ship a few that run anyway.
                    this.missing.list = list.into_iter().filter(|m| m.wrong_version.is_none()).collect();
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// "This instance | Paxi (12) | …" above Resource packs.
    pub(crate) fn pack_tabs(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_RESOURCE_PACKS || self.globals.dirs.is_empty() {
            return None;
        }
        let ix = self
            .globals
            .picked
            .and_then(|p| self.globals.dirs.iter().position(|(d, _)| *d == p))
            .map_or(0, |i| i + 1);
        let dirs = self.globals.dirs.clone();
        Some(
            h_flex()
                .px_6()
                .child(
                    TabBar::new("rp-folders")
                        .segmented()
                        .small()
                        .selected_index(ix)
                        .child(Tab::new().label("This instance"))
                        .children(dirs.iter().zip(&self.globals.counts).map(|((_, name), n)| {
                            Tab::new().label(format!("{name} {n}"))
                        }))
                        .on_click(cx.listener(move |this, ix: &usize, _, cx| {
                            let picked = ix.checked_sub(1).and_then(|i| dirs.get(i)).map(|(d, _)| *d);
                            this.globals.picked = picked;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// A global folder's packs, shown instead of the table.
    pub(crate) fn global_packs(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_RESOURCE_PACKS {
            return None;
        }
        let rel = self.globals.picked?;
        let dir = inst.root.join(rel);
        let theme = cx.theme();
        let packs = self.picked_packs();
        let loading = !self.globals.packs.contains_key(rel);
        let n = packs.len();
        let d2 = dir.clone();
        let root = inst.root.clone();
        let store = self.store.read(cx);
        let rows = packs.iter().map(|p| {
            let (p1, p2, root) = (p.path.clone(), p.path.clone(), root.clone());
            let hit = crate::pack_lookup::found(p).flatten();
            let pid = hit.as_ref().map(|f| f.project.id);
            // The project's icon, else the pack's own pack.png.
            let icon = hit
                .as_ref()
                .and_then(|f| f.project.icon())
                .and_then(|u| store.image(u))
                .map(|p| p.to_path_buf())
                .or_else(|| p.icon.clone());
            let gid = |k: &str| ElementId::Name(format!("{k}:{}", p.path.display()).into());
            h_flex()
                .id(ElementId::Name(format!("gp:{}", p.path.display()).into()))
                .h(rems(3.5))
                .px_3()
                .gap_3()
                .rounded(theme.radius)
                .hover(|s| s.bg(theme.list_hover))
                .child(match icon {
                    Some(i) => div()
                        .size_10()
                        .flex_none()
                        .rounded(theme.radius)
                        .overflow_hidden()
                        .child(img(i).size_full())
                        .into_any_element(),
                    None => widgets::thumb(None, rems(2.5), cx),
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(
                            h_flex()
                                .gap_2()
                                .min_w_0()
                                .child(
                                    div()
                                        .id(gid("gp-name"))
                                        .text_sm()
                                        .font_medium()
                                        .truncate()
                                        .when_some(pid, |d, pid| {
                                            d.cursor_pointer()
                                                .hover(|s| s.text_color(theme.primary))
                                                .on_click(cx.listener(move |this, _, _, cx| this.open_peek(pid, cx)))
                                        })
                                        .child(hit.as_ref().map_or_else(|| p.name.clone(), |f| f.project.name.clone())),
                                )
                                .children(hit.as_ref().map(|f| widgets::site_tag(f.project.modrinth.is_some()))),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .min_w_0()
                                .children(hit.as_ref().and_then(|f| crate::pack_lookup::version_tag(f, p, cx)))
                                // One line: a pack.mcmeta's description may run to several.
                                .when(!p.description.is_empty(), |c| {
                                    let one = p.description.split_whitespace().collect::<Vec<_>>().join(" ");
                                    c.child(widgets::caption(one, cx).text_xs().truncate())
                                }),
                        ),
                )
                .child(
                    widgets::caption(
                        format!(
                            "{} · {:.1} MB",
                            hopper::time::relative_secs(p.modified),
                            p.size as f64 / 1_048_576.0
                        ),
                        cx,
                    )
                    .text_xs()
                    .flex_none(),
                )
                .children(hit.as_ref().and_then(|f| {
                    crate::pack_lookup::update_button(gid("gp-update"), &self.store, root.clone(), p, f)
                }))
                .when_some(pid, |r, pid| {
                    r.child(
                        Button::new(gid("gp-page"))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Info)
                            .tooltip("Details")
                            .on_click(cx.listener(move |this, _, _, cx| this.open_peek(pid, cx))),
                    )
                })
                .child(
                    Button::new(ElementId::Name(format!("gp-open:{}", p.path.display()).into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::FolderOpen)
                        .tooltip("Open")
                        .on_click(move |_, _, _| open_path(&p1)),
                )
                .child(
                    Button::new(ElementId::Name(format!("gp-rm:{}", p.path.display()).into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Trash)
                        .tooltip("Move to .hopper-backups")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if let Err(e) = hopper::sync::backup_into(&root, &p2) {
                                window.push_notification(Notification::error(format!("{e:#}")), cx);
                            }
                            if let Some(rel) = this.globals.picked
                                && let Some(v) = this.globals.packs.get_mut(rel)
                            {
                                v.retain(|p| p.path.exists());
                                let len = v.len();
                                if let Some(i) = this.globals.dirs.iter().position(|(d, _)| *d == rel)
                                    && let Some(n) = this.globals.counts.get_mut(i)
                                {
                                    *n = len;
                                }
                            }
                            cx.notify();
                        })),
                )
        });
        let inst_root = inst.root.clone();
        Some(
            v_flex()
                .size_full()
                .gap_3()
                .child(
                    h_flex()
                        .px_6()
                        .gap_2()
                        .child(widgets::caption(
                            if loading {
                                format!("Reading the packs in {rel}…")
                            } else {
                                format!("{n} packs in {rel}: the mod loads them for every world, always on, in its load order.")
                            },
                            cx,
                        ))
                        .child(div().flex_1())
                        .child(
                            Button::new("gp-add")
                                .primary()
                                .small()
                                .icon(IconName::Plus)
                                .label("Add resource packs")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(AddonsEvent::AddTo {
                                        root: inst_root.clone(),
                                        class: cf::CLASS_RESOURCE_PACKS,
                                        dest: rel.to_string(),
                                    })
                                })),
                        )
                        .child(
                            Button::new("gp-folder")
                                .outline()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("Open folder")
                                .on_click(move |_, _, _| {
                                    let _ = std::fs::create_dir_all(&d2);
                                    open_path(&d2)
                                }),
                        ),
                )
                .child(
                    v_flex()
                        .id("global-packs")
                        .flex_1()
                        .min_h_0()
                        .px_6()
                        .pb_4()
                        .children(rows)
                        .overflow_y_scrollbar(),
                )
                .into_any_element(),
        )
    }

    /// "N mods are in twice", with Keep newest; and old files waiting for
    /// the game to let go of them.
    pub(crate) fn duplicate_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_MODS || self.missing.root.as_ref() != Some(&inst.root) {
            return None;
        }
        let dups = &self.missing.dups;
        let queued = self.missing.queued;
        if dups.is_empty() && queued == 0 {
            return None;
        }
        let theme = cx.theme();
        if dups.is_empty() {
            return Some(
                div()
                    .px_6()
                    .child(
                        callout(theme.info, cx).child(
                            div().text_sm().child(format!(
                                "{queued} old file{} the game still has open will be removed when it closes.",
                                if queued == 1 { "" } else { "s" }
                            )),
                        ),
                    )
                    .into_any_element(),
            );
        }
        let extra: Vec<String> = dups.iter().flat_map(|d| d.files[1..].to_vec()).collect();
        let rows = dups.iter().map(|d| {
            let older = d.files[1..].to_vec();
            let what = d.name.clone();
            h_flex()
                .gap_3()
                .text_sm()
                .child(div().font_semibold().child(d.name.clone()))
                .child(
                    widgets::caption(
                        format!("keep {}  ·  remove {}", d.files[0], older.join(", ")),
                        cx,
                    )
                    .flex_1()
                    .text_xs(),
                )
                .child(
                    Button::new(SharedString::from(format!("keep-newest:{}", d.files[0])))
                        .xsmall()
                        .outline()
                        .label("Keep newest")
                        .tooltip("Move the older jars to Hopper's backups")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.remove(older.clone(), what.clone(), window, cx)
                        })),
                )
                .into_any_element()
        });
        let n = dups.len();
        Some(
            div()
                .px_6()
                .child(
                    callout(theme.warning, cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_semibold()
                                        .text_color(theme.warning)
                                        .child(format!(
                                            "{n} mod{} in twice: the game won't start with both",
                                            if n == 1 { " is" } else { "s are" }
                                        )),
                                )
                                .when(n > 1, |r| {
                                    r.child(
                                        Button::new("keep-newest-all")
                                            .small()
                                            .primary()
                                            .label("Keep newest of all")
                                            .on_click(cx.listener(move |this, _, window, cx| {
                                                this.remove(extra.clone(), format!("{n} older duplicates"), window, cx)
                                            })),
                                    )
                                }),
                        )
                        .when(queued > 0, |c| {
                            c.child(widgets::caption(
                                format!("{queued} old file{} will be removed when the game closes.", if queued == 1 { "" } else { "s" }),
                                cx,
                            ))
                        })
                        .child(
                            div()
                                .id("dup-rows")
                                .when(n > 6, |d| d.h(rems(10.5)))
                                .overflow_y_scrollbar()
                                .child(v_flex().gap_2().children(rows)),
                        ),
                )
                .into_any_element(),
        )
    }

    /// "N required mods are missing", with Enable or Find for each.
    pub(crate) fn missing_banner(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_MODS
            || self.missing.root.as_ref() != Some(&inst.root)
            || self.missing.list.is_empty()
        {
            return None;
        }
        let theme = cx.theme();
        let n = self.missing.list.len();
        let mut installable: Vec<String> = self
            .missing
            .list
            .iter()
            .filter(|m| m.disabled_provider.is_none())
            .map(|m| m.needs.clone())
            .collect();
        installable.dedup();
        // One row per mod, naming everything it lacks.
        let mut groups: Vec<(&str, Vec<&hopper::deps::Missing>)> = Vec::new();
        for m in &self.missing.list {
            match groups.iter_mut().find(|(name, _)| *name == m.mod_name) {
                Some((_, v)) => v.push(m),
                None => groups.push((&m.mod_name, vec![m])),
            }
        }
        let rows: Vec<AnyElement> = groups
            .iter()
            .map(|(name, needs)| {
                // The installed mod's own page, when it came from a site.
                let pid = inst
                    .addons
                    .iter()
                    .find(|a| a.file_name.eq_ignore_ascii_case(&needs[0].file_name))
                    .map_or(0, |a| a.project_id);
                let chips = needs.iter().map(|m| {
                    let action = match &m.disabled_provider {
                        Some(f) => {
                            let f = f.clone();
                            Button::new(SharedString::from(format!("enable:{}:{}", name, m.needs)))
                                .xsmall()
                                .outline()
                                .label("Enable it")
                                .tooltip(f.clone())
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.enable_provider(&f, window, cx)
                                }))
                        }
                        None => {
                            let id = m.needs.clone();
                            Button::new(SharedString::from(format!("get:{}:{}", name, m.needs)))
                                .xsmall()
                                .primary()
                                .icon(IconName::Download)
                                .label("Install")
                                .tooltip("Download it from Modrinth or CurseForge, with what it needs")
                                .on_click(cx.listener(move |this, _, _, cx| this.install_missing(vec![id.clone()], cx)))
                        }
                    };
                    let dep = m.needs.clone();
                    h_flex()
                        .gap_1p5()
                        .child(
                            div()
                                .id(SharedString::from(format!("dep:{}:{}", name, m.needs)))
                                .font_family(theme.mono_font_family.clone())
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.primary))
                                .tooltip(|w, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Details").build(w, cx)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| this.peek_mod_id(dep.clone(), cx)))
                                .child(m.needs.clone()),
                        )
                        .when(!m.versions.is_empty() && m.versions != "*", |r| {
                            r.child(widgets::caption(m.versions.clone(), cx).text_xs())
                        })
                        .child(action)
                });
                h_flex()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_1()
                    .text_sm()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .id(SharedString::from(format!("needer:{name}")))
                                    .font_semibold()
                                    .when(pid != 0, |d| {
                                        d.cursor_pointer()
                                            .hover(|s| s.text_color(theme.primary))
                                            .tooltip(|w, cx| {
                                                gpui_kit::component::tooltip::Tooltip::new("Details").build(w, cx)
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| this.open_peek(pid, cx)))
                                    })
                                    .child(name.to_string()),
                            )
                            .child(widgets::caption("needs", cx)),
                    )
                    .children(chips)
                    .into_any_element()
            })
            .collect();
        let n_rows = rows.len();
        Some(
            div()
                .px_6()
                .child(
                    callout(theme.warning, cx)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    div()
                                        .flex_1()
                                        .font_semibold()
                                        .text_color(theme.warning)
                                        .child(format!(
                                            "{n} required mod{} missing: the game will stop at loading",
                                            if n == 1 { " is" } else { "s are" }
                                        )),
                                )
                                .when(installable.len() > 1, |r| {
                                    r.child(
                                        Button::new("install-all-missing")
                                            .small()
                                            .primary()
                                            .icon(IconName::Download)
                                            .label(format!("Install all {}", installable.len()))
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.install_missing(installable.clone(), cx)
                                            })),
                                    )
                                }),
                        )
                        // Past six, the rows scroll so the mods table stays in view.
                        .child(
                            div()
                                .id("missing-rows")
                                .when(n_rows > 6, |d| d.h(rems(10.5)))
                                .overflow_y_scrollbar()
                                .child(v_flex().gap_2().children(rows)),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Downloads mods jars ask for by id; one that can't be found is
    /// reported and the rest still go in. A failure offers a search.
    pub(crate) fn install_missing(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        let Some(mut inst) = store.instance(&root).cloned() else { return };
        let title = match ids.as_slice() {
            [one] => format!("Installing {one}"),
            many => format!("Installing {} missing mods", many.len()),
        };
        store.spawn_instance_job(&root, title, move |job| {
            let mut got = 0;
            for id in &ids {
                match hopper::install::install_mod_id(&mut inst, id, job) {
                    Ok(added) => {
                        got += 1;
                        job.note(format!("Installed {}", added.join(", ")));
                    }
                    Err(e) => job.note(format!("{id}: {e:#}. Try Browse to find it.")),
                }
            }
            if got == 0 {
                anyhow::bail!("Couldn't find {} on Modrinth or CurseForge. Search for it in Browse.", ids.join(", "));
            }
            Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
        });
    }

    /// The project panel for a mod id a jar asks for, looked up like
    /// Install does; one that can't be found is searched for in Browse.
    fn peek_mod_id(&mut self, mod_id: String, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let Some(inst) = self.store.read(cx).instance(&root).cloned() else { return };
        cx.spawn(async move |this, cx| {
            let id = mod_id.clone();
            let r = cx
                .background_spawn(async move { hopper::install::project_for_mod_id(&inst, &id) })
                .await;
            this.update(cx, |this, cx| match r {
                Ok(Some(m)) => this.open_peek(m.id, cx),
                _ => cx.emit(AddonsEvent::Find { root, text: mod_id }),
            })
            .ok();
        })
        .detach();
    }

    fn enable_provider(&mut self, file: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let base = file.trim_end_matches(".disabled").to_string();
        let r = self.store.update(cx, |s, cx| {
            let tracked = s
                .instance(&root)
                .is_some_and(|i| i.addons.iter().any(|a| a.file_name.eq_ignore_ascii_case(&base)));
            let r = if tracked {
                s.set_enabled(&root, std::slice::from_ref(&base), true)
            } else {
                let dir = root.join("mods");
                std::fs::rename(dir.join(file), dir.join(&base)).map_err(|e| format!("{e:#}"))
            };
            cx.notify();
            r
        });
        // An untracked jar leaves the instance as it was: re-read the folder.
        self.resync(cx);
        if let Err(e) = r {
            window.push_notification(Notification::error(e), cx);
        }
    }

    /// Shader packs: which is on in game, and whether a loader runs them.
    pub(crate) fn shader_notes(&self, inst: &Instance, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.class != cf::CLASS_SHADERS {
            return None;
        }
        let theme = cx.theme();
        // Read from disk when the table was built, not per frame.
        let (active, has_loader) = self.shader_state();
        let forge = inst.loader.starts_with("forge");
        let want = if forge { "Oculus" } else { "Iris" };
        let root = inst.root.clone();
        Some(
            v_flex()
                .px_6()
                .gap_2()
                .when_some(active, |c, sp| {
                    c.child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.info)
                            .child(Icon::new(IconName::Sun).small())
                            .child(format!(
                                "In use in game: {}",
                                hopper::instance_ops::strip_formatting(&sp)
                            )),
                    )
                })
                .when(!has_loader, |c| {
                    c.child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.warning)
                            .child(Icon::new(IconName::TriangleAlert).small())
                            .child(format!(
                                "Shader packs need {want} to run; this instance doesn't have it."
                            ))
                            .child(
                                Button::new("find-shader-loader")
                                    .xsmall()
                                    .outline()
                                    .label(format!("Find {want}"))
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(AddonsEvent::Find {
                                            root: root.clone(),
                                            text: want.to_lowercase(),
                                        })
                                    })),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    /// Named sets of disabled mods to switch between.
    pub(crate) fn profiles_button(&self, inst: &Instance, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let names: Vec<String> = inst.mod_profiles.iter().map(|p| p.name.clone()).collect();
        let view = cx.entity();
        Button::new("profiles")
            .small()
            .outline()
            .icon(IconName::Layers)
            .label("Profiles")
            .dropdown_caret(true)
            .dropdown_menu(move |menu, _, _| {
                let mut menu = menu.label("Switch which mods are enabled");
                for n in &names {
                    let (v, n) = (view.clone(), n.clone());
                    menu = menu.item(PopupMenuItem::new(format!("Use “{n}”")).on_click(
                        move |_, _, cx| v.update(cx, |this, cx| this.use_profile(n.clone(), cx)),
                    ));
                }
                let v = view.clone();
                menu.separator().item(
                    PopupMenuItem::new("Save current as…")
                        .icon(IconName::Plus)
                        .on_click(move |_, window, cx| save_profile_dialog(v.clone(), window, cx)),
                )
            })
    }

    fn use_profile(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        if let Some(mut inst) = store.instance(&root).cloned() {
            store.spawn_job(format!("Switching to profile {name}"), move |job| {
                let n = hopper::install::apply_profile(&mut inst, &name)?;
                job.detail(format!("{n} mods changed"));
                Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
            });
        }
    }
}

fn save_profile_dialog(view: Entity<AddonsView>, window: &mut Window, cx: &mut App) {
    let name = cx.new(|cx| InputState::new(window, cx).placeholder("Profile name"));
    window.open_dialog(cx, move |dialog, window, _| {
        let (view, n2) = (view.clone(), name.clone());
        dialog
            .title("Save mod profile")
            .w(rems(26.).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_2()
                    .child(Input::new(&name))
                    .child(div().text_sm().child(
                        "Remembers which mods are disabled now, to switch back to later.",
                    )),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("p-cancel").outline().label("Cancel")))
                    .child(DialogAction::new().child(Button::new("p-save").primary().label("Save"))),
            )
            .on_ok(move |_, window, cx| {
                let n = n2.read(cx).value().trim().to_string();
                if n.is_empty() {
                    return false;
                }
                let (store, root) = {
                    let v = view.read(cx);
                    (v.store.clone(), v.root.clone())
                };
                let Some(root) = root else { return true };
                let r = store.update(cx, |s, cx| {
                    let r = s.save_profile(&root, &n);
                    cx.notify();
                    r
                });
                if let Err(e) = r {
                    window.push_notification(Notification::error(e), cx);
                }
                true
            })
    });
}
