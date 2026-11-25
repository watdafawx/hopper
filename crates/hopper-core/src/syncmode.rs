//! Words and small helpers for two choices a player makes on this PC about
//! how friends sync works (`design/group-sync-modes.md`). Neither changes
//! what the relay keeps or sends: both decide only what this PC does with it.
//!
//! - **Take changes from** (per group): everyone, or only the person who
//!   made the group. Another member's changes then wait ([`Parked`]) until
//!   the player presses "Pull from Mia".
//! - **Share my changes** (per PC): right away, or when the player presses
//!   Push.
//!
//! What waits, and when it goes, is the sync core's (`synccore`); this file
//! only has what the chip and the buttons say about it.

/// Whether this PC takes `from`'s changes in by itself. `only`: the id of
/// the group's maker when the group is set to take changes only from them.
pub fn takes(only: Option<&str>, from: &str) -> bool {
    only.is_none_or(|o| o == from)
}

/// A member whose changes wait for the player to pull them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parked {
    /// Their mailbox id, and the name to show.
    pub id: String,
    pub name: String,
    /// About how many mods their waiting changes touch (0: can't tell).
    pub changes: usize,
}

/// What a pack held back from going out has that its last code didn't.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// How many changes (0: some, but can't tell how many).
    pub changes: usize,
    /// Anything to push at all.
    pub any: bool,
}

/// Whether a pack (`inst`) differs from the last code that went out
/// (`sent`; None when this PC has none). Runs off the UI thread: making the
/// code may read the pack's file list. A pack shared outside a group copy
/// only; a group copy's changes are counted by the sync core.
pub fn pending(inst: &crate::instances::Instance, sent: Option<&str>, job: &crate::jobs::Job) -> anyhow::Result<Pending> {
    let made = crate::sharecode::make(inst, job)?;
    Ok(match sent.filter(|s| !s.is_empty()) {
        Some(s) if made.code == s => Pending::default(),
        _ => Pending { changes: 0, any: true },
    })
}

/// "Push 3 changes", "Push 1 change", or "Push changes" with no count.
pub fn push_label(changes: usize) -> String {
    match changes {
        0 => "Push changes".to_string(),
        1 => "Push 1 change".to_string(),
        n => format!("Push {n} changes"),
    }
}

/// "3 changes", "1 change".
pub fn changes_words(n: usize) -> String {
    if n == 1 { "1 change".to_string() } else { format!("{n} changes") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_maker_is_taken_when_the_group_is_set_so() {
        assert!(takes(None, "mia"));
        assert!(takes(Some("noah"), "noah"));
        assert!(!takes(Some("noah"), "mia"));
    }

    #[test]
    fn push_words_read_plainly() {
        assert_eq!(push_label(0), "Push changes");
        assert_eq!(push_label(1), "Push 1 change");
        assert_eq!(push_label(3), "Push 3 changes");
        assert_eq!(changes_words(1), "1 change");
    }
}
