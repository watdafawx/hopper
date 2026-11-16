//! How much memory a game or server gets when the player hasn't picked any:
//! what the pack's author asks for, else what packs of its size need, never
//! more than the PC can spare. Both apps use it, so the same pack gets the
//! same amount in Hopper and on Hopper Server.
//!
//! Why these numbers (packs' own `recommendedRam`, Nov 2026): DeceasedCraft
//! (317 mods) asks 6 GB, All the Mods 10 (498) 8 GB, Better MC 5 (382)
//! 10.5 GB, TNP Limitless 8 (946) 12 GB. Players run the ATM packs on 10 to
//! 12 GB, so the tiers lean a little above the authors: running out of
//! memory crashes the game, a few spare GB cost nothing.

use crate::instances::Instance;

const GB: u32 = 1024;

/// Whose memory is shared: a game on the player's PC (Windows, a browser,
/// Discord beside it) or a server on its host (little else running).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Client,
    Server,
}

/// What an instance asks for and what it gets on this PC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick {
    /// What the pack's author recommends, or what packs its size need; whole GB.
    pub wanted_mb: u32,
    /// What it gets here: `wanted_mb` or, when the PC can't spare that, less.
    pub given_mb: u32,
    /// The PC's memory rounded to whole GB (0: not known).
    pub pc_mb: u32,
    /// `wanted_mb` is the pack author's recommendation (not an estimate).
    pub from_pack: bool,
    /// Mods the estimate counted (0 when the pack gave a number).
    pub mods: usize,
}

impl Pick {
    /// The PC has less to spare than the pack wants.
    pub fn capped(&self) -> bool {
        self.given_mb < self.wanted_mb
    }

    /// The line that says why it got less than it wants, or `None` when it
    /// got all of it.
    pub fn note(&self) -> Option<String> {
        if !self.capped() {
            return None;
        }
        let ask = if self.from_pack {
            format!("This pack asks for {}", gb(self.wanted_mb))
        } else {
            format!("A pack with {} mods needs about {}", self.mods, gb(self.wanted_mb))
        };
        Some(format!(
            "{ask}; your PC has {}, so Hopper gave it {}.",
            gb(self.pc_mb),
            gb(self.given_mb)
        ))
    }

    /// Where the amount comes from, for under the memory setting when it
    /// got all of it.
    pub fn source(&self) -> String {
        if self.from_pack {
            "What the pack asks for.".into()
        } else if self.mods == 0 {
            "Enough for Minecraft with a few mods.".into()
        } else {
            format!("About right for {} mods.", self.mods)
        }
    }
}

/// "12 GB".
pub fn gb(mb: u32) -> String {
    format!("{} GB", (mb + GB / 2) / GB)
}

/// `mb` to the nearest whole GB, at least 1 GB.
fn round_gb(mb: u32) -> u32 {
    ((mb + GB / 2) / GB).max(1) * GB
}

/// What a pack of `mods` mods on `loader` needs when its author doesn't
/// say. Fabric and Quilt mods are smaller (many are small libraries), so
/// those packs get one step less.
pub fn estimate_mb(mods: usize, loader: &str) -> u32 {
    let tiers = [4, 6, 8, 10, 12];
    let mut i: usize = match mods {
        0..=49 => 0,
        50..=149 => 1,
        150..=299 => 2,
        300..=599 => 3,
        _ => 4,
    };
    let l = loader.to_ascii_lowercase();
    if l.starts_with("fabric") || l.starts_with("quilt") {
        i = i.saturating_sub(1);
    }
    tiers[i] * GB
}

/// What the pack wants, whole GB, and whether its author said so: the
/// recommendation when there is a believable one (2 GB or more), else
/// [`estimate_mb`].
pub fn wanted_mb(recommended_mb: u32, mods: usize, loader: &str) -> (u32, bool) {
    if recommended_mb >= 2 * GB {
        (round_gb(recommended_mb), true)
    } else {
        (estimate_mb(mods, loader), false)
    }
}

/// The most Java may take on a PC with `pc_mb` (whole GB, rounded down),
/// leaving the rest for everything else: on a player's PC at least 4 GB and
/// 30% (Windows with a browser open uses 3 to 4 GB), on a server host at
/// least 2 GB and 20%. Never below 2 GB, which Minecraft needs to start.
/// No limit when the PC's memory isn't known.
pub fn spare_mb(pc_mb: u32, side: Side) -> u32 {
    if pc_mb == 0 {
        return u32::MAX;
    }
    let keep = match side {
        Side::Client => (4 * GB).max(pc_mb * 3 / 10),
        Side::Server => (2 * GB).max(pc_mb / 5),
    };
    (pc_mb.saturating_sub(keep) / GB * GB).max(2 * GB)
}

