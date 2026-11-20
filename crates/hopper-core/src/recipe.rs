//! Recipe sync: an instance as a share code, applied to an instance a peer
//! already has.
//!
//! Copying jars between PCs moves a lot of bytes for nothing: a share code
//! already names the pack, its exact version and only what differs from it,
//! and every file it names can be downloaded from CurseForge, Modrinth,
//! GitHub, Maven, Jenkins or the link it came from. So a peer sends the
//! code; the other PC fetches the files itself.
//!
//! Applying is in place: the local instance must already start from the
//! same pack (or the same plain game and loader). A newer version of that
//! pack is updated with [`crate::modpack::update_any`] first. Anything
//! else is refused with a message, rather than silently rebuilding a
//! different instance.

use crate::instances::Instance;
use crate::jobs::Job;
use crate::sharecode::{self, Base, Share};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{Duration, Instant};

/// One instance offered to a peer: the share code plus what it could not
/// describe, for the other side's "ask them to send these" list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recipe {
    /// The sender's instance folder name; how the receiver addresses it.
    pub inst: String,
    pub name: String,
    /// The `HOP-…` share code.
    pub code: String,
    /// Files with no source to download them from (a hand-made jar).
    pub left_out: Vec<String>,
}

/// Builds the recipe for `inst`, addressed as `key`.
pub fn of(inst: &Instance, key: &str, job: &Job) -> anyhow::Result<Recipe> {
    let made = sharecode::make(inst, job)?;
    Ok(Recipe {
        inst: key.to_string(),
        name: inst.name.clone(),
        code: made.code,
        left_out: made.left_out,
    })
}

/// The pack a newer recipe starts from, when the local instance has an
/// older file of the same pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackTarget {
    Curse { project: u64, file: u64 },
    Modrinth { project: String, version: String },
}

/// How a code fits the local instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Same pack file (or same plain game and loader): apply the entries.
    Same,
    /// Same pack, another version: update the pack first.
    PackUpdate(PackTarget),
    /// Another pack, or a plain code against a pack instance. The player
    /// has to install it as a new instance with the share code.
    Different(String),
}

/// Whether `share` can be applied to `inst`, and if so how.
pub fn fit(inst: &Instance, share: &Share) -> Fit {
    // A loader override in the code changes what the game runs; that is an
    // install, not a recipe applied over the current one.
    if let Some(l) = &share.loader
        && !l.is_empty()
        && inst.loader != *l
    {
        return Fit::Different(format!(
            "This recipe runs on {l}, but this instance runs {}.",
            inst.loader
        ));
    }
    match &share.base {
        Base::Plain {
            game_version,
            loader,
        } => {
            if let Some(p) = &inst.pack {
                return Fit::Different(format!(
                    "This recipe makes a plain instance, but this one is the pack {}. Install it as a new instance with its share code.",
                    p.name
                ));
            }
            if inst.game_version != *game_version {
                return Fit::Different(format!(
                    "This recipe is for Minecraft {game_version}, but this instance is {}.",
                    inst.game_version
                ));
            }
            if !loader.is_empty() && inst.loader != *loader {
                return Fit::Different(format!(
                    "This recipe uses {loader}, but this instance uses {}.",
                    inst.loader
                ));
            }
            Fit::Same
        }
        Base::Curse { project, file } => {
            let Some(p) = &inst.pack else {
                return Fit::Different(
                    "This recipe starts from a modpack, but this instance isn't one. Install it as a new instance with its share code."
                        .into(),
                );
            };
            if crate::modrinth::is_mr(p.project_id) || p.project_id != *project {
                return Fit::Different(format!(
                    "This recipe starts from {}, but this instance is {}.",
                    p.name,
                    pack_name(p)
                ));
            }
            if p.file_id == *file {
                Fit::Same
            } else {
                Fit::PackUpdate(PackTarget::Curse {
                    project: *project,
                    file: *file,
                })
            }
        }
        Base::Modrinth { project, version } => {
            let Some(p) = &inst.pack else {
                return Fit::Different(
                    "This recipe starts from a modpack, but this instance isn't one. Install it as a new instance with its share code."
                        .into(),
                );
            };
            let same_project = (!p.mr_project.is_empty() && p.mr_project == *project)
                || (p.project_id != 0 && p.project_id == crate::modrinth::num_id(project));
            if !same_project {
                return Fit::Different(format!(
                    "This recipe starts from {}, but this instance is {}.",
                    p.name,
                    pack_name(p)
                ));
            }
            let same_version = (!p.mr_version.is_empty() && p.mr_version == *version)
                || (p.file_id != 0 && p.file_id == crate::modrinth::num_id(version));
            if same_version {
                Fit::Same
            } else {
                Fit::PackUpdate(PackTarget::Modrinth {
                    project: project.clone(),
                    version: version.clone(),
                })
            }
        }
    }
}

