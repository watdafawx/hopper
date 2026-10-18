//! Renders the HTML CurseForge uses for descriptions and changelogs as
//! formatted text: headings, paragraphs, nested bullets and numbered lists,
//! bold, italics, code and code blocks, links, images, tables, rules and
//! YouTube embeds (as a thumbnail linking to the video). Scripts, styles and
//! other iframes are dropped.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: Option<String>,
    /// An image's URL; `text` is then its alt text.
    pub image: Option<String>,
    /// The width the page gave the image, in pixels.
    pub width: Option<u32>,
}

impl Span {
    /// The key an image is fetched and cached under: its URL, with the
    /// width to show it at (`url#w=156`) when the page set one.
    pub fn image_key(&self) -> Option<String> {
        Some(crate::imgcache::width_key(self.image.as_ref()?, self.width))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Para,
    Heading(u8),
    /// List item at a nesting depth (0 = top level).
    Bullet(u8),
    /// Numbered list item at a nesting depth.
    Numbered(u8),
    Rule,
    /// A preformatted block; its text keeps its line breaks.
    Code,
    /// A table row, one span list per cell. `spans` stays empty.
    Row(Vec<Vec<Span>>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: Kind,
    pub spans: Vec<Span>,
}

fn spans_empty(spans: &[Span]) -> bool {
    spans.iter().all(|s| s.image.is_none() && s.text.trim().is_empty())
}

impl Block {
    fn is_empty(&self) -> bool {
        match &self.kind {
            Kind::Rule => false,
            Kind::Row(cells) => cells.iter().all(|c| spans_empty(c)),
            _ => spans_empty(&self.spans),
        }
    }
}

#[derive(Default)]
struct Parser {
    blocks: Vec<Block>,
    cur: Option<Block>,
    bold: u32,
    italic: u32,
    code: u32,
    /// Inside `<pre>`: text keeps its whitespace and goes into a code block.
    pre: u32,
    link: Option<String>,
    /// Open lists, innermost last: `true` for a numbered one.
    lists: Vec<bool>,
    skip: u32,
}

impl Parser {
    fn flush(&mut self) {
        if let Some(b) = self.cur.take()
            && !b.is_empty()
        {
            self.blocks.push(b);
        }
    }

    fn start(&mut self, kind: Kind) {
        self.flush();
        self.cur = Some(Block {
            kind,
            spans: Vec::new(),
        });
    }

    fn in_row(&self) -> bool {
        matches!(self.cur, Some(Block { kind: Kind::Row(_), .. }))
    }

    /// Where inline content goes now: the current cell, else the current
    /// block (a paragraph is started when there is none).
    fn spans_mut(&mut self) -> &mut Vec<Span> {
        let block = self.cur.get_or_insert_with(|| Block {
            kind: Kind::Para,
            spans: Vec::new(),
        });
        match &mut block.kind {
            Kind::Row(cells) => {
                if cells.is_empty() {
                    cells.push(Vec::new());
                }
                cells.last_mut().unwrap()
            }
            _ => &mut block.spans,
        }
    }

    fn push(&mut self, span: Span) {
        let spans = self.spans_mut();
        // Merge with the previous span when styles match.
        match spans.last_mut() {
            Some(last)
                if span.image.is_none()
                    && last.image.is_none()
                    && last.bold == span.bold
                    && last.italic == span.italic
                    && last.code == span.code
                    && last.link == span.link =>
            {
                last.text.push_str(&span.text)
            }
            _ => spans.push(span),
        }
    }

    fn text(&mut self, raw: &str) {
        if self.skip > 0 {
            return;
        }
        let t = decode_entities(raw);
        if self.pre > 0 {
            if !matches!(self.cur, Some(Block { kind: Kind::Code, .. })) {
                self.start(Kind::Code);
            }
            let block = self.cur.as_mut().unwrap();
            match block.spans.last_mut() {
                Some(last) => last.text.push_str(&t),
                None => block.spans.push(Span {
                    text: t,
                    ..Default::default()
                }),
            }
            return;
        }
        // Collapse whitespace like a browser, but keep it inside <code>.
        let mut t = if self.code > 0 { t } else { collapse_ws(&t) };
        // Whitespace between <tr> and its first <td> is not a cell.
        let row_without_cells =
            matches!(&self.cur, Some(Block { kind: Kind::Row(c), .. }) if c.is_empty());
        if row_without_cells && t.trim().is_empty() {
            return;
        }
        let empty = spans_empty(self.spans_mut());
        if empty {
            t = t.trim_start().to_string();
        }
        if t.is_empty() {
            return;
        }
        let span = Span {
            text: t,
            bold: self.bold > 0,
            italic: self.italic > 0,
            code: self.code > 0,
            link: self.link.clone(),
            image: None,
            width: None,
        };
        self.push(span);
    }

    fn image(&mut self, url: String, alt: String, width: Option<u32>) {
        if self.skip > 0 {
            return;
        }
        let alt = collapse_ws(&alt).trim().to_string();
        self.push(Span {
            text: if alt.is_empty() { "image".into() } else { alt },
            link: self.link.clone(),
            image: Some(url),
            width,
            ..Default::default()
        });
    }

    fn tag(&mut self, tag: &str) {
        let closing = tag.starts_with('/');
        let body = tag.trim_start_matches('/');
        let name: String = body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("script" | "style", false) => self.skip += 1,
            ("script" | "style", true) => self.skip = self.skip.saturating_sub(1),
            ("iframe", false) => {
                self.skip += 1;
                if let Some(id) = attr(body, "src").as_deref().and_then(youtube_id) {
                    // CurseForge's video embeds: the poster, then a link.
                    let saved = self.link.take();
                    self.flush();
                    self.skip -= 1;
                    self.link = Some(format!("https://www.youtube.com/watch?v={id}"));
                    self.start(Kind::Para);
                    self.image(
                        format!("https://img.youtube.com/vi/{id}/mqdefault.jpg"),
                        "Video".into(),
                        None,
                    );
                    self.start(Kind::Para);
                    self.text("▶ Watch on YouTube");
                    self.flush();
                    self.skip += 1;
                    self.link = saved;
                }
            }
            ("iframe", true) => self.skip = self.skip.saturating_sub(1),
            ("br", _) => {
                // A line break inside a list item, heading or cell stays in it.
                if self
                    .cur
                    .as_ref()
                    .is_some_and(|b| matches!(b.kind, Kind::Para))
                {
                    self.start(Kind::Para);
                } else {
                    self.text(" ");
                }
            }
            ("p" | "div" | "blockquote" | "center", false) => {
                if self.in_row() {
                    self.text(" ");
                } else {
                    self.start(Kind::Para);
                    if name == "div" && attr(body, "class").is_some_and(|c| c.contains("spoiler")) {
                        self.bold += 1;
                        self.text("Spoiler");
                        self.bold -= 1;
                        self.start(Kind::Para);
                    }
                }
            }
            ("p" | "div" | "blockquote" | "center", true) => {
                if !self.in_row() {
                    self.flush();
                }
            }
            ("li", true) => self.flush(),
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                let level = name[1..].parse().unwrap_or(3);
                self.start(Kind::Heading(level));
            }
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", true) => self.flush(),
            ("summary", false) => {
                self.start(Kind::Para);
                self.bold += 1;
            }
            ("summary", true) => {
                self.bold = self.bold.saturating_sub(1);
                self.flush();
            }
            ("ul" | "ol", false) => {
                self.flush();
                self.lists.push(name == "ol");
            }
            ("ul" | "ol", true) => {
                self.flush();
                self.lists.pop();
            }
            ("li", false) => {
                let depth = self.lists.len().saturating_sub(1) as u8;
                self.start(if self.lists.last() == Some(&true) {
                    Kind::Numbered(depth)
                } else {
                    Kind::Bullet(depth)
                });
            }
            ("table" | "thead" | "tbody" | "tfoot", _) => self.flush(),
            ("tr", false) => self.start(Kind::Row(Vec::new())),
            ("tr", true) => self.flush(),
            ("td" | "th", false) => {
                if !self.in_row() {
                    self.start(Kind::Row(Vec::new()));
                }
                if let Some(Block {
                    kind: Kind::Row(cells),
                    ..
                }) = &mut self.cur
                {
                    cells.push(Vec::new());
                }
            }
            ("hr", _) => {
                self.flush();
                self.blocks.push(Block {
                    kind: Kind::Rule,
                    spans: Vec::new(),
                });
            }
            ("img", _) => {
                if let Some(src) = attr(body, "src").and_then(|s| abs_url(&s)) {
                    let alt = attr(body, "alt").unwrap_or_default();
                    let width = attr(body, "width")
                        .and_then(|w| w.trim().trim_end_matches("px").parse::<u32>().ok())
                        .filter(|w| *w > 0);
                    self.image(src, alt, width);
                }
            }
            ("strong" | "b", false) => self.bold += 1,
            ("strong" | "b", true) => self.bold = self.bold.saturating_sub(1),
            ("em" | "i", false) => self.italic += 1,
            ("em" | "i", true) => self.italic = self.italic.saturating_sub(1),
            ("code", false) => self.code += 1,
            ("code", true) => self.code = self.code.saturating_sub(1),
            ("pre", false) => {
                self.flush();
                self.pre += 1;
            }
            ("pre", true) => {
                self.pre = self.pre.saturating_sub(1);
                self.flush();
            }
            ("span", false) => {
                // Big text at the start of a paragraph is a heading in
                // disguise (Create's description, among others).
                let px = attr(body, "style").and_then(|s| font_px(&s));
                let fresh = match &self.cur {
                    None => true,
                    Some(b) => matches!(b.kind, Kind::Para) && spans_empty(&b.spans),
                };
                if let Some(px) = px
                    && px >= 18
                    && fresh
                    && !self.in_row()
                {
                    self.start(Kind::Heading(if px >= 24 { 2 } else { 3 }));
                }
            }
            ("a", false) => self.link = attr(body, "href").and_then(|h| abs_url(&h)),
            ("a", true) => self.link = None,
            _ => {}
        }
    }
}

