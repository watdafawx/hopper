//! The server list message as the editor shows it: one line as styled
//! pieces of text (colour, bold, italic, underline, strikethrough,
//! scrambled) with `{variables}` kept whole, instead of raw § codes. The
//! style buttons toggle over a selection, typing keeps the style around the
//! caret, and saving writes the fewest § codes Minecraft needs. Hopper
//! Server's web editor has the same rules in `ui/src/lib/motd.js`.
//!
//! Positions are byte offsets into [`Line::text`], the text the editor box
//! shows (codes left out, variables as typed: `{day}`).

use super::motd::COLORS;
use std::ops::Range;

/// How a piece of text looks. `color: None` is the list's plain gray.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub color: Option<char>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub obfuscated: bool,
}

/// One thing a style button changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attr {
    Color(char),
    Bold,
    Italic,
    Underline,
    Strike,
    Obfuscated,
}

impl Attr {
    /// The attribute for a § code (`'r'` has none: see [`Line::clear`]).
    pub fn from_code(c: char) -> Option<Attr> {
        let c = c.to_ascii_lowercase();
        Some(match c {
            'l' => Attr::Bold,
            'o' => Attr::Italic,
            'n' => Attr::Underline,
            'm' => Attr::Strike,
            'k' => Attr::Obfuscated,
            _ if COLORS.iter().any(|x| x.0 == c) => Attr::Color(c),
            _ => return None,
        })
    }
}

impl Style {
    pub fn has(&self, a: Attr) -> bool {
        match a {
            Attr::Color(c) => self.color == Some(c),
            Attr::Bold => self.bold,
            Attr::Italic => self.italic,
            Attr::Underline => self.underline,
            Attr::Strike => self.strike,
            Attr::Obfuscated => self.obfuscated,
        }
    }

    /// Turns `a` on or off. A colour turned off goes back to plain gray.
    pub fn set(&mut self, a: Attr, on: bool) {
        match a {
            Attr::Color(c) => self.color = on.then_some(c),
            Attr::Bold => self.bold = on,
            Attr::Italic => self.italic = on,
            Attr::Underline => self.underline = on,
            Attr::Strike => self.strike = on,
            Attr::Obfuscated => self.obfuscated = on,
        }
    }

    /// This style with button `code` pressed (for typing with nothing
    /// selected): a style flips, a colour goes on (off if it was on), §r
    /// clears everything.
    pub fn toggled(mut self, code: char) -> Style {
        match Attr::from_code(code) {
            Some(a) => {
                let on = !self.has(a);
                self.set(a, on);
                self
            }
            None => Style::default(),
        }
    }

    /// The colour as RGB (plain gray without one).
    pub fn rgb(&self) -> u32 {
        self.color.and_then(|c| COLORS.iter().find(|x| x.0 == c)).map(|x| x.2).unwrap_or(super::motd::DEFAULT_COLOR)
    }

    /// The style codes, in a fixed order.
    fn format_codes(&self) -> impl Iterator<Item = char> {
        [(self.bold, 'l'), (self.italic, 'o'), (self.underline, 'n'), (self.strike, 'm'), (self.obfuscated, 'k')]
            .into_iter()
            .filter_map(|(on, c)| on.then_some(c))
    }

    /// Every style `self` has, `other` has too.
    fn formats_within(&self, other: &Style) -> bool {
        (!self.bold || other.bold)
            && (!self.italic || other.italic)
            && (!self.underline || other.underline)
            && (!self.strike || other.strike)
            && (!self.obfuscated || other.obfuscated)
    }
}

/// A stretch of text in one style. A variable (`{day}`) is a span of its own
/// and is edited as a whole.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub style: Style,
    pub var: bool,
}

impl Span {
    /// The variable's name, for a variable span.
    pub fn var_name(&self) -> Option<&str> {
        self.var.then(|| &self.text[1..self.text.len() - 1])
    }
}

/// One message line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub spans: Vec<Span>,
}