fn pack_name(p: &crate::instances::PackRef) -> String {
    if p.name.is_empty() {
        "another pack".into()
    } else {
        p.name.clone()
    }
}

/// Applies `code` to the instance at `root`, updating its pack first when
/// the code is a newer version of the same pack. Mods from GitHub and other
/// links in `skip` are left out (the player said no, or wasn't there to
/// ask; see [`crate::sources::carry::auto_skip`]), with a note on the job.
/// `prev`: the code applied here before (empty for none), so mods the
/// sender removed or turned back on since then change here too.
/// Returns the saved instance.
pub fn apply_to(
    root: &Path,
    code: &str,
    prev: &str,
    skip: &[crate::sources::carry::Pin],
    job: &Job,
) -> anyhow::Result<Instance> {
    let (share, note) = sharecode::without(&sharecode::decode(code)?, skip);
    // An old code that no longer reads only means less is compared.
    let prev = (!prev.is_empty() && prev != code).then(|| sharecode::decode(prev).ok()).flatten();
    if let Some(n) = note {
        job.note(n);
    }
    // Same reason as groupsync::merge: nothing else changes the mods while
    // this copy is the one being applied and saved.
    let _edit = crate::instances::edit_lock(root);
    let mut inst = crate::instances::load(root)
        .with_context(|| format!("{} isn't an instance any more", root.display()))?;
    // An instance whose file couldn't be read loads blank, and a blank
    // instance fits nothing: say what's wrong instead of "this instance is
    // Minecraft ''". A file only held open reads fine on the next try.
    if inst.unreadable.is_some() {
        if crate::instances::may_read_later(&inst) {
            anyhow::bail!("Another program has this instance's files open.");
        }
        return Err(NoFit(format!("{}'s settings file is damaged.", inst.name)).into());
    }
    match fit(&inst, &share) {
        Fit::Same => {}
        Fit::PackUpdate(target) => {
            job.detail("Updating the pack to their version");
            // Whatever the pack's new version changes is the sender's in History.
            let _own = crate::actor::own_all();
            match target {
                PackTarget::Curse { project, file } => {
                    let f = crate::cf::file(project, file)
                        .context("The pack version they use isn't on CurseForge any more")?;
                    crate::modpack::update_any_synced(&mut inst, project, &f, job)?;
                }
                PackTarget::Modrinth { project, version } => {
                    let (p, v) = (
                        crate::modrinth::num_id(&project),
                        crate::modrinth::num_id(&version),
                    );
                    let f = crate::source::file(p, v)
                        .context("The pack version they use isn't on Modrinth any more")?;
                    crate::modpack::update_any_synced(&mut inst, p, &f, job)?;
                }
            }
            inst = crate::instances::load(root)
                .with_context(|| format!("{} isn't an instance any more", root.display()))?;
            if let Fit::Different(why) = fit(&inst, &share) {
                return Err(NoFit(why).into());
            }
        }
        Fit::Different(why) => return Err(NoFit(why).into()),
    }
    sharecode::apply_existing(&mut inst, &share, prev.as_ref(), job)?;
    Ok(inst)
}

/// What a follow kept in step with a group may call its applied code once
/// `code` was applied to `inst` ([`applied_code`]).
#[derive(Clone, Debug, PartialEq)]
pub struct AppliedCode {
    /// `code` without the files that didn't land ([`sharecode::landed`]).
    /// `code` itself when everything did.
    pub code: String,
    /// The files that failed to come in, for people ("a CurseForge mod
    /// (file 51)"), for the follow's `left_out`. Mods the player chose not
    /// to install are out of the code but not listed here (the apply said so).
    pub missing: Vec<String>,
}

