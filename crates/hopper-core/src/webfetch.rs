//! What Hopper's download window (`hopper --fetch DIR URL...`) is doing.
//! The window gets website-only CurseForge files: it waits through
//! Cloudflare's check and CurseForge's countdown, then saves each file. It
//! prints one line per step on stdout, so the app can show the player that
//! it's working instead of going back to "Install".

/// Every status line starts with this; anything else on stdout is ignored.
pub const PREFIX: &str = "hopper-fetch ";

/// One step the window reports. `usize` is the page's place in the URL list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// The window's handle, to bring it to the front.
    Window(i64),
    /// Page `n` is loading.
    Opening(usize),
    /// Cloudflare is checking the browser.
    Check(usize),
    /// CurseForge's page is up and counting down to the download.
    Countdown(usize),
    /// The download of page `n` has this many bytes so far.
    Bytes(usize, u64),
    /// Page `n`'s file is saved.
    Done(usize),
    /// Page `n` gave no file.
    Failed(usize),
    /// The player closed the window.
    Closed,
}

impl Event {
    /// The line the window prints for this step.
    pub fn line(&self) -> String {
        let rest = match self {
            Event::Window(h) => format!("window {h}"),
            Event::Opening(i) => format!("opening {i}"),
            Event::Check(i) => format!("check {i}"),
            Event::Countdown(i) => format!("countdown {i}"),
            Event::Bytes(i, n) => format!("bytes {i} {n}"),
            Event::Done(i) => format!("done {i}"),
            Event::Failed(i) => format!("failed {i}"),
            Event::Closed => "closed".to_string(),
        };
        format!("{PREFIX}{rest}")
    }

    /// Reads a line the window printed; `None` for any other output.
    pub fn parse(line: &str) -> Option<Event> {
        let mut w = line.trim().strip_prefix(PREFIX)?.split_whitespace();
        let kind = w.next()?;
        let mut num = || w.next()?.parse::<u64>().ok();
        Some(match kind {
            "window" => Event::Window(line.trim().rsplit(' ').next()?.parse().ok()?),
            "opening" => Event::Opening(num()? as usize),
            "check" => Event::Check(num()? as usize),
            "countdown" => Event::Countdown(num()? as usize),
            "bytes" => {
                let i = num()? as usize;
                Event::Bytes(i, num()?)
            }
            "done" => Event::Done(num()? as usize),
            "failed" => Event::Failed(num()? as usize),
            "closed" => Event::Closed,
            _ => return None,
        })
    }
}

/// Where one file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Step {
    /// Waiting for the files before it.
    #[default]
    Queued,
    Opening,
    Check,
    Countdown,
    /// Bytes so far.
    Downloading(u64),
    Done,
    /// The page gave no file in time.
    Failed,
    /// The window was closed before it came.
    Closed,
    /// It came, but isn't the file CurseForge lists (size or SHA-1), so
    /// Hopper didn't use it. The app decides this; the window never says it.
    Unusable,
}

impl Step {
    /// The window is working on it.
    pub fn is_active(self) -> bool {
        matches!(self, Step::Opening | Step::Check | Step::Countdown | Step::Downloading(_))
    }

    /// Still to come: working on it or waiting its turn.
    pub fn is_open(self) -> bool {
        self == Step::Queued || self.is_active()
    }

    /// It didn't come, and won't without the player.
    pub fn is_stuck(self) -> bool {
        matches!(self, Step::Failed | Step::Closed | Step::Unusable)
    }

    /// How far it is (0..=1) when that's known; `size` is the file's
    /// expected size, 0 when unknown.
    pub fn progress(self, size: u64) -> Option<f32> {
        match self {
            Step::Downloading(n) if size > 0 => Some((n as f64 / size as f64).min(1.0) as f32),
            Step::Done => Some(1.0),
            _ => None,
        }
    }

    /// What's happening, for the player: "Waiting for CurseForge's
    /// countdown", "Downloading 62%".
    pub fn label(self, size: u64) -> String {
        match self {
            Step::Queued => "Waiting its turn".to_string(),
            Step::Opening => "Opening CurseForge".to_string(),
            Step::Check => "Waiting for CurseForge's check".to_string(),
            Step::Countdown => "Waiting for CurseForge's countdown".to_string(),
            Step::Downloading(n) => match self.progress(size) {
                Some(p) => format!("Downloading {}%", (p * 100.0).floor() as u32),
                None => format!("Downloading {}", crate::instance_ops::human_bytes(n)),
            },
            Step::Done => "Downloaded".to_string(),
            Step::Failed => "CurseForge didn't start the download".to_string(),
            Step::Closed => "The download window was closed".to_string(),
            Step::Unusable => "The downloaded file didn't match what CurseForge lists".to_string(),
        }
    }
}

