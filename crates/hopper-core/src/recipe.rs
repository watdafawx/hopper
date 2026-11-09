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
/// the code is a newer version of the same pack. Returns the saved instance.
pub fn apply_to(root: &Path, code: &str, job: &Job) -> anyhow::Result<Instance> {
    let share = sharecode::decode(code)?;
    let mut inst = crate::instances::load(root)
        .with_context(|| format!("{} isn't an instance any more", root.display()))?;
    match fit(&inst, &share) {
        Fit::Same => {}
        Fit::PackUpdate(target) => {
            job.detail("Updating the pack to their version");
            match target {
                PackTarget::Curse { project, file } => {
                    let f = crate::cf::file(project, file)
                        .context("The pack version they use isn't on CurseForge any more")?;
                    crate::modpack::update_any(&mut inst, project, &f, None, job)?;
                }
                PackTarget::Modrinth { project, version } => {
                    let (p, v) = (
                        crate::modrinth::num_id(&project),
                        crate::modrinth::num_id(&version),
                    );
                    let f = crate::source::file(p, v)
                        .context("The pack version they use isn't on Modrinth any more")?;
                    crate::modpack::update_any(&mut inst, p, &f, None, job)?;
                }
            }
            inst = crate::instances::load(root)
                .with_context(|| format!("{} isn't an instance any more", root.display()))?;
            if let Fit::Different(why) = fit(&inst, &share) {
                anyhow::bail!("{why}");
            }
        }
        Fit::Different(why) => anyhow::bail!("{why}"),
    }
    sharecode::apply_existing(&mut inst, &share, job)?;
    Ok(inst)
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
        let inst = apply_to(&root, &code, &job).unwrap();
        assert!(!inst.addons.iter().find(|a| a.project_id == 101).unwrap().enabled);
        assert!(inst.addons.iter().all(|a| a.project_id != 303));
        assert!(!root.join("mods/one.jar").exists());
        assert!(root.join("mods/one.jar.disabled").exists());
        assert!(!root.join("mods/two.jar").exists());
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
        let err = apply_to(&root, &sharecode::encode(&share), &job()).unwrap_err();
        assert!(format!("{err:#}").contains("this instance is ATM"));
        assert!(root.join("mods/one.jar").exists());
    }
}