/// The code a group pack's follow keeps as applied, after [`apply_to`] (or
/// the install of a new follow): only what landed. An apply notes a file it
/// couldn't put in and goes on, but the group copy starts from this code
/// (`groupsync::reconcile`) and works out what's new from it, so a file
/// named in it that never came in would never be fetched again, with every
/// screen saying the copy is in step. Left out of it, the file shows as an
/// added mod the next time the sender's code is read, and goes in then.
/// `skip`: the sources the apply left out on purpose. Said in the task and
/// the log; a follow that isn't kept in step with a group keeps the code
/// whole (it is applied again by itself until it matches: don't call this).
pub fn applied_code(inst: &Instance, code: &str, skip: &[crate::sources::carry::Pin], job: &Job) -> AppliedCode {
    let whole = AppliedCode { code: code.to_string(), missing: Vec::new() };
    let Ok(share) = sharecode::decode(code) else { return whole };
    let l = sharecode::landed(inst, &share);
    if l.missing.is_empty() {
        return whole;
    }
    let failed: Vec<sharecode::Entry> =
        l.missing.iter().filter(|e| !matches!(e, sharecode::Entry::Source { pin, .. } if skip.contains(pin))).cloned().collect();
    if !failed.is_empty() {
        let what = sharecode::entries_say(&failed);
        log::warn!("followed pack at {}: {} file(s) didn't come in, kept out of the applied code: {what}", inst.root.display(), failed.len());
        job.show();
        job.note(format!(
            "{} didn't come in: {what}. Hopper counts {} as not received and tries again when it next reads the group's changes.",
            if failed.len() == 1 { "1 file" } else { "Some files" },
            if failed.len() == 1 { "it" } else { "them" },
        ));
    }
    AppliedCode { code: l.code, missing: failed.iter().map(sharecode::entry_says).filter(|n| !n.is_empty()).collect() }
}

/// [`apply_to`] refused: the code doesn't fit this instance any more (see
/// [`Fit::Different`]). Trying the same code again can't help.
#[derive(Debug)]
pub struct NoFit(pub String);

impl std::fmt::Display for NoFit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoFit {}

/// How long a followed pack waits after its `failures`-th failed automatic
/// apply before the next try: a few seconds at first, then every 15
/// seconds. Most failures pass within seconds (a file another program held,
/// the internet down for a moment); a try costs one look at the instance and
/// the mod sites, so trying often keeps the change landing soon after the
/// cause clears.
pub fn retry_after(failures: u32) -> Duration {
    const STEPS: [u64; 5] = [2, 3, 5, 10, 15];
    let i = (failures.max(1) as usize - 1).min(STEPS.len() - 1);
    Duration::from_secs(STEPS[i])
}

/// Quick tries of one code before Hopper stops trying it by itself (about a
/// minute and a half of [`retry_after`]'s waits). A failure that comes back
/// that often won't pass by itself: trying on every 15 seconds only fills
/// the log and keeps the instance busy. A newer code, a restart or the
/// player trying again starts over.
pub const GIVE_UP_AFTER: u32 = 10;

/// Past [`GIVE_UP_AFTER`], how long a failure that passes by itself (the
/// internet, a mod site down, a file another program holds) waits between
/// tries. Those are still tried: they end without the player doing anything.
pub const SLOW_RETRY: Duration = Duration::from_secs(300);

/// Whether `e` is the kind of failure that ends by itself.
fn passes(e: &anyhow::Error) -> bool {
    // The friends service down for a moment, or it couldn't save: its
    // message doesn't carry the status, so ask the error itself.
    if crate::mailbox::retryable(e) {
        return true;
    }
    let all = format!("{e:#}").to_lowercase();
    let words = ["dns", "connect", "timed out", "timeout", "network", "unreachable", "connection", "files open", "503", "502", "504", "500 "];
    words.iter().any(|w| all.contains(w))
}

