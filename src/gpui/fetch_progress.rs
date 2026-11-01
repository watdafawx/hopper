//! Progress of the download window for website-only CurseForge files
//! (`fetch_window`, [`hopper::webfetch`]): runs it and reads the steps it
//! prints, and the look of a button that waits on it. While a file comes
//! through the window, the button that asked for it says what's happening
//! (waiting for CurseForge, downloading 62%) instead of going back to
//! "Install"; a click brings the window to the front. When it fails, the
//! button turns into Retry.

use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::webfetch::{Event, Step};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};

/// The open download window's handle (0: none), for "Show window".
static WINDOW: AtomicI64 = AtomicI64::new(0);

/// Runs `hopper --fetch DIR URL...` and passes each step it reports to
/// `on`. Its exit code; `None` when it couldn't start.
pub fn run_window(dir: &Path, urls: &[String], mut on: impl FnMut(Event)) -> Option<i32> {
    use std::io::BufRead;
    let exe = std::env::current_exe().ok()?;
    let mut child = std::process::Command::new(exe)
        .arg("--fetch")
        .arg(dir)
        .args(urls)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut mine = 0;
    if let Some(out) = child.stdout.take() {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let Some(e) = Event::parse(&line) else { continue };
            if let Event::Window(h) = e {
                mine = h;
                WINDOW.store(h, Ordering::Relaxed);
            }
            on(e);
        }
    }
    let code = child.wait().ok().and_then(|s| s.code());
    // Another window may have opened meanwhile; that one stays.
    let _ = WINDOW.compare_exchange(mine, 0, Ordering::Relaxed, Ordering::Relaxed);
    code
}

/// Whether a download window is open.
pub fn window_open() -> bool {
    WINDOW.load(Ordering::Relaxed) != 0
}

/// Whether job detail `d` is the download window's (a pack zip from the
/// CurseForge website, see `hopper::modpack::fetch_from_website`) while
/// one is open, so its task and dialog offer Show window.
pub fn job_waits_on_window(d: &str) -> bool {
    window_open() && (d.contains("CurseForge") || d.starts_with("Downloading "))
}

