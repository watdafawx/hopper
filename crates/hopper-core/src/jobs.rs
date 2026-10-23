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
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.0 += 1;
        let id = g.0;
        g.1.push(JobState {
            id,
            title: title.into(),
            detail: String::new(),
            progress: None,
            done: false,
            error: None,
            notes: Vec::new(),
            announce: false,
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

    pub fn snapshot(&self) -> Vec<JobState> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone()
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

    /// Asks for a Windows notification when it finishes fine too.
    pub fn announce(&self) {
        self.jobs.update(self.id, |j| j.announce = true);
    }

    pub fn note(&self, n: impl Into<String>) {
        let n = n.into();
        self.jobs.update(self.id, |j| j.notes.push(n));
    }

    /// Marks the job finished with `result`.
    pub fn finish<T>(&self, result: &anyhow::Result<T>) {
        let err = result.as_ref().err().map(|e| format!("{e:#}"));
        self.jobs.update(self.id, |j| {
            j.done = true;
            j.progress = Some(1.0);
            j.error = err;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
