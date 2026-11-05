//! Groups a server's (or game's) crash reports by the failed start they come
//! from. One failed start can leave several reports: the one that says what
//! failed, and follow-ups whose error only came while the loader was undoing
//! the registries after it ("knock-on" reports). A follow-up says nothing
//! the explaining report doesn't, so lists fold it under that report, and
//! nothing ever picks it as the report to show while an explaining one
//! exists for the same start.

/// Reports written this close together (seconds) belong to one start.
pub const WINDOW_SECS: i64 = 15 * 60;

/// The start of the one-line verdict of a follow-up report (the advice text
/// for an error that came while the registries were being undone).
const FOLLOW_UP_VERDICT: &str = "This error came while the loader was undoing";

/// Whether a report's one-line verdict is the follow-up one.
pub fn is_follow_up_verdict(v: &str) -> bool {
    v.starts_with(FOLLOW_UP_VERDICT)
}

/// Whether a report's own text says it is a follow-up: its error came while
/// the loader was undoing the registries. (A verdict can't tell while the
/// current log explains every report beside it.)
pub fn is_follow_up_text(text: &str) -> bool {
    text.contains(crate::crashdetect::UNDO_FRAME)
}

/// One report in a list, newest first.
pub struct Item<'a> {
    /// Its text says it is a follow-up ([`is_follow_up_text`]).
    pub follow_up: bool,
    /// Modified time, unix seconds.
    pub modified: i64,
    /// Its verdict, when it has one (the newest few reports do).
    pub verdict: Option<&'a str>,
}

/// What a report is in the list.
#[derive(Debug, PartialEq, Eq)]
pub enum Role {
    /// Shown as it is.
    Plain,
    /// Explains its start; these (indexes into the list) followed from it.
    Lead(Vec<usize>),
    /// A follow-up of the report at this index (folded under it), or of one
    /// we can't find (stays in the list, with a plain title).
    FollowUp(Option<usize>),
}

/// The role of each report. A follow-up joins the nearest explaining report
/// within [`WINDOW_SECS`] (the nearer in time, the newer on a tie).
pub fn group(items: &[Item]) -> Vec<Role> {
    let follow = |i: usize| items[i].follow_up || items[i].verdict.is_some_and(is_follow_up_verdict);
    let lead = |i: usize| !follow(i) && items[i].verdict.is_some_and(|v| !v.is_empty());
    let mut roles: Vec<Role> = items.iter().map(|_| Role::Plain).collect();
    for i in (0..items.len()).filter(|&i| follow(i)) {
        let best = (0..items.len())
            .filter(|&j| lead(j))
            .map(|j| ((items[j].modified - items[i].modified).abs(), j))
            .filter(|(d, _)| *d <= WINDOW_SECS)
            .min();
        roles[i] = Role::FollowUp(best.map(|(_, j)| j));
        if let Some((_, j)) = best {
            match &mut roles[j] {
                Role::Lead(v) => v.push(i),
                r => *r = Role::Lead(vec![i]),
            }
        }
    }
    roles
}

/// The list title of a follow-up report: "Follow-up error (after Mystical
/// Agriculture Plus)" given the explaining report's verdict.
pub fn follow_up_title(lead_verdict: Option<&str>) -> String {
    let Some(v) = lead_verdict.map(str::trim).filter(|v| !v.is_empty()) else {
        return "Follow-up error".into();
    };
    let v = v.strip_prefix("likely ").unwrap_or(v);
    let cause = [" doesn't match ", " failed to load", " is client-only", " stopped "]
        .iter()
        .filter_map(|m| v.find(m))
        .min()
        .map_or(v, |i| &v[..i]);
    format!("Follow-up error (after {})", cause.trim())
}