/// Brings the download window to the front. False when none is open.
pub fn show_window() -> bool {
    let h = WINDOW.load(Ordering::Relaxed);
    if h == 0 {
        return false;
    }
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{IsIconic, SW_RESTORE, SetForegroundWindow, ShowWindow};
        let hwnd = HWND(h as isize as *mut _);
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

/// One file the download window gets (or got) for an instance.
#[derive(Clone, Debug, PartialEq)]
pub struct FetchFile {
    pub url: String,
    pub root: PathBuf,
    /// The project whose install or update asked for it (a mod that needs
    /// this one, a pack), when that's not the file's own project.
    pub by: Option<u64>,
    pub project_id: u64,
    pub name: String,
    /// Expected size, 0 when unknown.
    pub size: u64,
    pub step: Step,
}

/// Several files' progress as one: what a button or banner shows.
#[derive(Clone, Debug, PartialEq)]
pub struct FetchView {
    pub root: PathBuf,
    pub total: usize,
    pub done: usize,
    /// The file being worked on, else the first that didn't come.
    pub name: String,
    pub step: Step,
    pub size: u64,
    pub progress: Option<f32>,
    /// Pages of the files that didn't come, to try again.
    pub stuck: Vec<String>,
}

impl FetchView {
    /// `None` with no files. Files the window saved stay here until the
    /// download watcher puts them in place (the store lets go of them then),
    /// so the button doesn't flash back to "Install" in between.
    pub fn of<'a>(files: impl IntoIterator<Item = &'a FetchFile>) -> Option<FetchView> {
        let files: Vec<&FetchFile> = files.into_iter().collect();
        let lead = files
            .iter()
            .find(|f| f.step.is_active())
            .or_else(|| files.iter().find(|f| f.step == Step::Queued))
            .or_else(|| files.iter().find(|f| f.step.is_stuck()))
            .or_else(|| files.first())?;
        let total = files.len();
        let done = files.iter().filter(|f| f.step == Step::Done).count();
        let sum: f32 = files.iter().map(|f| f.step.progress(f.size).unwrap_or(0.0)).sum();
        let known = files.iter().any(|f| f.step.progress(f.size).is_some());
        Some(FetchView {
            root: lead.root.clone(),
            total,
            done,
            name: lead.name.clone(),
            step: lead.step,
            size: lead.size,
            progress: known.then(|| sum / total as f32),
            stuck: files.iter().filter(|f| f.step.is_stuck()).map(|f| f.url.clone()).collect(),
        })
    }

    /// The window is still on it, or its files are being put in place.
    pub fn running(&self) -> bool {
        self.step.is_open() || self.step == Step::Done
    }

    /// "2 of 5" while several come, else nothing.
    fn count(&self) -> String {
        if self.total > 1 {
            format!(" · {} of {}", (self.done + 1).min(self.total), self.total)
        } else {
            String::new()
        }
    }

    /// Short, for a button: "Waiting for CurseForge…", "Downloading 62%",
    /// "Retry".
    pub fn button_label(&self) -> String {
        match self.step {
            Step::Queued => format!("Waiting its turn{}", self.count()),
            Step::Opening => format!("Opening CurseForge…{}", self.count()),
            Step::Check | Step::Countdown => format!("Waiting for CurseForge…{}", self.count()),
            Step::Downloading(_) if self.total > 1 => {
                let p = self.progress.map(|p| format!(" {}%", (p * 100.0).floor() as u32)).unwrap_or_default();
                format!("Downloading{p}{}", self.count())
            }
            s @ Step::Downloading(_) => s.label(self.size),
            Step::Done => "Adding it…".to_string(),
            _ => "Retry".to_string(),
        }
    }

    /// Shortest, for the Mods tab's narrow update pill: "Waiting…",
    /// "Updating 62%", "Retry" (its tooltip has the rest).
    pub fn pill_label(&self) -> String {
        match self.step {
            Step::Queued => "Queued".to_string(),
            Step::Opening | Step::Check | Step::Countdown => "Waiting…".to_string(),
            Step::Downloading(_) => match self.progress {
                Some(p) => format!("Updating {}%", (p * 100.0).floor() as u32),
                None => "Updating".to_string(),
            },
            Step::Done => "Adding…".to_string(),
            _ => "Retry".to_string(),
        }
    }

    /// A sentence for a caption or tooltip: "Downloading from CurseForge:
    /// Waystones, waiting for CurseForge's countdown · 2 of 5".
    pub fn detail(&self) -> String {
        if self.step == Step::Done {
            if self.total > 1 {
                format!("Downloaded {} files; adding them to the instance", self.total)
            } else {
                format!("Downloaded {}; adding it to the instance", self.name)
            }
        } else if self.running() {
            let step = self.step.label(self.size);
            let step = step[..1].to_lowercase() + &step[1..];
            format!("Getting {} from the CurseForge website: {step}{}", self.name, self.count())
        } else if self.stuck.len() == 1 {
            format!("{}: {}", self.name, self.step.label(self.size).to_lowercase())
        } else {
            format!("{} of {} files didn't download: {}", self.stuck.len(), self.total, self.step.label(self.size).to_lowercase())
        }
    }
}

/// What a click on a waiting button does: shows the window while it runs,
/// tries the stuck files again after.
pub fn click(store: &Entity<Store>, v: &FetchView, cx: &mut App) {
    if v.running() {
        show_window();
    } else {
        store.read(cx).fetch_pages(&v.root, v.stuck.clone(), None);
    }
}

