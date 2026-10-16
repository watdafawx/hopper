//! "Other launchers": the Modrinth App, Prism, MultiMC… folders found on
//! this PC, each with a switch to manage its instances in Hopper. Shown in
//! Settings and in the first-run setup.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::launchers::Kind;
use std::path::{Path, PathBuf};

pub struct LaunchersList {
    store: Entity<Store>,
    /// Other launchers installed here: (launcher, folder, instances).
    found: Option<Vec<(Kind, PathBuf, usize)>>,
    /// Why a picked launcher folder wasn't added.
    note: Option<String>,
}

impl LaunchersList {
    pub fn new(store: Entity<Store>) -> Self {
        Self {
            store,
            found: None,
            note: None,
        }
    }

    /// Looks for other launchers' instances (in the background).
    fn detect(&mut self, cx: &mut Context<Self>) {
        self.found = Some(Vec::new());
        cx.spawn(async move |this, cx| {
            let found = cx.background_spawn(async { hopper::launchers::detect() }).await;
            this.update(cx, |this, cx| {
                this.found = Some(found);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Starts or stops managing a launcher's folder, then lists instances again.
    fn set_managed(&mut self, kind: Kind, path: PathBuf, on: bool, cx: &mut Context<Self>) {
        let dir = hopper::settings::LauncherDir {
            kind: kind.key().to_string(),
            path: path.to_string_lossy().into_owned(),
        };
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    st.launcher_dirs.retain(|d| d != &dir);
                    if on {
                        st.launcher_dirs.push(dir);
                    }
                },
                cx,
            );
            s.refresh();
            cx.notify();
        });
        cx.notify();
    }

    /// Asks for a launcher's folder and manages it, when it has instances.
    fn add_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Manage this launcher's instances".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(p))) = rx.await else { return };
            let Some(p) = p.into_iter().next() else { return };
            let probe = p.clone();
            let found = cx
                .background_spawn(async move {
                    Kind::ALL.into_iter().find_map(|k| {
                        let n = hopper::launchers::list(k, &probe).len();
                        (n > 0).then_some((k, n))
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                match found {
                    Some((k, n)) => {
                        this.note = None;
                        if let Some(list) = &mut this.found
                            && !list.iter().any(|(_, d, _)| *d == p)
                        {
                            list.push((k, p.clone(), n));
                        }
                        this.set_managed(k, p, true, cx);
                    }
                    None => {
                        this.note = Some(format!(
                            "No Modrinth App, Prism, MultiMC, ATLauncher, GDLauncher or FTB App instances in {}.",
                            p.display()
                        ));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// How many launcher folders Hopper manages, of how many listed.
    pub fn counts(&self, cx: &App) -> (usize, usize) {
        let managed = self.store.read(cx).settings().launcher_dirs.len();
        let found = self.found.as_ref().map_or(0, |f| f.len());
        (managed, found.max(managed))
    }
}

impl Render for LaunchersList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.found.is_none() {
            self.detect(cx);
        }
        let managed = self.store.read(cx).settings().launcher_dirs.clone();
        let mut rows: Vec<(Kind, PathBuf, Option<usize>)> = self
            .found
            .iter()
            .flatten()
            .map(|(k, p, n)| (*k, p.clone(), Some(*n)))
            .collect();
        // Folders added by hand (or no longer found) stay listed.
        for d in &managed {
            let p = PathBuf::from(&d.path);
            if let Some(k) = Kind::from_key(&d.kind)
                && !rows.iter().any(|(_, rp, _)| *rp == p)
            {
                rows.push((k, p, None));
            }
        }
        let list = v_flex().gap_2().children(rows.into_iter().enumerate().map(|(i, (k, p, n))| {
            let on = managed
                .iter()
                .any(|d| d.kind == k.key() && Path::new(&d.path) == p);
            let p2 = p.clone();
            h_flex()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(cx.theme().radius)
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().font_medium().child(k.name()))
                        .child(
                            widgets::caption(
                                match n {
                                    Some(n) => format!("{n} instances · {}", p.display()),
                                    None => p.display().to_string(),
                                },
                                cx,
                            )
                            .text_xs()
                            .truncate(),
                        ),
                )
                .child(
                    Switch::new(("manage-launcher", i))
                        .label("Manage in Hopper")
                        .checked(on)
                        .on_click(cx.listener(move |this, v: &bool, _, cx| {
                            this.set_managed(k, p2.clone(), *v, cx)
                        })),
                )
        }));
        let empty = self.found.as_ref().is_some_and(|l| l.is_empty()) && managed.is_empty();
        // Wrapping lines: this list also sits in the narrow setup dialog.
        let line = |text: &'static str| {
            div()
                .min_w_0()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        };
        v_flex()
            .gap_3()
            .child(line(
                "Manage their instances where they are, without copying. They keep working in that launcher too.",
            ))
            .when(empty, |c| {
                c.child(line(
                    "No Modrinth App, Prism, MultiMC, ATLauncher, GDLauncher or FTB App instances found on this PC.",
                ))
            })
            .child(list)
            .when_some(self.note.clone(), |c, n| {
                c.child(div().text_sm().text_color(cx.theme().warning).child(n))
            })
            .child(
                h_flex().child(
                    Button::new("add-launcher-folder")
                        .outline()
                        .icon(IconName::FolderOpen)
                        .label("Add a launcher folder…")
                        .tooltip("For a portable MultiMC or a moved instances folder")
                        .on_click(cx.listener(|this, _, window, cx| this.add_folder(window, cx))),
                ),
            )
    }
}