/// A character, or a whole variable, with its style: what editing works on.
#[derive(Clone)]
struct Unit {
    text: String,
    style: Style,
    var: bool,
}

impl Line {
    /// Reads a line with § codes (convert & shortcuts first with
    /// [`super::motd::amp_to_section`]). A colour clears the styles before
    /// it and §r clears everything, as in Minecraft.
    pub fn parse(raw: &str) -> Line {
        Line::from_units(parse_units(raw, Style::default()))
    }

    /// The text the box shows: no codes, variables as typed.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// The spans with where each sits in [`Line::text`].
    pub fn ranges(&self) -> impl Iterator<Item = (Range<usize>, &Span)> {
        let mut at = 0;
        self.spans.iter().map(move |s| {
            let r = at..at + s.text.len();
            at = r.end;
            (r, s)
        })
    }

    /// The line with the fewest § codes that draw it. A colour resets the
    /// styles in Minecraft, so styles still wanted are given again after
    /// one; dropping a style means a colour (or §r for plain gray) first.
    pub fn to_codes(&self) -> String {
        let mut out = String::new();
        let mut cur = Style::default();
        for s in &self.spans {
            if s.text.is_empty() {
                continue;
            }
            let st = s.style;
            if st != cur {
                if st.color == cur.color && cur.formats_within(&st) {
                    for c in st.format_codes().filter(|&c| !cur.format_codes().any(|x| x == c)) {
                        out.push('§');
                        out.push(c);
                    }
                } else {
                    out.push('§');
                    out.push(st.color.unwrap_or('r'));
                    for c in st.format_codes() {
                        out.push('§');
                        out.push(c);
                    }
                }
                cur = st;
            }
            out.push_str(&s.text);
        }
        out
    }

    /// The style typing at `pos` gets: that of the character before it
    /// (after it at the very start).
    pub fn style_at(&self, pos: usize) -> Style {
        let mut after = None;
        for (r, s) in self.ranges() {
            if r.start < pos && pos <= r.end {
                return s.style;
            }
            if r.start == pos && after.is_none() {
                after = Some(s.style);
            }
        }
        after.unwrap_or_default()
    }

    /// The styles every character in `a..b` shares: `Some(colour)` only
    /// when all have that colour, and each style only when all have it.
    /// With nothing selected, the style at the caret.
    pub fn common_style(&self, a: usize, b: usize) -> Style {
        if a >= b {
            return self.style_at(a);
        }
        let mut out: Option<Style> = None;
        for u in self.units_in(a, b) {
            out = Some(match out {
                None => u.style,
                Some(o) => Style {
                    color: if o.color == u.style.color { o.color } else { None },
                    bold: o.bold && u.style.bold,
                    italic: o.italic && u.style.italic,
                    underline: o.underline && u.style.underline,
                    strike: o.strike && u.style.strike,
                    obfuscated: o.obfuscated && u.style.obfuscated,
                },
            });
        }
        out.unwrap_or_else(|| self.style_at(a))
    }

    /// The style button `a` pressed over `a..b`: off when every character
    /// already has it, else on for all of them. A colour clicked again goes
    /// back to the colour around it (the text before, else after, else plain
    /// gray). A variable partly selected counts whole.
    pub fn toggle(&mut self, from: usize, to: usize, a: Attr) {
        if from >= to {
            return;
        }
        let on = !self.common_style(from, to).has(a);
        if let (Attr::Color(c), false) = (a, on) {
            let back = self.color_around(from, to, c);
            self.restyle(from, to, |s| s.color = back);
            return;
        }
        self.restyle(from, to, |s| s.set(a, on));
    }

