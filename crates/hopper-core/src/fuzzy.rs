//! Fuzzy matching and ranking for the command palette.
//!
//! The query's characters must appear in order in the text, case aside.
//! Matches score higher at word starts, in runs and near the front; a
//! prefix of the whole text scores highest. Nothing here allocates except
//! the caller's reusable buffers.

/// A query, lower-cased once per keystroke. Spaces are dropped: "tnp mods"
/// finds "TNP Limitless 8 › Mods".
#[derive(Default, Clone, Debug)]
pub struct Query {
    chars: Vec<char>,
}

impl Query {
    pub fn new(text: &str) -> Self {
        let mut q = Self::default();
        q.set(text);
        q
    }

    /// Replaces the query, reusing its buffer.
    pub fn set(&mut self, text: &str) {
        self.chars.clear();
        self.chars
            .extend(text.chars().filter(|c| !c.is_whitespace()).map(lower));
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }
}

fn lower(c: char) -> char {
    if c.is_ascii() {
        c.to_ascii_lowercase()
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

/// Whether `c` starts a word, after `prev`.
fn word_start(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => {
            !p.is_alphanumeric()
                || (p.is_lowercase() && c.is_uppercase())
                || (p.is_alphabetic() && c.is_ascii_digit())
        }
    }
}

/// How well `query` matches `text`, or None when it doesn't. Higher is
/// better; an empty query matches everything with 0.
pub fn score(query: &Query, text: &str) -> Option<i32> {
    run(query, text, None)
}

/// [`score`], also filling `out` with the byte offsets of the matched
/// characters (for highlighting).
pub fn positions(query: &Query, text: &str, out: &mut Vec<usize>) -> Option<i32> {
    out.clear();
    run(query, text, Some(out))
}

// Points per matched character, and what changes them.
const MATCH: i32 = 16;
const WORD_START: i32 = 12;
const RUN: i32 = 14;
/// Skipping ahead to a word start (how acronyms match).
const SKIP_TO_WORD: i32 = 3;

/// The cost of skipping `gap` characters to reach a match.
fn gap_cost(gap: usize, to_word_start: bool) -> i32 {
    if to_word_start {
        SKIP_TO_WORD
    } else {
        2 + gap.min(8) as i32
    }
}

/// The cost of a match starting `lead` characters in.
fn lead_cost(lead: usize) -> i32 {
    lead.min(12) as i32 / 3
}

fn run(query: &Query, text: &str, mut out: Option<&mut Vec<usize>>) -> Option<i32> {
    let q = &query.chars;
    if q.is_empty() {
        return Some(0);
    }
    // Cheap rejection first: most candidates don't match at all.
    let mut it = text.chars().map(lower);
    if !q.iter().all(|c| it.any(|t| t == *c)) {
        return None;
    }
    let (mut total, len) = match best(q, text, out.as_deref_mut()) {
        Some(r) => r,
        None => greedy(q, text, out),
    };
    // A prefix of the whole text is best, all of it better still; shorter
    // texts win ties.
    let prefix = text
        .chars()
        .map(lower)
        .zip(q.iter())
        .take_while(|(a, b)| a == *b)
        .count();
    if prefix == q.len() {
        total += 15;
        if len == q.len() {
            total += 20;
        }
    }
    Some(total - len as i32 / 12)
}

/// Texts and queries up to these lengths get the best alignment; longer
/// ones a greedy one.
const MAX_TEXT: usize = 96;
const MAX_QUERY: usize = 24;

/// The best-scoring way to match `q` in `text` (dynamic programming over
/// where each query character lands), with the text's length in chars.
/// None when either is too long for the fixed-size tables.
fn best(q: &[char], text: &str, out: Option<&mut Vec<usize>>) -> Option<(i32, usize)> {
    const NONE: i32 = i32::MIN / 4;
    let m = q.len();
    if m > MAX_QUERY {
        return None;
    }
    let mut ch = ['\0'; MAX_TEXT];
    let mut at = [0usize; MAX_TEXT];
    let mut ws = [false; MAX_TEXT];
    let mut n = 0;
    let mut prev = None;
    for (b, c) in text.char_indices() {
        if n == MAX_TEXT {
            return None;
        }
        ch[n] = lower(c);
        at[n] = b;
        ws[n] = word_start(prev, c);
        prev = Some(c);
        n += 1;
    }
    // score[j][i]: the best score with q[j] matched at i; from[j][i]: where
    // q[j - 1] was matched then.
    let mut score = [[NONE; MAX_TEXT]; MAX_QUERY];
    let mut from = [[0u8; MAX_TEXT]; MAX_QUERY];
    for i in 0..n {
        if ch[i] == q[0] {
            score[0][i] = MATCH + if ws[i] { WORD_START } else { 0 } - lead_cost(i);
        }
    }
    for j in 1..m {
        let (done, rest) = score.split_at_mut(j);
        let last = &done[j - 1];
        let row = &mut rest[0];
        // The best of last[p] for p <= i - 10: gaps of 9 or more cost the same.
        let (mut far, mut far_at) = (NONE, 0);
        for i in j..n {
            if i >= 10 && last[i - 10] > far {
                far = last[i - 10];
                far_at = i - 10;
            }
            if ch[i] != q[j] {
                continue;
            }
            let (mut v, mut p) = (NONE, 0);
            if last[i - 1] > NONE {
                v = last[i - 1] + RUN;
                p = i - 1;
            }
            for gap in 1..=8 {
                if i < gap + 1 {
                    break;
                }
                let k = i - 1 - gap;
                if last[k] > NONE && last[k] - gap_cost(gap, ws[i]) > v {
                    v = last[k] - gap_cost(gap, ws[i]);
                    p = k;
                }
            }
            if far > NONE && far - gap_cost(9, ws[i]) > v {
                v = far - gap_cost(9, ws[i]);
                p = far_at;
            }
            if v > NONE {
                row[i] = v + MATCH + if ws[i] { WORD_START } else { 0 };
                from[j][i] = p as u8;
            }
        }
    }
    let (mut end, mut top) = (0, NONE);
    for (i, &v) in score[m - 1][..n].iter().enumerate() {
        if v > top {
            top = v;
            end = i;
        }
    }
    if top <= NONE {
        return None;
    }
    if let Some(out) = out {
        out.clear();
        out.resize(m, 0);
        let mut i = end;
        for j in (0..m).rev() {
            out[j] = at[i];
            i = from[j][i] as usize;
        }
    }
    Some((top, n))
}

/// For long texts: the tightest window holding a match, matched greedily.
/// The caller has checked that `q` matches.
fn greedy(q: &[char], text: &str, mut out: Option<&mut Vec<usize>>) -> (i32, usize) {
    // Forward: where the earliest complete match ends.
    let mut qi = 0;
    let mut end = text.len();
    for (b, c) in text.char_indices() {
        if lower(c) == q[qi] {
            qi += 1;
            if qi == q.len() {
                end = b + c.len_utf8();
                break;
            }
        }
    }
    // Backward from there: where the window starts.
    let mut qi = q.len();
    let mut start = 0;
    for (b, c) in text[..end].char_indices().rev() {
        if lower(c) == q[qi - 1] {
            qi -= 1;
            if qi == 0 {
                start = b;
                break;
            }
        }
    }
    let mut prev = text[..start].chars().next_back();
    let mut qi = 0;
    let mut gap = 0;
    let mut total = -lead_cost(text[..start].chars().count());
    for (off, c) in text[start..end].char_indices() {
        if qi < q.len() && lower(c) == q[qi] {
            let starts = word_start(prev, c);
            total += MATCH + if starts { WORD_START } else { 0 };
            if qi > 0 {
                total += if gap == 0 { RUN } else { -gap_cost(gap, starts) };
            }
            gap = 0;
            qi += 1;
            if let Some(out) = out.as_deref_mut() {
                out.push(start + off);
            }
        } else {
            gap += 1;
        }
        prev = Some(c);
    }
    (total, text.chars().count())
}

/// Whether some word of `keywords` starts with the query.
fn word_prefix(q: &[char], keywords: &str) -> bool {
    keywords.split_whitespace().any(|w| {
        let mut w = w.chars().map(lower);
        q.iter().all(|c| w.next() == Some(*c))
    })
}

/// One ranked item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    /// Index in the order the candidates were given.
    pub item: u32,
    pub section: u8,
    pub score: i32,
}