/// The video id of a YouTube embed URL.
fn youtube_id(src: &str) -> Option<String> {
    let rest = src
        .split_once("youtube.com/embed/")
        .or_else(|| src.split_once("youtube-nocookie.com/embed/"))
        .map(|(_, r)| r)?;
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!id.is_empty()).then_some(id)
}

/// `font-size: 20px` in a style attribute, in pixels (pt scaled).
fn font_px(style: &str) -> Option<u32> {
    let lower = style.to_ascii_lowercase();
    let i = lower.find("font-size")? + "font-size".len();
    let v = lower[i..].trim_start().strip_prefix(':')?.trim_start();
    let digits: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f32 = digits.parse().ok()?;
    let unit = v[digits.len()..].trim_start();
    Some(if unit.starts_with("pt") {
        (n * 4. / 3.) as u32
    } else if unit.starts_with("em") || unit.starts_with("rem") {
        (n * 16.) as u32
    } else {
        n as u32
    })
}

/// An href or src as an absolute URL: CurseForge's `/linkout?remoteUrl=`
/// redirects unwrapped, site-relative paths completed, anchors and
/// javascript dropped.
fn abs_url(h: &str) -> Option<String> {
    let h = h.trim();
    if let Some(enc) = h
        .strip_prefix("/linkout?remoteUrl=")
        .or_else(|| h.strip_prefix("https://www.curseforge.com/linkout?remoteUrl="))
    {
        let once = percent_decode(enc);
        let twice = percent_decode(&once);
        return Some(twice).filter(|u| u.starts_with("http"));
    }
    if h.starts_with("http://") || h.starts_with("https://") {
        return Some(h.to_string());
    }
    if let Some(rest) = h.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    if h.starts_with('/') && !h.starts_with("//") {
        return Some(format!("https://www.curseforge.com{h}"));
    }
    None
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(hex, 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    // Whole attribute names only: `src=` must not match `data-src=`.
    let mut from = 0;
    let i = loop {
        let i = lower[from..].find(&format!("{name}="))? + from;
        let before = lower[..i].chars().next_back();
        if before.is_none_or(|c| c.is_whitespace()) {
            break i + name.len() + 1;
        }
        from = i + name.len();
    };
    let rest = &tag[i..];
    let (q, rest) = match rest.chars().next()? {
        c @ ('"' | '\'') => (c, &rest[1..]),
        _ => (' ', rest),
    };
    Some(decode_entities(&rest[..rest.find(q).unwrap_or(rest.len())]))
}

pub fn parse(html: &str) -> Vec<Block> {
    let mut p = Parser::default();
    let mut rest = html;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                let end = rest.find('>').unwrap_or(rest.len() - 1);
                let tag = &rest[1..end];
                if !tag.starts_with('!') {
                    p.tag(tag.trim_end_matches('/').trim());
                }
                rest = &rest[end + 1..];
            }
            Some(i) => {
                p.text(&rest[..i]);
                rest = &rest[i..];
            }
            None => {
                p.text(rest);
                rest = "";
            }
        }
    }
    p.flush();
    // Rules at the very start or end only add noise.
    while p.blocks.last().is_some_and(|b| b.kind == Kind::Rule) {
        p.blocks.pop();
    }
    while p.blocks.first().is_some_and(|b| b.kind == Kind::Rule) {
        p.blocks.remove(0);
    }
    // Plain-text changelogs: turn "- item" / "* item" lines into bullets.
    for b in &mut p.blocks {
        if b.kind == Kind::Para
            && let Some(first) = b.spans.first_mut()
            && first.image.is_none()
        {
            for prefix in ["- ", "* ", "• "] {
                if let Some(t) = first.text.trim_start().strip_prefix(prefix) {
                    first.text = t.to_string();
                    b.kind = Kind::Bullet(0);
                    break;
                }
            }
        }
    }
    p.blocks
}