    /// The colour next to `from..to` that isn't `not`: the text before it,
    /// else after it.
    fn color_around(&self, from: usize, to: usize, not: char) -> Option<char> {
        let mut at = 0;
        let placed: Vec<(Range<usize>, Style)> = self
            .units()
            .into_iter()
            .map(|u| {
                let r = at..at + u.text.len();
                at = r.end;
                (r, u.style)
            })
            .collect();
        let first = placed.iter().position(|(r, _)| r.end > from)?;
        let last = placed.iter().rposition(|(r, _)| r.start < to)?;
        let ok = |i: usize| placed.get(i).map(|p| p.1.color).filter(|c| *c != Some(not));
        first.checked_sub(1).and_then(ok).or_else(|| ok(last + 1)).flatten()
    }

    /// Plain gray, no styles, over `a..b` (the R button).
    pub fn clear(&mut self, a: usize, b: usize) {
        self.restyle(a, b, |s| *s = Style::default());
    }

    fn restyle(&mut self, a: usize, b: usize, f: impl Fn(&mut Style)) {
        let mut units = self.units();
        let mut at = 0;
        for u in &mut units {
            let end = at + u.text.len();
            if at < b && end > a {
                f(&mut u.style);
            }
            at = end;
        }
        *self = Line::from_units(units);
    }

    /// Brings the line in step with what the box now shows (`new`), after
    /// the player typed, deleted or pasted. New text takes `typing` (a style
    /// picked with nothing selected) or the style where it went in; pasted §
    /// codes are read as codes; a variable cut into is deleted whole.
    /// Returns where the caret belongs. When [`Line::text`] then differs
    /// from `new`, put it back in the box.
    pub fn edit(&mut self, new: &str, typing: Option<Style>) -> usize {
        let old = self.text();
        if old == new {
            return new.len();
        }
        let mut p = old.bytes().zip(new.bytes()).take_while(|(x, y)| x == y).count();
        while !old.is_char_boundary(p) || !new.is_char_boundary(p) {
            p -= 1;
        }
        let max = old.len().min(new.len()) - p;
        let mut s = old.bytes().rev().zip(new.bytes().rev()).take_while(|(x, y)| x == y).count().min(max);
        while !old.is_char_boundary(old.len() - s) || !new.is_char_boundary(new.len() - s) {
            s -= 1;
        }
        let (gone, put) = (p..old.len() - s, &new[p..new.len() - s]);
        let style = typing.unwrap_or_else(|| self.style_at(p));
        let put = put.replace(['\r', '\n'], " ");
        let added = if put.contains('§') {
            parse_units(&put, style)
        } else {
            put.chars().map(|c| Unit { text: c.to_string(), style, var: false }).collect()
        };
        let n: usize = added.iter().map(|u| u.text.len()).sum();
        // Typing inside a variable turns it into plain letters; cutting into
        // one takes it whole.
        let mut units = Vec::new();
        let mut at = 0;
        for u in self.units() {
            let r = at..at + u.text.len();
            at = r.end;
            if u.var && gone.is_empty() && r.start < p && p < r.end {
                units.extend(u.text.chars().map(|c| Unit { text: c.to_string(), style: u.style, var: false }));
            } else {
                units.push(u);
            }
        }
        let (mut from, mut to) = (gone.start, gone.end);
        let mut at = 0;
        for u in &units {
            let r = at..at + u.text.len();
            at = r.end;
            if !gone.is_empty() && r.start < gone.end && r.end > gone.start {
                from = from.min(r.start);
                to = to.max(r.end);
            }
        }
        let mut out: Vec<Unit> = Vec::new();
        let mut tail = Vec::new();
        let mut at = 0;
        for u in units {
            let r = at..at + u.text.len();
            at = r.end;
            if r.end <= from {
                out.push(u);
            } else if r.start >= to {
                tail.push(u);
            }
        }
        let caret = out_len(&out) + n;
        out.extend(added);
        out.extend(tail);
        *self = Line::from_units(out);
        caret
    }

    /// The variable around or touching `pos` (for its hover card).
    pub fn var_at(&self, pos: usize) -> Option<(Range<usize>, &str)> {
        self.ranges().find(|(r, s)| s.var && r.start <= pos && pos < r.end).and_then(|(r, s)| Some((r, s.var_name()?)))
    }

