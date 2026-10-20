//! An instance's Worlds and Screenshots tabs.

use crate::store::{Msg, Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::instance_ops::{self, World};
use std::collections::{HashMap, HashSet};
use crate::addons_extra::{datapacks, is_hopper_file};
use std::path::{Path, PathBuf};

/// What these tabs ask the Library to do.
pub enum ContentEvent {
    /// Find content of a class; data packs go to `dest` (instance-relative).
    Browse { class: u32, dest: Option<String> },
    /// Search Browse by name.
    Find { text: String },
}

/// "1 world", "3 worlds".
fn plural(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// A rounded card for one group of things.
fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .gap_3()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
}

// ---- Worlds --------------------------------------------------------------

pub struct WorldsView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    worlds: Vec<World>,
    /// Data packs per world folder.
    packs: HashMap<PathBuf, Vec<PathBuf>>,
    open: HashSet<PathBuf>,
}

impl EventEmitter<ContentEvent> for WorldsView {}

impl WorldsView {
    pub fn new(store: Entity<Store>, _: &mut Window, _: &mut Context<Self>) -> Self {
        Self {
            store,
            root: None,
            worlds: Vec::new(),
            packs: HashMap::new(),
            open: HashSet::new(),
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.root = Some(root);
        self.reload(cx);
    }

    /// Reads worlds and data packs from disk (quick: folder listings).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(inst) = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|i| i.clone_header())
        else {
            return;
        };
        self.worlds = instance_ops::worlds(&inst);
        self.worlds.retain(|w| !is_hopper_file(&w.dir));
        self.packs = self
            .worlds
            .iter()
            .map(|w| {
                (
                    w.dir.clone(),
                    datapacks(&w.dir.join("datapacks")),
                )
            })
            .collect();
        cx.notify();
    }

    fn remove_pack(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if let Err(e) = hopper::sync::backup_into(&root, &path) {
            window.push_notification(Notification::error(format!("{e:#}")), cx);
        }
        self.reload(cx);
    }

