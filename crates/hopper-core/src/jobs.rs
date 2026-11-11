//! Background work with progress the UI can show.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct JobState {
    pub id: u64,
    pub title: String,
    pub detail: String,
    /// 0..=1, or `None` while the total is unknown.
    pub progress: Option<f32>,
    pub done: bool,
    pub error: Option<String>,
    /// Lines the user should act on (e.g. files to download by hand).
    pub notes: Vec<String>,
    /// Worth a Windows notification when it finishes fine (a modpack
    /// download); failures always get one.
    pub announce: bool,
    /// Kept out of the task list (a game launch): it only shows up if it
    /// fails, so the error isn't lost.
    pub quiet: bool,
    /// Files it still waits for after its own work ended (website-only
    /// downloads that Hopper's download window gets, keyed by page): it
    /// counts as finished only once they're in. See [`Job::wait_for`].
    pub waits: Vec<String>,
    /// How many it waited for in all, and how many of those didn't come.
    pub waited: usize,
    pub waits_failed: usize,
    /// Its work ended fine while [`JobState::waits`] remained.
    held: bool,
    /// A modpack being installed (false) or updated (true), by name, for
    /// what Discord shows (see [`Job::presence`]).
    pub presence: Option<(String, bool)>,
    /// Stopped because the user cancelled it: `error` is just "Cancelled",
    /// and lists show it as a quiet grey line, not a failure.
    pub cancelled: bool,
}

#[derive(Clone, Default)]
pub struct Jobs {
    inner: Arc<Mutex<(u64, Vec<JobState>)>>,
    waker: Option<crate::waker::Waker>,
    /// Wakes the UI for a running job's detail or progress only, which
    /// just the tasks list shows: pages needn't redraw for it.
    ticker: Option<crate::waker::Waker>,
    /// Cancel flags of jobs still listed, by id.
    cancels: Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>,
}

/// A handle a worker thread updates.
#[derive(Clone)]
pub struct Job {
    id: u64,
    jobs: Jobs,
    cancelled: Arc<AtomicBool>,
}

impl Jobs {
    pub fn new(waker: crate::waker::Waker) -> Self {
        Self {
            inner: Default::default(),
            waker: Some(waker),
            ticker: None,
            cancels: Default::default(),
        }
    }

    /// Like [`Jobs::new`], with detail and progress waking `ticker` instead.
    pub fn with_ticker(waker: crate::waker::Waker, ticker: crate::waker::Waker) -> Self {
        Self {
            ticker: Some(ticker),
            ..Self::new(waker)
        }
    }

    pub fn start(&self, title: impl Into<String>) -> Job {
        self.start_with(title.into(), false)
    }

    /// A job left out of [`Jobs::snapshot`] unless it fails (see [`JobState::quiet`]).
    pub fn start_quiet(&self, title: impl Into<String>) -> Job {
        self.start_with(title.into(), true)
    }

    fn start_with(&self, title: String, quiet: bool) -> Job {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.0 += 1;
        let id = g.0;
        g.1.push(JobState {
            id,
            title,
            detail: String::new(),
            progress: None,
            done: false,
            error: None,
            notes: Vec::new(),
            announce: false,
            quiet,
            waits: Vec::new(),
            waited: 0,
            waits_failed: 0,
            held: false,
            presence: None,
            cancelled: false,
        });
        drop(g);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, cancelled.clone());
        self.repaint();
        Job {
            id,
            jobs: self.clone(),
            cancelled,
        }
    }

    /// Asks job `id` to stop. Work that checks [`Job::is_cancelled`] ends
    /// with the error "Cancelled"; other work runs to the end.
    pub fn cancel(&self, id: u64) {
        if let Some(c) = self
            .cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
        {
            c.store(true, Ordering::Relaxed);
        }
    }

    fn forget_cancels(&self) {
        let live: Vec<u64> = self.snapshot().iter().map(|j| j.id).collect();
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|id, _| live.contains(id));
    }

    /// The jobs the task list shows (quiet ones only once they fail).
    pub fn snapshot(&self) -> Vec<JobState> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .iter()
            .filter(|j| !j.quiet)
            .cloned()
            .collect()
    }

    pub fn dismiss(&self, id: u64) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .retain(|j| j.id != id);
        self.forget_cancels();
    }

    /// Removes every finished job, failed ones and ones with notes too:
    /// the user is looking at them when they clear. Running jobs stay
    /// (their X cancels them).
    pub fn clear_finished(&self) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .retain(|j| !j.done);
        self.forget_cancels();
    }

    /// A file job(s) waited for ([`Job::wait_for`]) came (`ok`) or won't.
    /// A job whose work already ended finishes with its last one, with a
    /// note for the ones that didn't come.
    pub fn settle(&self, key: &str, ok: bool) {
        let mut changed = false;
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            for j in g.1.iter_mut().filter(|j| j.waits.iter().any(|w| w == key)) {
                changed = true;
                j.waits.retain(|w| w != key);
                if !ok {
                    j.waits_failed += 1;
                }
                let got = j.waited - j.waits.len();
                if !j.held {
                    continue;
                }
                if j.waits.is_empty() {
                    j.held = false;
                    j.done = true;
                    j.progress = Some(1.0);
                    j.detail = waited_line(j.waited, j.waits_failed);
                    if j.waits_failed > 0 {
                        j.notes.push(format!(
                            "{} of {} files from the CurseForge website didn't download. Retry them from the instance's Mods tab.",
                            j.waits_failed, j.waited
                        ));
                    }
                } else {
                    j.progress = Some(got as f32 / j.waited as f32);
                    j.detail = format!("Getting files from the CurseForge website: {got} of {}", j.waited);
                }
            }
        }
        if changed {
            self.repaint();
        }
    }

    fn repaint(&self) {
        if let Some(w) = &self.waker {
            w();
        }
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut JobState)) {
        self.change(id, f);
        self.repaint();
    }

    /// An update only the tasks list shows.
    fn tick(&self, id: u64, f: impl FnOnce(&mut JobState)) {
        self.change(id, f);
        match &self.ticker {
            Some(t) => t(),
            None => self.repaint(),
        }
    }

    fn change(&self, id: u64, f: impl FnOnce(&mut JobState)) {
        if let Some(j) = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .iter_mut()
            .find(|j| j.id == id)
        {
            f(j);
        }
    }
}