/// The amount for a pack: what it wants, fitted to a PC with `pc_mb`.
pub fn pick(recommended_mb: u32, mods: usize, loader: &str, pc_mb: u32, side: Side) -> Pick {
    let pc_mb = if pc_mb == 0 { 0 } else { round_gb(pc_mb) };
    let (wanted_mb, from_pack) = wanted_mb(recommended_mb, mods, loader);
    Pick {
        wanted_mb,
        given_mb: wanted_mb.min(spare_mb(pc_mb, side)),
        pc_mb,
        from_pack,
        mods: if from_pack { 0 } else { mods },
    }
}

/// The mods that load: enabled jars in the mods folder.
fn mods_of(inst: &Instance) -> usize {
    inst.addons
        .iter()
        .filter(|a| a.enabled && crate::instances::folder_for_class(a.class_id) == "mods")
        .count()
}

/// The pick for `inst` on a PC with `pc_mb` (counts its mods as they are now).
pub fn pick_for(inst: &Instance, pc_mb: u32) -> Pick {
    pick(inst.settings.pack_memory_mb, mods_of(inst), &inst.loader, pc_mb, Side::Client)
}

/// The memory `inst` starts with: its own setting, else Hopper's pick when
/// it lets Hopper pick, else `default_mb` (Settings, for instances made
/// before Hopper picked).
pub fn effective_mb(inst: &Instance, default_mb: u32) -> u32 {
    let s = &inst.settings;
    if s.memory_mb > 0 {
        s.memory_mb
    } else if s.memory_auto {
        pick_for(inst, pc_mb()).given_mb
    } else {
        default_mb
    }
}

/// The line to show on `inst`'s first start when this PC can't give it what
/// it wants, once: `None` when it has its own setting, got all it wants or
/// was told already.
pub fn first_start_note(inst: &Instance, pc_mb: u32) -> Option<String> {
    let s = &inst.settings;
    if s.memory_mb > 0 || !s.memory_auto || s.memory_told {
        return None;
    }
    pick_for(inst, pc_mb).note()
}

