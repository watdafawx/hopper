//! Versions and changelogs of a modpack or one of its mods: what changed up
//! to the newest version, the installed one, and every other version to
//! switch to.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::cf;
use hopper::install::{ChangelogEntry, Update};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct VersionsView {
    store: Entity<Store>,
    root: PathBuf,
    project_id: u64,
    pack: bool,
    name: String,
    icon: String,
    from: String,
    installed_file: u64,
    /// A locked mod: its version can't be changed until it's unlocked.
    locked: bool,
    /// The pending update's file name.
    to: Option<String>,
    entries: Option<Result<Vec<(ChangelogEntry, SharedString)>, String>>,
    all: Vec<cf::File>,
    /// Changelogs of other versions, opened on demand.
    others: HashMap<u64, Option<Result<SharedString, String>>>,
}

impl VersionsView {
    fn load(&mut self, update: Option<cf::File>, cx: &mut Context<Self>) {
        let Some(inst) = self.store.read(cx).instance(&self.root).cloned() else {
            return;
        };
        let (pid, installed, pack) = (self.project_id, self.installed_file, self.pack);
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    hopper::install::changelogs(&inst, pid, installed, update.as_ref(), pack)
                })
                .await;
            this.update(cx, |this, cx| {
                this.entries = Some(match r {
                    Ok((entries, mut all)) => {
                        // Server packs are separate downloads, not versions.
                        all.retain(|f| f.is_server_pack != Some(true));
                        if !pack {
                            let (root, files) = (this.root.clone(), all.clone());
                            this.store.update(cx, |s, cx| {
                                if s.note_update(&root, &files, pid) {
                                    cx.notify();
                                }
                            });
                        }
                        this.all = all;
                        Ok(entries
                            .into_iter()
                            .map(|e| {
                                let md = hopper::richtext::to_markdown(&e.blocks).into();
                                (e, md)
                            })
                            .collect())
                    }
                    Err(e) => Err(format!("{e:#}")),
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_other(&mut self, f: cf::File, cx: &mut Context<Self>) {
        if self.others.contains_key(&f.id) {
            self.others.remove(&f.id);
            cx.notify();
            return;
        }
        self.others.insert(f.id, None);
        cx.spawn(async move |this, cx| {
            let id = f.id;
            let r = cx
                .background_spawn(async move { hopper::source::changelog(&f) })
                .await
                .map(|b| SharedString::from(hopper::richtext::to_markdown(&b)))
                .map_err(|e| format!("{e:#}"));
            this.update(cx, |this, cx| {
                this.others.insert(id, Some(r));
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn install(view: &Entity<Self>, file: cf::File, window: &mut Window, cx: &mut App) {
        let v = view.read(cx);
        let (store, root, pack) = (v.store.clone(), v.root.clone(), v.pack);
        let (project_id, name, from) = (v.project_id, v.name.clone(), v.from.clone());
        if pack {
            window.close_dialog(cx);
            crate::pack_update::open(store, root, file, window, cx);
            return;
        } else {
            let u = Update {
                project_id,
                name,
                from,
                file,
            };
            store.read(cx).run_updates(&root, vec![u]);
        }
        window.close_dialog(cx);
    }

    fn changelog(&self, id: ElementId, md: SharedString, cx: &App) -> AnyElement {
        if md.trim().is_empty() {
            return widgets::caption("No changelog for this version.", cx)
                .italic()
                .into_any_element();
        }
        let (store, root) = (self.store.clone(), self.root.clone());
        // The list around it scrolls; the text must not catch the wheel.
        TextView::markdown(id, md)
            .selectable(true)
            .scrollable(false)
            .on_link_click(move |url, _, window, cx| open_link(&store, &root, url, window, cx))
            .into_any_element()
    }
}

impl Render for VersionsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The list scrolls inside what's left of the window under the
        // dialog's title and this header, so every version can be reached.
        // The dialog's own title and padding take about 6rem of the window.
        let total_h = (window.viewport_size().height - window.rem_size() * 8.).max(window.rem_size() * 16.);
        let store = self.store.read(cx);
        let theme = cx.theme();
        let icon = store.image(&self.icon).map(Path::to_path_buf);
        let line = match &self.to {
            Some(to) => format!("{}  →  {to}", self.from),
            None => format!("Installed: {}", self.from),
        };
        let header = h_flex()
            .gap_3()
            .child(widgets::thumb(icon.as_deref(), rems(2.5), cx))
            .child(
                v_flex()
                    .min_w_0()
                    .child(div().font_semibold().truncate().child(self.name.clone()))
                    .child(widgets::caption(line, cx).text_xs()),
            );
        let lock_note = self.locked.then(|| {
            let (store, root, file) = (self.store.clone(), self.root.clone(), self.from.clone());
            h_flex()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .bg(theme.muted)
                .child(Icon::new(IconName::Lock).small().text_color(theme.muted_foreground))
                .child(
                    div()
                        .flex_1()
                        .text_sm()
                        .child("Locked: Hopper and CurseForge keep this version. Unlock it to update or switch."),
                )
                .child(
                    Button::new("unlock")
                        .small()
                        .outline()
                        .icon(IconName::LockOpen)
                        .label("Unlock")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            crate::addons::set_locks(&store, root.clone(), vec![file.clone()], false, window, cx);
                            this.locked = false;
                            cx.notify();
                        })),
                )
        });
        let locked = self.locked;
        let body: AnyElement = match &self.entries {
            None => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Fetching versions", cx))
                .into_any_element(),
            Some(Err(e)) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(e.clone())
                .into_any_element(),
            Some(Ok(entries)) => {
                let shown: std::collections::HashSet<&str> =
                    entries.iter().map(|(e, _)| e.name.as_str()).collect();
                // Without an update offer (a locked mod has none) newer files
                // are only in "Other versions"; count them too.
                let newer_others: Vec<&cf::File> = self
                    .all
                    .iter()
                    .filter(|f| f.id > self.installed_file && !shown.contains(f.display_name.as_str()))
                    .collect();
                let behind = entries.iter().filter(|(e, _)| !e.installed).count() + newer_others.len();
                let status = match behind {
                    0 => "Up to date".to_string(),
                    1 => "1 version behind".to_string(),
                    n => format!("{n} versions behind · newest first"),
                };
                let latest = self
                    .all
                    .iter()
                    .find(|f| Some(&f.file_name) == self.to.as_ref())
                    .or_else(|| newer_others.iter().copied().max_by_key(|f| f.id))
                    .cloned();
                let others: Vec<cf::File> = self
                    .all
                    .iter()
                    .filter(|f| !shown.contains(f.display_name.as_str()))
                    .cloned()
                    .collect();
                let cards = entries.iter().enumerate().map(|(i, (e, md))| {
                    let target = i == 0 && !e.installed;
                    let file = self.all.iter().find(|f| f.display_name == e.name).cloned();
                    let view = cx.entity();
                    // The installed one: the same file again, to get back
                    // what it ships (a pack's configs, a mod's jar).
                    let pack = self.pack;
                    let reinstall = file.clone().filter(|_| e.installed).map(|f| {
                        let view = view.clone();
                        Button::new(SharedString::from(format!("reinstall:{}", f.id)))
                            .small()
                            .outline()
                            .icon(IconName::RotateCcw)
                            .label("Reinstall")
                            .tooltip(if pack {
                                "Install this version again: lists every config that differs from the pack's so you can reset it, and puts back missing files"
                            } else {
                                "Download this version again in place of the installed file"
                            })
                            .disabled(locked)
                            .on_click(move |_, window, cx| VersionsView::install(&view, f.clone(), window, cx))
                    });
                    let action = file.filter(|_| !e.installed).map(|f| {
                        let b = Button::new(SharedString::from(format!("install:{}", f.id)))
                            .small()
                            .label(if target { "Update to this" } else { "Install this version" });
                        let b = if target { b.primary() } else { b.outline() };
                        b.disabled(locked).on_click(move |_, window, cx| {
                            VersionsView::install(&view, f.clone(), window, cx)
                        })
                    });
                    v_flex()
                        .gap_2()
                        .p_4()
                        .rounded(theme.radius_lg)
                        .border_1()
                        .border_color(if target { theme.primary } else { theme.border })
                        .when(e.installed, |c| c.bg(theme.list_active))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().font_semibold().truncate().child(e.name.clone()))
                                .child(Tag::secondary().xsmall().child(e.release))
                                .when(e.installed, |r| {
                                    r.child(Tag::success().xsmall().child("Installed"))
                                })
                                .child(div().flex_1())
                                .child(widgets::caption(hopper::time::relative(&e.date), cx).text_xs())
                                .children(action)
                                .children(reinstall),
                        )
                        .child(self.changelog(
                            ElementId::Name(format!("cl:{}", e.name).into()),
                            md.clone(),
                            cx,
                        ))
                });
                let other_cards = others.into_iter().map(|f| {
                    let older = f.id < self.installed_file;
                    let open = self.others.get(&f.id);
                    let view = cx.entity();
                    let (f1, f2) = (f.clone(), f.clone());
                    let body: Option<AnyElement> = open.map(|r| match r {
                        None => h_flex()
                            .gap_2()
                            .child(Spinner::new().xsmall())
                            .child(widgets::caption("Loading the changelog", cx))
                            .into_any_element(),
                        Some(Ok(md)) => self.changelog(
                            ElementId::Name(format!("other-cl:{}", f.id).into()),
                            md.clone(),
                            cx,
                        ),
                        Some(Err(e)) => div()
                            .text_sm()
                            .text_color(theme.danger)
                            .child(e.clone())
                            .into_any_element(),
                    });
                    v_flex()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .rounded(theme.radius_lg)
                        .bg(theme.muted)
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    h_flex()
                                        .id(ElementId::Name(format!("other:{}", f.id).into()))
                                        .flex_1()
                                        .min_w_0()
                                        .gap_2()
                                        .cursor_pointer()
                                        .child(
                                            Icon::new(if open.is_some() {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .small()
                                            .text_color(theme.muted_foreground),
                                        )
                                        .child(div().text_sm().font_medium().truncate().child(f.display_name.clone()))
                                        .child(Tag::secondary().xsmall().child(f.release_name()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.open_other(f1.clone(), cx)
                                        })),
                                )
                                .child(widgets::caption(hopper::time::relative(&f.file_date), cx).text_xs())
                                .child(
                                    Button::new(SharedString::from(format!("other-install:{}", f.id)))
                                        .xsmall()
                                        .outline()
                                        .label(if older { "Downgrade to this" } else { "Install this version" })
                                        .disabled(locked)
                                        .on_click(move |_, window, cx| {
                                            VersionsView::install(&view, f2.clone(), window, cx)
                                        }),
                                ),
                        )
                        .children(body)
                });
                let view = cx.entity();
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_medium()
                                    .text_color(if behind == 0 { theme.success } else { theme.primary })
                                    .child(status),
                            )
                            .when_some(latest, |r, f| {
                                r.child(
                                    Button::new("update-latest")
                                        .small()
                                        .primary()
                                        .icon(IconName::Download)
                                        .label("Update")
                                        .disabled(locked)
                                        .on_click(move |_, window, cx| {
                                            VersionsView::install(&view, f.clone(), window, cx)
                                        }),
                                )
                            }),
                    )
                    .children(cards)
                    .when(self.all.len() > shown_len(entries), |r| {
                        r.child(widgets::caption("Other versions", cx).text_xs().pt_2())
                    })
                    .children(other_cards)
                    .into_any_element()
            }
        };
        // Once loaded, a column the window's height whose list takes the
        // rest and scrolls (a max height on the scroll area doesn't scroll).
        let loaded = matches!(self.entries, Some(Ok(_)));
        v_flex()
            .when(loaded, |c| c.h(total_h))
            .gap_4()
            .child(header)
            .children(lock_note)
            .child(
                // Until then the column has no height, and a flex_1 body
                // would shrink to nothing and hide the spinner or error.
                div()
                    .id("versions-body")
                    .when(loaded, |d| d.flex_1().min_h_0())
                    .overflow_y_scrollbar()
                    .child(body),
            )
    }
}

