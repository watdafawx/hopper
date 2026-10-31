//! The store's side of [`hopper::disk_watch`]: instances added, removed or
//! changed outside Hopper show up without a manual refresh.

use super::{Msg, Store, StoreEvent};
use gpui_kit::*;
use hopper::disk_watch::{Area, Change, DiskWatch};
use hopper::instances;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

/// Starts the watcher; its changes come back as [`Msg::Disk`].
pub(super) fn start(tx: Sender<Msg>, wake: hopper::waker::Waker) -> DiskWatch {
    DiskWatch::start(move |changes| {
        let _ = tx.send(Msg::Disk(changes));
        wake();
    })
}

impl Store {
    /// The instance open in the Library: its mods, packs, worlds and
    /// screenshots folders are watched.
    pub fn watch_open(&self, root: Option<PathBuf>) {
        self.disk.set_open(root);
    }

    /// Looks for changes made outside Hopper now (the window is back in
    /// focus), in case file events were missed.
    pub fn disk_check(&self) {
        self.disk.check();
    }

    /// Whether Hopper itself may be writing this folder: a job changing the
    /// instance or a launch. A folder no instance owns yet may be a pack
    /// Hopper is installing, so any running job counts for it.
    fn writing(&self, root: &Path) -> bool {
        if self.is_busy(root) || self.is_launching(root) {
            return true;
        }
        self.instance(root).is_none() && self.jobs.snapshot().iter().any(|j| !j.done && !j.quiet)
    }

    pub(super) fn disk_changed(&mut self, changes: Vec<Change>, cx: &mut Context<Self>) {
        for c in changes {
            let root = match &c {
                Change::Instance(r) | Change::Gone(r) | Change::Content(r, _) => r.clone(),
            };
            // Hopper's own job reads the instance again when it ends; what
            // happened meanwhile is looked at then.
            if self.writing(&root) {
                if !self.disk_later.contains(&c) {
                    self.disk_later.push(c);
                }
                continue;
            }
            match c {
                Change::Gone(root) => self.disk_gone(&root),
                Change::Instance(root) => self.disk_reload(root, None),
                Change::Content(root, area) => match area {
                    // These tabs list what the instance file records, plus
                    // loose files: read it again first.
                    Area::Mods | Area::ResourcePacks | Area::ShaderPacks => self.disk_reload(root, Some(area)),
                    Area::Worlds | Area::Screenshots => {
                        if self.instance(&root).is_some() {
                            cx.emit(StoreEvent::DiskContent(root, area));
                        }
                    }
                },
            }
        }
    }

    /// Tries changes held back by a job again, once nothing writes there.
    pub(super) fn retry_disk(&mut self, cx: &mut Context<Self>) {
        if self.disk_later.is_empty() {
            return;
        }
        let later = std::mem::take(&mut self.disk_later);
        self.disk_changed(later, cx);
    }

    /// Reads an instance again off the UI thread (a big pack's file runs to
    /// megabytes).
    fn disk_reload(&self, root: PathBuf, area: Option<Area>) {
        // Only instances folders and the open instance are watched; an
        // instance from another launcher reloads only if Hopper lists it.
        let known = self.instance(&root).is_some();
        let main = root.parent() == Some(Path::new(&self.settings.instances_root));
        if !known && !main {
            return;
        }
        self.spawn(move || {
            let inst = if root.is_dir() { instances::load(&root) } else { None };
            Msg::DiskLoaded(root, inst.map(Box::new), area)
        });
    }

    pub(super) fn disk_loaded(
        &mut self,
        root: PathBuf,
        inst: Option<Box<instances::Instance>>,
        area: Option<Area>,
        cx: &mut Context<Self>,
    ) {
        let Some(inst) = inst else { return };
        // A job started while it was being read: its own reload wins.
        if self.is_busy(&root) {
            return;
        }
        let new = self.instance(&root).is_none();
        if new {
            log::info!("{}: found a new instance", root.display());
        }
        self.put_instance(*inst);
        if new {
            self.instances.sort_by_key(|i| i.name.to_lowercase());
            self.check_updates(&root, false);
        }
        if let Some(area) = area {
            cx.emit(StoreEvent::DiskContent(root, area));
        }
    }

    /// An instance folder was deleted or renamed outside Hopper: it leaves
    /// the list. Its groups and pairings stay in settings, so a folder
    /// renamed back (or a drive that comes back) finds them again.
    fn disk_gone(&mut self, root: &Path) {
        if root.exists() || self.instance(root).is_none() {
            return;
        }
        log::info!("{}: instance folder is gone", root.display());
        self.instances.retain(|i| i.root != root);
        self.updates.remove(root);
        self.pending.remove(root);
        self.sync_watched();
    }
}