/// Every image key ([`Span::image_key`]) in `blocks`, in order, each once.
pub fn images(blocks: &[Block]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |spans: &[Span]| {
        for s in spans {
            if let Some(k) = s.image_key()
                && !out.contains(&k)
            {
                out.push(k);
            }
        }
    };
    for b in blocks {
        match &b.kind {
            Kind::Row(cells) => cells.iter().for_each(|c| add(c)),
            _ => add(&b.spans),
        }
    }
    out
}

/// Markdown (Modrinth changelogs and descriptions) as blocks. Converted to
/// the HTML subset [`parse`] understands; raw HTML in the text passes through.
pub fn parse_markdown(md: &str) -> Vec<Block> {
    parse(&markdown_to_html(md))
}

fn markdown_to_html(md: &str) -> String {
    let mut out = String::new();
    let mut para: Vec<String> = Vec::new();
    // Open lists, as (indentation width, numbered?).
    let mut lists: Vec<(usize, bool)> = Vec::new();
    let mut in_code = false;
    let mut in_table = false;
    let flush_para = |out: &mut String, para: &mut Vec<String>| {
        if !para.is_empty() {
            out.push_str("<p>");
            out.push_str(&inline_md(&para.join(" ")));
            out.push_str("</p>");
            para.clear();
        }
    };
    let close_lists = |out: &mut String, lists: &mut Vec<(usize, bool)>, to: usize| {
        while lists.len() > to {
            let (_, numbered) = lists.pop().unwrap();
            out.push_str(if numbered { "</ol>" } else { "</ul>" });
        }
    };
    let close_table = |out: &mut String, in_table: &mut bool| {
        if *in_table {
            out.push_str("</table>");
            *in_table = false;
        }
    };
    for raw in md.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            flush_para(&mut out, &mut para);
            close_table(&mut out, &mut in_table);
            out.push_str(if in_code { "</pre>" } else { "<pre>" });
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push_str(&line.replace('&', "&amp;").replace('<', "&lt;"));
            out.push('\n');
            continue;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        // GitHub-style tables: rows of cells between pipes.
        if trimmed.starts_with('|') && trimmed.len() > 1 {
            let inner = trimmed.trim_matches('|');
            if inner
                .chars()
                .all(|c| matches!(c, '-' | ':' | '|' | ' '))
            {
                continue; // the header separator
            }
            flush_para(&mut out, &mut para);
            close_lists(&mut out, &mut lists, 0);
            if !in_table {
                out.push_str("<table>");
                in_table = true;
            }
            out.push_str("<tr>");
            for cell in split_cells(inner) {
                out.push_str("<td>");
                out.push_str(&inline_md(cell.trim()));
                out.push_str("</td>");
            }
            out.push_str("</tr>");
            continue;
        }
        close_table(&mut out, &mut in_table);
        if trimmed.is_empty() {
            flush_para(&mut out, &mut para);
            close_lists(&mut out, &mut lists, 0);
            continue;
        }
        if let Some(level) = (1..=6)
            .rev()
            .find(|&n| trimmed.starts_with(&format!("{} ", "#".repeat(n))))
        {
            flush_para(&mut out, &mut para);
            close_lists(&mut out, &mut lists, 0);
            out.push_str(&format!(
                "<h{level}>{}</h{level}>",
                inline_md(trimmed[level + 1..].trim())
            ));
            continue;
        }
        if matches!(trimmed, "---" | "***" | "___") {
            flush_para(&mut out, &mut para);
            close_lists(&mut out, &mut lists, 0);
            out.push_str("<hr>");
            continue;
        }
        let bullet = ["- ", "* ", "+ "]
            .iter()
            .find_map(|b| trimmed.strip_prefix(b))
            .map(|item| (item, false))
            .or_else(|| {
                let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
                (digits > 0)
                    .then(|| trimmed[digits..].strip_prefix(". "))
                    .flatten()
                    .map(|item| (item, true))
            });
        if let Some((item, numbered)) = bullet {
            flush_para(&mut out, &mut para);
            while lists.last().is_some_and(|&(d, _)| indent < d) {
                let (_, n) = lists.pop().unwrap();
                out.push_str(if n { "</ol>" } else { "</ul>" });
            }
            if lists.last().is_none_or(|&(d, _)| indent > d) {
                lists.push((indent, numbered));
                out.push_str(if numbered { "<ol>" } else { "<ul>" });
            }
            out.push_str(&format!("<li>{}</li>", inline_md(item)));
            continue;
        }
        if !lists.is_empty() && indent > 0 {
            // Continuation of a list item.
            out.push_str(&format!("<li>{}</li>", inline_md(trimmed)));
            continue;
        }
        close_lists(&mut out, &mut lists, 0);
        if let Some(q) = trimmed.strip_prefix("> ") {
            flush_para(&mut out, &mut para);
            out.push_str(&format!("<p><i>{}</i></p>", inline_md(q)));
            continue;
        }
        para.push(trimmed.to_string());
        if raw.ends_with("  ") {
            flush_para(&mut out, &mut para);
        }
    }
    flush_para(&mut out, &mut para);
    close_lists(&mut out, &mut lists, 0);
    close_table(&mut out, &mut in_table);
    if in_code {
        out.push_str("</pre>");
    }
    out
}

