//! The pieces of Settings › Library & folders › Recently deleted that
//! aren't layout: what each kind looks like, the summary line, and putting
//! an entry back.

use gpui_kit::assets::IconName;
use hopper::recently_deleted::{self as rd, Entry, Kind};
use std::path::PathBuf;

pub fn icon(k: Kind) -> IconName {
    match k {
        Kind::Instance => IconName::Package,
        Kind::World => IconName::Globe,
        Kind::Backup => IconName::Archive,
        Kind::Server => IconName::Server,
        Kind::Screenshot => IconName::Image,
        Kind::File => IconName::File,
        Kind::Log => IconName::ScrollText,
    }
}

/// "Nothing here now.", "1 item, 6.1 GB.", "4 items, 9.3 GB."
pub fn summary(list: Option<&[(Entry, u64)]>) -> String {
    match list {
        None => "Counting…".into(),
        Some([]) => "Nothing here now.".into(),
        Some(l) => {
            let bytes = hopper::instance_ops::human_bytes(l.iter().map(|(_, b)| b).sum());
            if l.len() == 1 { format!("1 item, {bytes}.") } else { format!("{} items, {bytes}.", l.len()) }
        }
    }
}

/// Every entry Hopper knows of, with its size, newest first (off the UI
/// thread: it reads folders, and counts any whose size isn't stored).
pub fn load(beside: &[PathBuf]) -> Vec<(Entry, u64)> {
    rd::list_all(&rd::known_places(beside)).into_iter().map(|e| {
        let b = e.size();
        (e, b)
    }).collect()
}

/// What a restore did, for the notice and the reload after.
pub struct Restored {
    pub kind: Kind,
    pub name: String,
    pub to: PathBuf,
    /// Its name was taken: it came back under another.
    pub renamed: bool,
    /// What Hopper's settings held for an instance (its group, paired
    /// servers), to put back for it.
    pub settings: Option<hopper::settings::InstanceSettings>,
}

/// Puts `e` back (off the UI thread). An instance that comes back beside
/// a new one of the same name is called "<name> (restored)" in the
/// Library too.
pub fn restore(e: &Entry) -> anyhow::Result<Restored> {
    let to = rd::restore(e)?;
    let renamed = !matches!(e.meta.kind, Kind::Backup) && !e.meta.kind.is_files() && to != e.meta.from;
    if renamed && e.meta.kind == Kind::Instance {
        if let Some(mut i) = hopper::instances::load(&to) {
            i.name = format!("{} (restored)", i.name);
            if let Err(err) = i.save() {
                log::warn!("naming the restored {}: {err:#}", to.display());
            }
        }
    }
    let settings = e.meta.settings.clone().and_then(|v| serde_json::from_value(v).ok());
    Ok(Restored { kind: e.meta.kind, name: e.meta.name.clone(), to, renamed, settings })
}

impl Restored {
    pub fn notice(&self) -> String {
        match (self.kind, self.renamed) {
            (Kind::Backup, _) => format!("{} is back in Backups.", self.name),
            (_, true) => format!(
                "{} is back, as {} (its name was taken).",
                self.name,
                self.to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ),
            (_, false) => format!("{} is back.", self.name),
        }
    }
}