/// A followed pack whose automatic apply failed, until a try works.
#[derive(Clone, Debug, PartialEq)]
pub struct Retry {
    /// The code that failed; a newer one is tried at once.
    pub code: String,
    pub failures: u32,
    /// When the next try is due; `None` when trying the same code again
    /// can't help (it doesn't fit, see [`NoFit`]).
    pub next: Option<Instant>,
    /// What went wrong, for the player.
    pub error: String,
    /// The failed task, taken off the task list when the next try starts,
    /// so retries don't pile up there.
    pub job: u64,
    /// GitHub's rate limit is why: nothing is tried (not even a newer code)
    /// before it resets.
    pub limited: Option<Instant>,
    /// Whether this failure is news for the player: the first one, or a
    /// different reason than the last. The same reason again (another
    /// retry, a newer code failing the same way) stays quiet; the row
    /// still says it.
    pub tell: bool,
    /// Hopper stopped trying this code by itself: it failed
    /// [`GIVE_UP_AFTER`] times in a row for a reason that won't pass on
    /// its own. `next` is `None`; `error` says why. A newer code is still
    /// tried at once.
    pub gave_up: bool,
}

impl Retry {
    /// After a failed try of `code` (`prev`: the state before it).
    pub fn failed(prev: Option<&Retry>, code: &str, e: &anyhow::Error, job: u64, now: Instant) -> Retry {
        let failures = prev.filter(|p| p.code == code).map_or(0, |p| p.failures) + 1;
        let stuck = e.downcast_ref::<NoFit>().is_some();
        let limit = crate::sources::http::rate_limited(e);
        let limited = limit.map(|l| now + l.wait());
        let error = says(e);
        // A limit's text names its end time, which may move; it's the same
        // failure all the same.
        let same = prev.is_some_and(|p| if limited.is_some() { p.limited.is_some() } else { p.limited.is_none() && p.error == error });
        let many = failures >= GIVE_UP_AFTER && limited.is_none();
        let passes = passes(e);
        let gave_up = many && !stuck && !passes;
        let next = if stuck || gave_up {
            None
        } else if let Some(t) = limited {
            // Back when GitHub resets, not every few seconds.
            Some(t)
        } else if many {
            Some(now + SLOW_RETRY)
        } else {
            Some(now + retry_after(failures))
        };
        Retry {
            code: code.to_string(),
            failures,
            next,
            error,
            job,
            limited,
            // Stopping is news even when the reason is the same as before.
            tell: !same || (gave_up && !prev.is_some_and(|p| p.gave_up)),
            gave_up,
        }
    }

    /// Still waiting out GitHub's rate limit at `now`.
    pub fn waits_for_limit(&self, now: Instant) -> bool {
        self.limited.is_some_and(|t| now < t)
    }
}

/// Whether follow `f` has a newer code to apply by itself now: one that
/// isn't applied yet, from a pack still shared, unless a failed try of that
/// same code is still waiting out its delay (or can't work). `busy`: a
/// task is changing the instance or its game is launching; it waits for
/// that to end. A game that is running doesn't count: the follow applies
/// while it plays, and a jar it holds open changes when it closes
/// ([`crate::locked`]).
pub fn due(f: &crate::settings::FollowedRecipe, retry: Option<&Retry>, busy: bool, now: Instant) -> bool {
    if !f.waiting() || busy {
        return false;
    }
    match retry {
        Some(r) if r.waits_for_limit(now) => false,
        Some(r) if r.code == f.seen => r.next.is_some_and(|t| now >= t),
        _ => true,
    }
}

/// A failed apply in plain words: the reason it gives, or for a network
/// problem, that the mod sites couldn't be reached.
pub fn says(e: &anyhow::Error) -> String {
    if let Some(n) = e.downcast_ref::<NoFit>() {
        return n.0.clone();
    }
    if let Some(l) = crate::sources::http::rate_limited(e) {
        return l.to_string();
    }
    let all = format!("{e:#}").to_lowercase();
    let net = ["dns", "connect", "timed out", "timeout", "network", "unreachable", "connection"];
    if net.iter().any(|w| all.contains(w)) {
        return "Couldn't reach CurseForge or Modrinth. Check your internet connection.".into();
    }
    let s = e.to_string();
    if s.ends_with('.') { s } else { format!("{s}.") }
}