/// This PC's memory in MB (read once per run).
pub fn pc_mb() -> u32 {
    static PC: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *PC.get_or_init(|| (crate::hosting::local_panel::total_memory() / (1024 * 1024)).min(u32::MAX as u64) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PCS: [u32; 5] = [4, 8, 16, 32, 64];

    /// The given GB on 4/8/16/32/64 GB PCs.
    fn given(rec_gb: f32, mods: usize, loader: &str) -> Vec<u32> {
        let rec = (rec_gb * 1024.) as u32;
        PCS.iter().map(|pc| pick(rec, mods, loader, pc * GB, Side::Client).given_mb / GB).collect()
    }

    #[test]
    fn spare_keeps_windows_room() {
        let s: Vec<u32> = PCS.iter().map(|pc| spare_mb(pc * GB, Side::Client) / GB).collect();
        // 4 GB PC: only the 2 GB floor; 8: keep 4; 16: keep 4.8; 32: keep 9.6; 64: keep 19.2.
        assert_eq!(s, [2, 4, 11, 22, 44]);
        let s: Vec<u32> = PCS.iter().map(|pc| spare_mb(pc * GB, Side::Server) / GB).collect();
        assert_eq!(s, [2, 6, 12, 25, 51]);
        assert_eq!(spare_mb(12 * GB, Side::Client), 8 * GB);
        assert_eq!(spare_mb(0, Side::Client), u32::MAX);
    }

    #[test]
    fn with_recommendation() {
        // TNP Limitless 8: 12 GB.
        assert_eq!(given(12., 946, "neoforge-21.1.1"), [2, 4, 11, 12, 12]);
        // ATM10's manifest says 8196 MB: 8 GB, not 9.
        assert_eq!(given(8196. / 1024., 498, "neoforge"), [2, 4, 8, 8, 8]);
        // Better MC 5: 10720 MB rounds to 10 GB.
        assert_eq!(given(10720. / 1024., 382, "neoforge"), [2, 4, 10, 10, 10]);
        // The author wins over the estimate, also when it's less.
        assert_eq!(given(6., 317, "forge"), [2, 4, 6, 6, 6]);
        // A pack asking more than any of these PCs spares.
        assert_eq!(given(48., 900, "forge"), [2, 4, 11, 22, 44]);
        // Under 2 GB isn't believable: estimated instead.
        assert_eq!(given(1., 300, "forge"), [2, 4, 10, 10, 10]);
        assert!(pick(12 * GB, 0, "", 0, Side::Client).from_pack);
    }

    #[test]
    fn without_recommendation() {
        assert_eq!(given(0., 0, ""), [2, 4, 4, 4, 4]);
        assert_eq!(given(0., 49, "forge"), [2, 4, 4, 4, 4]);
        assert_eq!(given(0., 100, "neoforge"), [2, 4, 6, 6, 6]);
        assert_eq!(given(0., 250, "neoforge"), [2, 4, 8, 8, 8]);
        assert_eq!(given(0., 400, "forge"), [2, 4, 10, 10, 10]);
        assert_eq!(given(0., 700, "neoforge"), [2, 4, 11, 12, 12]);
        // Fabric and Quilt: one step less.
        assert_eq!(given(0., 100, "fabric-0.16.5"), [2, 4, 4, 4, 4]);
        assert_eq!(given(0., 250, "quilt"), [2, 4, 6, 6, 6]);
        assert_eq!(given(0., 700, "Fabric"), [2, 4, 10, 10, 10]);
    }

    #[test]
    fn tier_edges() {
        let e = |n| estimate_mb(n, "forge") / GB;
        assert_eq!([e(49), e(50), e(149), e(150), e(299), e(300), e(599), e(600)], [4, 6, 6, 8, 8, 10, 10, 12]);
    }

    #[test]
    fn odd_pc_sizes_round_first() {
        // Windows reports a little under the stick size (15.8 GB for 16).
        let p = pick(12 * GB, 0, "", 16180, Side::Client);
        assert_eq!((p.pc_mb, p.given_mb), (16 * GB, 11 * GB));
        // Unknown PC: no limit.
        assert_eq!(pick(12 * GB, 0, "", 0, Side::Client).given_mb, 12 * GB);
    }

    #[test]
    fn notes() {
        let p = pick(12 * GB, 0, "", 16 * GB, Side::Client);
        assert_eq!(p.note().as_deref(), Some("This pack asks for 12 GB; your PC has 16 GB, so Hopper gave it 11 GB."));
        let p = pick(0, 480, "neoforge", 8 * GB, Side::Client);
        assert_eq!(p.note().as_deref(), Some("A pack with 480 mods needs about 10 GB; your PC has 8 GB, so Hopper gave it 4 GB."));
        // Got all of it: nothing to say.
        let p = pick(8 * GB, 0, "", 32 * GB, Side::Client);
        assert_eq!(p.note(), None);
        assert_eq!(p.source(), "What the pack asks for.");
        assert_eq!(pick(0, 120, "forge", 32 * GB, Side::Client).source(), "About right for 120 mods.");
    }

    fn inst(memory_mb: u32, auto: bool, told: bool) -> Instance {
        let mut i = Instance { loader: "neoforge".into(), ..Default::default() };
        i.settings.memory_mb = memory_mb;
        i.settings.memory_auto = auto;
        i.settings.memory_told = told;
        i.settings.pack_memory_mb = 12 * GB;
        i
    }

    #[test]
    fn own_and_old_instances_keep_theirs() {
        // Set by hand: always that.
        assert_eq!(effective_mb(&inst(6 * GB, true, false), 8 * GB), 6 * GB);
        assert_eq!(effective_mb(&inst(6 * GB, false, false), 8 * GB), 6 * GB);
        // Made before Hopper picked: the Settings default, as before.
        assert_eq!(effective_mb(&inst(0, false, false), 8 * GB), 8 * GB);
        // Hopper picks: the pack's 12 GB, fitted to this PC.
        assert_eq!(effective_mb(&inst(0, true, false), 8 * GB), pick_for(&inst(0, true, false), pc_mb()).given_mb);
    }

    #[test]
    fn first_start_note_once() {
        let pc = 16 * GB;
        assert!(first_start_note(&inst(0, true, false), pc).is_some());
        assert_eq!(first_start_note(&inst(0, true, true), pc), None);
        assert_eq!(first_start_note(&inst(8 * GB, true, false), pc), None);
        assert_eq!(first_start_note(&inst(0, false, false), pc), None);
        // A PC with room: nothing to tell.
        assert_eq!(first_start_note(&inst(0, true, false), 64 * GB), None);
    }
}
