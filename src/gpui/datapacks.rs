//! An instance's Data packs tab: the global loader folders (Paxi,
//! OpenLoader, Global Packs) and each world's `datapacks/`, side by side.

use crate::addons_extra::{is_hopper_file, pack_files};
use crate::content::ContentEvent;
use crate::crash_help::callout;
use crate::store::{Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::instance_ops::{self, PackFile};
use hopper::cf;
use std::path::PathBuf;
use std::time::SystemTime;

use crate::pack_lookup::{self, found};

/// Paxi's optional packs, offered when a world is made rather than loaded
/// into every world.
const PAXI_OPTIONAL: &str = "config/paxi/optional_data";

/// A folder data packs can live in.
#[derive(Clone)]
struct Source {
    /// "Paxi", or the world's name.
    name: String,
    /// Folder relative to the instance, with forward slashes.
    rel: String,
    /// A global folder (every world) rather than one world's.
    global: bool,
    /// The world's icon.
    icon: Option<PathBuf>,
    count: usize,
}

pub struct DatapacksView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    sources: Vec<Source>,
    /// `rel` of the folder shown.
    picked: Option<String>,
    packs: Vec<PackFile>,
    /// No global loader folder or mod in the instance.
    no_loader: bool,
    /// The modpack's icon and a word of its name ("tnp"), for packs that
    /// came with the modpack rather than from a site.
    modpack: Option<(String, String)>,
    /// Folder times as of the last read, to notice files coming and going.
    stamp: Vec<Option<SystemTime>>,
    /// Bumped per read, so a slow one doesn't land over a newer one.
    serial: usize,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ContentEvent> for DatapacksView {}

fn mtime(p: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl DatapacksView {
    pub fn new(store: Entity<Store>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let subs = vec![cx.observe(&store, |this, _, cx| {
            // Installs from Browse and dropped files land while the tab is
            // open: re-read when a folder changed.
            if this.root.is_some() && this.stamps() != this.stamp {
                this.reload(cx);
            }
            // Project icons arriving.
            cx.notify();
        })];
        Self {
            store,
            root: None,
            sources: Vec::new(),
            picked: None,
            packs: Vec::new(),
            no_loader: false,
            modpack: None,
            stamp: Vec::new(),
            serial: 0,
            _subscriptions: subs,
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.picked = None;
            self.packs.clear();
        }
        self.root = Some(root);
        self.reload(cx);
    }

    /// The folder new files go to (dropped on the window).
    pub fn folder(&self) -> Option<String> {
        self.picked.clone()
    }

    fn stamps(&self) -> Vec<Option<SystemTime>> {
        let Some(root) = &self.root else { return Vec::new() };
        std::iter::once(mtime(&root.join("saves")))
            .chain(self.sources.iter().map(|s| mtime(&root.join(&s.rel))))
            .collect()
    }

    /// Lists the folders (quick), then reads the picked one's packs in the
    /// background (zips are opened for pack.mcmeta and pack.png).
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(inst) = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).instance(r))
            .map(|i| i.clone_header())
        else {
            return;
        };
        let jars: Vec<String> = std::fs::read_dir(inst.mods_dir())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_lowercase())
            .collect();
        let count = |rel: &str| {
            instance_ops::datapacks(&inst.root.join(rel))
                .iter()
                .filter(|p| !is_hopper_file(p))
                .count()
        };
        let mut sources: Vec<Source> = instance_ops::GLOBAL_DATAPACK_DIRS
            .iter()
            .filter(|(dir, _, jar)| inst.root.join(dir).is_dir() || jars.iter().any(|j| j.contains(jar)))
            .map(|(dir, name, _)| Source {
                name: name.to_string(),
                rel: dir.to_string(),
                global: true,
                icon: None,
                count: count(dir),
            })
            .collect();
        self.no_loader = sources.is_empty();
        self.modpack = inst.pack.as_ref().and(inst.icon.clone()).and_then(|icon| {
            const COMMON: &[&str] = &["all", "the", "mods", "pack", "modpack", "minecraft", "and", "of"];
            let word = inst
                .name
                .split(|c: char| !c.is_alphanumeric())
                .map(str::to_lowercase)
                .find(|w| w.len() >= 3 && !COMMON.contains(&w.as_str()))?;
            Some((icon, word))
        });
        if inst.root.join(PAXI_OPTIONAL).is_dir() {
            sources.push(Source {
                name: "Paxi optional".into(),
                rel: PAXI_OPTIONAL.into(),
                global: true,
                icon: None,
                count: count(PAXI_OPTIONAL),
            });
        }
        for w in instance_ops::worlds(&inst) {
            if is_hopper_file(&w.dir) {
                continue;
            }
            let rel = format!("saves/{}/datapacks", w.name);
            sources.push(Source {
                count: count(&rel),
                name: w.name,
                rel,
                global: false,
                icon: w.icon,
            });
        }
        if !self.picked.as_ref().is_some_and(|p| sources.iter().any(|s| &s.rel == p)) {
            self.picked = sources.first().map(|s| s.rel.clone());
        }
        self.sources = sources;
        self.stamp = self.stamps();
        self.read_packs(cx);
        cx.notify();
    }

    fn read_packs(&mut self, cx: &mut Context<Self>) {
        self.serial += 1;
        let (Some(root), Some(rel)) = (self.root.clone(), self.picked.clone()) else {
            self.packs.clear();
            return;
        };
        let serial = self.serial;
        cx.spawn(async move |this, cx| {
            let dir = root.join(&rel);
            let packs = cx.background_spawn(async move { pack_files(&dir) }).await;
            this.update(cx, |this, cx| {
                if this.serial == serial {
                    this.packs = packs;
                    this.identify(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Looks up the shown packs not looked up yet on CurseForge and
    /// Modrinth, then fetches the found ones' icons.
    fn identify(&mut self, cx: &mut Context<Self>) {
        let urls: Vec<String> = self
            .packs
            .iter()
            .filter_map(|p| found(p).flatten())
            .filter_map(|f| f.project.icon().map(str::to_string))
            .chain(self.modpack.as_ref().map(|(i, _)| i.clone()))
            .collect();
        self.store.update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
        let want: Vec<PackFile> = self.packs.iter().filter(|p| found(p).is_none()).cloned().collect();
        let Some(inst) = self.root.as_ref().and_then(|r| self.store.read(cx).instance(r)).cloned() else {
            return;
        };
        if want.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let want2 = want.clone();
            let urls = cx
                .background_spawn(async move { pack_lookup::look_up(&want2, cf::CLASS_DATAPACKS) })
                .await;
            this.update(cx, |this, cx| {
                this.store.update(cx, |s, _| urls.iter().for_each(|u| s.want_image(u)));
                cx.notify();
            })
            .ok();
            if cx.background_spawn(async move { pack_lookup::check_versions(&want, &inst) }).await {
                this.update(cx, |_, cx| cx.notify()).ok();
            }
        })
        .detach();
    }

    fn pick(&mut self, rel: String, cx: &mut Context<Self>) {
        if self.picked.as_deref() != Some(&rel) {
            self.picked = Some(rel);
            self.packs.clear();
            self.read_packs(cx);
            cx.notify();
        }
    }

    fn remove(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        if let Err(e) = hopper::sync::backup_into(&root, &path) {
            window.push_notification(Notification::error(format!("{e:#}")), cx);
        }
        self.reload(cx);
    }

    /// Moves a pack to another folder (a world's into Paxi, say).
    fn move_to(&mut self, path: PathBuf, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let dir = root.join(&rel);
        let name = path.file_name().unwrap_or_default().to_owned();
        let dest = dir.join(&name);
        let r = if dest.exists() {
            Err(anyhow::anyhow!("{} is already in {rel}", name.to_string_lossy()))
        } else {
            std::fs::create_dir_all(&dir)
                .and_then(|_| std::fs::rename(&path, &dest))
                .map_err(Into::into)
        };
        match r {
            Ok(()) => window.push_notification(
                Notification::success(format!("Moved {} to {rel}", name.to_string_lossy())),
                cx,
            ),
            Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
        }
        self.reload(cx);
    }

    fn source_row(&self, s: &Source, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let active = self.picked.as_deref() == Some(&s.rel);
        let rel = s.rel.clone();
        let icon = match (&s.icon, s.global) {
            (Some(p), _) => div()
                .size_6()
                .flex_none()
                .rounded(theme.radius)
                .overflow_hidden()
                .child(img(p.clone()).size_full())
                .into_any_element(),
            (None, true) => Icon::new(IconName::Layers)
                .small()
                .text_color(if active { theme.primary } else { theme.muted_foreground })
                .into_any_element(),
            (None, false) => Icon::new(IconName::Globe)
                .small()
                .text_color(theme.muted_foreground)
                .into_any_element(),
        };
        h_flex()
            .id(ElementId::Name(format!("dps:{}", s.rel).into()))
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(theme.radius)
            .cursor_pointer()
            .text_sm()
            .when(active, |d| d.bg(theme.primary.opacity(0.12)).text_color(theme.foreground))
            .when(!active, |d| d.text_color(theme.muted_foreground))
            .hover(|d| d.bg(theme.list_hover).text_color(theme.foreground))
            .child(icon)
            .child(div().flex_1().min_w_0().truncate().child(s.name.clone()))
            .child(div().text_xs().text_color(theme.muted_foreground).child(s.count.to_string()))
            .on_click(cx.listener(move |this, _, _, cx| this.pick(rel.clone(), cx)))
            .into_any_element()
    }

    /// Folders down the left: every-world ones, then each world.
    fn sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let globals: Vec<AnyElement> = self
            .sources
            .clone()
            .iter()
            .filter(|s| s.global)
            .map(|s| self.source_row(s, cx))
            .collect();
        let worlds: Vec<AnyElement> = self
            .sources
            .clone()
            .iter()
            .filter(|s| !s.global)
            .map(|s| self.source_row(s, cx))
            .collect();
        let heading = |t: &'static str, cx: &App| {
            div()
                .px_2()
                .pt_2()
                .text_xs()
                .font_semibold()
                .text_color(cx.theme().muted_foreground)
                .child(t)
        };
        let no_worlds = worlds.is_empty();
        v_flex()
            .id("dp-sources")
            .w(rems(15.))
            .flex_none()
            .gap_0p5()
            .child(heading("EVERY WORLD", cx))
            .children(globals)
            .when(self.no_loader, |c| {
                c.child(
                    callout(cx.theme().info, cx)
                        .mx_1()
                        .child(div().text_xs().child(
                            "Paxi loads data packs into every world, new ones included.",
                        ))
                        .child(
                            Button::new("dp-get-paxi")
                                .xsmall()
                                .outline()
                                .icon(IconName::Search)
                                .label("Find Paxi")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(ContentEvent::Find { text: "paxi".into() })
                                })),
                        ),
                )
            })
            .child(heading("WORLDS", cx))
            .children(worlds)
            .when(no_worlds, |c| {
                c.child(widgets::caption("No worlds yet.", cx).px_2().text_xs())
            })
            .overflow_y_scrollbar()
            .into_any_element()
    }

    fn pack_row(&self, p: &PackFile, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (p1, p2, p3) = (p.path.clone(), p.path.clone(), p.path.clone());
        let id = |k: &str| ElementId::Name(format!("{k}:{}", p.path.display()).into());
        let others: Vec<(String, String)> = self
            .sources
            .iter()
            .filter(|s| Some(&s.rel) != self.picked.as_ref())
            .map(|s| {
                let label = if s.global {
                    format!("{} (every world)", s.name)
                } else {
                    format!("World: {}", s.name)
                };
                (label, s.rel.clone())
            })
            .collect();
        let view = cx.entity();
        let hit = found(p).flatten();
        let project = hit.as_ref().map(|f| f.project.clone());
        let store = self.store.read(cx);
        // A site's icon, else the pack's own pack.png, else the modpack's
        // for a pack that names it ("TNP Data").
        let from_modpack = project.is_none()
            && self.modpack.as_ref().is_some_and(|(_, w)| {
                let text = format!("{} {}", p.name, p.description).to_lowercase();
                text.split(|c: char| !c.is_alphanumeric()).any(|t| t == w)
            });
        let icon = project
            .as_ref()
            .and_then(|m| m.icon())
            .and_then(|u| store.image(u))
            .map(|p| p.to_path_buf())
            .or_else(|| p.icon.clone())
            .or_else(|| {
                self.modpack
                    .as_ref()
                    .filter(|_| from_modpack)
                    .and_then(|(u, _)| store.image(u))
                    .map(|p| p.to_path_buf())
            });
        let title = project.as_ref().map_or_else(|| p.name.clone(), |m| m.name.clone());
        let blurb = if p.description.is_empty() {
            project.as_ref().map(|m| m.summary.clone()).unwrap_or_default()
        } else {
            p.description.clone()
        };
        let site = project.as_ref().map(|m| m.modrinth.is_some());
        let pid = project.as_ref().map(|m| m.id);
        h_flex()
            .id(id("dp"))
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
                                    .id(id("dp-name"))
                                    .text_sm()
                                    .font_medium()
                                    .truncate()
                                    .when_some(pid, |d, pid| {
                                        d.cursor_pointer()
                                            .hover(|s| s.text_color(theme.primary))
                                            .on_click(cx.listener(move |_, _, _, cx| {
                                                cx.emit(ContentEvent::Peek { project: pid })
                                            }))
                                    })
                                    .child(title),
                            )
                            .when_some(site, |r, site| {
                                r.child(widgets::site_tag(site))
                            })
                            .when(from_modpack, |r| {
                                r.child(widgets::caption("from the modpack", cx).text_xs().flex_none())
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .children(hit.as_ref().and_then(|f| pack_lookup::version_tag(f, p, cx)))
                            .when(!blurb.is_empty(), |c| {
                                let one = blurb.split_whitespace().collect::<Vec<_>>().join(" ");
                                c.child(widgets::caption(one, cx).text_xs().truncate())
                            }),
                    ),
            )
            .child(
                widgets::caption(
                    if p.path.is_dir() {
                        format!("{} · folder", hopper::time::relative_secs(p.modified))
                    } else {
                        format!(
                            "{} · {}",
                            hopper::time::relative_secs(p.modified),
                            instance_ops::human_bytes(p.size)
                        )
                    },
                    cx,
                )
                .text_xs()
                .flex_none(),
            )
            .children(hit.as_ref().zip(self.root.clone()).and_then(|(f, root)| {
                pack_lookup::update_button(id("dp-update"), &self.store, root, p, f)
            }))
            .when_some(pid, |r, pid| {
                r.child(
                    Button::new(id("dp-page"))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Info)
                        .tooltip("Details")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ContentEvent::Peek { project: pid })
                        })),
                )
            })
            .child(
                Button::new(id("dp-open"))
                    .ghost()
                    .xsmall()
                    .icon(IconName::FolderOpen)
                    .tooltip("Open")
                    .on_click(move |_, _, _| open_path(&p1)),
            )
            .when(!others.is_empty(), |r| {
                r.child(
                    Button::new(id("dp-move"))
                        .ghost()
                        .xsmall()
                        .icon(IconName::ArrowRightLeft)
                        .tooltip("Move to…")
                        .dropdown_menu(move |mut menu, _, _| {
                            menu = menu.label("Move to");
                            for (label, rel) in &others {
                                let (v, path, rel) = (view.clone(), p2.clone(), rel.clone());
                                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
                                    move |_, window, cx| {
                                        v.update(cx, |this, cx| {
                                            this.move_to(path.clone(), rel.clone(), window, cx)
                                        })
                                    },
                                ));
                            }
                            menu
                        }),
                )
            })
            .child(
                Button::new(id("dp-rm"))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Trash)
                    .tooltip("Move to .hopper-backups")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.remove(p3.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }

    /// The picked folder: what it does, its packs, Add and Open.
    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let (Some(root), Some(src)) = (
            self.root.clone(),
            self.sources.iter().find(|s| Some(&s.rel) == self.picked.as_ref()).cloned(),
        ) else {
            return v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(widgets::caption("No data pack folders: install Paxi or play once to make a world.", cx))
                .into_any_element();
        };
        let what = if src.rel == PAXI_OPTIONAL {
            "Optional packs Paxi offers when a world is made"
        } else if src.global {
            "Loaded into every world, always on"
        } else {
            "Loaded into this world only"
        };
        let dir = root.join(&src.rel);
        let rel = src.rel.clone();
        let rows: Vec<AnyElement> = self.packs.clone().iter().map(|p| self.pack_row(p, cx)).collect();
        let empty = rows.is_empty() && src.count == 0;
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_semibold().truncate().child(src.name.clone()))
                            .child(widgets::caption(format!("{what} · {}", src.rel), cx).text_xs().truncate()),
                    )
                    .child(
                        Button::new("dp-add")
                            .primary()
                            .small()
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
                        Button::new("dp-folder")
                            .outline()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label("Open folder")
                            .on_click(move |_, _, _| {
                                let _ = std::fs::create_dir_all(&dir);
                                open_path(&dir)
                            }),
                    ),
            )
            .child(
                v_flex()
                    .id("dp-packs")
                    .flex_1()
                    .min_h_0()
                    .children(rows)
                    .when(empty, |c| {
                        c.child(
                            div().py_10().flex().justify_center().child(widgets::caption(
                                "No data packs here. Add some, or drop .zip files on the window.",
                                cx,
                            )),
                        )
                    })
                    .overflow_y_scrollbar(),
            )
            .into_any_element()
    }
}

impl Render for DatapacksView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.root.is_none() {
            return div().into_any_element();
        }
        h_flex()
            .size_full()
            .items_stretch()
            .px_6()
            .pb_6()
            .gap_6()
            .child(self.sidebar(cx))
            .child(self.body(cx))
            .into_any_element()
    }
}
