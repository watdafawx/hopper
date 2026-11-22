//! Keeps a busy state from flashing. A check that ends in a blink should
//! never show at all, and one that did show stays long enough to read:
//! [`Settle`] turns "is it busy now" into "show it now", from the time alone,
//! so a page can't make a notice flash by asking again on every redraw.

use std::time::Duration;

/// The busy state must last this long before it shows.
pub const SHOW_AFTER: Duration = Duration::from_millis(700);
/// Once shown it stays at least this long.
pub const HOLD_FOR: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy)]
pub struct Settle {
    show_after: Duration,
    hold_for: Duration,
    /// When the current busy stretch began.
    busy_since: Option<Duration>,
    /// When it started showing.
    shown_at: Option<Duration>,
}

/// What [`Settle::step`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub show: bool,
    /// When the answer will change by itself: ask again after this long.
    pub wake_in: Option<Duration>,
}

impl Default for Settle {
    fn default() -> Self {
        Self::new(SHOW_AFTER, HOLD_FOR)
    }
}

impl Settle {
    pub fn new(show_after: Duration, hold_for: Duration) -> Self {
        Self { show_after, hold_for, busy_since: None, shown_at: None }
    }

    /// `busy`: the state is on right now; `now`: any clock that only grows.
    pub fn step(&mut self, busy: bool, now: Duration) -> Step {
        if busy {
            let since = *self.busy_since.get_or_insert(now);
            if self.shown_at.is_none() {
                let wait = self.show_after.saturating_sub(now.saturating_sub(since));
                if !wait.is_zero() {
                    return Step { show: false, wake_in: Some(wait) };
                }
                // It began showing when the delay ran out, not when asked.
                self.shown_at = Some(since + self.show_after);
            }
            return Step { show: true, wake_in: None };
        }
        self.busy_since = None;
        match self.shown_at {
            Some(at) => {
                let left = self.hold_for.saturating_sub(now.saturating_sub(at));
                if left.is_zero() {
                    self.shown_at = None;
                    Step { show: false, wake_in: None }
                } else {
                    Step { show: true, wake_in: Some(left) }
                }
            }
            None => Step { show: false, wake_in: None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_blink_never_shows() {
        let mut s = Settle::default();
        assert_eq!(s.step(true, ms(0)), Step { show: false, wake_in: Some(ms(700)) });
        assert_eq!(s.step(true, ms(300)).show, false);
        assert_eq!(s.step(false, ms(400)), Step { show: false, wake_in: None });
    }

    #[test]
    fn shows_after_the_delay_and_holds_when_it_ends() {
        let mut s = Settle::default();
        s.step(true, ms(0));
        assert_eq!(s.step(true, ms(700)).show, true);
        // Ended 100 ms after it showed: stays for the rest of the hold.
        assert_eq!(s.step(false, ms(800)), Step { show: true, wake_in: Some(ms(1400)) });
        assert_eq!(s.step(false, ms(2199)).show, true);
        assert_eq!(s.step(false, ms(2200)), Step { show: false, wake_in: None });
    }

    #[test]
    fn long_busy_ends_at_once() {
        let mut s = Settle::default();
        s.step(true, ms(0));
        s.step(true, ms(5000));
        assert_eq!(s.step(false, ms(5001)).show, false);
    }

    #[test]
    fn busy_again_inside_the_hold_keeps_it_shown() {
        let mut s = Settle::default();
        s.step(true, ms(0));
        s.step(true, ms(700));
        s.step(false, ms(900));
        assert_eq!(s.step(true, ms(1000)), Step { show: true, wake_in: None });
        // The hold counts from when it first showed: ended at 1100, shown at 700.
        assert_eq!(s.step(false, ms(1100)).show, true);
        assert_eq!(s.step(false, ms(2200)).show, false);
    }
}
