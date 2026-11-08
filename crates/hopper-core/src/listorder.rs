//! The lead groups of the Mods tab's sort: "Updates first" and "Added by you
//! first". One rule here so the desktop table and the server panel (which
//! mirrors it in `Mods.svelte`) agree.

use std::cmp::Ordering;

/// Which toggles of the sort menu are on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Lead {
    pub updates_first: bool,
    pub yours_first: bool,
}

/// What a row is, as far as the lead groups care.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    /// Has an update waiting.
    pub update: bool,
    /// Marked "Added by you" (never true for a list with no pack).
    pub yours: bool,
}

impl Lead {
    /// The group a row sorts into: 0 first. Updates lead, then the player's
    /// own, then the rest; a toggle that is off adds no group.
    pub fn group(self, f: Facts) -> u8 {
        if self.updates_first && f.update {
            0
        } else if self.yours_first && f.yours {
            1
        } else {
            2
        }
    }

    /// Orders two rows by group only. Chain the chosen sort after it
    /// (`lead.cmp(a, b).then(chosen)`) so ties keep that sort.
    pub fn cmp(self, a: Facts, b: Facts) -> Ordering {
        self.group(a).cmp(&self.group(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // (name, update, yours) sorted the way the table does: lead groups,
    // then the chosen order (here: name A to Z).
    fn order(lead: Lead) -> Vec<&'static str> {
        let mut rows = [
            ("a-plain", false, false),
            ("b-yours", false, true),
            ("c-update", true, false),
            ("d-both", true, true),
            ("e-plain", false, false),
            ("f-yours", false, true),
            ("g-update", true, false),
        ];
        rows.sort_by(|x, y| {
            lead.cmp(Facts { update: x.1, yours: x.2 }, Facts { update: y.1, yours: y.2 })
                .then(x.0.cmp(y.0))
        });
        rows.iter().map(|r| r.0).collect()
    }

    #[test]
    fn neither_keeps_the_chosen_sort() {
        assert_eq!(
            order(Lead::default()),
            ["a-plain", "b-yours", "c-update", "d-both", "e-plain", "f-yours", "g-update"]
        );
    }

    #[test]
    fn updates_first_alone() {
        let l = Lead { updates_first: true, yours_first: false };
        assert_eq!(
            order(l),
            ["c-update", "d-both", "g-update", "a-plain", "b-yours", "e-plain", "f-yours"]
        );
    }

    #[test]
    fn yours_first_alone() {
        let l = Lead { updates_first: false, yours_first: true };
        assert_eq!(
            order(l),
            ["b-yours", "d-both", "f-yours", "a-plain", "c-update", "e-plain", "g-update"]
        );
    }

    #[test]
    fn both_updates_then_yours_then_rest() {
        let l = Lead { updates_first: true, yours_first: true };
        assert_eq!(
            order(l),
            ["c-update", "d-both", "g-update", "b-yours", "f-yours", "a-plain", "e-plain"]
        );
    }
}
