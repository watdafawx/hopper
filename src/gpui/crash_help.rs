//! An instance's Crash help: points to Crash Assistant, the mod that
//! explains crashes in game, and installs it when the pack lacks it. Hopper
//! only checks whether the mod is there; the diagnosis is the mod's own.

use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};use gpui_kit::*;
use std::path::{Path, PathBuf};

const MODRINTH: &str = "https://modrinth.com/mod/crash-assistant";
const CURSEFORGE: &str = "https://www.curseforge.com/minecraft/mc-mods/crash-assistant";

/// A bordered, tinted box that calls attention to one thing.
pub fn callout(color: Hsla, cx: &App) -> Div {
    v_flex()
        .gap_2()
        .p_4()
        .rounded(cx.theme().radius_lg)
        .border_1()
        .border_color(color.opacity(0.6))
        .bg(color.opacity(0.06))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Present {
    On,
    Off,
    Missing,
}

/// Whether a jar in `mods/` is Crash Assistant, by its file name.
fn crash_assistant(root: &Path) -> Present {
    let mut found = Present::Missing;
    for e in std::fs::read_dir(root.join("mods")).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_lowercase();
        if !name.replace(['-', '_', ' '], "").contains("crashassistant") {
            continue;
        }
        if name.ends_with(".jar") {
            return Present::On;
        }
        if name.ends_with(".jar.disabled") {
            found = Present::Off;
        }
    }
    found
}

pub struct CrashHelp {
    store: Entity<Store>,
    root: Option<PathBuf>,
    present: Present,
    _subscriptions: Vec<Subscription>,
}

impl CrashHelp {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        // An install or a change in Mods shows here at once.
        let sub = cx.observe(&store, |this, _, cx| this.recheck(cx));
        Self { store, root: None, present: Present::Missing, _subscriptions: vec![sub] }
    }

    pub fn show(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.root = Some(root);
        self.recheck(cx);
    }

    fn recheck(&mut self, cx: &mut Context<Self>) {
        let Some(root) = &self.root else { return };
        let now = crash_assistant(root);
        if now != self.present {
            self.present = now;
            cx.notify();
        }
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        let Some(mut inst) = store.instance(&root).cloned() else { return };
        store.spawn_instance_job(&root, "Installing Crash Assistant", move |job| {
            let added = hopper::install::install_mod_id(&mut inst, "crash_assistant", job)?;
            job.note(format!("Installed {}", added.join(", ")));
            Ok(crate::store::Msg::InstanceChanged(inst.root.clone()))
        });
    }
}

impl Render for CrashHelp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (color, title, text) = match self.present {
            Present::On => (
                theme.success,
                "Crash Assistant is installed",
                "When the game crashes, Crash Assistant opens a window that explains why, names the mod at fault and helps share the logs.",
            ),
            Present::Off => (
                theme.warning,
                "Crash Assistant is turned off",
                "It's in this instance's mods but disabled. Turn it on in the Mods tab to get its crash explanations.",
            ),
            Present::Missing => (
                theme.primary,
                "Get crash explanations with Crash Assistant",
                "Crash Assistant by KostromDan is a mod that explains crashes when they happen: what went wrong, which mod caused it, and how to share the logs. Hopper can add it to this instance.",
            ),
        };
        let links = h_flex()
            .gap_2()
            .when(self.present == Present::Missing, |r| {
                r.child(
                    Button::new("ca-install")
                        .small()
                        .primary()
                        .icon(IconName::Download)
                        .label("Install Crash Assistant")
                        .on_click(cx.listener(|this, _, _, cx| this.install(cx))),
                )
            })
            .child(
                Button::new("ca-modrinth")
                    .small()
                    .ghost()
                    .icon(IconName::ExternalLink)
                    .label("Modrinth")
                    .text_color(crate::widgets::site_color(true))
                    .on_click(|_, _, cx| cx.open_url(MODRINTH)),
            )
            .child(
                Button::new("ca-curseforge")
                    .small()
                    .ghost()
                    .icon(IconName::ExternalLink)
                    .label("CurseForge")
                    .text_color(crate::widgets::site_color(false))
                    .on_click(|_, _, cx| cx.open_url(CURSEFORGE)),
            );
        v_flex()
            .gap_3()
            .child(
                callout(color, cx)
                    .child(
                        h_flex()
                            .gap_2()
                            .text_color(color)
                            .child(Icon::new(IconName::Stethoscope).small())
                            .child(div().font_semibold().child(title)),
                    )
                    .child(div().text_sm().text_color(cx.theme().foreground).child(text))
                    .child(links),
            )
    }
}

use gpui_kit::prelude::FluentBuilder as _;
