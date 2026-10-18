//! Back and forward through the places you've been: pages, instances and
//! their tabs, servers, settings, and the project panel beside a list. The
//! mouse's back and forward buttons and Alt+Left / Alt+Right walk it.

use crate::app::Page;
use crate::library::InstTab;
use crate::settings_page::Category;
use hopper::cf;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How many places back are kept.
const KEEP: usize = 100;

/// A place: the page, and where on it.
#[derive(Clone, PartialEq)]
pub enum Loc {
    /// An instance, its tab, and the project open beside it.
    Library(Option<PathBuf>, InstTab, Option<u64>),
    /// The type shown, and the project open.
    Browse(u32, Option<u64>),
    /// A server and its tab.
    Servers(Option<PathBuf>, usize),
    Sync(Option<PathBuf>),
    Settings(Category),
}

impl Loc {
    pub fn page(&self) -> Page {
        match self {
            Loc::Library(..) => Page::Library,
            Loc::Browse(..) => Page::Browse,
            Loc::Servers(..) => Page::Servers,
            Loc::Sync(_) => Page::Sync,
            Loc::Settings(_) => Page::Settings,
        }
    }
}

#[derive(Default)]
pub struct History {
    back: Vec<Loc>,
    forward: Vec<Loc>,
    here: Option<Loc>,
    /// A place being gone back to, and since when: what shows on the way
    /// (a project panel still loading) isn't a new place.
    going: Option<(Loc, Instant)>,
    /// Browse projects seen, to open them again.
    projects: HashMap<u64, cf::Mod>,
}

impl History {
    /// Notes where the app is now; a new place pushes the last one.
    pub fn saw(&mut self, now: Loc) {
        if self.here.as_ref() == Some(&now) {
            return;
        }
        if let Some((to, at)) = &self.going {
            let arrived = *to == now;
            if arrived || at.elapsed() < Duration::from_millis(1500) {
                if arrived {
                    self.going = None;
                }
                self.here = Some(now);
                return;
            }
            self.going = None;
        }
        if let Some(prev) = self.here.replace(now) {
            self.back.push(prev);
            if self.back.len() > KEEP {
                self.back.remove(0);
            }
            self.forward.clear();
        }
    }

    /// The place one step back (or forward), now the one being gone to.
    pub fn step(&mut self, back: bool) -> Option<Loc> {
        let (from, to) = if back {
            (&mut self.back, &mut self.forward)
        } else {
            (&mut self.forward, &mut self.back)
        };
        let loc = from.pop()?;
        if let Some(here) = self.here.take() {
            to.push(here);
        }
        self.going = Some((loc.clone(), Instant::now()));
        self.here = Some(loc.clone());
        Some(loc)
    }

    /// A Browse project, kept to open it again.
    pub fn remember(&mut self, m: &cf::Mod) {
        self.projects.entry(m.id).or_insert_with(|| m.clone());
    }

    pub fn project(&self, id: u64) -> Option<cf::Mod> {
        self.projects.get(&id).cloned()
    }
}
