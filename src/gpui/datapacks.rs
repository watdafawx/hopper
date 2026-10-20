//! An instance's Data packs tab: the global loader folders (Paxi,
//! OpenLoader, Global Packs) and each world's `datapacks/`, side by side.

use crate::addons_extra::{is_hopper_file, pack_files};
use crate::content::ContentEvent;
use crate::crash_help::callout;
use crate::store::{Store, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex,
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
    /// The picked folder's packs, in the same table as Mods and Resource
    /// packs (switch, update, author, remove).
    table: Entity<crate::addons::AddonsView>,
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
    pub fn new(
        store: Entity<Store>,
        table: Entity<crate::addons::AddonsView>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
            table,
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
        // The table reads the folder too, for its rows.
        if let (Some(root), Some(rel)) = (self.root.clone(), self.picked.clone()) {
            let others: Vec<(String, String)> = self
                .sources
                .iter()
                .filter(|s| s.rel != rel)
                .map(|s| {
                    let label = if s.global { format!("{} (every world)", s.name) } else { format!("world {}", s.name) };
                    (label, s.rel.clone())
                })
                .collect();
            self.table.update(cx, |t, cx| t.show_folder(root, rel, others, cx));
        }
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
        let empty = self.packs.is_empty() && src.count == 0;
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
            .child(if empty {
                let rel = src.rel.clone();
                v_flex()
                    .py_10()
                    .gap_3()
                    .items_center()
                    .child(widgets::caption("No data packs here. Add some, or drop .zip files on the window.", cx))
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
                    .into_any_element()
            } else {
                div().flex_1().min_h_0().child(self.table.clone()).into_any_element()
            })
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