    fn units(&self) -> Vec<Unit> {
        let mut out = Vec::new();
        for s in &self.spans {
            if s.var {
                out.push(Unit { text: s.text.clone(), style: s.style, var: true });
            } else {
                out.extend(s.text.chars().map(|c| Unit { text: c.to_string(), style: s.style, var: false }));
            }
        }
        out
    }

    /// The characters and variables touching `a..b`.
    fn units_in(&self, a: usize, b: usize) -> impl Iterator<Item = Unit> {
        let mut at = 0;
        self.units().into_iter().filter(move |u| {
            let r = at..at + u.text.len();
            at = r.end;
            r.start < b && r.end > a
        })
    }

    /// Joins characters of one style and finds the variables in them.
    fn from_units(units: Vec<Unit>) -> Line {
        let mut joined: Vec<Span> = Vec::new();
        for u in units {
            match joined.last_mut() {
                Some(l) if !l.var && !u.var && l.style == u.style => l.text.push_str(&u.text),
                _ => joined.push(Span { text: u.text, style: u.style, var: u.var }),
            }
        }
        let mut spans = Vec::new();
        for s in joined {
            if s.var {
                spans.push(s);
            } else {
                split_vars(&s.text, s.style, &mut spans);
            }
        }
        Line { spans }
    }
}

fn out_len(out: &[Unit]) -> usize {
    out.iter().map(|u| u.text.len()).sum()
}

/// § codes read into characters, starting in `start`.
fn parse_units(raw: &str, start: Style) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut st = start;
    let mut it = raw.chars().peekable();
    while let Some(c) = it.next() {
        if c == '§'
            && let Some(&n) = it.peek()
        {
            it.next();
            let n = n.to_ascii_lowercase();
            match Attr::from_code(n) {
                Some(Attr::Color(col)) => st = Style { color: Some(col), ..Default::default() },
                Some(a) => st.set(a, true),
                None if n == 'r' => st = Style::default(),
                None => {}
            }
            continue;
        }
        out.push(Unit { text: c.to_string(), style: st, var: false });
    }
    out
}

/// Splits plain text into text and `{name}` variables, the way
/// [`super::motd::expand`] reads them (`{{` and `}}` are literal braces).
fn split_vars(text: &str, style: Style, out: &mut Vec<Span>) {
    let b = text.as_bytes();
    let mut plain = String::new();
    let mut i = 0;
    while i < b.len() {
        if (b[i] == b'{' && b.get(i + 1) == Some(&b'{')) || (b[i] == b'}' && b.get(i + 1) == Some(&b'}')) {
            plain.push_str(&text[i..i + 2]);
            i += 2;
            continue;
        }
        if b[i] == b'{' {
            let n = b[i + 1..].iter().take_while(|c| c.is_ascii_lowercase() || **c == b'_').count();
            if n > 0 && b.get(i + 1 + n) == Some(&b'}') {
                if !plain.is_empty() {
                    out.push(Span { text: std::mem::take(&mut plain), style, var: false });
                }
                out.push(Span { text: text[i..i + n + 2].to_string(), style, var: true });
                i += n + 2;
                continue;
            }
        }
        let ch = text[i..].chars().next().unwrap();
        plain.push(ch);
        i += ch.len_utf8();
    }
    if !plain.is_empty() {
        out.push(Span { text: plain, style, var: false });
    }
}

/// Where byte `pos` of `raw` (with & shortcuts) lands once they're turned
/// into § codes (`&` is one byte, `§` two).
pub fn amp_pos(raw: &str, pos: usize) -> usize {
    pos + super::motd::amp_to_section(&raw[..pos.min(raw.len())]).len() - pos.min(raw.len())
}

/// Byte `pos` of a raw line (§ codes) as a position in [`Line::text`].
pub fn raw_to_text(raw: &str, pos: usize) -> usize {
    let mut n = 0;
    let mut it = raw.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        if i >= pos {
            break;
        }
        if c == '§' && it.peek().is_some() {
            it.next();
            continue;
        }
        n += c.len_utf8();
    }
    n
}

