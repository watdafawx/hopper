//! Dates without a date crate: ISO 8601 strings in, friendly text out.

use std::time::{SystemTime, UNIX_EPOCH};

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

/// ISO 8601 UTC timestamp, e.g. "2026-10-11T09:13:00.000Z".
pub fn now_iso() -> String {
    iso_from_secs(now_secs())
}

/// [`now_iso`] for seconds since the epoch.
pub fn iso_from_secs(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Seconds since the epoch for the date part (and time, when present).
pub fn parse_iso(s: &str) -> Option<i64> {
    let y = s.get(0..4)?.parse().ok()?;
    let m = s.get(5..7)?.parse().ok()?;
    let d = s.get(8..10)?.parse().ok()?;
    let mut secs = days_from_civil(y, m, d) * 86_400;
    if let (Some(h), Some(mi)) = (
        s.get(11..13).and_then(|v| v.parse::<i64>().ok()),
        s.get(14..16).and_then(|v| v.parse::<i64>().ok()),
    ) {
        secs += h * 3600 + mi * 60;
    }
    Some(secs)
}

/// "Today", "Yesterday", "3 days ago", "2 weeks ago", "5 months ago",
/// "2 years ago". [`date`] gives the exact day (for a tooltip).
pub fn relative(s: &str) -> String {
    match parse_iso(s) {
        // CurseForge writes 0001-01-01 for "never".
        Some(t) if t < 946_684_800 => String::new(),
        Some(t) => relative_secs(t),
        None => String::new(),
    }
}

/// [`relative`] for seconds since the epoch.
pub fn relative_secs(t: i64) -> String {
    let days = now_secs().div_euclid(86_400) - t.div_euclid(86_400);
    match days {
        i64::MIN..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => format!("{days} days ago"),
        7..=13 => "1 week ago".into(),
        14..=29 => format!("{} weeks ago", days / 7),
        30..=59 => "1 month ago".into(),
        60..=364 => format!("{} months ago", days / 30),
        365..=729 => "1 year ago".into(),
        _ => format!("{} years ago", days / 365),
    }
}

/// A calendar date for seconds since the epoch, "27/09/2026".
pub fn date(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86_400));
    format!("{d:02}/{m:02}/{y}")
}

/// Seconds since the epoch, now.
pub fn now() -> i64 {
    now_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(civil_from_days(days_from_civil(2026, 9, 27)), (2026, 9, 27));
        assert_eq!(parse_iso("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(relative(&now_iso()), "Today");
        assert!(relative("2020-09-27T10:00:00Z").ends_with("years ago"));
        assert!(now_iso().ends_with('Z'));
        assert_eq!(relative("0001-01-01T00:00:00"), "");
        assert_eq!(date(days_from_civil(2026, 9, 27) * 86_400 + 5), "27/09/2026");
    }
}

/// The exact day of an ISO time, "27/09/2026" (empty when unknown).
pub fn date_of(s: &str) -> String {
    match parse_iso(s) {
        Some(t) if t >= 946_684_800 => date(t),
        _ => String::new(),
    }
}