    fn pack_rows(&self, packs: &[PathBuf], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        v_flex().gap_0p5().children(packs.iter().map(|p| {
            let path = p.clone();
            h_flex()
                .id(ElementId::Name(format!("dp:{}", p.display()).into()))
                .gap_3()
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .hover(|s| s.bg(theme.list_hover))
                .child(
                    Icon::new(IconName::Package)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .truncate()
                        .child(file_name(p)),
                )
                .child(
                    Button::new(ElementId::Name(format!("dp-rm:{}", p.display()).into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Trash)
                        .tooltip("Move to .hopper-backups")
                        .on_click(
                            cx.listener(move |this, _, w, cx| {
                                this.remove_pack(path.clone(), w, cx)
                            }),
                        ),
                )
        }))
    }

    fn pack_actions(
        &self,
        id: &str,
        dir: PathBuf,
        rel: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        h_flex()
            .gap_2()
            .child(
                Button::new(ElementId::Name(format!("dp-add:{id}").into()))
                    .xsmall()
                    .outline()
                    .icon(IconName::Plus)
                    .label("Add data packs")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(ContentEvent::Browse {
                            class: hopper::cf::CLASS_DATAPACKS,
                            dest: Some(rel.clone()),
                        })
                    })),
            )
            .child(
                Button::new(ElementId::Name(format!("dp-open:{id}").into()))
                    .xsmall()
                    .ghost()
                    .icon(IconName::FolderOpen)
                    .label("Open folder")
                    .on_click(move |_, _, _| {
                        let _ = std::fs::create_dir_all(&dir);
                        open_path(&dir);
                    }),
            )
    }

    fn world_card(&self, w: &World, cx: &mut Context<Self>) -> AnyElement {
        let root = self.root.clone().unwrap_or_default();
        let packs = self.packs.get(&w.dir).cloned().unwrap_or_default();
        let expanded = self.open.contains(&w.dir);
        let id = |p: &str| ElementId::Name(format!("{p}:{}", w.dir.display()).into());
        let (dir1, dir2, dir3) = (w.dir.clone(), w.dir.clone(), w.dir.clone());
        let world = w.clone();
        let store = self.store.clone();
        let permanent = self.store.read(cx).settings().delete_permanently;
        let rel = w
            .dir
            .join("datapacks")
            .strip_prefix(&root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let theme = cx.theme();
        let icon = match &w.icon {
            Some(p) => div()
                .size_12()
                .flex_none()
                .rounded(theme.radius)
                .overflow_hidden()
                .child(img(p.clone()).size_full())
                .into_any_element(),
            None => widgets::thumb(None, rems(3.), cx),
        };
        let name = w.name.clone();
        card(cx)
            .child(
                h_flex()
                    .gap_4()
                    .child(icon)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .child(div().font_semibold().truncate().child(w.name.clone()))
                            .child(widgets::caption(
                                format!(
                                    "Last played {}",
                                    hopper::time::relative_secs(w.last_played)
                                ),
                                cx,
                            )),
                    )
                    .child(
                        Button::new(id("w-open"))
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .tooltip("Open world folder")
                            .on_click(move |_, _, _| open_path(&dir1)),
                    )
                    .child(
                        Button::new(id("w-backup"))
                            .outline()
                            .small()
                            .icon(IconName::Archive)
                            .label("Back up")
                            .tooltip("Zip into .hopper-backups/worlds")
                            .on_click(move |_, _, cx| {
                                let s = store.read(cx);
                                if let Some(full) = s.instance(&root).cloned() {
                                    let world = world.clone();
                                    s.spawn_job(format!("Backing up {}", world.name), move |job| {
                                        let out = instance_ops::backup_world(&full, &world, job)?;
                                        job.detail(format!("Saved {}", out.display()));
                                        Ok(Msg::Nothing)
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new(id("w-delete"))
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip(if permanent { "Delete" } else { "Delete (to the Recycle Bin)" })
                            .on_click(cx.listener(move |_, _, window, cx| {
                                let (dir, name, view) = (dir2.clone(), name.clone(), cx.entity());
                                window.open_alert_dialog(cx, move |alert, _, _| {
                                    let (dir, view) = (dir.clone(), view.clone());
                                    alert
                                        .title(format!("Delete {name}?"))
                                        .description(if permanent {
                                            "The world is deleted for good."
                                        } else {
                                            "The world moves to the Recycle Bin."
                                        })
                                        .confirm()
                                        .ok_text("Delete world")
                                        .ok_variant(ButtonVariant::Danger)
                                        .on_ok(move |_, window, cx| {
                                            let r = if permanent {
                                                hopper::instance_ops::remove_for_good(&dir)
                                            } else {
                                                trash::delete(&dir).map_err(Into::into)
                                            };
                                            if let Err(e) = r {
                                                window.push_notification(
                                                    Notification::error(format!("{e:#}")),
                                                    cx,
                                                );
                                            }
                                            view.update(cx, |v, cx| v.reload(cx));
                                            true
                                        })
                                });
                            })),
                    ),
            )
            .child(
                h_flex()
                    .id(id("w-packs"))
                    .gap_2()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .hover(|s| s.text_color(theme.foreground))
                    .child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .small(),
                    )
                    .child(format!("Data packs ({})", packs.len()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.open.remove(&dir3) {
                            this.open.insert(dir3.clone());
                        }
                        cx.notify();
                    })),
            )
            .when(expanded, |c| {
                let dir = w.dir.join("datapacks");
                c.child(widgets::enter(
                    id("w-packs-body"),
                    v_flex()
                        .gap_2()
                        .pl_6()
                        .child(self.pack_rows(&packs, cx))
                        .child(self.pack_actions(&w.dir.display().to_string(), dir, rel, cx)),
                ))
            })
            .into_any_element()
    }
}

impl Render for WorldsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        let (saves, backups) = (
            root.join("saves"),
            root.join(hopper::sync::BACKUP_DIR).join("worlds"),
        );
        let worlds: Vec<AnyElement> = self
            .worlds
            .clone()
            .iter()
            .map(|w| self.world_card(w, cx))
            .collect();
        let empty = worlds.is_empty();
        v_flex()
            .id("worlds")
            .size_full()
            .px_6()
            .pb_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::caption(plural(self.worlds.len(), "world"), cx))
                    .child(div().flex_1())
                    .child(
                        Button::new("saves")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Saves folder")
                            .on_click(move |_, _, _| open_path(&saves)),
                    )
                    .child(
                        Button::new("world-backups")
                            .outline()
                            .small()
                            .icon(IconName::Archive)
                            .label("Backups")
                            .on_click(move |_, _, _| {
                                let _ = std::fs::create_dir_all(&backups);
                                open_path(&backups)
                            }),
                    )
                    .child(
                        Button::new("get-worlds")
                            .primary()
                            .small()
                            .icon(IconName::Plus)
                            .label("Get worlds")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(ContentEvent::Browse {
                                    class: hopper::cf::CLASS_WORLDS,
                                    dest: None,
                                })
                            })),
                    ),
            )
            .children(worlds)
            .when(empty, |r| {
                r.child(
                    div()
                        .py_10()
                        .flex()
                        .justify_center()
                        .child(widgets::caption(
                            "No worlds yet. Play once to create one.",
                            cx,
                        )),
                )
            })
            .overflow_y_scrollbar()
            .into_any_element()
    }
}