/// Sections are `u8`s below this.
pub const MAX_SECTIONS: usize = 16;

/// A candidate: its section (below [`MAX_SECTIONS`]), title, extra words
/// it also answers to, and a bonus (e.g. for recent picks).
pub struct Candidate<'a> {
    pub section: u8,
    pub title: &'a str,
    pub keywords: &'a str,
    pub boost: i32,
}

/// Below this per query character, a hit is scattered letters (each
/// matched mid-word, with gaps), listed only when nothing scores better.
const FLOOR: i32 = 24;

/// Ranks the candidates matching `query` into `out` (cleared first):
/// grouped by section, sections ordered by their best hit, best first within
/// each, at most `cap(section)` per section. Ties keep the given order.
/// Scattered-letter hits are left out when there are better ones.
pub fn rank<'a>(
    query: &Query,
    candidates: impl IntoIterator<Item = Candidate<'a>>,
    cap: impl Fn(u8) -> usize,
    out: &mut Vec<Hit>,
) {
    out.clear();
    let mut best = [i32::MIN; MAX_SECTIONS];
    for (i, c) in candidates.into_iter().enumerate() {
        let title = score(query, c.title);
        // Keywords count when one starts with the query, a little less than
        // the title.
        let words = if word_prefix(&query.chars, c.keywords) {
            score(query, c.keywords).map(|s| s - 20)
        } else {
            None
        };
        let Some(s) = title.max(words) else { continue };
        let s = s + c.boost;
        let sec = (c.section as usize).min(MAX_SECTIONS - 1);
        best[sec] = best[sec].max(s);
        out.push(Hit {
            item: i as u32,
            section: sec as u8,
            score: s,
        });
    }
    let floor = FLOOR * query.chars.len() as i32;
    if best.iter().any(|b| *b >= floor) {
        out.retain(|h| h.score >= floor);
    }
    out.sort_unstable_by(|a, b| {
        best[b.section as usize]
            .cmp(&best[a.section as usize])
            .then(a.section.cmp(&b.section))
            .then(b.score.cmp(&a.score))
            .then(a.item.cmp(&b.item))
    });
    let mut taken = [0usize; MAX_SECTIONS];
    out.retain(|h| {
        let n = &mut taken[h.section as usize];
        *n += 1;
        *n <= cap(h.section)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(q: &str, t: &str) -> Option<i32> {
        score(&Query::new(q), t)
    }

    #[test]
    fn matches_in_order_ignoring_case() {
        assert!(s("tnp", "TNP Limitless 8").is_some());
        assert!(s("lmt8", "TNP Limitless 8").is_some());
        assert!(s("8tnp", "TNP Limitless 8").is_none());
        assert!(s("xyz", "Mekanism").is_none());
        assert_eq!(s("", "anything"), Some(0));
    }

    #[test]
    fn spaces_in_the_query_are_ignored() {
        assert!(s("tnp mods", "TNP Limitless 8 › Mods").is_some());
    }

    #[test]
    fn prefix_beats_middle() {
        assert!(s("set", "Settings") > s("set", "Reset zoom"));
    }

    #[test]
    fn exact_beats_prefix() {
        assert!(s("create", "Create") > s("create", "Create instance"));
    }

    #[test]
    fn word_starts_beat_scattered_letters() {
        // "ci" as Create Instance vs. c…i inside one word.
        assert!(s("ci", "Create instance") > s("ci", "Music"));
        assert!(s("jei", "Just Enough Items") > s("jei", "Majestic"));
    }

    #[test]
    fn shorter_texts_win_ties() {
        assert!(s("jei", "Just Enough Items (JEI)") > s("jei", "Distraction Free Recipes (EMI / REI / JEI)"));
    }

    #[test]
    fn the_best_alignment_is_found_not_the_first() {
        // "set" in "Server settings" lands on "settings".
        let mut p = Vec::new();
        let text = "Server settings";
        positions(&Query::new("set"), text, &mut p).unwrap();
        let at = text.find("set").unwrap();
        assert_eq!(p, vec![at, at + 1, at + 2]);
    }

    #[test]
    fn long_texts_still_match() {
        let long = format!("{} Mekanism", "x".repeat(200));
        let mut p = Vec::new();
        assert!(positions(&Query::new("mek"), &long, &mut p).is_some());
        assert_eq!(&long[p[0]..p[0] + 1], "M");
    }

    #[test]
    fn keywords_need_a_word_prefix() {
        let browse = || [Candidate { section: 0, title: "Browse", keywords: "curseforge modrinth", boost: 0 }];
        let mut out = Vec::new();
        rank(&Query::new("set"), browse(), |_| 10, &mut out);
        assert!(out.is_empty());
        rank(&Query::new("modr"), browse(), |_| 10, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn runs_beat_gaps() {
        assert!(s("mod", "Mods") > s("mod", "Mekanism Ore Dust"));
    }

    #[test]
    fn tightest_window_wins() {
        // "mods" should land on the last word, not on Limitless' m.
        let mut p = Vec::new();
        let text = "TNP Limitless 8 › Mods";
        positions(&Query::new("mods"), text, &mut p).unwrap();
        let at = text.find("Mods").unwrap();
        assert_eq!(p, vec![at, at + 1, at + 2, at + 3]);
    }

    #[test]
    fn positions_are_byte_offsets() {
        let mut p = Vec::new();
        let text = "Rosé Pine › Dawn";
        positions(&Query::new("épd"), text, &mut p).unwrap();
        assert_eq!(&text[p[0]..p[0] + 'é'.len_utf8()], "é");
        assert_eq!(&text[p[2]..p[2] + 1], "D");
    }

    #[test]
    fn camel_case_and_digits_are_word_starts() {
        assert!(s("jm", "JourneyMap") > s("jm", "Jumbo"));
        assert!(word_start(Some('s'), '8'));
    }

    fn c<'a>(section: u8, title: &'a str) -> Candidate<'a> {
        Candidate { section, title, keywords: "", boost: 0 }
    }

    #[test]
    fn rank_groups_sections_by_their_best_hit() {
        let items = [
            c(0, "Open folder"),         // 0: weak "mod" match? none
            c(1, "Mekanism Ore Dust"),   // 1
            c(2, "Mod"),                 // 2: exact
            c(1, "Modular Routers"),     // 3
            c(2, "TNP › Mods"),          // 4
        ];
        let mut out = Vec::new();
        rank(&Query::new("mod"), items, |_| 10, &mut out);
        let order: Vec<u32> = out.iter().map(|h| h.item).collect();
        // Section 2 holds the best hit ("Mod"), so it comes first, and
        // within section 1 the run beats the scattered match.
        assert_eq!(order, vec![2, 4, 3, 1]);
        // Sections stay together.
        let secs: Vec<u8> = out.iter().map(|h| h.section).collect();
        assert_eq!(secs, vec![2, 2, 1, 1]);
    }

    #[test]
    fn rank_drops_scattered_matches_when_there_are_good_ones() {
        let items = [c(0, "Settings"), c(1, "Hunter'sReturn"), c(1, "Detected setBlock")];
        let mut out = Vec::new();
        rank(&Query::new("set"), items, |_| 10, &mut out);
        assert_eq!(out.iter().map(|h| h.item).collect::<Vec<_>>(), vec![0, 2]);
    }

    #[test]
    fn scattered_matches_stay_when_nothing_is_better() {
        let mut out = Vec::new();
        rank(&Query::new("set"), [c(1, "Hunter'sReturn")], |_| 10, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn rank_caps_each_section() {
        let items = (0..20).map(|_| c(3, "Mod"));
        let mut out = Vec::new();
        rank(&Query::new("mod"), items, |s| if s == 3 { 5 } else { 99 }, &mut out);
        assert_eq!(out.len(), 5);
        // Ties keep the given order.
        assert_eq!(out.iter().map(|h| h.item).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn keywords_match_but_rank_below_titles() {
        let items = [
            Candidate { section: 0, title: "Toggle theme", keywords: "dark light mode", boost: 0 },
            Candidate { section: 0, title: "Dark souls", keywords: "", boost: 0 },
        ];
        let mut out = Vec::new();
        rank(&Query::new("dark"), items, |_| 10, &mut out);
        assert_eq!(out.iter().map(|h| h.item).collect::<Vec<_>>(), vec![1, 0]);
    }

    #[test]
    fn boost_lifts_recent_picks() {
        let items = [
            Candidate { section: 0, title: "Settings", keywords: "", boost: 0 },
            Candidate { section: 0, title: "Server settings", keywords: "", boost: 60 },
        ];
        let mut out = Vec::new();
        rank(&Query::new("set"), items, |_| 10, &mut out);
        assert_eq!(out[0].item, 1);
    }

    #[test]
    fn reusing_a_query_replaces_it() {
        let mut q = Query::new("abc");
        q.set("Mo");
        assert!(score(&q, "mods").is_some());
        assert!(score(&q, "abc").is_none());
        q.set("  ");
        assert!(q.is_empty());
    }
}
