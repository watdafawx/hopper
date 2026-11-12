//! "Only on this PC" (see `hopper::onlyhere`): which packs keep their mods
//! the same as friends, and marking mods to stay out of that.

use super::{Msg, Store, server_mods};
use hopper::groupsync::addon_key;
use hopper::onlyhere::Kept;
use std::collections::BTreeMap;
use std::path::Path;

/// Who keeps a pack the same as this PC.
pub struct SyncedWith {
    /// The groups among them, by name.
    pub groups: Vec<String>,
    /// The friends it's shared with directly, by name.
    pub friends: Vec<String>,
}

impl SyncedWith {
    /// Who a change reaches, after "for": the group by name ("Weekend
    /// crew"), one friend by name ("Mia"), several as a count ("3 friends").
    pub fn for_whom(&self) -> String {
        let friends = match self.friends.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            many => Some(format!("{} friends", many.len())),
        };
        let names: Vec<String> = self.groups.iter().cloned().chain(friends).collect();
        and_list(&names)
    }
}

/// Locked mods a change turns off or removes for friends too, as friends
/// had them just before, for the toast's "Just for me".
pub struct LockedChange {
    pub with: SyncedWith,
    /// Their names, in the order picked.
    pub names: Vec<String>,
    /// Each one's "Only on this PC" mark as it would have been made just
    /// before the change, so friends get it back the way it was.
    pub kept: BTreeMap<String, Kept>,
}

/// "Mia", "Mia and Sam", "Mia, Sam and Ana", "Mia, Sam and 3 more".
pub fn and_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, c] => format!("{a}, {b} and {c}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

fn name_or(name: &str, or: &str) -> String {
    if name.trim().is_empty() { or.to_string() } else { name.to_string() }
}

impl Store {
    /// The friends and groups who keep this pack the same as this PC: it's
    /// shared with them, or it's a group copy kept both ways. `None` when
    /// nobody does (turning a mod off here then only changes this PC).
    pub fn synced_with(&self, root: &Path) -> Option<SyncedWith> {
        if server_mods::remote(root).is_some() || self.is_server_list(root) {
            return None;
        }
        let inst = self.instance(root)?;
        if inst.id.is_empty() {
            return None;
        }
        let copy = self.group_copy(root).map(|c| c.group.clone());
        let friends: Vec<String> = self
            .mailbox_friends()
            .iter()
            .filter(|f| self.settings().shares_with(&inst.id, &f.id))
            .map(|f| name_or(&f.name, "a friend"))
            .collect();
        let groups: Vec<String> = self
            .mailbox_groups()
            .iter()
            .filter(|g| g.me.joined)
            .filter(|g| copy.as_deref() == Some(g.id.as_str()) || self.settings().shares_with_group(&inst.id, &g.id))
            .map(|g| name_or(&g.name, "your group"))
            .collect();
        (!friends.is_empty() || !groups.is_empty()).then_some(SyncedWith { groups, friends })
    }

    /// Before turning off (`remove`: removing) these files: the locked mods
    /// among them that the change reaches friends with, frozen as they are
    /// now. `None` when there are none, or nobody keeps the pack the same.
    pub fn locked_change(&self, root: &Path, files: &[String], remove: bool) -> Option<LockedChange> {
        let mut inst = self.instance(root)?.clone();
        // Already off: turning it off changes nothing for anyone.
        let picked: Vec<(String, String)> = files
            .iter()
            .filter_map(|f| inst.addon_by_file(f))
            .filter(|a| a.locked && (remove || a.enabled) && !hopper::onlyhere::is_kept(&inst, a))
            .map(|a| (addon_key(a), a.name.clone()))
            .collect();
        if picked.is_empty() {
            return None;
        }
        let with = self.synced_with(root)?;
        let keys: Vec<String> = picked.iter().map(|(k, _)| k.clone()).collect();
        hopper::onlyhere::mark_here(&mut inst, &keys);
        let kept = keys.iter().filter_map(|k| Some((k.clone(), inst.only_here.remove(k)?))).collect();
        Some(LockedChange { with, names: picked.into_iter().map(|(_, n)| n).collect(), kept })
    }

    /// "Just for me" on that toast: the mods go back to friends the way
    /// they were, and stay as they are now here. A mark made since stays.
    pub fn keep_only_here_as(&mut self, root: &Path, kept: BTreeMap<String, Kept>) -> Result<(), String> {
        let inst = self.inst_mut(root).ok_or("That instance is gone.")?;
        for (k, v) in kept {
            inst.only_here.entry(k).or_insert(v);
        }
        inst.save().map_err(|e| format!("{e:#}"))
    }

    /// Marks these files' mods "Only on this PC", as they are now (before
    /// the change the player is making). Saved at once, so the change that
    /// follows publishes them the way friends have them.
    pub fn keep_only_here(&mut self, root: &Path, files: &[String]) -> Result<(), String> {
        let inst = self.inst_mut(root).ok_or("That instance is gone.")?;
        let keys: Vec<String> = inst.addons.iter().filter(|a| files.contains(&a.file_name)).map(addon_key).collect();
        hopper::onlyhere::mark_here(inst, &keys);
        inst.save().map_err(|e| format!("{e:#}"))
    }

    /// "Sync with friends again": takes the marks off and puts each mod
    /// back the way friends have it.
    pub fn sync_again(&self, root: &Path, keys: Vec<String>, what: &str) {
        let Some(mut inst) = self.instance(root).cloned() else { return };
        let root = root.to_path_buf();
        self.spawn_instance_job(&root.clone(), format!("Syncing {what} with friends again"), move |job| {
            hopper::onlyhere::sync_again(&mut inst, &keys, job)?;
            Ok(Msg::InstanceChanged(root))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::and_list;

    #[test]
    fn names_read_as_a_sentence() {
        let n = |v: &[&str]| and_list(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(n(&["Mia"]), "Mia");
        assert_eq!(n(&["Mia", "Sam"]), "Mia and Sam");
        assert_eq!(n(&["Mia", "Sam", "Ana"]), "Mia, Sam and Ana");
        assert_eq!(n(&["Mia", "Sam", "Ana", "Bo", "Cy"]), "Mia, Sam and 3 more");
    }

    #[test]
    fn a_change_names_who_it_went_to() {
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let w = |groups: &[&str], friends: &[&str]| super::SyncedWith { groups: s(groups), friends: s(friends) }.for_whom();
        assert_eq!(w(&["drunk gamblers"], &[]), "drunk gamblers");
        assert_eq!(w(&[], &["Mia"]), "Mia");
        assert_eq!(w(&[], &["Mia", "Sam", "Ana"]), "3 friends");
        assert_eq!(w(&["drunk gamblers"], &["Mia"]), "drunk gamblers and Mia");
        assert_eq!(w(&["drunk gamblers"], &["Mia", "Sam"]), "drunk gamblers and 2 friends");
    }
}