/// Table cells split on pipes that aren't inside inline code or escaped.
fn split_cells(row: &str) -> Vec<String> {
    let mut cells = vec![String::new()];
    let mut code = false;
    let mut chars = row.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cells.last_mut().unwrap().push('|');
                chars.next();
            }
            '`' => {
                code = !code;
                cells.last_mut().unwrap().push(c);
            }
            '|' if !code => cells.push(String::new()),
            _ => cells.last_mut().unwrap().push(c),
        }
    }
    cells
}

/// `[text](url "title")` at the start of `s`, with nested brackets (an image
/// inside a link) allowed: the text, the url and the length consumed.
fn link_at(s: &str) -> Option<(&str, &str, usize)> {
    let mut depth = 0usize;
    let mut close = None;
    for (i, c) in s.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = &s[close + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let end = after.find(')')?;
    let mut url = &after[1..end];
    if let Some((u, _)) = url.split_once(' ') {
        url = u;
    }
    let url = url.trim_matches(|c| c == '<' || c == '>');
    Some((&s[1..close], url, close + 1 + end + 1))
}

/// Inline Markdown: code, images, links, bold, italics.
fn inline_md(s: &str) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    let mut bold = false;
    let mut italic = false;
    while i < b.len() {
        let rest = &s[i..];
        if let Some(after) = rest.strip_prefix('`')
            && let Some(end) = after.find('`')
        {
            out.push_str(&format!(
                "<code>{}</code>",
                after[..end].replace('<', "&lt;")
            ));
            i += end + 2;
            continue;
        }
        if rest.starts_with("![")
            && let Some((alt, url, used)) = link_at(&rest[1..])
        {
            out.push_str(&format!(
                "<img src=\"{}\" alt=\"{}\">",
                url.replace('"', "%22"),
                alt.replace('"', "&quot;")
            ));
            i += used + 1;
            continue;
        }
        if rest.starts_with('[')
            && let Some((text, url, used)) = link_at(rest)
        {
            out.push_str(&format!(
                "<a href=\"{}\">{}</a>",
                url.replace('"', "%22"),
                inline_md(text)
            ));
            i += used;
            continue;
        }
        if rest.starts_with("**") || rest.starts_with("__") {
            out.push_str(if bold { "</strong>" } else { "<strong>" });
            bold = !bold;
            i += 2;
            continue;
        }
        if rest.starts_with('*') {
            out.push_str(if italic { "</em>" } else { "<em>" });
            italic = !italic;
            i += 1;
            continue;
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    if bold {
        out.push_str("</strong>");
    }
    if italic {
        out.push_str("</em>");
    }
    out
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        // By byte: ten bytes can end inside a character ("&️"…).
        let Some(end) = rest.as_bytes()[..rest.len().min(10)].iter().position(|&b| b == b';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "rsquo" | "lsquo" => Some('\''),
            "ldquo" | "rdquo" => Some('"'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "middot" => Some('·'),
            "bull" => Some('•'),
            "copy" => Some('©'),
            "trade" => Some('™'),
            "rarr" => Some('→'),
            "larr" => Some('←'),
            _ if ent.starts_with("#x") => u32::from_str_radix(&ent[2..], 16)
                .ok()
                .and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One image of a [`Segment::Badges`] row.
#[derive(Clone, Debug, PartialEq)]
pub struct Badge {
    /// The key it is cached under ([`Span::image_key`]).
    pub key: String,
    /// The full image's URL.
    pub url: String,
    pub link: Option<String>,
    pub alt: String,
    /// The width the page gave it, in pixels.
    pub width: Option<u32>,
}

/// A piece of a description: Markdown, or a paragraph made only of images
/// (badge rows), which Markdown views stack instead of lining up.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Markdown(String),
    Badges(Vec<Badge>),
}

/// The badges of a paragraph holding two or more images and nothing else
/// but whitespace.
fn badge_row(b: &Block) -> Option<Vec<Badge>> {
    if b.kind != Kind::Para {
        return None;
    }
    let mut out = Vec::new();
    for s in &b.spans {
        match &s.image {
            Some(url) => out.push(Badge {
                key: s.image_key()?,
                url: url.clone(),
                link: s.link.clone(),
                alt: s.text.clone(),
                width: s.width,
            }),
            None if s.text.trim().is_empty() => {}
            None => return None,
        }
    }
    (out.len() >= 2).then_some(out)
}

/// Blocks as [`Segment`]s: rows of badges on their own, everything between
/// them as [`to_markdown`] makes it.
pub fn to_segments(blocks: &[Block]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut start = 0;
    let flush = |out: &mut Vec<Segment>, run: &[Block]| {
        let md = to_markdown(run);
        if !md.is_empty() {
            out.push(Segment::Markdown(md));
        }
    };
    for (i, b) in blocks.iter().enumerate() {
        if let Some(row) = badge_row(b) {
            flush(&mut out, &blocks[start..i]);
            out.push(Segment::Badges(row));
            start = i + 1;
        }
    }
    flush(&mut out, &blocks[start..]);
    out
}

/// `text` split into runs of plain text and bare `http(s)://` URLs (true).
/// A URL ends at whitespace; trailing punctuation, and a `)` it didn't open,
/// stay text.
pub fn bare_urls(text: &str) -> Vec<(&str, bool)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("http://").into_iter().chain(rest.find("https://")).min() {
        let tail = &rest[start..];
        let mut end = tail.find(|c: char| c.is_whitespace() || c == '<' || c == '"').unwrap_or(tail.len());
        loop {
            let u = &tail[..end];
            let trim = match u.chars().last() {
                Some('.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*') => true,
                Some(')') => u.matches('(').count() < u.matches(')').count(),
                _ => false,
            };
            if !trim {
                break;
            }
            end -= 1;
        }
        let scheme = if tail.starts_with("https://") { 8 } else { 7 };
        if end <= scheme {
            out.push((&rest[..start + scheme], false));
            rest = &rest[start + scheme..];
            continue;
        }
        if start > 0 {
            out.push((&rest[..start], false));
        }
        out.push((&tail[..end], true));
        rest = &tail[end..];
    }
    if !rest.is_empty() {
        out.push((rest, false));
    }
    out
}

/// Blocks as Markdown, for UIs that render Markdown themselves. An image
/// without a link of its own links to itself, so a click can open it.
pub fn to_markdown(blocks: &[Block]) -> String {
    fn escape(t: &str) -> String {
        let mut out = String::with_capacity(t.len());
        for c in t.chars() {
            if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '#' | '<' | '>' | '|' | '~') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }
    fn url(u: &str) -> String {
        format!("<{}>", u.replace('>', "%3E"))
    }
    fn span(s: &Span) -> String {
        if let Some(img) = &s.image {
            let alt: String = s.text.chars().filter(|c| !matches!(c, '[' | ']' | '\n')).collect();
            let t = format!("![{alt}]({})", url(&s.image_key().unwrap_or_default()));
            let link = s.link.as_deref().unwrap_or(img);
            return format!("[{t}]({})", url(link));
        }
        let text = s.text.replace('\n', " ");
        if text.trim().is_empty() {
            return text;
        }
        // Markers must hug the text, so spaces move outside them.
        let (lead, rest) = text.split_at(text.len() - text.trim_start().len());
        let (core, trail) = rest.split_at(rest.trim_end().len());
        let mut t = if s.code {
            format!("`{}`", core.replace('`', "'"))
        } else if s.link.is_none() {
            // A bare URL becomes a link of its own; escaped, its `#` or `_`
            // would end up in the address.
            bare_urls(core)
                .into_iter()
                .map(|(part, is_url)| match is_url {
                    true => format!("[{}]({})", escape(part), url(part)),
                    false => escape(part),
                })
                .collect()
        } else {
            escape(core)
        };
        if s.italic {
            t = format!("*{t}*");
        }
        if s.bold {
            t = format!("**{t}**");
        }
        if let Some(u) = &s.link {
            t = format!("[{t}]({})", url(u));
        }
        format!("{lead}{t}{trail}")
    }
    fn line(spans: &[Span]) -> String {
        spans.iter().map(span).collect::<String>().trim().to_string()
    }
    let mut out = String::new();
    let mut table_cols: Option<usize> = None;
    for b in blocks {
        if !matches!(b.kind, Kind::Row(_)) && table_cols.take().is_some() {
            out.push('\n');
        }
        match &b.kind {
            Kind::Rule => out.push_str("---\n\n"),
            Kind::Heading(level) => {
                if out.ends_with('\n') && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                out.push_str(&"#".repeat((*level).clamp(1, 6) as usize));
                out.push(' ');
                out.push_str(&line(&b.spans));
                out.push_str("\n\n");
            }
            Kind::Bullet(depth) => {
                out.push_str(&"  ".repeat(*depth as usize));
                out.push_str("- ");
                out.push_str(&line(&b.spans));
                out.push('\n');
            }
            Kind::Numbered(depth) => {
                out.push_str(&"   ".repeat(*depth as usize));
                out.push_str("1. ");
                out.push_str(&line(&b.spans));
                out.push('\n');
            }
            Kind::Code => {
                if out.ends_with('\n') && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                let text: String = b.spans.iter().map(|s| s.text.as_str()).collect();
                out.push_str("```\n");
                out.push_str(text.trim_matches('\n').replace("```", "'''").as_str());
                out.push_str("\n```\n\n");
            }
            Kind::Row(cells) => {
                let first = table_cols.is_none();
                if first && out.ends_with('\n') && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                let n = *table_cols.get_or_insert(cells.len().max(1));
                let mut row: Vec<String> = cells.iter().map(|c| line(c)).collect();
                row.resize(n.max(row.len()), String::new());
                out.push_str("| ");
                out.push_str(&row.join(" | "));
                out.push_str(" |\n");
                if first {
                    out.push('|');
                    out.push_str(&" --- |".repeat(n));
                    out.push('\n');
                }
            }
            Kind::Para => {
                // A paragraph after a list needs a blank line first.
                if out.ends_with('\n') && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                out.push_str(&line(&b.spans));
                out.push_str("\n\n");
            }
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn badge_rows_become_their_own_segments() {
        let blocks = parse(
            "<p>Intro</p><p><a href=\"https://a\"><img src=\"https://x/1.png\" width=\"156\"></a>              <img src=\"https://x/2.png\"></p><p><img src=\"https://x/3.png\"></p><p>End</p>",
        );
        let segs = super::to_segments(&blocks);
        assert_eq!(segs.len(), 3, "{segs:?}");
        assert!(matches!(&segs[0], super::Segment::Markdown(m) if m == "Intro"));
        let super::Segment::Badges(b) = &segs[1] else { panic!("{segs:?}") };
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].key, "https://x/1.png#w=156");
        assert_eq!(b[0].url, "https://x/1.png");
        assert_eq!(b[0].link.as_deref(), Some("https://a"));
        assert_eq!(b[1].link, None);
        // A lone image stays in the Markdown.
        assert!(matches!(&segs[2], super::Segment::Markdown(m) if m.contains("3.png") && m.contains("End")));
    }

    #[test]
    fn markdown_keeps_structure_and_escapes() {
        let blocks = super::parse(
            "<h2>Features</h2><ul><li>Fast <b>and</b> small</li></ul><p>Use *stars* <a href=\"https://x.y\">here</a></p>",
        );
        let md = super::to_markdown(&blocks);
        assert!(md.starts_with("## Features"), "{md}");
        assert!(md.contains("- Fast **and** small"), "{md}");
        assert!(md.contains("Use \\*stars\\* [here](<https://x.y>)"), "{md}");
    }

    use super::*;

    fn text(b: &Block) -> String {
        b.spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn structure_and_styles() {
        let b = parse(
            "<h2>Changes</h2><ul><li>Fixed <strong>crash</strong> with <a href=\"https://x.y\">Create</a></li><li>Nested<ul><li>deep</li></ul></li></ul><hr><p>Thanks &amp; enjoy</p><img src=a>",
        );
        assert_eq!(b[0].kind, Kind::Heading(2));
        assert_eq!(text(&b[0]), "Changes");
        assert_eq!(b[1].kind, Kind::Bullet(0));
        assert!(b[1].spans.iter().any(|s| s.bold && s.text == "crash"));
        assert!(
            b[1].spans
                .iter()
                .any(|s| s.link.as_deref() == Some("https://x.y"))
        );
        assert!(
            b.iter()
                .any(|x| x.kind == Kind::Bullet(1) && text(x) == "deep")
        );
        assert!(b.iter().any(|x| text(x) == "Thanks & enjoy"));
        assert!(b.iter().any(|x| x.kind == Kind::Rule));
        // A relative image path is not an image.
        assert!(b.iter().all(|x| x.spans.iter().all(|s| s.image.is_none())));
    }

    #[test]
    fn images_links_and_embeds() {
        let b = parse(
            r#"<p><img src="https://cdn/x.png" alt="Banner" width="1315"></p>
<p><a href="/linkout?remoteUrl=https%253a%252f%252fpatreon.com%252fme" rel="nofollow"><img src="https://i.imgur.com/p.png" alt="Patreon"></a></p>
<p><span><iframe src="https://www.youtube.com/embed/zjnh7Rygd4M" width="560"></iframe></span></p>
<p>Text with <img src="https://cdn/i.png"> inline</p>"#,
        );
        assert_eq!(b[0].kind, Kind::Para);
        assert_eq!(b[0].spans[0].image.as_deref(), Some("https://cdn/x.png"));
        assert_eq!(b[0].spans[0].text, "Banner");
        assert_eq!(b[1].spans[0].link.as_deref(), Some("https://patreon.com/me"));
        assert_eq!(
            b[2].spans[0].image.as_deref(),
            Some("https://img.youtube.com/vi/zjnh7Rygd4M/mqdefault.jpg")
        );
        assert_eq!(
            b[2].spans[0].link.as_deref(),
            Some("https://www.youtube.com/watch?v=zjnh7Rygd4M")
        );
        assert_eq!(text(&b[3]), "▶ Watch on YouTube");
        assert_eq!(b[4].spans.len(), 3, "{:?}", b[4]);
        assert_eq!(b[4].spans[1].text, "image");
        assert_eq!(b[0].spans[0].width, Some(1315));
        assert_eq!(
            images(&b),
            vec![
                "https://cdn/x.png#w=1315",
                "https://i.imgur.com/p.png",
                "https://img.youtube.com/vi/zjnh7Rygd4M/mqdefault.jpg",
                "https://cdn/i.png"
            ]
        );
        let md = to_markdown(&b);
        assert!(md.contains("[![Banner](<https://cdn/x.png#w=1315>)](<https://cdn/x.png>)"), "{md}");
        assert!(md.contains("[![Patreon](<https://i.imgur.com/p.png>)](<https://patreon.com/me>)"), "{md}");
    }

    #[test]
    fn code_blocks_numbered_lists_and_tables() {
        let b = parse(
            "<ol><li>one</li><li>two</li></ol><pre><code>a &lt; b\n  indented\n</code></pre><table><tr><th>Key</th><th>Value</th></tr><tr><td>x<br>y</td><td><b>1</b></td></tr></table><p><span style=\"font-size:20px;color:#202c44\">About</span></p>",
        );
        assert_eq!(b[0].kind, Kind::Numbered(0));
        assert_eq!(b[1].kind, Kind::Numbered(0));
        assert_eq!(b[2].kind, Kind::Code);
        assert_eq!(text(&b[2]), "a < b\n  indented\n");
        let Kind::Row(h) = &b[3].kind else { panic!("{:?}", b[3]) };
        assert_eq!(h.len(), 2);
        let Kind::Row(r) = &b[4].kind else { panic!("{:?}", b[4]) };
        assert_eq!(r[0].iter().map(|s| s.text.as_str()).collect::<String>(), "x y");
        assert!(r[1][0].bold);
        assert_eq!(b[5].kind, Kind::Heading(3));
        assert_eq!(text(&b[5]), "About");
        let md = to_markdown(&b);
        assert!(md.starts_with("1. one\n1. two\n\n```\na < b\n  indented\n```"), "{md}");
        assert!(md.contains("| Key | Value |\n| --- | --- |\n| x y | **1** |\n\n### About"), "{md}");
    }

    #[test]
    fn plain_text_dash_lines_become_bullets() {
        let b = parse("Version 1.2<br>- fixed a thing<br>- added stuff");
        assert_eq!(b[0].kind, Kind::Para);
        assert_eq!(b[1].kind, Kind::Bullet(0));
        assert_eq!(text(&b[1]), "fixed a thing");
    }

    #[test]
    fn markdown_structure() {
        let b = parse_markdown(
            "## Changes
- Fixed **crash** with [Create](https://x.y)
  - nested `code`

Thanks *all*

---
end",
        );
        assert_eq!(b[0].kind, Kind::Heading(2));
        assert_eq!(b[1].kind, Kind::Bullet(0));
        assert!(b[1].spans.iter().any(|s| s.bold && s.text == "crash"));
        assert!(
            b[1].spans
                .iter()
                .any(|s| s.link.as_deref() == Some("https://x.y"))
        );
        assert!(
            b.iter()
                .any(|x| x.kind == Kind::Bullet(1) && x.spans.iter().any(|s| s.code))
        );
        assert!(
            b.iter()
                .any(|x| x.spans.iter().any(|s| s.italic && s.text == "all"))
        );
        assert!(b.iter().any(|x| x.kind == Kind::Rule));
    }

    #[test]
    fn markdown_images_code_and_tables() {
        let b = parse_markdown(
            "[![Badge](https://img/b.png)](https://site) and ![Shot](https://img/s.png \"Title\")

1. first
2. second

```
let x = 1;
```

| A | B |
|---|---|
| `a|b` | 2 |

<img src=\"https://img/h.png\" width=\"200\">",
        );
        assert_eq!(b[0].spans[0].image.as_deref(), Some("https://img/b.png"));
        assert_eq!(b[0].spans[0].link.as_deref(), Some("https://site"));
        assert_eq!(b[0].spans[2].image.as_deref(), Some("https://img/s.png"));
        assert_eq!(b[1].kind, Kind::Numbered(0));
        assert_eq!(b[3].kind, Kind::Code);
        assert_eq!(text(&b[3]), "let x = 1;\n");
        let Kind::Row(r) = &b[5].kind else { panic!("{:?}", b[5]) };
        assert_eq!(r[0][0].text, "a|b");
        assert!(r[0][0].code);
        assert_eq!(b[6].spans[0].image.as_deref(), Some("https://img/h.png"));
        assert_eq!(b[6].spans[0].width, Some(200));
    }

    #[test]
    fn entities() {
        assert_eq!(
            decode_entities("a &amp; b &#8217; &#x2014; &bogus"),
            "a & b ’ — &bogus"
        );
        // Moonlight Lib's description: an emoji right after a bare "&".
        let emoji = "Tips &abcd\u{1f600}\u{1f600}x"; // byte 10 is inside the 2nd emoji
        assert_eq!(decode_entities(emoji), emoji);
        assert_eq!(decode_entities("&\u{1f600}&amp;"), "&\u{1f600}&");
    }

    #[test]
    fn attrs_and_urls() {
        assert_eq!(attr("img data-src=\"a\" src='b'", "src").as_deref(), Some("b"));
        assert_eq!(abs_url("/minecraft/mc-mods/jei").as_deref(), Some("https://www.curseforge.com/minecraft/mc-mods/jei"));
        assert_eq!(abs_url("#top"), None);
        assert_eq!(abs_url("javascript:void(0)"), None);
        assert_eq!(font_px("color:#fff; font-size: 14pt"), Some(18));
    }

    #[test]
    fn bare_urls_become_links() {
        let md = to_markdown(&parse("<p>https://github.com/a/b/blob/master/CHANGELOG.md#1110</p>"));
        assert_eq!(
            md,
            r"[https://github.com/a/b/blob/master/CHANGELOG.md\#1110](<https://github.com/a/b/blob/master/CHANGELOG.md#1110>)"
        );
        assert_eq!(
            bare_urls("see (https://x.io/a_(b)), ok."),
            [("see (", false), ("https://x.io/a_(b)", true), ("), ok.", false)]
        );
        assert_eq!(bare_urls("just https:// text"), [("just https://", false), (" text", false)]);
    }
}