/// A link in a changelog: a CurseForge or Modrinth project opens its page
/// beside the instance; anything else (or one that can't be found) opens in
/// the browser.
fn open_link(store: &Entity<Store>, root: &Path, url: &str, window: &mut Window, cx: &mut App) {
    let Some(link) = hopper::source::project_link(url) else {
        cx.open_url(url);
        return;
    };
    let (store, root, url) = (store.clone(), root.to_path_buf(), url.to_string());
    window
        .spawn(cx, async move |cx| {
            let r = cx
                .background_spawn(async move { hopper::source::project_from_link(&link) })
                .await;
            cx.update(|window, cx| match r {
                Ok(project) => {
                    window.close_dialog(cx);
                    store.update(cx, |_, cx| cx.emit(crate::store::StoreEvent::Peek { root, project }));
                }
                Err(e) => {
                    log::warn!("changelog link {url}: {e:#}");
                    cx.open_url(&url);
                }
            })
            .ok();
        })
        .detach();
}

fn shown_len(entries: &[(ChangelogEntry, SharedString)]) -> usize {
    entries.len()
}

/// Opens versions and changelogs for a project in an instance (its modpack
/// when `pack`), with the pending update if any.
pub fn open(
    store: Entity<Store>,
    root: PathBuf,
    project_id: u64,
    pack: bool,
    update: Option<cf::File>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(inst) = store.read(cx).instance(&root).cloned() else {
        return;
    };
    let (name, icon, from, installed_file, locked) = if pack {
        let Some(p) = inst.pack.clone() else { return };
        let from = if p.file_name.is_empty() {
            p.version.clone()
        } else {
            p.file_name.clone()
        };
        (p.name, inst.icon.clone().unwrap_or_default(), from, p.file_id, false)
    } else {
        let Some(a) = inst.addons.iter().find(|a| a.project_id == project_id) else {
            return;
        };
        (a.name.clone(), a.icon.clone(), a.file_name.clone(), a.file_id, a.locked)
    };
    store.update(cx, |s, _| s.want_image(&icon));
    let to = update.as_ref().map(|f| f.file_name.clone());
    let title = "Versions and changelogs";
    let view = cx.new(|cx| {
        let mut v = VersionsView {
            store,
            root,
            project_id,
            pack,
            name,
            icon,
            from,
            installed_file,
            locked,
            to,
            entries: None,
            all: Vec::new(),
            others: HashMap::new(),
        };
        v.load(update, cx);
        v
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title(title)
            .w(rems(46.).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

/// One version's changelog, from a project's Files list.
struct FileLog {
    store: Entity<Store>,
    /// The instance its links open beside; none opens them in the browser.
    root: Option<PathBuf>,
    file: cf::File,
    md: Option<Result<SharedString, String>>,
}

impl Render for FileLog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let h = (window.viewport_size().height - window.rem_size() * 8.)
            .clamp(window.rem_size() * 10., window.rem_size() * 32.);
        let body: AnyElement = match &self.md {
            None => h_flex()
                .gap_2()
                .child(Spinner::new().small())
                .child(widgets::caption("Loading the changelog", cx))
                .into_any_element(),
            Some(Err(e)) => div().text_sm().text_color(cx.theme().danger).child(e.clone()).into_any_element(),
            Some(Ok(md)) if md.trim().is_empty() => widgets::caption("No changelog for this version.", cx)
                .italic()
                .into_any_element(),
            Some(Ok(md)) => {
                let (store, root) = (self.store.clone(), self.root.clone());
                TextView::markdown("file-log", md.clone())
                    .selectable(true)
                    .scrollable(false)
                    .on_link_click(move |url, _, window, cx| match &root {
                        Some(root) => open_link(&store, root, url, window, cx),
                        None => cx.open_url(url),
                    })
                    .into_any_element()
            }
        };
        v_flex()
            .h(h)
            .gap_3()
            .child(
                v_flex()
                    .child(div().font_semibold().truncate().child(self.file.display_name.clone()))
                    .child(widgets::caption(self.file.release_name(), cx).text_xs()),
            )
            .child(div().id("file-log-scroll").flex_1().min_h_0().overflow_y_scrollbar().child(body))
    }
}

/// Opens the changelog of a project's file known only by id (a pack in a
/// global or Paxi folder, which the instance's versions don't cover): the
/// dialog opens at once and shows the spinner while the file is looked up.
pub fn open_file_id(
    store: Entity<Store>,
    root: Option<PathBuf>,
    project_id: u64,
    file_id: u64,
    name: String,
    window: &mut Window,
    cx: &mut App,
) {
    let placeholder = cf::File {
        id: file_id,
        mod_id: project_id,
        display_name: name,
        ..Default::default()
    };
    let view = cx.new(|cx| {
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move {
                    let f = hopper::source::file(project_id, file_id)?;
                    let md = hopper::richtext::to_markdown(&hopper::source::changelog(&f)?);
                    anyhow::Ok((f, SharedString::from(md)))
                })
                .await;
            this.update(cx, |this: &mut FileLog, cx| {
                match r {
                    Ok((f, md)) => {
                        this.file = f;
                        this.md = Some(Ok(md));
                    }
                    Err(e) => this.md = Some(Err(format!("{e:#}"))),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        FileLog {
            store,
            root,
            file: placeholder,
            md: None,
        }
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Changelog")
            .w(rems(46.).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

/// Opens one version's changelog.
pub fn open_file(store: Entity<Store>, root: Option<PathBuf>, file: cf::File, window: &mut Window, cx: &mut App) {
    let view = cx.new(|cx| {
        let f = file.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_spawn(async move { hopper::source::changelog(&f) })
                .await
                .map(|b| SharedString::from(hopper::richtext::to_markdown(&b)))
                .map_err(|e| format!("{e:#}"));
            this.update(cx, |this: &mut FileLog, cx| {
                this.md = Some(r);
                cx.notify();
            })
            .ok();
        })
        .detach();
        FileLog {
            store,
            root,
            file,
            md: None,
        }
    });
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Changelog")
            .w(rems(46.).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}
