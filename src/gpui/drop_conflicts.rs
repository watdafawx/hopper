//! A jar dropped on an instance that is a mod the instance already has (a
//! newer or different version, matched by the mod id inside the jar, not by
//! file name): ask whether to replace the old one, keep both, or skip the
//! dropped file. Replaced jars go where a removal puts them (Backups). The
//! Hopper Server web UI asks the same thing.

use crate::widgets;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::*;
use hopper::scan;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone)]
pub struct Conflict {
    pub path: PathBuf,
    pub mod_name: String,
    pub old_file: String,
    pub old_version: String,
    pub new_version: String,
    /// The old jar has the dropped file's name: both can't stay.
    pub same_name: bool,
}

/// What the answers add up to.
#[derive(Clone, Default)]
pub struct Plan {
    /// Dropped files to leave out.
    pub skip: Vec<PathBuf>,
    /// Jars already in mods/ that the dropped ones replace.
    pub replace_old: Vec<String>,
}

type Then = Rc<dyn Fn(Plan, &mut Window, &mut App)>;

fn version_of(m: &hopper::model::ModInfo) -> String {
    if m.version.is_empty() { scan::version_from_file_name(&m.file_name) } else { m.version.clone() }
}

/// The dropped jars that are a mod `mods_dir` has already.
pub fn find(paths: &[PathBuf], mods_dir: &Path) -> Vec<Conflict> {
    let jars: Vec<&PathBuf> = paths.iter().filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("jar"))).collect();
    if jars.is_empty() || !mods_dir.is_dir() {
        return Vec::new();
    }
    let Ok(have) = scan::scan_folder(mods_dir) else { return Vec::new() };
    jars.into_iter()
        .filter_map(|p| {
            let new = scan::scan_file(p);
            let old = have
                .iter()
                .find(|m| m.file_name.eq_ignore_ascii_case(&new.file_name) || (!new.mod_id.is_empty() && m.mod_id == new.mod_id))?;
            Some(Conflict {
                path: p.clone(),
                mod_name: new.mod_name.clone(),
                old_file: old.file_name.clone(),
                old_version: version_of(old),
                new_version: version_of(&new),
                same_name: old.file_name.eq_ignore_ascii_case(&new.file_name),
            })
        })
        .collect()
}

/// Asks about each conflict in turn, then runs `then` with the plan.
pub fn ask(conflicts: Vec<Conflict>, plan: Plan, window: &mut Window, cx: &mut App, then: Then) {
    let Some(c) = conflicts.first().cloned() else {
        then(plan, window, cx);
        return;
    };
    let rest: Vec<Conflict> = conflicts[1..].to_vec();
    window.open_dialog(cx, move |dialog, window, cx| {
        let theme = cx.theme().clone();
        let (c1, c3) = (c.clone(), c.clone());
        let (r1, r2, r3) = (rest.clone(), rest.clone(), rest.clone());
        let (p1, p2, p3) = (plan.clone(), plan.clone(), plan.clone());
        let (t1, t2, t3) = (then.clone(), then.clone(), then.clone());
        let v = |s: &str| if s.is_empty() { String::new() } else { format!(" {s}") };
        let title = format!(
            "Replace {}{} with{}?",
            c.mod_name,
            v(&c.old_version),
            if c.new_version.is_empty() { " the new file".to_string() } else { v(&c.new_version) }
        );
        let mut footer = DialogFooter::new().child(DialogClose::new().child(Button::new("drop-skip").outline().label("Skip")));
        if !c.same_name {
            footer = footer.child(div().child(Button::new("drop-both").outline().label("Keep both").on_click(move |_, window, cx| {
                window.close_dialog(cx);
                let (r, p, t) = (r2.clone(), p2.clone(), t2.clone());
                window.defer(cx, move |window, cx| ask(r, p, window, cx, t));
            })));
        }
        let same = !c.old_version.is_empty() && c.old_version == c.new_version;
        dialog
            .title(title)
            .w(rems(32.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(
                v_flex()
                    .gap_2()
                    .child(div().text_sm().child(format!(
                        "{} is in this instance already{}. Replacing moves it to Backups, where it can be put back.",
                        c.old_file,
                        if same { " with the same version" } else { "" }
                    )))
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
                    ),
            )
            .footer(footer.child(DialogAction::new().child(Button::new("drop-replace").primary().label("Replace"))))
            // Skip (or closing it): this file stays out.
            .on_cancel(move |_, window, cx| {
                let mut p = p1.clone();
                p.skip.push(c1.path.clone());
                let (r, t) = (r1.clone(), t1.clone());
                window.defer(cx, move |window, cx| ask(r, p, window, cx, t));
                true
            })
            .on_ok(move |_, window, cx| {
                let mut p = p3.clone();
                // The same name is replaced by the copy itself (the old one is kept aside).
                if !c3.same_name {
                    p.replace_old.push(c3.old_file.clone());
                }
                let (r, t) = (r3.clone(), t3.clone());
                window.defer(cx, move |window, cx| ask(r, p, window, cx, t));
                true
            })
    });
}