/// Position `pos` in [`Line::text`] as a byte in raw `raw`: after the codes
/// there when `after_codes` (a selection's start), else before them.
pub fn text_to_raw(raw: &str, pos: usize, after_codes: bool) -> usize {
    let mut n = 0;
    let mut it = raw.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let code = c == '§' && it.peek().is_some();
        if n == pos && !(after_codes && code) {
            return i;
        }
        if code {
            it.next();
            continue;
        }
        n += c.len_utf8();
    }
    raw.len()
}

#[cfg(test)]
mod tests {
    use super::super::motd::amp_to_section;
    use super::*;

    fn round(raw: &str) -> String {
        Line::parse(raw).to_codes()
    }

    fn at(l: &Line, s: &str) -> (usize, usize) {
        let t = l.text();
        let a = t.find(s).unwrap();
        (a, a + s.len())
    }

    #[test]
    fn reads_and_writes_the_same_codes() {
        let l1 = "§fT§6N§fP §6L§fimit§6l§fess 8 Server | v{pack_version}";
        assert_eq!(round(l1), l1);
        let l = Line::parse(l1);
        assert_eq!(l.text(), "TNP Limitless 8 Server | v{pack_version}");
        assert!(l.spans.last().unwrap().var);
        assert_eq!(l.spans.last().unwrap().var_name(), Some("pack_version"));
        // Codes that change nothing go.
        assert_eq!(round("§m§n{extra_mods}§r Mods§l"), "§n§m{extra_mods}§r Mods");
        assert_eq!(round("§6§6a§r§rb"), "§6a§rb");
        assert_eq!(round("plain {{day}} }}"), "plain {{day}} }}");
        assert!(!Line::parse("{{day}}").spans.iter().any(|s| s.var));
    }

    #[test]
    fn a_colour_resets_styles_so_they_are_written_again() {
        // Bold gold then bold white: the bold goes again after §f.
        let mut l = Line::parse("§6§lGold White");
        let (a, b) = at(&l, "White");
        l.toggle(a, b, Attr::Color('f'));
        assert_eq!(l.to_codes(), "§6§lGold §f§lWhite");
        // Dropping a style needs a colour (or §r) first.
        let mut l = Line::parse("§6§l§oab");
        l.toggle(1, 2, Attr::Bold);
        assert_eq!(l.to_codes(), "§6§l§oa§6§ob");
        let mut l = Line::parse("§l§nab");
        l.toggle(1, 2, Attr::Underline);
        assert_eq!(l.to_codes(), "§l§na§r§lb");
    }

    #[test]
    fn style_buttons_toggle_over_a_selection() {
        let mut l = Line::parse("Day {day} - Added {extra_mods} Mods");
        let (a, b) = at(&l, "{extra_mods}");
        l.toggle(a, b, Attr::Strike);
        assert_eq!(l.to_codes(), "Day {day} - Added §m{extra_mods}§r Mods");
        l.toggle(a, b, Attr::Underline);
        assert_eq!(l.to_codes(), "Day {day} - Added §n§m{extra_mods}§r Mods");
        // Clicked again: off, and no codes left over.
        l.toggle(a, b, Attr::Strike);
        l.toggle(a, b, Attr::Underline);
        assert_eq!(l.to_codes(), "Day {day} - Added {extra_mods} Mods");
        // Partly bold: the first click makes all of it bold.
        let mut l = Line::parse("a§lb§rc");
        l.toggle(0, 3, Attr::Bold);
        assert_eq!(l.to_codes(), "§labc");
        l.toggle(0, 3, Attr::Bold);
        assert_eq!(l.to_codes(), "abc");
        // A colour clicked twice goes back to plain gray.
        let mut l = Line::parse("TNP");
        l.toggle(1, 2, Attr::Color('6'));
        assert_eq!(l.to_codes(), "T§6N§rP");
        l.toggle(1, 2, Attr::Color('6'));
        assert_eq!(l.to_codes(), "TNP");
        // ...or to the colour of the text around it.
        let mut l = Line::parse("§fTNP 8 Server");
        let (a, b) = at(&l, "8 Server");
        l.toggle(a, b, Attr::Color('6'));
        assert_eq!(l.to_codes(), "§fTNP §68 Server");
        l.toggle(a, b, Attr::Color('6'));
        assert_eq!(l.to_codes(), "§fTNP 8 Server");
        let mut l = Line::parse("§6Gold§b aqua");
        l.toggle(0, 4, Attr::Color('6'));
        assert_eq!(l.to_codes(), "§bGold aqua");
        // R clears the selection only.
        let mut l = Line::parse("§6§lGold");
        l.clear(2, 4);
        assert_eq!(l.to_codes(), "§6§lGo§rld");
    }

