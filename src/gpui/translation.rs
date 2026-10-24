//! Offline translation of changelogs and descriptions, as the window sees
//! it: which texts are translated, which are being translated, and which
//! the user switched back to the original. The work is in
//! `hopper::translate`.
//!
//! Nothing here runs unless Settings > General > "Translate mod text" is on.

use gpui_kit::SharedString;
use hopper::translate::{self, Lang};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash as _, Hasher as _};
use std::sync::{Arc, Mutex};

pub enum Tr {
    Working,
    Ready(SharedString),
    Failed(String),
}

/// What a translate bar shows for a set of texts.
pub enum BarState {
    /// Not translated (or switched back to the original).
    Idle,
    /// The language's model is downloading: progress 0..=1 when known.
    Downloading(Option<f32>),
    Working,
    /// Shown in English.
    Done,
    Failed(String),
}

pub fn key(md: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    md.hash(&mut h);
    h.finish()
}

pub struct Translations {
    state: Arc<Mutex<HashMap<u64, Tr>>>,
    /// Translated texts the user switched back to the original.
    original: RefCell<HashSet<u64>>,
    /// Detected language by text set, so a redraw doesn't detect again.
    detected: RefCell<HashMap<u64, Option<&'static Lang>>>,
    /// Model downloads in progress: their job ids, by language code.
    pub(crate) jobs: HashMap<&'static str, u64>,
    /// Texts to translate once a language's download ends.
    pub(crate) after: HashMap<&'static str, Vec<String>>,
    /// Tells the store a translation finished (wakes its poll).
    done: Arc<dyn Fn() + Send + Sync>,
}

impl Translations {
    pub fn new(done: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            state: Default::default(),
            original: Default::default(),
            detected: Default::default(),
            jobs: HashMap::new(),
            after: HashMap::new(),
            done,
        }
    }

    /// Wakes the views showing translations (a download moved on).
    pub fn notifier(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.done.clone()
    }

    /// The non-English language of `texts` that can be translated.
    pub fn lang(&self, texts: &[SharedString]) -> Option<&'static Lang> {
        let k = key(&texts.iter().map(|t| t.as_ref()).collect::<Vec<&str>>().join("\n\n"));
        *self.detected.borrow_mut().entry(k).or_insert_with(|| {
            translate::detect(&texts.iter().map(|t| t.as_ref()).collect::<Vec<&str>>().join("\n\n"))
        })
    }

    /// `md` in English, when it was translated and isn't switched back.
    pub fn shown(&self, md: &str) -> Option<SharedString> {
        let k = key(md);
        if self.original.borrow().contains(&k) {
            return None;
        }
        match self.state.lock().unwrap().get(&k) {
            Some(Tr::Ready(t)) => Some(t.clone()),
            _ => None,
        }
    }

    pub fn bar(&self, texts: &[SharedString]) -> BarState {
        let state = self.state.lock().unwrap();
        let original = self.original.borrow();
        let mut any_done = false;
        for t in texts {
            let k = key(t);
            match state.get(&k) {
                Some(Tr::Working) => return BarState::Working,
                Some(Tr::Failed(e)) => return BarState::Failed(e.clone()),
                Some(Tr::Ready(_)) if !original.contains(&k) => any_done = true,
                _ => {}
            }
        }
        if any_done { BarState::Done } else { BarState::Idle }
    }

    /// Translates `texts` on a thread (or shows them in English again when
    /// they were translated before). The language must be downloaded.
    pub fn request(&self, texts: Vec<String>, lang: &'static Lang) {
        let mut todo = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            let mut original = self.original.borrow_mut();
            for t in texts {
                let k = key(&t);
                original.remove(&k);
                match state.get(&k) {
                    Some(Tr::Ready(_) | Tr::Working) => {}
                    _ => {
                        state.insert(k, Tr::Working);
                        todo.push((k, t));
                    }
                }
            }
        }
        if todo.is_empty() {
            (self.done)();
            return;
        }
        let (state, done) = (self.state.clone(), self.done.clone());
        std::thread::spawn(move || {
            for (k, t) in todo {
                let r = match translate::translate(&t, lang) {
                    Ok(out) => Tr::Ready(out.into()),
                    Err(e) => {
                        log::warn!("translating: {e:#}");
                        Tr::Failed(format!("{e:#}"))
                    }
                };
                state.lock().unwrap().insert(k, r);
                done();
            }
        });
    }

    /// Shows `texts` as their authors wrote them.
    pub fn show_original(&self, texts: &[SharedString]) {
        let mut original = self.original.borrow_mut();
        for t in texts {
            original.insert(key(t));
        }
        (self.done)();
    }

    /// Whether `texts` were asked for already (so "always" doesn't ask twice).
    pub fn asked(&self, texts: &[SharedString]) -> bool {
        let state = self.state.lock().unwrap();
        let original = self.original.borrow();
        texts.iter().any(|t| {
            let k = key(t);
            state.contains_key(&k) || original.contains(&k)
        })
    }

    /// Forgets failed attempts, to try again.
    pub fn clear_failed(&self, texts: &[SharedString]) {
        let mut state = self.state.lock().unwrap();
        for t in texts {
            if let Some(Tr::Failed(_)) = state.get(&key(t)) {
                state.remove(&key(t));
            }
        }
    }

    /// Forgets every translation (a model was removed).
    pub fn forget_all(&self) {
        self.state.lock().unwrap().clear();
        self.detected.borrow_mut().clear();
    }
}