/// One run of the download window: each page's step, in URL order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub steps: Vec<Step>,
    pub window: Option<i64>,
}

impl Run {
    pub fn new(pages: usize) -> Self {
        Run { steps: vec![Step::Queued; pages], window: None }
    }

    /// Takes in a step the window reported.
    pub fn apply(&mut self, e: Event) {
        // A finished page stays finished; a late byte count can't undo it.
        fn set(steps: &mut [Step], i: usize, s: Step) {
            if let Some(x) = steps.get_mut(i)
                && !matches!(*x, Step::Done | Step::Failed)
            {
                *x = s;
            }
        }
        let steps = &mut self.steps;
        match e {
            Event::Window(h) => self.window = Some(h),
            Event::Opening(i) => set(steps, i, Step::Opening),
            // The check and countdown come before the download; once it
            // started, a title change doesn't take it back.
            Event::Check(i) | Event::Countdown(i) if matches!(steps.get(i), Some(Step::Downloading(_))) => {}
            Event::Check(i) => set(steps, i, Step::Check),
            Event::Countdown(i) => set(steps, i, Step::Countdown),
            Event::Bytes(i, n) => set(steps, i, Step::Downloading(n)),
            Event::Done(i) => set(steps, i, Step::Done),
            Event::Failed(i) => set(steps, i, Step::Failed),
            Event::Closed => self.end(Step::Closed),
        }
    }

    /// The window is gone: pages it didn't finish end as `how`.
    pub fn end(&mut self, how: Step) {
        for s in &mut self.steps {
            if s.is_open() {
                *s = how;
            }
        }
        self.window = None;
    }

    /// The page being worked on.
    pub fn current(&self) -> Option<usize> {
        self.steps.iter().position(|s| s.is_active())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip() {
        for e in [
            Event::Window(-123456),
            Event::Opening(0),
            Event::Check(1),
            Event::Countdown(2),
            Event::Bytes(3, 1_048_576),
            Event::Done(4),
            Event::Failed(5),
            Event::Closed,
        ] {
            assert_eq!(Event::parse(&e.line()), Some(e));
        }
        assert_eq!(Event::parse("hopper-fetch bytes 1 12\r\n"), Some(Event::Bytes(1, 12)));
        assert_eq!(Event::parse("[INFO] fetch: https://x"), None);
        assert_eq!(Event::parse("hopper-fetch bytes 1"), None);
        assert_eq!(Event::parse("hopper-fetch whatever 1"), None);
    }

    #[test]
    fn run_follows_the_window() {
        let mut r = Run::new(3);
        r.apply(Event::Window(7));
        r.apply(Event::Opening(0));
        r.apply(Event::Check(0));
        assert_eq!(r.steps[0], Step::Check);
        r.apply(Event::Countdown(0));
        r.apply(Event::Bytes(0, 50));
        // A title change after the download started changes nothing.
        r.apply(Event::Countdown(0));
        assert_eq!(r.steps[0], Step::Downloading(50));
        assert_eq!(r.current(), Some(0));
        r.apply(Event::Done(0));
        r.apply(Event::Bytes(0, 60));
        assert_eq!(r.steps[0], Step::Done);
        r.apply(Event::Opening(1));
        r.apply(Event::Failed(1));
        r.apply(Event::Opening(2));
        assert_eq!(r.window, Some(7));
        r.apply(Event::Closed);
        assert_eq!(r.steps, vec![Step::Done, Step::Failed, Step::Closed]);
        assert_eq!(r.window, None);
        assert_eq!(r.current(), None);
        // Out-of-range pages are ignored.
        r.apply(Event::Done(9));
    }

    #[test]
    fn labels_and_progress() {
        assert_eq!(Step::Downloading(62).label(100), "Downloading 62%");
        assert_eq!(Step::Downloading(2048).label(0), "Downloading 2 KB");
        assert_eq!(Step::Downloading(500).progress(100), Some(1.0));
        assert_eq!(Step::Countdown.progress(100), None);
        assert_eq!(Step::Countdown.label(0), "Waiting for CurseForge's countdown");
        assert!(Step::Queued.is_open() && !Step::Queued.is_active());
        assert!(Step::Closed.is_stuck() && !Step::Done.is_stuck());
        assert!(Step::Unusable.is_stuck() && !Step::Unusable.is_open());
    }
}
