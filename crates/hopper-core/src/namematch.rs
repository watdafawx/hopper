//! Typo-tolerant matching of project names, for Browse search.
//!
//! CurseForge and Modrinth search by keyword, so "sodum" or "minecolonis"
//! bring back noise and "jei" depends on the site's luck. This scores how
//! close a project's name (or slug) is to what was typed, so Browse can
//! lift close names to the top of a page and offer "Did you mean X?" when
//! nothing on the page is close.
//!
//! Scores come in tiers: exact > prefix > acronym > all words > typo.
//! Zero means no match.

pub const EXACT: u32 = 100;
pub const PREFIX: u32 = 90;
pub const ACRONYM: u32 = 85;
pub const WORDS: u32 = 75;
/// The best a typo match can score; each typo costs a few points.
pub const TYPO: u32 = 60;

/// Short names players use for mods whose real names don't start with
/// them. Both sides are already normalized (see [`norm`]).
const ALIASES: &[(&str, &str)] = &[
    ("jei", "just enough items"),
    ("rei", "roughly enough items"),
    ("jer", "just enough resources"),
    ("ae", "applied energistics 2"),
    ("ae2", "applied energistics 2"),
    ("rs", "refined storage"),
    ("ftbq", "ftb quests"),
    ("ftbc", "ftb chunks"),
    ("ftbu", "ftb ultimine"),
    ("ftbb", "ftb backups"),
    ("ftbt", "ftb teams"),
    ("ftbl", "ftb library"),
    ("tcon", "tinkers construct"),
    ("ticon", "tinkers construct"),
    ("ie", "immersive engineering"),
    ("eio", "ender io"),
    ("tf", "the twilight forest"),
    ("bop", "biomes o plenty"),
    ("byg", "oh the biomes youll go"),
    ("jm", "journeymap"),
    ("cc", "cc tweaked"),
    ("cct", "cc tweaked"),
    ("mek", "mekanism"),
    ("pe", "projecte"),
    ("aa", "actually additions"),
    ("sb", "sophisticated backpacks"),
    ("ss", "sophisticated storage"),
    ("bm", "blood magic"),
    ("pnc", "pneumaticcraft repressurized"),
    ("ic2", "industrialcraft 2"),
    ("gtm", "gregtech ceu modern"),
    ("gtceu", "gregtech ceu modern"),
    ("fd", "farmers delight"),
    ("ars", "ars nouveau"),
    ("ctm", "connectedtexturesmod"),
    ("xu2", "extra utilities 2"),
];