    #[test]
    fn variables_stay_whole() {
        let mut l = Line::parse("v{pack_version}!");
        // Selecting part of a variable styles all of it.
        l.toggle(3, 5, Attr::Bold);
        assert_eq!(l.to_codes(), "v§l{pack_version}§r!");
        // Backspace on its last brace deletes the variable.
        let t = l.text();
        let cut = format!("{}{}", &t[..t.len() - 2], "!");
        let caret = l.edit(&cut, None);
        assert_eq!(l.text(), "v!");
        assert_eq!(caret, 1);
        // Typing a variable makes one.
        let mut l = Line::parse("Day ");
        l.edit("Day {day", None);
        assert!(!l.spans.iter().any(|s| s.var));
        l.edit("Day {day}", None);
        assert_eq!(l.var_at(5).map(|(r, n)| (r, n.to_string())), Some((4..9, "day".to_string())));
    }

    #[test]
    fn typing_keeps_the_style_around_it() {
        let mut l = Line::parse("§6Gold§r gray");
        // After "Gold": typed text is gold.
        let caret = l.edit("Golden gray", None);
        assert_eq!(caret, 6);
        assert_eq!(l.to_codes(), "§6Golden§r gray");
        // A style picked with nothing selected applies to what's typed.
        let bold = Style { bold: true, ..l.style_at(11) };
        l.edit("Golden gray!", Some(bold));
        assert_eq!(l.to_codes(), "§6Golden§r gray§l!");
        // Pasted codes are read as codes, from the style where they land.
        let mut l = Line::parse("§6ab");
        let caret = l.edit("§6a§lX§rb", None);
        // "b" keeps its own gold.
        assert_eq!(l.to_codes(), "§6a§lX§6b");
        // Codes typed in the formatted box aren't shown; the box is reset.
        assert_eq!(l.text(), "aXb");
        assert_eq!(caret, 2);
        // Deleting a selection.
        let mut l = Line::parse("§6abc§fdef");
        l.edit("§6ab", None);
        assert_eq!(l.text(), "ab");
        let mut l = Line::parse("§6abc§fdef");
        l.edit("abef", None);
        assert_eq!(l.to_codes(), "§6ab§fef");
        // Non-ASCII stays on character boundaries.
        let mut l = Line::parse("é§6ü");
        l.edit("éxü", None);
        assert_eq!(l.to_codes(), "éx§6ü");
    }

    #[test]
    fn shortcuts_and_raw_positions() {
        let raw = "&6Gold &lbold";
        let s = amp_to_section(raw);
        assert_eq!(round(&s), "§6Gold §lbold");
        assert_eq!(amp_pos(raw, 7), 8);
        let raw = "§6Gold §lbold";
        let at = raw.find("bold").unwrap();
        assert_eq!(raw_to_text(raw, at), 5);
        assert_eq!(text_to_raw(raw, 5, true), at);
        assert_eq!(text_to_raw(raw, 5, false), at - "§l".len());
        assert_eq!(text_to_raw(raw, 9, false), raw.len());
    }
}