/// The key a mod from GitHub or a link goes by when the player answered
/// about it: its source and file shape, so a newer file of it counts too.
pub fn pin_key(p: &crate::sources::carry::Pin) -> String {
    format!("{}#{}", p.source.key(), p.source.pattern)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::{Addon, PackRef};
    use crate::jobs::Jobs;
    use crate::sharecode::{Entry, Target};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp(name: &str) -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("hopper-recipe-{name}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn job() -> Job {
        let jobs = Jobs::new(Arc::new(|| {}));
        jobs.start_quiet("test")
    }

    fn addon(project: u64, file: u64, name: &str) -> Addon {
        Addon {
            project_id: project,
            file_id: file,
            file_name: format!("{name}.jar"),
            name: name.into(),
            enabled: true,
            ..Default::default()
        }
    }

    fn plain(name: &str) -> (std::path::PathBuf, Instance) {
        let root = temp(name);
        std::fs::create_dir_all(root.join("mods")).unwrap();
        let inst = Instance {
            name: name.into(),
            root: root.clone(),
            game_version: "1.21.1".into(),
            loader: "neoforge-21.1.265".into(),
            addons: vec![addon(101, 202, "one"), addon(303, 404, "two")],
            ..Default::default()
        };
        for a in &inst.addons {
            std::fs::write(inst.addon_path(a), b"jar").unwrap();
        }
        inst.save().unwrap();
        (root, inst)
    }

    #[test]
    fn a_plain_instance_round_trips_as_a_recipe() {
        let (_, inst) = plain("round-trip");
        let job = job();
        let r = of(&inst, "key", &job).unwrap();
        assert_eq!(r.inst, "key");
        assert_eq!(r.name, "round-trip");
        assert!(r.left_out.is_empty());
        let share = sharecode::decode(&r.code).unwrap();
        assert_eq!(
            share.base,
            Base::Plain {
                game_version: "1.21.1".into(),
                loader: "neoforge-21.1.265".into()
            }
        );
        assert_eq!(fit(&inst, &share), Fit::Same);
    }

    #[test]
    fn base_mismatches_are_said_not_guessed() {
        let (_, inst) = plain("fit");
        let job = job();
        let mut share = sharecode::decode(&of(&inst, "key", &job).unwrap().code).unwrap();
        share.base = Base::Plain {
            game_version: "1.20.1".into(),
            loader: "neoforge-21.1.265".into(),
        };
        assert!(matches!(fit(&inst, &share), Fit::Different(_)));

        // Same pack, other file: an update.
        let mut packed = inst.clone();
        packed.pack = Some(PackRef {
            project_id: 7,
            file_id: 1,
            name: "ATM".into(),
            ..Default::default()
        });
        share.base = Base::Curse {
            project: 7,
            file: 2,
        };
        assert_eq!(
            fit(&packed, &share),
            Fit::PackUpdate(PackTarget::Curse {
                project: 7,
                file: 2
            })
        );

        // Another pack: no.
        share.base = Base::Curse {
            project: 8,
            file: 2,
        };
        assert!(matches!(fit(&packed, &share), Fit::Different(_)));
    }

    #[test]
    fn applying_off_and_remove_changes_the_instance_in_place() {
        let (root, _) = plain("apply");
        let job = job();
        let share = Share {
            base: Base::Plain {
                game_version: "1.21.1".into(),
                loader: "neoforge-21.1.265".into(),
            },
            name: None,
            loader: None,
            entries: vec![
                Entry::Off(Target::Curse(101)),
                Entry::Remove(Target::Curse(303)),
            ],
        };
        let code = sharecode::encode(&share);
        let inst = apply_to(&root, &code, "", &[], &job).unwrap();
        assert!(!inst.addons.iter().find(|a| a.project_id == 101).unwrap().enabled);
        assert!(inst.addons.iter().all(|a| a.project_id != 303));
        assert!(!root.join("mods/one.jar").exists());
        assert!(root.join("mods/one.jar.disabled").exists());
        assert!(!root.join("mods/two.jar").exists());
    }

    fn plain_share(entries: Vec<Entry>) -> String {
        sharecode::encode(&Share {
            base: Base::Plain {
                game_version: "1.21.1".into(),
                loader: "neoforge-21.1.265".into(),
            },
            name: None,
            loader: None,
            entries,
        })
    }

    /// The follower's report: a mod turned off on the sender's PC went off
    /// here, but turning it back on (and removing one) never arrived, as a
    /// code only lists what's added or off.
    #[test]
    fn a_followed_pack_turns_mods_back_on_and_removes_what_they_removed() {
        let (root, mut inst) = plain("follow-diff");
        // As their last code left it: "one" (101) off, "two" (303, file
        // 404) a file of theirs.
        let i = inst.addons.iter().position(|a| a.project_id == 101).unwrap();
        crate::install::set_enabled(&mut inst, i, false).unwrap();
        let first = plain_share(vec![Entry::Off(Target::Curse(101)), Entry::CurseFile { file: 404, enabled: true }]);

        // They turn "one" back on and remove "two": the new code lists
        // neither. Compared with the last one, both change here.
        let second = plain_share(Vec::new());
        let inst = apply_to(&root, &second, &first, &[], &job()).unwrap();
        assert!(inst.addons.iter().find(|a| a.project_id == 101).unwrap().enabled);
        assert!(root.join("mods/one.jar").exists());
        assert!(inst.addons.iter().all(|a| a.project_id != 303));
        assert!(!root.join("mods/two.jar").exists());

        // Without the last code nothing is guessed: what's here stays.
        let (root, _) = plain("follow-no-prev");
        let inst = apply_to(&root, &second, "", &[], &job()).unwrap();
        assert_eq!(inst.addons.len(), 2);
    }

    #[test]
    fn a_different_pack_is_refused_without_touching_files() {
        let (root, mut inst) = plain("refuse");
        inst.pack = Some(PackRef {
            project_id: 7,
            file_id: 1,
            name: "ATM".into(),
            ..Default::default()
        });
        inst.save().unwrap();
        let share = Share {
            base: Base::Curse {
                project: 8,
                file: 2,
            },
            name: None,
            loader: None,
            entries: Vec::new(),
        };
        let err = apply_to(&root, &sharecode::encode(&share), "", &[], &job()).unwrap_err();
        assert!(format!("{err:#}").contains("this instance is ATM"));
        assert!(root.join("mods/one.jar").exists());
        // Trying it again can't help: no retry is planned.
        let r = Retry::failed(None, "HOP-x", &err, 1, Instant::now());
        assert_eq!(r.next, None);
        assert!(r.error.contains("this instance is ATM"));
    }

    /// The follower's report: a friend's change failed to apply once (the
    /// instance's file held open by another program) and showed "Update"
    /// for good. Held open is a failure to try again, not "a different
    /// instance", and says so.
    #[cfg(windows)]
    #[test]
    fn an_instance_held_open_is_tried_again_not_called_different() {
        use std::os::windows::fs::OpenOptionsExt;
        let (root, _) = plain("held-open");
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(root.join(crate::instances::HOPPER_FILE))
            .unwrap();
        let code = plain_share(vec![Entry::Off(Target::Curse(101))]);
        let err = apply_to(&root, &code, "", &[], &job()).unwrap_err();
        assert!(err.downcast_ref::<NoFit>().is_none(), "{err:#}");
        assert_eq!(says(&err), "Another program has this instance's files open.");
        let now = Instant::now();
        let r = Retry::failed(None, &code, &err, 1, now);
        assert_eq!(r.next, Some(now + Duration::from_secs(2)));
        drop(held);
        // Once it lets go, the same code applies.
        let inst = apply_to(&root, &code, "", &[], &job()).unwrap();
        assert!(!inst.addons.iter().find(|a| a.project_id == 101).unwrap().enabled);
    }

    #[test]
    fn failed_follows_wait_longer_each_time_up_to_15_seconds() {
        let secs: Vec<u64> = (0..9).map(|n| retry_after(n).as_secs()).collect();
        assert_eq!(secs, [2, 2, 3, 5, 10, 15, 15, 15, 15]);

        let t0 = Instant::now();
        let net = anyhow::anyhow!("dns error: no such host").context("Looking up the mods");
        let r1 = Retry::failed(None, "HOP-1", &net, 10, t0);
        assert_eq!((r1.failures, r1.next), (1, Some(t0 + Duration::from_secs(2))));
        let r2 = Retry::failed(Some(&r1), "HOP-1", &net, 11, t0);
        assert_eq!((r2.failures, r2.next), (2, Some(t0 + Duration::from_secs(3))));
        // A newer code starts over.
        let r3 = Retry::failed(Some(&r2), "HOP-2", &net, 12, t0);
        assert_eq!(r3.failures, 1);
        assert_eq!(r3.error, "Couldn't reach CurseForge or Modrinth. Check your internet connection.");
    }

    #[test]
    fn an_apply_that_always_fails_stops_after_ten_tries_and_says_so() {
        use crate::settings::FollowedRecipe;
        let t0 = Instant::now();
        let e = anyhow::anyhow!("CurseForge has no file 123");
        let mut r = Retry::failed(None, "HOP-1", &e, 1, t0);
        assert!(r.tell && !r.gave_up);
        for n in 2..GIVE_UP_AFTER {
            r = Retry::failed(Some(&r), "HOP-1", &e, n as u64, t0);
            assert!(!r.gave_up && !r.tell && r.next.is_some(), "try {n}");
        }
        let last = Retry::failed(Some(&r), "HOP-1", &e, 99, t0);
        assert_eq!((last.failures, last.gave_up, last.next), (GIVE_UP_AFTER, true, None));
        assert!(last.tell, "stopping is said once");
        assert_eq!(last.error, "CurseForge has no file 123.");
        let f = FollowedRecipe { code: "HOP-0".into(), seen: "HOP-1".into(), ..Default::default() };
        assert!(!due(&f, Some(&last), false, t0 + Duration::from_secs(86_400)), "not tried again by itself");
        // A newer code goes at once, and counts from one again.
        let newer = FollowedRecipe { seen: "HOP-2".into(), ..f.clone() };
        assert!(due(&newer, Some(&last), false, t0));
        let again = Retry::failed(Some(&last), "HOP-2", &e, 100, t0);
        assert!(!again.gave_up && again.failures == 1 && !again.tell);
    }

    #[test]
    fn a_failure_that_passes_by_itself_is_tried_slowly_not_given_up() {
        let t0 = Instant::now();
        let unsaved = || -> anyhow::Error {
            crate::mailbox::MailboxError { status: 503, code: "save_failed".into(), message: "could not save that; try again".into() }.into()
        };
        for e in [anyhow::anyhow!("dns error: no such host"), anyhow::anyhow!("Another program has this instance's files open."), unsaved()] {
            let mut r = Retry::failed(None, "HOP-1", &e, 1, t0);
            for n in 2..=GIVE_UP_AFTER + 3 {
                r = Retry::failed(Some(&r), "HOP-1", &e, n as u64, t0);
            }
            assert!(!r.gave_up, "{e}");
            assert_eq!(r.next, Some(t0 + SLOW_RETRY), "{e}");
        }
    }

    #[test]
    fn a_github_rate_limit_waits_for_its_reset_and_is_said_once() {
        use crate::sources::http::RateLimited;
        use crate::settings::FollowedRecipe;
        let t0 = Instant::now();
        let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let limit = || anyhow::Error::new(RateLimited { until: unix + 600 }).context("EZActions wasn't added");
        let r1 = Retry::failed(None, "HOP-1", &limit(), 1, t0);
        assert!(r1.tell);
        assert!(r1.error.starts_with("GitHub is limiting downloads for now. Hopper tries again at "), "{}", r1.error);
        // Waits about ten minutes, not 2 to 15 seconds.
        let wait = r1.next.unwrap() - t0;
        assert!(wait > Duration::from_secs(590) && wait <= Duration::from_secs(600), "{wait:?}");
        // A retry, or a newer code from another member, failing the same
        // way: quiet, and a newer code waits for the reset too.
        let r2 = Retry::failed(Some(&r1), "HOP-1", &limit(), 2, t0);
        let r3 = Retry::failed(Some(&r2), "HOP-2", &limit(), 3, t0);
        assert!(!r2.tell && !r3.tell);
        let f = FollowedRecipe { code: "HOP-1".into(), seen: "HOP-3".into(), ..Default::default() };
        assert!(!due(&f, Some(&r3), false, t0 + Duration::from_secs(60)));
        assert!(due(&f, Some(&r3), false, t0 + Duration::from_secs(601)));
        assert!(!due(&f, Some(&r3), false, t0 + Duration::from_secs(589)));
        // Another reason after that is news again; the same one twice isn't.
        let net = anyhow::anyhow!("dns error");
        let r4 = Retry::failed(Some(&r3), "HOP-3", &net, 4, t0);
        let r5 = Retry::failed(Some(&r4), "HOP-4", &net, 5, t0);
        assert!(r4.tell && !r5.tell && r4.limited.is_none());
    }

    #[test]
    fn a_follow_applies_by_itself_unless_it_waits_or_is_up_to_date() {
        use crate::settings::FollowedRecipe;
        let t0 = Instant::now();
        let f = FollowedRecipe { code: "HOP-1".into(), seen: "HOP-2".into(), ..Default::default() };
        // A new code, nothing in the way: now. On start too, as nothing
        // in memory says otherwise.
        assert!(due(&f, None, false, t0));
        // Another task changes the instance or its game is launching.
        assert!(!due(&f, None, true, t0));
        // Up to date, never got a code, or the pack isn't shared any more.
        assert!(!due(&FollowedRecipe { seen: "HOP-1".into(), ..f.clone() }, None, false, t0));
        assert!(!due(&FollowedRecipe { code: String::new(), seen: String::new(), ..f.clone() }, None, false, t0));
        assert!(!due(&FollowedRecipe { ended: "unshared".into(), ..f.clone() }, None, false, t0));

        // Failed: waits out the delay, then goes again.
        let e = anyhow::anyhow!("CurseForge said 503");
        let r = Retry::failed(None, "HOP-2", &e, 1, t0);
        assert!(!due(&f, Some(&r), false, t0 + Duration::from_secs(1)));
        assert!(due(&f, Some(&r), false, t0 + Duration::from_secs(2)));
        // Can't work: waits for a newer code, which goes at once.
        let stuck = Retry::failed(None, "HOP-2", &anyhow::Error::new(NoFit("no".into())), 1, t0);
        assert!(!due(&f, Some(&stuck), false, t0 + Duration::from_secs(3600)));
        let newer = FollowedRecipe { seen: "HOP-3".into(), ..f.clone() };
        assert!(due(&newer, Some(&stuck), false, t0));
    }

    #[test]
    fn a_follow_kept_in_step_keeps_only_the_files_that_came_in() {
        let jobs = Jobs::new(Arc::new(|| {}));
        let job = jobs.start_quiet("test");
        let code = sharecode::encode(&Share {
            base: Base::Plain { game_version: "1.21.1".into(), loader: "neoforge".into() },
            name: None,
            loader: None,
            entries: vec![
                Entry::CurseFile { file: 11, enabled: true },
                Entry::CurseFile { file: 21, enabled: true },
                Entry::CurseFile { file: 31, enabled: true },
            ],
        });
        // File 21 failed to come in; the apply noted it and went on.
        let inst = Instance { addons: vec![addon(1, 11, "a"), addon(3, 31, "c")], ..Default::default() };
        let a = applied_code(&inst, &code, &[], &job);
        let kept: Vec<u64> = sharecode::decode(&a.code)
            .unwrap()
            .entries
            .iter()
            .filter_map(|e| if let Entry::CurseFile { file, .. } = e { Some(*file) } else { None })
            .collect();
        assert_eq!(kept, [11, 31], "the code the copy starts from has only what arrived");
        assert_eq!(a.missing, ["a CurseForge mod (file 21)"], "and the missing file is named");
        let notes = &jobs.snapshot()[0].notes;
        assert_eq!(notes.len(), 1, "said in the task, not silent: {notes:?}");
        assert!(notes[0].contains("file 21"), "{notes:?}");
        // All of it came in: the code is kept as it is, nothing said.
        let jobs = Jobs::new(Arc::new(|| {}));
        let job = jobs.start_quiet("test");
        let all = Instance { addons: vec![addon(1, 11, "a"), addon(2, 21, "b"), addon(3, 31, "c")], ..Default::default() };
        let a = applied_code(&all, &code, &[], &job);
        assert_eq!(a, AppliedCode { code: code.clone(), missing: Vec::new() });
        assert_eq!(job.note_count(), 0);
    }
}