/// Lower case, letters and digits only, words split by single spaces.
/// Apostrophes join ("Farmer's" → "farmers"); other marks split.
pub fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut gap = false;
    for c in s.chars() {
        if c == '\'' || c == '’' {
            continue;
        }
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.extend(c.to_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

/// The full name an abbreviation stands for ("jei" → "just enough items").
pub fn alias(query: &str) -> Option<&'static str> {
    let q = norm(query);
    ALIASES.iter().find(|(a, _)| *a == q).map(|(_, full)| *full)
}

/// How many typos a word of `len` letters may have and still match.
fn allowed(len: usize) -> usize {
    match len {
        0..=3 => 0,
        4..=7 => 1,
        _ => 2,
    }
}

/// Edit distance with swaps of two neighbours counting as one edit
/// (optimal string alignment).
fn distance(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

/// The fewest edits turning `q` into the start of `t` (any length of it).
fn prefix_distance(q: &[char], t: &[char]) -> usize {
    let lo = q.len().saturating_sub(2);
    let hi = (q.len() + 2).min(t.len());
    (lo..=hi).map(|k| distance(q, &t[..k.min(t.len())])).min().unwrap_or(usize::MAX)
}

/// The first letters of a name's words, numbers kept whole:
/// "applied energistics 2" → "ae2".
fn initials(n: &str) -> String {
    n.split(' ')
        .map(|w| {
            if w.chars().all(|c| c.is_ascii_digit()) {
                w.to_string()
            } else {
                w.chars().next().map(String::from).unwrap_or_default()
            }
        })
        .collect()
}

/// How close `text` is to an already-normalized query, 0 for no match.
fn score_norm(q: &str, text: &str) -> u32 {
    let t = norm(text);
    if q.is_empty() || t.is_empty() {
        return 0;
    }
    if t == q {
        return EXACT;
    }
    // "create aeronautic" → "Create Aeronautics", at a word boundary or not.
    if t.starts_with(q) {
        return PREFIX;
    }
    // Also the name without spaces: "journey map" → "JourneyMap".
    let qs: String = q.chars().filter(|c| *c != ' ').collect();
    let ts: String = t.chars().filter(|c| *c != ' ').collect();
    if ts == qs {
        return EXACT;
    }
    if ts.starts_with(&qs) && qs.len() >= 3 {
        return PREFIX;
    }
    if !q.contains(' ') && q.len() >= 2 && t.contains(' ') && initials(&t) == q {
        return ACRONYM;
    }
    let qw: Vec<&str> = q.split(' ').collect();
    let tw: Vec<&str> = t.split(' ').collect();
    if qw.iter().all(|w| tw.iter().any(|t| t.starts_with(w))) {
        return WORDS;
    }
    // Typos: the whole name, then the start of it, then word by word.
    let qc: Vec<char> = qs.chars().collect();
    let tc: Vec<char> = ts.chars().collect();
    let room = allowed(qc.len());
    if room == 0 {
        return 0;
    }
    let whole = distance(&qc, &tc);
    if whole <= room {
        return TYPO - 5 * whole as u32;
    }
    let start = prefix_distance(&qc, &tc);
    if start <= room {
        return TYPO - 10 - 5 * start as u32;
    }
    let mut total = 0;
    for w in &qw {
        let wc: Vec<char> = w.chars().collect();
        let best = tw
            .iter()
            .map(|t| {
                let tc: Vec<char> = t.chars().collect();
                distance(&wc, &tc).min(prefix_distance(&wc, &tc))
            })
            .min()
            .unwrap_or(usize::MAX);
        if best > allowed(wc.len()) {
            return 0;
        }
        total += best;
    }
    (TYPO - 15).saturating_sub(3 * total as u32)
}

/// How close a project is to `query`, by its name and slug (0: not at all).
/// An abbreviation counts as its full name too.
pub fn score(query: &str, name: &str, slug: &str) -> u32 {
    let q = norm(query);
    let mut best = score_norm(&q, name).max(score_norm(&q, slug));
    if let Some(full) = alias(&q) {
        // The name an abbreviation stands for is the mod meant, ahead of
        // others that merely start with the abbreviation ("JEI Tweaker").
        let s = score_norm(full, name).max(score_norm(full, slug));
        best = best.max(if s >= PREFIX { EXACT } else { s });
    }
    best
}

/// Moves projects whose names match `query` closely to the front, best
/// first; the rest keep the site's order. Nothing is dropped.
pub fn rerank<T>(query: &str, items: &mut Vec<T>, fields: impl Fn(&T) -> (&str, &str)) {
    if norm(query).is_empty() {
        return;
    }
    let mut scored: Vec<(u32, usize)> = items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let (name, slug) = fields(it);
            (score(query, name, slug), i)
        })
        .collect();
    // Stable: equal scores keep the site's order.
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    let order: Vec<usize> = scored.into_iter().map(|(_, i)| i).collect();
    apply_order(items, order);
}

fn apply_order<T>(items: &mut Vec<T>, order: Vec<usize>) {
    let mut old: Vec<Option<T>> = items.drain(..).map(Some).collect();
    items.extend(order.into_iter().filter_map(|i| old[i].take()));
}

/// Whether the first few results miss what was typed: none of them has
/// all its words, starts with it, or is it. Then a suggestion helps.
pub fn is_weak<'a>(query: &str, top: impl IntoIterator<Item = (&'a str, &'a str)>) -> bool {
    if norm(query).is_empty() {
        return false;
    }
    !top.into_iter().take(5).any(|(name, slug)| score(query, name, slug) >= WORDS)
}

/// A shorter search that still finds what a typo hid: each word cut to
/// its first ~60% (at least 3 letters), since typos sit mostly at the end
/// ("minecolonis" → "minecol"). None when that changes nothing.
pub fn probe(query: &str) -> Option<String> {
    let q = norm(query);
    let cut: Vec<String> = q
        .split(' ')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let n = w.chars().count();
            let keep = ((n * 3).div_ceil(5)).max(3).min(n);
            w.chars().take(keep).collect()
        })
        .collect();
    let p = cut.join(" ");
    (!p.is_empty() && p != q).then_some(p)
}