impl Job {
    /// The job's id, as [`JobState::id`] lists it.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Asks this job to stop (see [`Jobs::cancel`]).
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// `Err("Cancelled")` once the job was cancelled.
    pub fn check_cancelled(&self) -> anyhow::Result<()> {
        if self.is_cancelled() {
            anyhow::bail!("Cancelled");
        }
        Ok(())
    }

    pub fn detail(&self, d: impl Into<String>) {
        let d = d.into();
        self.jobs.tick(self.id, |j| j.detail = d);
    }

    pub fn progress(&self, done: usize, total: usize) {
        let p = if total == 0 {
            None
        } else {
            Some(done as f32 / total as f32)
        };
        self.jobs.tick(self.id, |j| j.progress = p);
    }

    /// Marks the job as installing (or updating) the named modpack, which
    /// Discord shows while it runs.
    pub fn presence(&self, pack: impl Into<String>, update: bool) {
        let pack = pack.into();
        self.jobs.update(self.id, |j| j.presence = Some((pack, update)));
    }

    /// Asks for a Windows notification when it finishes fine too.
    pub fn announce(&self) {
        self.jobs.update(self.id, |j| j.announce = true);
    }

    pub fn note(&self, n: impl Into<String>) {
        let n = n.into();
        self.jobs.update(self.id, |j| j.notes.push(n));
    }

    /// A quiet job that turned out to change something joins the task
    /// list, so the player sees what happened.
    pub fn show(&self) {
        self.jobs.update(self.id, |j| j.quiet = false);
    }

    /// How many notes the job has, for [`Job::drop_notes_from`].
    pub fn note_count(&self) -> usize {
        self.jobs.snapshot().iter().find(|j| j.id == self.id).map_or(0, |j| j.notes.len())
    }

    /// Forgets the notes added since there were `n`.
    pub fn drop_notes_from(&self, n: usize) {
        self.jobs.update(self.id, |j| j.notes.truncate(n));
    }

    /// The job isn't finished until these files came (or failed): each
    /// key is settled by [`Jobs::settle`]. Called before [`Job::finish`].
    pub fn wait_for(&self, keys: &[String]) {
        self.jobs.update(self.id, |j| {
            for k in keys {
                if !j.waits.contains(k) {
                    j.waits.push(k.clone());
                    j.waited += 1;
                }
            }
        });
    }

    /// Marks the job finished with `result`. Work that ended fine while
    /// files it waits for are still coming stays open until they're in.
    pub fn finish<T>(&self, result: &anyhow::Result<T>) {
        // A cancel surfaces as whatever step noticed it ("download https://…:
        // Cancelled"); the player only needs to know they stopped it.
        let cancelled = result.is_err() && self.is_cancelled();
        let err = match result {
            Err(_) if cancelled => Some("Cancelled".to_string()),
            Err(e) => Some(format!("{e:#}")),
            Ok(_) => None,
        };
        let failed = err.is_some() && !cancelled;
        self.jobs.update(self.id, |j| {
            if !failed && !j.waits.is_empty() {
                j.held = true;
                let got = j.waited - j.waits.len();
                j.progress = Some(got as f32 / j.waited as f32);
                j.detail = format!("Getting files from the CurseForge website: {got} of {}", j.waited);
                return;
            }
            j.waits.clear();
            j.done = true;
            j.progress = Some(1.0);
            j.error = err;
            j.cancelled = cancelled;
            // A quiet job that failed joins the list, with its error.
            if failed {
                j.quiet = false;
            }
        });
        // One that went fine leaves no trace.
        let gone = self.jobs.inner.lock().unwrap_or_else(|e| e.into_inner()).1.iter().any(|j| j.id == self.id && j.quiet && j.done);
        if gone {
            self.jobs.dismiss(self.id);
        }
    }
}