/// `b` while the download window works for it: the busy look with the
/// step and progress, clickable to show the window; Retry once it failed.
pub fn button(b: Button, v: &FetchView, cx: &App) -> Button {
    if v.running() {
        crate::widgets::busy_button(b.icon(IconName::Hourglass).label(v.button_label()), v.progress, cx)
            .disabled(false)
            .tooltip(format!("{}. Click to show the download window", v.detail()))
    } else {
        b.icon(IconName::RefreshCw)
            .label("Retry")
            .tooltip(format!("{}. Retry opens the download window again", v.detail()))
    }
}

/// A caption line for a card with room for it: the step, and Show window
/// while it runs, or "Pick the file myself" after it failed (the card's
/// own button is Retry then).
pub fn status_line(store: &Entity<Store>, v: &FetchView, id: &str, cx: &App) -> Div {
    let theme = cx.theme();
    let color = if v.running() { theme.muted_foreground } else { theme.warning };
    h_flex()
        .gap_2()
        .flex_wrap()
        .text_sm()
        .text_color(color)
        .child(div().min_w_0().child(v.detail()))
        .when(v.step.is_open(), |r| {
            r.child(
                Button::new(SharedString::from(format!("{id}-show")))
                    .xsmall()
                    .ghost()
                    .icon(IconName::ExternalLink)
                    .label("Show window")
                    .on_click(|_, _, _| {
                        show_window();
                    }),
            )
        })
        .when(!v.running(), |r| {
            let (s2, v2) = (store.clone(), v.clone());
            r.child(
                Button::new(SharedString::from(format!("{id}-hand")))
                    .xsmall()
                    .ghost()
                    .icon(IconName::ExternalLink)
                    .label("Pick the file myself")
                    .tooltip("Opens the download page in your browser; Hopper puts the file in place from Downloads")
                    .on_click(move |_, _, cx| {
                        s2.update(cx, |s, cx| {
                            s.fetch_by_hand(&v2.root, &v2.stuck, cx);
                        })
                    }),
            )
        })
}

#[cfg(test)]
mod tests {
    // Not `super::*`: that brings GPUI's own `test` attribute along.
    use super::{FetchFile, FetchView};
    use hopper::webfetch::Step;
    use std::path::PathBuf;

    fn file(step: Step) -> FetchFile {
        FetchFile {
            url: format!("u{step:?}"),
            root: PathBuf::from("r"),
            by: None,
            project_id: 1,
            name: "Waystones".into(),
            size: 100,
            step,
        }
    }

    #[test]
    fn one_view_for_several_files() {
        assert_eq!(FetchView::of(&[]), None);
        // Saved, waiting to be put in place: still busy, not "Install".
        let saved = FetchView::of(&[file(Step::Done)]).unwrap();
        assert!(saved.running());
        assert_eq!(saved.button_label(), "Adding it…");
        let fs = [file(Step::Done), file(Step::Downloading(50)), file(Step::Queued), file(Step::Queued)];
        let v = FetchView::of(&fs).unwrap();
        assert!(v.running());
        assert_eq!((v.total, v.done), (4, 1));
        assert_eq!(v.progress, Some(0.375));
        assert_eq!(v.button_label(), "Downloading 37% · 2 of 4");
        let one = FetchView::of(&[file(Step::Countdown)]).unwrap();
        assert_eq!(one.button_label(), "Waiting for CurseForge…");
        assert_eq!(one.progress, None);
        assert_eq!(one.detail(), "Getting Waystones from the CurseForge website: waiting for CurseForge's countdown");
        // Stuck ones show only after the rest finished.
        let fs = [file(Step::Closed), file(Step::Check)];
        assert!(FetchView::of(&fs).unwrap().running());
        let fs = [file(Step::Closed), file(Step::Done)];
        let v = FetchView::of(&fs).unwrap();
        assert!(!v.running());
        assert_eq!(v.button_label(), "Retry");
        assert_eq!(v.stuck, vec!["uClosed".to_string()]);
        assert_eq!(v.detail(), "Waystones: the download window was closed");
    }
}