/// The roles of report files (newest first), read from the files
/// themselves: a report is a follow-up when its error came while the loader
/// undid the registries. For lists that have no verdicts (a game's).
pub fn group_files(reports: &[std::path::PathBuf]) -> Vec<Role> {
    let read: Vec<(i64, bool)> = reports
        .iter()
        .map(|p| {
            let t = std::fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            let follow = p.extension().is_some_and(|e| e == "txt") && is_follow_up_text(&crate::logs::tail(p, 200_000));
            (t.map_or(0, |d| d.as_secs() as i64), follow)
        })
        .collect();
    let items: Vec<Item> =
        read.iter().map(|&(modified, follow)| Item { modified, follow_up: follow, verdict: Some(if follow { FOLLOW_UP_VERDICT } else { "report" }) }).collect();
    group(&items)
}

/// "+4 follow-up reports" for the fold under an explaining report.
pub fn fold_label(n: usize) -> String {
    if n == 1 { "+1 follow-up report".into() } else { format!("+{n} follow-up reports") }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOLLOW: &str = "This error came while the loader was undoing the server's registries after an earlier error, so it isn't the cause.";
    const LEAD: &str = "Mystical Agriculture Plus doesn't match LogisticsNetworks";

    fn items(v: &[(i64, Option<&'static str>)]) -> Vec<Item<'static>> {
        v.iter().map(|&(modified, verdict)| Item { modified, follow_up: false, verdict }).collect()
    }

    #[test]
    fn follow_ups_fold_under_the_report_that_explains_them() {
        // The list as the user saw it: the explaining report, then four
        // knock-ons written minutes apart.
        let list = items(&[(10_000, Some(LEAD)), (9_900, Some(FOLLOW)), (9_800, Some(FOLLOW)), (9_700, Some(FOLLOW)), (9_600, Some(FOLLOW))]);
        let roles = group(&list);
        assert_eq!(roles[0], Role::Lead(vec![1, 2, 3, 4]));
        assert_eq!(roles[3], Role::FollowUp(Some(0)));
    }

    #[test]
    fn a_follow_up_before_its_explaining_report_still_folds() {
        let list = items(&[(10_000, Some(FOLLOW)), (9_990, Some(LEAD))]);
        let roles = group(&list);
        assert_eq!(roles[0], Role::FollowUp(Some(1)));
        assert_eq!(roles[1], Role::Lead(vec![0]));
    }

    #[test]
    fn a_follow_up_far_from_any_explaining_report_stays_where_it_is() {
        let list = items(&[(100_000, Some(LEAD)), (50_000, Some(FOLLOW))]);
        let roles = group(&list);
        assert_eq!(roles, [Role::Plain, Role::FollowUp(None)]);
    }

    #[test]
    fn the_reports_own_text_decides_when_the_log_explains_every_report() {
        // Read with the current log, every report's verdict names the mod;
        // only the text tells the follow-ups apart.
        let mut list = items(&[(10_000, Some(LEAD)), (9_990, Some(LEAD)), (9_980, Some(LEAD))]);
        list[0].follow_up = true;
        list[1].follow_up = true;
        let roles = group(&list);
        assert_eq!(roles[2], Role::Lead(vec![0, 1]));
        assert_eq!(roles[0], Role::FollowUp(Some(2)));
        assert!(is_follow_up_text("at net.neoforged.neoforge.registries.RegistryManager.revertToVanilla(RegistryManager.java:1)"));
        assert!(!is_follow_up_text("java.lang.NoSuchMethodError"));
    }

    #[test]
    fn reports_without_a_verdict_stay_plain() {
        let list = items(&[(10_000, None), (9_999, Some(FOLLOW))]);
        assert_eq!(group(&list), [Role::Plain, Role::FollowUp(None)]);
    }

    #[test]
    fn titles() {
        assert_eq!(follow_up_title(Some(LEAD)), "Follow-up error (after Mystical Agriculture Plus)");
        assert_eq!(follow_up_title(Some("A, B and C failed to load")), "Follow-up error (after A, B and C)");
        assert_eq!(follow_up_title(Some("likely Create")), "Follow-up error (after Create)");
        assert_eq!(follow_up_title(None), "Follow-up error");
        assert_eq!(fold_label(4), "+4 follow-up reports");
        assert_eq!(fold_label(1), "+1 follow-up report");
    }
}