/// A held job's last line once its files are in.
fn waited_line(n: usize, failed: usize) -> String {
    match (n, failed) {
        (1, 0) => "Done, with 1 file from the CurseForge website".to_string(),
        (n, 0) => format!("Done, with {n} files from the CurseForge website"),
        (n, f) => format!("Done; {f} of {n} files from the CurseForge website didn't download"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_waits_for_its_web_downloads() {
        let jobs = Jobs::default();
        let job = jobs.start("Installing a pack");
        job.wait_for(&["a".into(), "b".into()]);
        // One came before the install's own work ended.
        jobs.settle("a", true);
        assert!(!jobs.snapshot()[0].done);
        job.finish(&anyhow::Ok(()));
        let s = &jobs.snapshot()[0];
        assert!(!s.done, "still getting b");
        assert_eq!(s.detail, "Getting files from the CurseForge website: 1 of 2");
        jobs.settle("other", true);
        assert!(!jobs.snapshot()[0].done);
        jobs.settle("b", false);
        let s = &jobs.snapshot()[0];
        assert!(s.done && s.error.is_none());
        assert_eq!(s.detail, "Done; 1 of 2 files from the CurseForge website didn't download");
        assert_eq!(s.notes.len(), 1);
        // A failed job doesn't wait.
        let bad = jobs.start("bad");
        bad.wait_for(&["c".into()]);
        bad.finish::<()>(&Err(anyhow::anyhow!("boom")));
        assert!(jobs.snapshot().iter().any(|j| j.title == "bad" && j.done));
    }

    #[test]
    fn a_cancelled_job_says_cancelled_not_the_step_that_noticed() {
        let jobs = Jobs::default();
        let job = jobs.start("Making a server");
        job.cancel();
        job.finish::<()>(&Err(anyhow::anyhow!("Cancelled").context("download https://edge.forgecdn.net/x.zip")));
        let s = &jobs.snapshot()[0];
        assert!(s.done && s.cancelled);
        assert_eq!(s.error.as_deref(), Some("Cancelled"));
        // A failure that wasn't a cancel keeps its reason.
        let bad = jobs.start("bad");
        bad.finish::<()>(&Err(anyhow::anyhow!("disk full")));
        let s = jobs.snapshot().into_iter().find(|j| j.title == "bad").unwrap();
        assert!(!s.cancelled);
        assert_eq!(s.error.as_deref(), Some("disk full"));
    }

    #[test]
    fn quiet_jobs_show_only_when_they_fail() {
        let jobs = Jobs::default();
        let ok = jobs.start_quiet("launch");
        assert!(jobs.snapshot().is_empty());
        ok.finish(&anyhow::Ok(()));
        assert!(jobs.snapshot().is_empty());
        let bad = jobs.start_quiet("launch");
        bad.finish::<()>(&Err(anyhow::anyhow!("no Java")));
        let s = jobs.snapshot();
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].error.as_deref(), Some("no Java"));
    }

    #[test]
    fn clear_removes_every_finished_job_and_keeps_running_ones() {
        let jobs = Jobs::default();
        let ok = jobs.start("ok");
        ok.finish(&anyhow::Ok(()));
        let failed = jobs.start("failed");
        failed.finish::<()>(&Err(anyhow::anyhow!("boom")));
        let noted = jobs.start("noted");
        noted.note("download these by hand");
        noted.finish(&anyhow::Ok(()));
        let running = jobs.start("running");
        running.note("still going");
        jobs.clear_finished();
        let left: Vec<String> = jobs.snapshot().into_iter().map(|j| j.title).collect();
        assert_eq!(left, ["running"]);
        // The running job can still be cancelled.
        jobs.cancel(running_id(&jobs));
        assert!(running.is_cancelled());
    }

    #[test]
    fn progress_wakes_the_ticker_and_the_rest_wake_the_page() {
        use std::sync::atomic::AtomicUsize;
        let (woke, ticked) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (w, t) = (woke.clone(), ticked.clone());
        let jobs = Jobs::with_ticker(
            Arc::new(move || {
                w.fetch_add(1, Ordering::Relaxed);
            }),
            Arc::new(move || {
                t.fetch_add(1, Ordering::Relaxed);
            }),
        );
        let job = jobs.start("download");
        job.progress(1, 4);
        job.detail("1 of 4");
        job.finish(&anyhow::Ok(()));
        assert_eq!((woke.load(Ordering::Relaxed), ticked.load(Ordering::Relaxed)), (2, 2));
        assert_eq!(jobs.snapshot()[0].detail, "1 of 4");
    }

    fn running_id(jobs: &Jobs) -> u64 {
        jobs.snapshot()[0].id
    }
}