// ---- Screenshots ---------------------------------------------------------

pub struct ScreenshotsView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    shots: Vec<PathBuf>,
    thumbs: HashMap<PathBuf, PathBuf>,
    /// The screenshot shown large.
    open: Option<usize>,
    focus: FocusHandle,
}

impl ScreenshotsView {
    pub fn new(store: Entity<Store>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            store,
            root: None,
            shots: Vec::new(),
            thumbs: HashMap::new(),
            open: None,
            focus: cx.focus_handle(),
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.open = None;
        }
        self.root = Some(root);
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(inst) = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|i| i.clone_header())
        else {
            return;
        };
        self.shots = instance_ops::screenshots(&inst);
        self.shots.retain(|p| !is_hopper_file(p));
        let missing: Vec<PathBuf> = self
            .shots
            .iter()
            .filter(|p| !self.thumbs.contains_key(*p))
            .cloned()
            .collect();
        for p in missing {
            cx.spawn(async move |this, cx| {
                let src = p.clone();
                let t = cx
                    .background_spawn(async move { hopper::imgcache::thumbnail(&src, 480) })
                    .await;
                if let Ok(t) = t {
                    this.update(cx, |this, cx| {
                        this.thumbs.insert(p, t);
                        cx.notify();
                    })
                    .ok();
                }
            })
            .detach();
        }
        cx.notify();
    }

    fn go(&mut self, to: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.open = to.filter(|i| *i < self.shots.len());
        if self.open.is_some() {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn copy(&self, p: &Path, window: &mut Window, cx: &mut App) {
        let format = match p
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("jpg" | "jpeg") => ImageFormat::Jpeg,
            _ => ImageFormat::Png,
        };
        match std::fs::read(p) {
            Ok(bytes) => {
                cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(format, bytes)));
                window.push_notification(Notification::success("Copied the screenshot."), cx);
            }
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
    }

    fn delete(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = self.shots.get(i).cloned() else {
            return;
        };
        match trash::delete(&p) {
            Ok(()) => {
                self.reload(cx);
                // Stay in the viewer, on the neighbour.
                let n = i.min(self.shots.len().saturating_sub(1));
                self.open = (!self.shots.is_empty()).then_some(n);
            }
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
        cx.notify();
    }

    fn viewer(&self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let p = self.shots[i].clone();
        let n = self.shots.len();
        let (p1, p2, p3) = (p.clone(), p.clone(), p.clone());
        v_flex()
            .id("shot-viewer")
            .track_focus(&self.focus)
            .size_full()
            .px_6()
            .pb_6()
            .gap_3()
            .on_key_down(cx.listener(move |this, e: &KeyDownEvent, window, cx| {
                match e.keystroke.key.as_str() {
                    "left" if i > 0 => this.go(Some(i - 1), window, cx),
                    "right" => this.go(Some(i + 1), window, cx),
                    "escape" => this.go(None, window, cx),
                    _ => {}
                }
            }))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("shot-back")
                            .ghost()
                            .small()
                            .icon(IconName::ChevronLeft)
                            .label("All screenshots")
                            .on_click(cx.listener(|this, _, w, cx| this.go(None, w, cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("shot-prev")
                            .outline()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .tooltip("Previous (←)")
                            .disabled(i == 0)
                            .on_click(
                                cx.listener(move |this, _, w, cx| this.go(i.checked_sub(1), w, cx)),
                            ),
                    )
                    .child(widgets::caption(format!("{} of {n}", i + 1), cx).px_2())
                    .child(
                        Button::new("shot-next")
                            .outline()
                            .small()
                            .icon(IconName::ArrowRight)
                            .tooltip("Next (→)")
                            .disabled(i + 1 >= n)
                            .on_click(
                                cx.listener(move |this, _, w, cx| this.go(Some(i + 1), w, cx)),
                            ),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("shot-copy")
                            .outline()
                            .small()
                            .icon(IconName::Copy)
                            .label("Copy")
                            .on_click(cx.listener(move |this, _, w, cx| this.copy(&p1, w, cx))),
                    )
                    .child(
                        Button::new("shot-open")
                            .outline()
                            .small()
                            .icon(IconName::ExternalLink)
                            .label("Open")
                            .on_click(move |_, _, _| open_path(&p2)),
                    )
                    .child(
                        Button::new("shot-folder")
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .tooltip("Show in folder")
                            .on_click(move |_, _, _| {
                                let _ = std::process::Command::new("explorer")
                                    .arg(format!("/select,{}", p3.display()))
                                    .spawn();
                            }),
                    )
                    .child(
                        Button::new("shot-delete")
                            .ghost()
                            .small()
                            .icon(IconName::Trash)
                            .tooltip("Delete (to the Recycle Bin)")
                            .on_click(cx.listener(move |this, _, w, cx| this.delete(i, w, cx))),
                    ),
            )
            .child(widgets::caption(file_name(&p), cx).text_xs())
            .child(widgets::enter(
                ElementId::Name(format!("shot:{}", p.display()).into()),
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(cx.theme().radius_lg)
                    .overflow_hidden()
                    // Pinned to the box rather than sized by it: an img in
                    // flow takes its own height and the bottom gets cut off.
                    .child(img(p).absolute().inset_0().size_full().object_fit(ObjectFit::Contain)),
            ))
            .into_any_element()
    }
}

impl Render for ScreenshotsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        if let Some(i) = self.open.filter(|i| *i < self.shots.len()) {
            return self.viewer(i, cx);
        }
        let dir = root.join("screenshots");
        let theme = cx.theme();
        let tiles = self.shots.iter().enumerate().map(|(i, p)| {
            let thumb = self.thumbs.get(p).cloned();
            div()
                .id(ElementId::Name(format!("tile:{}", p.display()).into()))
                .w_64()
                .h(rems(9.))
                .flex_none()
                .rounded(theme.radius_lg)
                .overflow_hidden()
                .bg(theme.muted)
                .cursor_pointer()
                .border_1()
                .border_color(theme.border)
                .hover(|s| s.border_color(theme.primary))
                .tooltip({
                    let name = file_name(p);
                    move |w, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(name.clone()).build(w, cx)
                    }
                })
                .on_click(cx.listener(move |this, _, w, cx| this.go(Some(i), w, cx)))
                .when_some(thumb, |d, t| {
                    d.child(img(t).size_full().object_fit(ObjectFit::Cover))
                })
        });
        let empty = self.shots.is_empty();
        v_flex()
            .id("screenshots")
            .size_full()
            .px_6()
            .pb_6()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::caption(plural(self.shots.len(), "screenshot"), cx))
                    .child(div().flex_1())
                    .child(
                        Button::new("shots-folder")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Screenshots folder")
                            .on_click(move |_, _, _| {
                                let _ = std::fs::create_dir_all(&dir);
                                open_path(&dir)
                            }),
                    ),
            )
            .child(h_flex().flex_wrap().gap_3().children(tiles))
            .when(empty, |r| {
                r.child(
                    div()
                        .py_10()
                        .flex()
                        .justify_center()
                        .child(widgets::caption(
                            "No screenshots yet. Press F2 in game.",
                            cx,
                        )),
                )
            })
            .overflow_y_scrollbar()
            .into_any_element()
    }
}
