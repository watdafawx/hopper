//! Groups a server's (or game's) crash reports by the failed start they come
//! from. One failed start can leave several reports: the one that says what
//! failed, and follow-ups whose error only came while the loader was undoing
//! the registries after it ("knock-on" reports). A follow-up says nothing
//! the explaining report doesn't, so lists fold it under that report, and
//! nothing ever picks it as the report to show while an explaining one
//! exists for the same start. The same failure over and over (a server
//! restarted into the same broken mod) folds the same way: one row, with
//! how many times.

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
    /// Explains its start; these (indexes into the list) fold under it:
    /// its follow-ups, and earlier times the same failure came (with
    /// theirs).
    Lead(Vec<usize>),
    /// A follow-up of the report at this index (folded under it), or of one
    /// we can't find (stays in the list, with a plain title).
    FollowUp(Option<usize>),
    /// An earlier time of the same failure as the (newer) report at this
    /// index, folded under it.
    Repeat(usize),
}

/// The role of each report. A follow-up joins the nearest explaining report
/// within [`WINDOW_SECS`] (the nearer in time, the newer on a tie). Then
/// reports next to each other in the list with the same verdict are the
/// same failure again: the older ones fold under the newest, with their
/// follow-ups.
pub fn group(items: &[Item]) -> Vec<Role> {
    let mut roles = group_follow_ups(items);
    let head = |r: &Role| matches!(r, Role::Plain | Role::Lead(_));
    let same = |a: usize, b: usize| {
        let v = |i: usize| items[i].verdict.map(str::trim).filter(|v| !v.is_empty() && !is_follow_up_verdict(v));
        v(a).is_some() && v(a) == v(b)
    };
    let mut newest: Option<usize> = None;
    for i in 0..items.len() {
        if !head(&roles[i]) {
            continue;
        }
        match newest {
            Some(h) if same(h, i) => {
                let mut moved = vec![i];
                if let Role::Lead(f) = std::mem::replace(&mut roles[i], Role::Repeat(h)) {
                    for &j in &f {
                        roles[j] = Role::FollowUp(Some(h));
                    }
                    moved.extend(f);
                }
                match &mut roles[h] {
                    Role::Lead(v) => v.extend(moved),
                    r => *r = Role::Lead(moved),
                }
            }
            _ => newest = Some(i),
        }
    }
    roles
}

/// How many times the failure of the report at `i` came: itself and the
/// earlier times folded under it.
pub fn times(roles: &[Role], i: usize) -> usize {
    match &roles[i] {
        Role::Lead(f) => 1 + f.iter().filter(|&&j| matches!(roles.get(j), Some(Role::Repeat(_)))).count(),
        _ => 1,
    }
}

/// The fold under a report: "3 earlier times · 4 follow-up reports".
pub fn fold_text(roles: &[Role], i: usize) -> Option<String> {
    let Role::Lead(f) = &roles[i] else { return None };
    let repeats = times(roles, i) - 1;
    let follow = f.len() - repeats;
    let mut parts = Vec::new();
    if repeats > 0 {
        parts.push(if repeats == 1 { "1 earlier time".to_string() } else { format!("{repeats} earlier times") });
    }
    if follow > 0 {
        parts.push(if follow == 1 { "1 follow-up report".to_string() } else { format!("{follow} follow-up reports") });
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// [`group`] without folding repeats.
fn group_follow_ups(items: &[Item]) -> Vec<Role> {
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
    // A start can leave only follow-up reports: the real error was in the
    // log, not in a report of its own. The oldest of them whose verdict
    // names a cause (read with that log) explains the start, and the rest
    // fold under it, so the list never leads with "Follow-up error".
    let real = |i: usize| items[i].verdict.is_some_and(|v| !v.trim().is_empty() && !is_follow_up_verdict(v));
    let orphans: Vec<usize> = (0..items.len()).filter(|&i| roles[i] == Role::FollowUp(None)).collect();
    for &h in orphans.iter().rev().filter(|&&i| real(i)) {
        if roles[h] != Role::FollowUp(None) {
            continue;
        }
        let folded: Vec<usize> = orphans
            .iter()
            .copied()
            .filter(|&j| j != h && roles[j] == Role::FollowUp(None) && (items[j].modified - items[h].modified).abs() <= WINDOW_SECS)
            .collect();
        for &j in &folded {
            roles[j] = Role::FollowUp(Some(h));
        }
        roles[h] = if folded.is_empty() { Role::Plain } else { Role::Lead(folded) };
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
    fn a_start_with_only_follow_up_reports_leads_with_the_one_that_names_the_cause() {
        // The screenshot case: the newest report's text says it is a
        // follow-up, but no report of that start explains it; read with the
        // log, its verdict names the mod. Older reports from days before.
        let mut list = items(&[(500_000, Some(LEAD)), (499_990, Some(FOLLOW)), (10_000, Some("likely Myths & Legends"))]);
        list[0].follow_up = true;
        list[1].follow_up = true;
        let roles = group(&list);
        assert_eq!(roles[0], Role::Lead(vec![1]));
        assert_eq!(roles[1], Role::FollowUp(Some(0)));
        assert_eq!(roles[2], Role::Plain);
        // Alone, it is just a report.
        let mut one = items(&[(500_000, Some(LEAD))]);
        one[0].follow_up = true;
        assert_eq!(group(&one), [Role::Plain]);
    }

    #[test]
    fn the_same_failure_again_folds_under_the_newest_time() {
        // The TNP-LL8 morning: four starts an hour apart, each leaving only a
        // follow-up report; read with each start's own log, each names the
        // same mod. Then an older, different crash.
        let mut list = items(&[(14_000, Some(LEAD)), (10_000, Some(LEAD)), (6_000, Some(LEAD)), (2_000, Some(LEAD)), (1_000, Some("likely Create"))]);
        for i in 0..4 {
            list[i].follow_up = true;
        }
        let roles = group(&list);
        assert_eq!(roles[0], Role::Lead(vec![1, 2, 3]));
        assert_eq!(roles[2], Role::Repeat(0));
        assert_eq!(roles[4], Role::Plain);
        assert_eq!(times(&roles, 0), 4);
        assert_eq!(fold_text(&roles, 0).unwrap(), "3 earlier times");
        // A repeat takes its own follow-ups along.
        let list = items(&[(10_000, Some(LEAD)), (9_990, Some(FOLLOW)), (5_000, Some(LEAD)), (4_990, Some(FOLLOW))]);
        let roles = group(&list);
        assert_eq!(roles[0], Role::Lead(vec![1, 2, 3]));
        assert_eq!(roles[3], Role::FollowUp(Some(0)));
        assert_eq!(fold_text(&roles, 0).unwrap(), "1 earlier time · 2 follow-up reports");
        // Another crash between them: not the same run of failures.
        let list = items(&[(3, Some(LEAD)), (2, Some("likely Create")), (1, Some(LEAD))]);
        assert_eq!(group(&list), [Role::Plain, Role::Plain, Role::Plain]);
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
    }
}
