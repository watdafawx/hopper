//! What to do on disk, and what a pack update does for each friend, before
//! anything runs (design §6, §7). Pure.

use super::model::{ConfigMode, Dot, Key, MemberId, PackMode, Value, Why};
use super::wire::MemberStatus;

/// One file change to make, and whose change it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub key: Key,
    pub from: Value,
    pub to: Value,
    pub by: Dot,
    pub name: String,
    /// Why the author made it (a pack update carries its stated mode).
    pub why: Why,
}

/// Why a key isn't changed yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    GameRunning,
    Review(u64),
    Source,
    /// The author blocked downloads: by hand.
    ByHand,
    /// Tried and failed; again after this many seconds.
    Retry { after_secs: u64 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApplyPlan {
    pub steps: Vec<Step>,
    pub waits: Vec<(Key, Wait)>,
}

/// What a pack update does on one friend's PC, from their last status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FriendPlan {
    pub member: MemberId,
    pub name: String,
    pub configs: ConfigMode,
    /// Their changed config files that stay theirs.
    pub kept_files: usize,
    /// Pack mods they turned off that stay off.
    pub stays_off: Vec<Key>,
    pub waits_for_game: bool,
    /// The line the dialog shows ("Mia: keeps her 3 changed settings files.").
    pub says: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackPlan {
    /// The line for this PC.
    pub here: String,
    pub friends: Vec<FriendPlan>,
}

/// What a pack update to `version` does on each PC, before the player presses
/// Update (design §6). `from` is the version this PC is on; `members` are the
/// friends' last statuses. The mode comes from the record the update will
/// make, so every PC applies what is shown here.
pub fn pack_plan(from: &str, version: &str, mode: &PackMode, members: &[MemberStatus]) -> PackPlan {
    let to = version_label(version);
    let here = match mode.here {
        ConfigMode::Keep => format!("This PC moves to {to}. Its changed settings files stay as they are."),
        ConfigMode::Clean => format!("This PC moves to {to}. Its settings files go back to the pack's."),
    };
    let _ = from;
    let friends = members
        .iter()
        .map(|m| {
            let name = if m.name.is_empty() { m.member.clone() } else { m.name.clone() };
            let kept_files = if mode.friends == ConfigMode::Keep { m.edited_configs.len() } else { 0 };
            let stays_off = if mode.keep_off { m.off_pack_mods.clone() } else { Vec::new() };
            let mut says = Vec::new();
            if m.game_running {
                says.push("Waits until their game closes.".to_string());
            }
            if m.pack == version {
                says.push(format!("Already on {to}."));
            }
            let files = m.edited_configs.len();
            if files > 0 {
                says.push(match mode.friends {
                    ConfigMode::Keep => format!("Keeps their {}.", count(files, "changed settings file")),
                    ConfigMode::Clean => format!(
                        "Their {} {} back to the pack's.",
                        count(files, "changed settings file"),
                        if files == 1 { "goes" } else { "go" }
                    ),
                });
            }
            if !stays_off.is_empty() {
                says.push(format!("{} stay{} off.", count(stays_off.len(), "mod"), if stays_off.len() == 1 { "s" } else { "" }));
            } else if !m.off_pack_mods.is_empty() {
                says.push(format!("{} come back on.", count(m.off_pack_mods.len(), "turned-off mod")));
            }
            if says.is_empty() {
                says.push(format!("Moves to {to}."));
            }
            FriendPlan {
                member: m.member.clone(),
                says: format!("{name}: {}", says.join(" ")),
                name,
                configs: mode.friends,
                kept_files,
                stays_off,
                waits_for_game: m.game_running,
            }
        })
        .collect();
    PackPlan { here, friends }
}

/// "8700" of "cf:100/8700"; another project's own text when it has no slash.
fn version_label(version: &str) -> &str {
    version.rsplit('/').next().unwrap_or(version)
}

fn count(n: usize, what: &str) -> String {
    format!("{n} {what}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str) -> MemberStatus {
        MemberStatus { member: id.into(), name: id.into(), ..MemberStatus::default() }
    }

    fn keep() -> PackMode {
        PackMode { here: ConfigMode::Keep, friends: ConfigMode::Keep, keep_off: true }
    }

    #[test]
    fn a_friend_who_changed_nothing_just_moves() {
        let p = pack_plan("cf:100/8600", "cf:100/8700", &keep(), &[member("Mia")]);
        assert_eq!(p.friends[0].says, "Mia: Moves to 8700.");
        assert_eq!(p.friends[0].kept_files, 0);
        assert!(p.here.contains("8700"));
    }

    #[test]
    fn kept_files_and_mods_that_stay_off_are_said_per_friend() {
        let mut mia = member("Mia");
        mia.edited_configs = vec!["a.toml".into(), "b.toml".into(), "c.toml".into()];
        mia.off_pack_mods = vec![Key::Mod("cf:4".into())];
        let p = pack_plan("cf:100/8600", "cf:100/8700", &keep(), &[mia]);
        let f = &p.friends[0];
        assert_eq!(f.kept_files, 3);
        assert_eq!(f.stays_off, vec![Key::Mod("cf:4".into())]);
        assert_eq!(f.says, "Mia: Keeps their 3 changed settings files. 1 mod stays off.");
    }

    #[test]
    fn clean_here_leaves_friends_files_unless_everyone_is_asked() {
        let mut mia = member("Mia");
        mia.edited_configs = vec!["a.toml".into()];
        let here_only = PackMode { here: ConfigMode::Clean, friends: ConfigMode::Keep, keep_off: true };
        let p = pack_plan("", "cf:100/8700", &here_only, std::slice::from_ref(&mia));
        assert!(p.here.contains("go back to the pack's"));
        assert_eq!(p.friends[0].configs, ConfigMode::Keep);
        assert_eq!(p.friends[0].kept_files, 1);
        let everyone = PackMode { here: ConfigMode::Clean, friends: ConfigMode::Clean, keep_off: false };
        mia.off_pack_mods = vec![Key::Mod("cf:4".into()), Key::Mod("cf:5".into())];
        let p = pack_plan("", "cf:100/8700", &everyone, &[mia]);
        let f = &p.friends[0];
        assert_eq!(f.configs, ConfigMode::Clean);
        assert_eq!(f.kept_files, 0);
        assert!(f.stays_off.is_empty());
        assert_eq!(f.says, "Mia: Their 1 changed settings file goes back to the pack's. 2 turned-off mods come back on.");
    }

    #[test]
    fn a_running_game_and_an_up_to_date_friend_are_said() {
        let mut noah = member("Noah");
        noah.game_running = true;
        let mut mia = member("Mia");
        mia.pack = "cf:100/8700".into();
        let p = pack_plan("", "cf:100/8700", &keep(), &[noah, mia]);
        assert!(p.friends[0].waits_for_game);
        assert_eq!(p.friends[0].says, "Noah: Waits until their game closes.");
        assert!(!p.friends[1].waits_for_game);
        assert_eq!(p.friends[1].says, "Mia: Already on 8700.");
    }
}
