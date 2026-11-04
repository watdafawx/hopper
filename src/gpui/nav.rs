//! Back and forward through the places you've been: pages, instances and
//! their tabs, servers, settings, and the project panel beside a list. The
//! mouse's back and forward buttons and Alt+Left / Alt+Right walk it.

use crate::app::Page;
use crate::library::InstTab;
use crate::settings_page::Category;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How many places back are kept.
const KEEP: usize = 100;

/// A place: the page, and where on it.
#[derive(Clone, PartialEq)]
pub enum Loc {
    /// An instance, its tab, and the project open beside it.
    Library(Option<PathBuf>, InstTab, Option<u64>),
    /// A place in Browse (its search, type, filters and open project; see
    /// `BrowseView::place_id`), which keeps what that place showed.
    Browse(u64),
    /// A server and its tab (Sync among them).
    Servers(Option<PathBuf>, usize),
    Settings(Category),
}

impl Loc {
    pub fn page(&self) -> Page {
        match self {
            Loc::Library(..) => Page::Library,
            Loc::Browse(..) => Page::Browse,
            Loc::Servers(..) => Page::Servers,
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
}

impl History {
    /// Notes where the app is now; a new place pushes the last one.
    pub fn saw(&mut self, now: Loc) {
        // Arriving ends the grace below, even when the step already set
        // `here`: a place picked right after is a new one.
        if self.going.as_ref().is_some_and(|(to, _)| *to == now) {
            self.going = None;
            self.here = Some(now);
            return;
        }
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

    /// Whether there is a place to go back (or forward) to.
    pub fn can_step(&self, back: bool) -> bool {
        if back { !self.back.is_empty() } else { !self.forward.is_empty() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: usize) -> Loc {
        Loc::Servers(None, n)
    }

    #[test]
    fn new_places_push_and_repeats_dont() {
        let mut h = History::default();
        h.saw(s(0));
        h.saw(s(0));
        assert!(!h.can_step(true));
        h.saw(s(1));
        assert!(h.can_step(true));
        assert!(!h.can_step(false));
        assert!(h.step(true) == Some(s(0)));
        assert!(!h.can_step(true));
        assert!(h.can_step(false));
    }

    #[test]
    fn forward_after_back_then_new_place_clears_forward() {
        let mut h = History::default();
        for n in 0..3 {
            h.saw(s(n));
        }
        assert!(h.step(true) == Some(s(1)));
        h.saw(s(1));
        assert!(h.step(false) == Some(s(2)));
        h.saw(s(2));
        assert!(h.step(true) == Some(s(1)));
        h.saw(s(1));
        // Somewhere new from the middle: forward is gone.
        h.saw(s(7));
        assert!(!h.can_step(false));
        assert!(h.step(true) == Some(s(1)));
        h.saw(s(1));
        assert!(h.step(true) == Some(s(0)));
    }

    #[test]
    fn whats_shown_on_the_way_back_is_not_a_place() {
        let mut h = History::default();
        h.saw(Loc::Browse(1));
        h.saw(Loc::Browse(2));
        assert!(h.step(true) == Some(Loc::Browse(1)));
        // A panel still loading on the way (another place for a moment).
        h.saw(Loc::Browse(3));
        h.saw(Loc::Browse(1));
        assert!(h.can_step(false));
        assert!(h.step(false) == Some(Loc::Browse(2)));
        assert!(!h.can_step(false));
    }

    #[test]
    fn keeps_a_bounded_past() {
        let mut h = History::default();
        for n in 0..(KEEP + 20) {
            h.saw(s(n));
        }
        let mut back = 0;
        while h.step(true).is_some() {
            back += 1;
        }
        assert!(back == KEEP);
    }
}
