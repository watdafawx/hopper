//! One line of tasks per instance: a task that changes an instance waits
//! for the ones before it, in the order they were asked for, instead of
//! being refused or running beside them. Two at once would both write the
//! mods folder and History.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// How often a waiting task looks whether it was cancelled (a cancel sets
/// a flag; nothing wakes the line for it).
const CANCEL_POLL: Duration = Duration::from_millis(200);

#[derive(Default)]
pub struct Lanes {
    lines: Mutex<HashMap<PathBuf, VecDeque<(u64, String)>>>,
    moved: Condvar,
}

impl Lanes {
    /// Puts task `id` at the end of `key`'s line. Returns the title of the
    /// task running now when this one has to wait.
    pub fn join(&self, key: &Path, id: u64, title: &str) -> Option<String> {
        let mut m = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let line = m.entry(key.to_path_buf()).or_default();
        line.push_back((id, title.to_string()));
        (line.len() > 1).then(|| line[0].1.clone())
    }

    /// Blocks until task `id` is first in its line. `ahead` hears the title
    /// of the task it waits for whenever that changes. Returns false, out of
    /// the line, when `cancelled` turned true first.
    pub fn wait_turn(&self, key: &Path, id: u64, cancelled: impl Fn() -> bool, mut ahead: impl FnMut(&str)) -> bool {
        let mut m = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let mut last: Option<u64> = None;
        loop {
            let Some(line) = m.get(key) else { return true };
            match line.front() {
                Some((first, _)) if *first == id => return true,
                Some((first, t)) => {
                    if last != Some(*first) {
                        last = Some(*first);
                        ahead(t);
                    }
                }
                None => return true,
            }
            if cancelled() {
                Self::remove(&mut m, key, id);
                self.moved.notify_all();
                return false;
            }
            m = self.moved.wait_timeout(m, CANCEL_POLL).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Task `id` ended (fine, failed or cancelled): the next one may start.
    pub fn leave(&self, key: &Path, id: u64) {
        let mut m = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        Self::remove(&mut m, key, id);
        self.moved.notify_all();
    }

    fn remove(m: &mut HashMap<PathBuf, VecDeque<(u64, String)>>, key: &Path, id: u64) {
        if let Some(line) = m.get_mut(key) {
            line.retain(|(i, _)| *i != id);
            if line.is_empty() {
                m.remove(key);
            }
        }
    }

    /// Whether any task runs or waits on `key`.
    pub fn busy(&self, key: &Path) -> bool {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).contains_key(key)
    }

    /// The ids in `key`'s line, the running one first.
    pub fn line(&self, key: &Path) -> Vec<u64> {
        let m = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        m.get(key).map(|l| l.iter().map(|(i, _)| *i).collect()).unwrap_or_default()
    }
}

/// "Waiting for 'Updating mods in Test' to finish": a waiting task's line
/// in the task list.
pub fn waiting_text(ahead: &str) -> String {
    format!("Waiting for '{ahead}' to finish")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, mpsc};

    fn key() -> PathBuf {
        PathBuf::from("C:/inst/a")
    }

    /// Runs task `id` on a thread: waits its turn, records that it started,
    /// then waits for `go` before it ends (and fails when told to).
    fn task(
        lanes: &Arc<Lanes>,
        id: u64,
        started: mpsc::Sender<u64>,
        go: mpsc::Receiver<bool>,
        cancel: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<Result<(), String>> {
        let lanes = lanes.clone();
        std::thread::spawn(move || {
            if !lanes.wait_turn(&key(), id, || cancel.load(Ordering::Relaxed), |_| {}) {
                return Err("Cancelled".into());
            }
            started.send(id).unwrap();
            let ok = go.recv().unwrap_or(true);
            lanes.leave(&key(), id);
            if ok { Ok(()) } else { Err("failed".into()) }
        })
    }

    #[test]
    fn join_says_what_it_waits_for() {
        let l = Lanes::default();
        assert_eq!(l.join(&key(), 1, "Updating 3 mods"), None);
        assert_eq!(l.join(&key(), 2, "Undo"), Some("Updating 3 mods".into()));
        assert_eq!(l.join(&key(), 3, "Add"), Some("Updating 3 mods".into()));
        // Another instance has its own line.
        assert_eq!(l.join(Path::new("C:/inst/b"), 4, "Add"), None);
        assert_eq!(l.line(&key()), vec![1, 2, 3]);
        assert_eq!(waiting_text("Updating 3 mods"), "Waiting for 'Updating 3 mods' to finish");
    }

    #[test]
    fn keeps_order_and_next_starts_after_a_failure() {
        let lanes = Arc::new(Lanes::default());
        let (tx, rx) = mpsc::channel();
        let mut gos = Vec::new();
        let mut hs = Vec::new();
        for id in 1..=3 {
            lanes.join(&key(), id, &format!("task {id}"));
        }
        for id in 1..=3 {
            let (g, r) = mpsc::channel();
            gos.push(g);
            hs.push(task(&lanes, id, tx.clone(), r, Arc::new(AtomicBool::new(false))));
        }
        // Only the first runs; the others wait.
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
        // The first fails: the second still starts.
        gos[0].send(false).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 2);
        gos[1].send(true).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
        gos[2].send(true).unwrap();
        let r: Vec<_> = hs.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(r, vec![Err("failed".into()), Ok(()), Ok(())]);
        assert!(!lanes.busy(&key()));
    }

    #[test]
    fn cancel_while_waiting_leaves_the_line() {
        let lanes = Arc::new(Lanes::default());
        let (tx, rx) = mpsc::channel();
        for id in 1..=3 {
            lanes.join(&key(), id, &format!("task {id}"));
        }
        let (g1, r1) = mpsc::channel();
        let (g2, r2) = mpsc::channel();
        let (g3, r3) = mpsc::channel();
        let c2 = Arc::new(AtomicBool::new(false));
        let h1 = task(&lanes, 1, tx.clone(), r1, Arc::new(AtomicBool::new(false)));
        let h2 = task(&lanes, 2, tx.clone(), r2, c2.clone());
        let h3 = task(&lanes, 3, tx.clone(), r3, Arc::new(AtomicBool::new(false)));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        // The second is cancelled while the first still runs.
        c2.store(true, Ordering::Relaxed);
        assert_eq!(h2.join().unwrap(), Err("Cancelled".into()));
        assert_eq!(lanes.line(&key()), vec![1, 3]);
        drop(g2);
        g1.send(true).unwrap();
        // The third goes next.
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 3);
        g3.send(true).unwrap();
        assert_eq!(h1.join().unwrap(), Ok(()));
        assert_eq!(h3.join().unwrap(), Ok(()));
        assert!(!lanes.busy(&key()));
    }

    #[test]
    fn ahead_hears_each_task_it_waits_for() {
        let lanes = Arc::new(Lanes::default());
        lanes.join(&key(), 1, "first");
        lanes.join(&key(), 2, "second");
        lanes.join(&key(), 3, "third");
        let (tx, rx) = mpsc::channel();
        let l = lanes.clone();
        let h = std::thread::spawn(move || {
            l.wait_turn(&key(), 3, || false, |t| tx.send(t.to_string()).unwrap())
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "first");
        lanes.leave(&key(), 1);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "second");
        lanes.leave(&key(), 2);
        assert!(h.join().unwrap());
    }
}
