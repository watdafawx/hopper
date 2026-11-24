//! The Downloads chip in an instance's hero: files the pack waits for from
//! the CurseForge website (their authors only allow downloads there), and
//! Hopper's download window working on them. It replaces the strip that
//! pushed the page down while a download ran; the words, tones and fold
//! rules are `hopper::syncchip::download_look`.
//!
//! The popover keeps what the strip had: Download / Show window / Retry,
//! Show files, Check now, and Forget them (the strip's ✕).

use crate::fetch_progress;
use crate::store::Store;
use crate::sync_chip::{Action, Chip, Section};
use gpui_kit::assets::IconName;
use gpui_kit::*;
use hopper::syncchip::{DownloadState, download_look};
use std::path::Path;
use std::rc::Rc;

/// The chip for the pack at `root`, or `None` when no file waits.
pub fn chip(store: &Entity<Store>, root: &Path, cx: &App) -> Option<Chip> {
    let s = store.read(cx);
    let pending = s.pending(root).to_vec();
    if pending.is_empty() {
        return None;
    }
    let (fetch, _) = s.fetch_files(root);
    let n = pending.len();
    let (state, title, line, progress) = match &fetch {
        Some(v) if v.running() => (
            DownloadState::Working { done: v.done, total: v.total },
            format!("Downloading from CurseForge…{}", if v.total > 1 { format!(" {} of {}", (v.done + 1).min(v.total), v.total) } else { String::new() }),
            v.detail(),
            Some(v.progress.unwrap_or(0.0)),
        ),
        Some(v) => (DownloadState::Stuck, "Some files didn't come".to_string(), v.detail(), None),
        None => (
            DownloadState::Waiting { files: n },
            if n == 1 { format!("{} to download in your browser", pending[0].name) } else { format!("{n} files to download in your browser") },
            "Their authors only allow downloads from the CurseForge website.".to_string(),
            None,
        ),
    };
    let look = download_look(&state);
    let urls: Vec<String> = pending.iter().map(|p| p.url.clone()).filter(|u| !u.is_empty()).collect();
    let mut actions: Vec<Action> = Vec::new();
    // Download before it starts, Show window while it runs, Retry for what
    // didn't come (`fetch_progress::download_button`'s three states).
    match &fetch {
        Some(v) if v.running() => {
            let (st, v2) = (store.clone(), v.clone());
            actions.push(Action {
                label: "Show window".into(),
                icon: Some(IconName::ExternalLink),
                primary: false,
                disabled: !v.step.is_open(),
                run: Rc::new(move |_, cx| fetch_progress::click(&st, &v2, cx)),
            });
        }
        Some(v) => {
            let (st, v2) = (store.clone(), v.clone());
            actions.push(Action {
                label: "Retry".into(),
                icon: Some(IconName::RefreshCw),
                primary: true,
                disabled: false,
                run: Rc::new(move |_, cx| fetch_progress::click(&st, &v2, cx)),
            });
        }
        None => {
            let (st, r, urls2) = (store.clone(), root.to_path_buf(), urls.clone());
            actions.push(Action {
                label: if urls.len() == 1 { "Download".into() } else { format!("Download {}", urls.len()).into() },
                icon: Some(IconName::Download),
                primary: true,
                disabled: urls.is_empty(),
                run: Rc::new(move |_, cx| st.read(cx).fetch_pages(&r, urls2.clone(), None)),
            });
        }
    }
    let (st, r, list) = (store.clone(), root.to_path_buf(), pending);
    actions.push(Action {
        label: "Show files".into(),
        icon: None,
        primary: false,
        disabled: false,
        run: Rc::new(move |w, cx| fetch_progress::show_files(&st, &r, &list, w, cx)),
    });
    let (st, r) = (store.clone(), root.to_path_buf());
    actions.push(Action {
        label: "Check now".into(),
        icon: Some(IconName::RefreshCw),
        primary: false,
        disabled: false,
        // Hopper checks Downloads every few seconds; this re-runs the
        // pack's install too.
        run: Rc::new(move |_, cx| st.read(cx).finish_pack(&r)),
    });
    // Hopper stops waiting for these files.
    let (st, r) = (store.clone(), root.to_path_buf());
    actions.push(Action {
        label: "Forget them".into(),
        icon: None,
        primary: false,
        disabled: false,
        run: Rc::new(move |_, cx| {
            st.update(cx, |s, cx| {
                s.forget_pending(&r);
                cx.notify();
            })
        }),
    });
    Some(Chip {
        id: "downloads".into(),
        look,
        progress,
        sections: vec![Section { title: title.into(), line: line.into(), progress, body: None, actions }],
    })
}