/// The known name closest to `query`, for "Did you mean X?". Candidates
/// earlier in the list win ties (pass the most popular first). Never the
/// query itself.
pub fn suggest<'a>(query: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let q = norm(query);
    if q.is_empty() {
        return None;
    }
    let mut best: Option<(u32, &str)> = None;
    for c in candidates {
        let n = norm(c);
        if n == q || n.is_empty() {
            continue;
        }
        let s = score(query, c, "");
        // A bare prefix ("sod" → "Sodium") isn't a correction.
        if s == 0 || (s == PREFIX && alias(&q).is_none()) {
            continue;
        }
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, c));
        }
    }
    best.map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(norm("Farmer's Delight"), "farmers delight");
        assert_eq!(norm("Create: Aeronautics"), "create aeronautics");
        assert_eq!(norm("  Just Enough Items (JEI) "), "just enough items jei");
        assert_eq!(norm("sophisticated-backpacks"), "sophisticated backpacks");
    }

    #[test]
    fn distance_counts_swaps_once() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(distance(&c("sodum"), &c("sodium")), 1);
        assert_eq!(distance(&c("sodimu"), &c("sodium")), 1);
        assert_eq!(distance(&c("minecolonis"), &c("minecolonies")), 1);
        assert_eq!(distance(&c(""), &c("abc")), 3);
    }

    #[test]
    fn tiers_rank_in_order() {
        assert_eq!(score("sodium", "Sodium", "sodium"), EXACT);
        assert_eq!(score("create aeronautic", "Create Aeronautics", "create-aeronautics"), PREFIX);
        assert_eq!(score("journey map", "JourneyMap", ""), EXACT);
        assert_eq!(score("ae2", "Applied Energistics 2", ""), EXACT); // via alias
        assert_eq!(score("aie", "Another Item Enhancer", ""), ACRONYM);
        assert_eq!(score("aeronautics create", "Create Aeronautics", ""), WORDS);
        let typo = score("sodum", "Sodium", "sodium");
        assert!(typo > 0 && typo < WORDS);
        assert!(score("sodum", "Sodium", "") > score("sodum", "Sodium Extra", ""));
        assert_eq!(score("sodum", "Random Utilities", "random-utilities"), 0);
    }

    #[test]
    fn short_words_need_exact_letters() {
        assert_eq!(score("jeo", "Jade", ""), 0);
        assert_eq!(score("abc", "xyz", ""), 0);
    }

    #[test]
    fn aliases_find_full_names() {
        assert_eq!(alias("JEI"), Some("just enough items"));
        assert_eq!(score("jei", "Just Enough Items (JEI)", "jei"), EXACT);
        assert!(score("jei", "JEI Integration", "jei-integration") < EXACT);
        assert!(score("ae2", "Applied Energistics 2", "") >= PREFIX);
        assert_eq!(alias("sodium"), None);
    }

    #[test]
    fn rerank_lifts_close_names_and_keeps_the_rest() {
        let mut v = vec![("Performance Pack", "x"), ("Random Utilities", "r"), ("Sodium", "sodium"), ("Iris", "iris")];
        rerank("sodum", &mut v, |t| (t.0, t.1));
        assert_eq!(v[0].0, "Sodium");
        assert_eq!(v.len(), 4);
        // Unmatched ones keep the site's order.
        let rest: Vec<_> = v.iter().map(|t| t.0).filter(|n| *n != "Sodium").collect();
        assert_eq!(rest, vec!["Performance Pack", "Random Utilities", "Iris"]);
    }

    #[test]
    fn rerank_is_stable_for_equal_scores() {
        let mut v = vec!["Create: Steam", "Create", "Create: Aeronautics", "Other"];
        rerank("create", &mut v, |t| (t, ""));
        assert_eq!(v, vec!["Create", "Create: Steam", "Create: Aeronautics", "Other"]);
    }

    #[test]
    fn apply_order_permutes() {
        let mut v = vec!['a', 'b', 'c', 'd', 'e'];
        apply_order(&mut v, vec![3, 0, 4, 1, 2]);
        assert_eq!(v, vec!['d', 'a', 'e', 'b', 'c']);
    }

    #[test]
    fn weak_when_nothing_on_top_matches() {
        assert!(!is_weak("sodum", [("Sodum Extra Pack", ""), ("Random", "")].into_iter()));
        assert!(is_weak("minecolonis", [("Colony Tweaks", ""), ("Random", "")].into_iter()));
        assert!(!is_weak("create", [("Create", "create")].into_iter()));
        assert!(is_weak("x", std::iter::empty()));
    }

    #[test]
    fn probes_with_shorter_words() {
        assert_eq!(probe("minecolonis").as_deref(), Some("minecol"));
        assert_eq!(probe("sodum").as_deref(), Some("sod"));
        assert_eq!(probe("Create Aeronautic").as_deref(), Some("crea aerona"));
        assert_eq!(probe("jei"), None);
        assert_eq!(probe(""), None);
    }

    #[test]
    fn suggests_the_closest_known_name() {
        let known = ["Just Enough Items (JEI)", "Sodium", "Sodium Extra", "MineColonies", "Create", "Create Aeronautics"];
        assert_eq!(suggest("sodum", known), Some("Sodium"));
        assert_eq!(suggest("minecolonis", known), Some("MineColonies"));
        assert_eq!(suggest("jei", known), Some("Just Enough Items (JEI)"));
        assert_eq!(suggest("creat aeronautic", known), Some("Create Aeronautics"));
        // Already right, or just the start of a name: nothing to suggest.
        assert_eq!(suggest("Sodium", known), None);
        assert_eq!(suggest("sod", known), None);
        assert_eq!(suggest("qqqqqq", known), None);
    }
}
