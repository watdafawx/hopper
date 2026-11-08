//! Renders the HTML CurseForge uses for descriptions and changelogs as
//! formatted text: headings, paragraphs, nested bullets and numbered lists,
//! bold, italics, code and code blocks, links, images, tables, rules and
//! videos (YouTube, Vimeo and Streamable embeds, links to them standing alone
//! in a paragraph, and `.mp4`/`.webm` files). Scripts, styles and other
//! iframes are dropped.

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
    /// The height the page gave the image, in pixels (badges often set
    /// only this: `<img height="48">`).
    pub height: Option<u32>,
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
    /// A video. `spans` stays empty.
    Video(Video),
}

/// A video in a description: a YouTube, Vimeo or Streamable one, or a
/// video file.
#[derive(Clone, Debug, PartialEq)]
pub struct Video {
    /// Its page (or the file), for the browser.
    pub url: String,
    /// The player page to embed; `None` for a video file.
    pub embed: Option<String>,
    /// A poster image URL, when one is known.
    pub thumb: Option<String>,
}

impl Video {
    /// The site it plays on, for labels.
    pub fn site(&self) -> &'static str {
        if self.url.contains("youtube.com/") {
            "YouTube"
        } else if self.url.contains("vimeo.com/") {
            "Vimeo"
        } else if self.url.contains("streamable.com/") {
            "Streamable"
        } else {
            "video"
        }
    }
}

/// `u` as a video, when it is one: a YouTube page, short link or embed, a
/// Vimeo or Streamable page or player, or a `.mp4`/`.webm` file.
pub fn video_of(u: &str) -> Option<Video> {
    let u = u.trim();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"))
        .or_else(|| u.strip_prefix("//"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .unwrap_or(&host);
    let id_of = |s: &str| -> Option<String> {
        let id: String = s
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .collect();
        (!id.is_empty()).then_some(id)
    };
    let youtube = |id: String| Video {
        url: format!("https://www.youtube.com/watch?v={id}"),
        embed: Some(format!("https://www.youtube.com/embed/{id}")),
        thumb: Some(format!("https://img.youtube.com/vi/{id}/hqdefault.jpg")),
    };
    match host {
        "youtube.com" | "youtube-nocookie.com" | "music.youtube.com" => {
            if let Some(p) = path
                .strip_prefix("embed/")
                .or_else(|| path.strip_prefix("shorts/"))
                .or_else(|| path.strip_prefix("live/"))
                .or_else(|| path.strip_prefix("v/"))
            {
                return id_of(p).map(youtube);
            }
            let query = path.strip_prefix("watch")?.split_once('?')?.1;
            let v = query.split('&').find_map(|kv| kv.strip_prefix("v="))?;
            id_of(v).map(youtube)
        }
        "youtu.be" => id_of(path).map(youtube),
        "vimeo.com" | "player.vimeo.com" => {
            let p = path.strip_prefix("video/").unwrap_or(path);
            let id: String = p.chars().take_while(char::is_ascii_digit).collect();
            if id.is_empty() {
                return None;
            }
            // Unlisted videos carry a hash: vimeo.com/ID/HASH or ?h=HASH.
            let hash = p[id.len()..]
                .strip_prefix('/')
                .and_then(id_of)
                .or_else(|| {
                    let q = p.split_once('?')?.1;
                    q.split('&').find_map(|kv| kv.strip_prefix("h=")).and_then(id_of)
                });
            Some(Video {
                url: match &hash {
                    Some(h) => format!("https://vimeo.com/{id}/{h}"),
                    None => format!("https://vimeo.com/{id}"),
                },
                embed: Some(match &hash {
                    Some(h) => format!("https://player.vimeo.com/video/{id}?h={h}"),
                    None => format!("https://player.vimeo.com/video/{id}"),
                }),
                thumb: None,
            })
        }
        "streamable.com" => {
            let p = path
                .strip_prefix("e/")
                .or_else(|| path.strip_prefix("o/"))
                .unwrap_or(path);
            let id = id_of(p)?;
            Some(Video {
                url: format!("https://streamable.com/{id}"),
                embed: Some(format!("https://streamable.com/e/{id}")),
                thumb: None,
            })
        }
        _ => {
            let file = path.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
            (file.ends_with(".mp4") || file.ends_with(".webm")).then(|| Video {
                url: format!("https://{rest}"),
                embed: None,
                thumb: None,
            })
        }
    }
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
            Kind::Rule | Kind::Video(_) => false,
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
    /// Inside `<video>`: its poster, and whether a source was taken yet.
    video: Option<(Option<String>, bool)>,
    /// Open `<span>`s, and the depth of a CurseForge "Embed Removed" one
    /// being read: its text is dropped and its link becomes the video.
    spans: u32,
    embed_removed: Option<u32>,
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
        if self.skip > 0 || self.embed_removed.is_some() {
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
        // GitHub `:sparkles:` shortcodes show as the emoji, as they do on
        // GitHub; code keeps them as written.
        let mut t = if self.code > 0 { t } else { crate::emoji::shortcodes(&collapse_ws(&t)).into_owned() };
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
            height: None,
        };
        self.push(span);
    }

    fn image(&mut self, url: String, alt: String, width: Option<u32>, height: Option<u32>) {
        if self.skip > 0 || self.embed_removed.is_some() {
            return;
        }
        let alt = collapse_ws(&alt).trim().to_string();
        self.push(Span {
            text: if alt.is_empty() { "image".into() } else { alt },
            link: self.link.clone(),
            image: Some(url),
            width,
            height,
            ..Default::default()
        });
    }

    /// A video block, ending the current block.
    fn video(&mut self, v: Video) {
        if self.skip > 0 {
            return;
        }
        self.flush();
        self.blocks.push(Block {
            kind: Kind::Video(v),
            spans: Vec::new(),
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
                if let Some(v) = attr(body, "src").as_deref().and_then(video_of) {
                    self.video(v);
                }
                self.skip += 1;
            }
            ("iframe", true) => self.skip = self.skip.saturating_sub(1),
            ("video", false) => {
                let poster = attr(body, "poster").and_then(|s| abs_url(&s));
                let src = attr(body, "src").and_then(|s| abs_url(&s));
                self.video = Some((poster.clone(), src.is_some()));
                if let Some(url) = src {
                    self.video(Video { url, embed: None, thumb: poster });
                }
            }
            ("source", _) => {
                if let Some((poster, false)) = self.video.clone()
                    && let Some(url) = attr(body, "src").and_then(|s| abs_url(&s))
                {
                    self.video = Some((poster.clone(), true));
                    self.video(Video { url, embed: None, thumb: poster });
                }
            }
            ("video", true) => self.video = None,
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
                    // CurseForge spoilers (`<div class="spoiler">`) show their body inline.
                    self.start(Kind::Para);
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
                    let style = attr(body, "style").unwrap_or_default();
                    let width = attr(body, "width").and_then(|w| px_size(&w)).or_else(|| style_px(&style, "width"));
                    let height = attr(body, "height").and_then(|h| px_size(&h)).or_else(|| style_px(&style, "height"));
                    self.image(src, alt, width, height);
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
            ("span", true) => {
                if self.embed_removed == Some(self.spans) {
                    self.embed_removed = None;
                }
                self.spans = self.spans.saturating_sub(1);
            }
            ("span", false) if attr(body, "class").is_some_and(|c| c.split_whitespace().any(|c| c == "embed-removed")) => {
                // CurseForge swaps an embedded video for "Embed Removed:
                // <link>"; the link is the video.
                self.spans += 1;
                self.embed_removed = Some(self.spans);
            }
            ("span", false) => {
                self.spans += 1;
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
            ("a", false) => {
                self.link = attr(body, "href").and_then(|h| abs_url(&h));
                if self.embed_removed.is_some()
                    && let Some(v) = self.link.as_deref().and_then(video_of)
                {
                    let depth = self.embed_removed.take();
                    self.video(v);
                    self.embed_removed = depth;
                }
            }
            ("a", true) => self.link = None,
            _ => {}
        }
    }
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

/// A size attribute as whole pixels: "48", "48px", "150.775" and the
/// "750/" a self-closing tag leaves behind all read; percentages don't.
fn px_size(v: &str) -> Option<u32> {
    let v = v.trim();
    let num: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let rest = v[num.len()..].trim_start();
    if rest.starts_with('%') || rest.starts_with("em") || rest.starts_with("rem") || rest.starts_with("vw") {
        return None;
    }
    let n: f32 = num.parse().ok()?;
    (n >= 1.).then(|| n.round() as u32)
}

/// `width: 120px` (or another property) in a style attribute, in pixels.
fn style_px(style: &str, prop: &str) -> Option<u32> {
    style.split(';').find_map(|d| {
        let (k, v) = d.split_once(':')?;
        (k.trim().eq_ignore_ascii_case(prop)).then(|| px_size(v))?
    })
}

/// Tags an author's HTML may hold that CurseForge's Markdown editor shows
/// escaped (`&lt;center&gt;`), read back as tags. Code keeps its text.
const ESCAPED_TAGS: [&str; 30] = [
    "center", "p", "div", "img", "a", "br", "details", "summary", "table", "thead", "tbody", "tr", "td", "th", "h1",
    "h2", "h3", "h4", "h5", "h6", "ul", "ol", "li", "b", "strong", "i", "em", "span", "hr", "u",
];

/// `html` with escaped tags of [`ESCAPED_TAGS`] turned back into tags (not
/// inside `<pre>` or `<code>`), and Markdown links CurseForge left as text
/// around its own link (`[Name](<a href=u>u</a>)`) made the link.
fn unescape_tags(html: &str) -> std::borrow::Cow<'_, str> {
    if !html.contains("&lt;") && !html.contains("](<a ") {
        return std::borrow::Cow::Borrowed(html);
    }
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut code = 0u32;
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        let lrest = &lower[i..];
        if rest.starts_with('<') {
            let end = rest.find('>').map_or(rest.len(), |e| e + 1);
            let tag = &lrest[..end];
            if tag.starts_with("<pre") || tag.starts_with("<code") {
                code += 1;
            } else if tag.starts_with("</pre") || tag.starts_with("</code") {
                code = code.saturating_sub(1);
            }
            out.push_str(&rest[..end]);
            i += end;
            continue;
        }
        if code == 0
            && let Some(after) = lrest.strip_prefix("&lt;")
        {
            let name_start = usize::from(after.starts_with('/'));
            let name: String = after[name_start..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            let next = after[name_start + name.len()..].chars().next();
            // The tag ends at the next "&gt;", before any other tag starts.
            let close = after
                .find("&gt;")
                .filter(|&c| c < 600 && !after[..c].contains('<') && !after[..c].contains("&lt;"));
            if ESCAPED_TAGS.contains(&name.as_str())
                && matches!(next, Some(' ' | '/' | '&' | '\n'))
                && let Some(c) = close
            {
                out.push('<');
                out.push_str(&decode_entities(&rest[4..4 + c]));
                out.push('>');
                i += 4 + c + 4;
                continue;
            }
        }
        if code == 0
            && rest.starts_with('[')
            && let Some(mid) = rest.find("](<a ")
            && mid < 300
            && !rest[1..mid].contains(['[', ']', '<', '>'])
            && let Some(tag_end) = rest[mid..].find('>')
            && let Some(close) = rest[mid..].find("</a>)")
        {
            let tag = &rest[mid + 3..mid + tag_end];
            if let Some(href) = attr(tag, "href") {
                out.push_str(&format!("<a href=\"{}\">{}</a>", href.replace('"', "&quot;"), &rest[1..mid]));
                i += mid + close + 5;
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        i += c.len_utf8();
    }
    std::borrow::Cow::Owned(out)
}

/// An href or src as an absolute URL: CurseForge's `/linkout?remoteUrl=`
/// redirects unwrapped, site-relative paths completed, anchors and
/// javascript dropped.
fn abs_url(h: &str) -> Option<String> {
    let h = h.trim();
    // Any CurseForge host's linkout (www., elerium.) wraps the real one.
    let linkout = || {
        let rest = h.strip_prefix("https://")?;
        let (host, path) = rest.split_once('/')?;
        (host == "curseforge.com" || host.ends_with(".curseforge.com")).then_some(())?;
        path.strip_prefix("linkout?remoteUrl=")
    };
    if let Some(enc) = h.strip_prefix("/linkout?remoteUrl=").or_else(linkout) {
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
    let html = unescape_tags(html);
    let mut rest = html.as_ref();
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
    p.blocks = p.blocks.into_iter().flat_map(split_images).collect();
    // A paragraph holding only a link to a video (a URL, a titled link or a
    // linked image) is that video.
    for b in &mut p.blocks {
        if let Some(v) = lone_video(b) {
            *b = Block {
                kind: Kind::Video(v),
                spans: Vec::new(),
            };
        }
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

/// The widest image that stays in the line of text around it: an icon.
const INLINE_IMAGE_MAX: u32 = 48;

/// A paragraph or heading mixing text and images, split so each run of
/// images is a paragraph of its own. A browser wraps a big image onto its
/// own line ("Ibel<img>A lady who…" in Blackey's Boss Battle); in a line of
/// text the image would shrink to the text's height. Images the page gave
/// a small width stay inline.
fn split_images(b: Block) -> Vec<Block> {
    if !matches!(b.kind, Kind::Para | Kind::Heading(_)) {
        return vec![b];
    }
    let big = |s: &Span| {
        s.image.is_some() && s.width.or(s.height).is_none_or(|w| w > INLINE_IMAGE_MAX)
    };
    let has_text = b.spans.iter().any(|s| s.image.is_none() && !s.text.trim().is_empty());
    if !has_text || !b.spans.iter().any(big) {
        return vec![b];
    }
    let mut out: Vec<Block> = Vec::new();
    // Whitespace between two images stays with them.
    let mut cur: Option<(bool, Vec<Span>)> = None;
    let push = |out: &mut Vec<Block>, (images, spans): (bool, Vec<Span>)| {
        let kind = if images { Kind::Para } else { b.kind.clone() };
        let mut block = Block { kind, spans };
        if !images {
            // Text loses the space that sat against the image.
            if let Some(f) = block.spans.first_mut() {
                f.text = f.text.trim_start().to_string();
            }
            if let Some(l) = block.spans.last_mut() {
                l.text = l.text.trim_end().to_string();
            }
        }
        if !block.is_empty() {
            out.push(block);
        }
    };
    for s in b.spans.iter().cloned() {
        let images = if big(&s) {
            true
        } else if s.image.is_none() && s.text.trim().is_empty() {
            cur.as_ref().is_some_and(|c| c.0)
        } else {
            false
        };
        match &mut cur {
            Some((kind, spans)) if *kind == images => spans.push(s),
            _ => {
                if let Some(c) = cur.take() {
                    push(&mut out, c);
                }
                cur = Some((images, vec![s]));
            }
        }
    }
    if let Some(c) = cur {
        push(&mut out, c);
    }
    out
}

/// The video a paragraph of nothing but one link to it stands for. A linked
/// image becomes the poster of a video that has none.
fn lone_video(b: &Block) -> Option<Video> {
    if b.kind != Kind::Para {
        return None;
    }
    let mut spans = b
        .spans
        .iter()
        .filter(|s| s.image.is_some() || !s.text.trim().is_empty());
    let s = spans.next()?;
    if spans.next().is_some() {
        return None;
    }
    let target = match &s.link {
        Some(l) => l.clone(),
        None if s.image.is_none() => match bare_urls(s.text.trim()).as_slice() {
            [(u, true)] => u.to_string(),
            _ => return None,
        },
        None => return None,
    };
    let mut v = video_of(&target)?;
    if v.thumb.is_none() {
        v.thumb = s.image.clone();
    }
    Some(v)
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
            Kind::Video(Video { thumb: Some(t), .. }) => add(&[Span {
                image: Some(t.clone()),
                ..Default::default()
            }]),
            _ => add(&b.spans),
        }
    }
    out
}

/// A CurseForge changelog. It is HTML, but many authors type Markdown into
/// the box, which CurseForge wraps in paragraphs untouched: `# Changelog`,
/// `**bold**`, `` `code` `` and `- item` would show as typed. When the text
/// reads as Markdown, it is parsed as Markdown.
///
/// Two more shapes of the same thing are read too: the whole text inside
/// `<pre>` / `<code>` (it would show monospace), and Markdown whose line
/// breaks were lost on the way (see [`restore_breaks`]).
pub fn parse_changelog(html: &str) -> Vec<Block> {
    let blocks = parse(html);
    if let Some(text) = all_code_text(&blocks)
        && has_markdown(&text)
    {
        return parse_markdown(&restore_breaks(&text));
    }
    if looks_like_markdown(&blocks) {
        return parse_markdown(&restore_breaks(&html_to_lines(html)));
    }
    blocks
}

/// Markdown release notes (Modrinth, GitHub) as blocks, with lost line
/// breaks restored first.
pub fn parse_markdown_changelog(md: &str) -> Vec<Block> {
    parse_markdown(&restore_breaks(md))
}

/// The text of a changelog that is nothing but code (a `<pre>`, or
/// paragraphs of inline code), one line per block: someone's Markdown the
/// page shows in monospace. `None` when anything else is in it.
fn all_code_text(blocks: &[Block]) -> Option<String> {
    let mut out = Vec::new();
    for b in blocks {
        match &b.kind {
            Kind::Code => out.push(b.spans.iter().map(|s| s.text.as_str()).collect::<String>()),
            Kind::Para if b.spans.iter().all(|s| s.code && s.image.is_none()) => {
                out.push(b.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            }
            _ => return None,
        }
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// Whether `text` is Markdown rather than, say, a config snippet: it has a
/// `# Heading` (at the start, on a line, or run into the line) or two or
/// more `**bold**` pairs.
fn has_markdown(text: &str) -> bool {
    let t = text.trim_start();
    is_heading_start(t)
        || t.lines().any(|l| is_heading_start(l.trim_start()))
        || heading_marks(t).next().is_some()
        || t.matches("**").count() >= 4
}

/// `s` begins with `#` to `######` and a space.
fn is_heading_start(s: &str) -> bool {
    let n = s.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&n) && s[n..].starts_with(' ')
}

/// Byte offsets of heading marks (`## `) that follow a space inside a line.
fn heading_marks(s: &str) -> impl Iterator<Item = usize> + '_ {
    s.match_indices(" #").filter_map(|(i, _)| {
        let rest = &s[i + 1..];
        is_heading_start(rest).then_some(i + 1)
    })
}

/// Markdown that lost its line breaks (they became spaces somewhere on the
/// way: `# Changelog  ## [5.1.0]  ### Added - Boulders ...`) with them put
/// back: before a heading mark, before `- ` followed by a capital letter or
/// a link, and at runs of two or more spaces. Text that still has line
/// breaks is returned as it is.
pub fn restore_breaks(md: &str) -> String {
    if md.trim().contains('\n') || !has_markdown(md) {
        return md.to_string();
    }
    let mut out = String::with_capacity(md.len() + 32);
    let marks: Vec<usize> = heading_marks(md).collect();
    let b = md.as_bytes();
    let mut i = 0;
    while i < md.len() {
        let c = md[i..].chars().next().unwrap();
        if marks.contains(&i) {
            // The space before the mark is already out.
            out.truncate(out.trim_end().len());
            out.push_str("\n\n");
        } else if c == ' ' {
            let run = b[i..].iter().take_while(|&&c| c == b' ').count();
            let after = &md[i + run..];
            let bullet = after.starts_with("- ")
                && after[2..].chars().next().is_some_and(|n| n.is_uppercase() || n == '[' || n == '`');
            if run >= 2 {
                out.truncate(out.trim_end().len());
                out.push_str("\n\n");
                i += run;
                continue;
            } else if bullet {
                out.truncate(out.trim_end().len());
                out.push('\n');
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Whether parsed HTML still holds Markdown as plain text: a `# Heading`
/// line when the page has no real headings, or `**bold**` / `` `code` ``
/// typed in two or more blocks.
fn looks_like_markdown(blocks: &[Block]) -> bool {
    if blocks.iter().any(|b| matches!(b.kind, Kind::Heading(_) | Kind::Code)) {
        return false;
    }
    let text = |b: &Block| -> String {
        b.spans.iter().filter(|s| s.image.is_none() && !s.code).map(|s| s.text.as_str()).collect()
    };
    let mut marked = 0;
    for b in blocks.iter().filter(|b| matches!(b.kind, Kind::Para | Kind::Bullet(_) | Kind::Numbered(_))) {
        let t = text(b);
        let t = t.trim_start();
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            return true;
        }
        let pairs = |m: &str| t.matches(m).count() >= 2;
        if pairs("**") || pairs("`") {
            marked += 1;
        }
    }
    marked >= 2
}

/// `html` as text lines for [`parse_markdown`]: block ends and `<br>` become
/// line breaks, links and images stay tags, other tags go, entities decode.
fn html_to_lines(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..i]));
        rest = &rest[i..];
        let end = rest.find('>').map_or(rest.len(), |e| e + 1);
        let tag = &rest[..end];
        let name: String = tag
            .trim_start_matches('<')
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match name.as_str() {
            "a" | "img" => out.push_str(tag),
            "br" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "ul" | "ol" => {
                if tag.starts_with("</") || name == "br" {
                    out.push('\n');
                }
            }
            _ => {}
        }
        rest = &rest[end..];
    }
    out.push_str(&decode_entities(rest));
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
        // Raw HTML passes through whole: a `*` or `_` in an address is not
        // emphasis.
        if rest.starts_with('<')
            && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic() || c == '/')
            && let Some(end) = rest.find('>')
        {
            out.push_str(&rest[..=end]);
            i += end + 1;
            continue;
        }
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

/// Latin letters with a mark, like `eacute` (é) or `Ouml` (Ö): pack pages
/// write "Pok&eacute;mon".
fn accented(ent: &str) -> Option<char> {
    const MARKS: [(&str, &str, &str); 7] = [
        ("acute", "aeiouyAEIOUY", "áéíóúýÁÉÍÓÚÝ"),
        ("grave", "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        ("circ", "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        ("uml", "aeiouyAEIOU", "äëïöüÿÄËÏÖÜ"),
        ("tilde", "anoANO", "ãñõÃÑÕ"),
        ("cedil", "cC", "çÇ"),
        ("ring", "aA", "åÅ"),
    ];
    let mut cs = ent.chars();
    let letter = cs.next()?;
    let mark = cs.as_str();
    let (_, from, to) = MARKS.iter().find(|(m, _, _)| *m == mark)?;
    let i = from.chars().position(|c| c == letter)?;
    to.chars().nth(i)
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
            "deg" => Some('°'),
            "times" => Some('×'),
            "szlig" => Some('ß'),
            _ if accented(ent).is_some() => accented(ent),
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
    /// The height the page gave it, in pixels.
    pub height: Option<u32>,
}

/// A piece of a description: Markdown, a paragraph made only of images
/// (badge rows), which Markdown views stack instead of lining up, or a video.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    Markdown(String),
    Badges(Vec<Badge>),
    /// A table whose cells hold only images (a screenshot grid): rows of
    /// cells of images. The view shares the width between the cells and
    /// scales each image to its cell, wrapping when they would get tiny.
    Grid(Vec<Vec<Vec<Badge>>>),
    Video(Video),
}

/// The images of one table cell when it holds nothing else but whitespace.
fn image_cell(spans: &[Span]) -> Option<Vec<Badge>> {
    let mut out = Vec::new();
    for s in spans {
        match &s.image {
            Some(url) => out.push(Badge {
                key: s.image_key()?,
                url: url.clone(),
                link: s.link.clone(),
                alt: s.text.clone(),
                width: s.width,
                height: s.height,
            }),
            None if s.text.trim().is_empty() => {}
            None => return None,
        }
    }
    Some(out)
}

/// The cells of a table row made only of images (empty cells allowed, at
/// least one image).
fn image_row(b: &Block) -> Option<Vec<Vec<Badge>>> {
    let Kind::Row(cells) = &b.kind else { return None };
    let row: Vec<Vec<Badge>> = cells.iter().map(|c| image_cell(c)).collect::<Option<_>>()?;
    row.iter().any(|c| !c.is_empty()).then_some(row)
}

/// The badges of a paragraph holding two or more images and nothing else
/// but whitespace, or one image the page sized by its height only (a
/// Markdown view can't size it that way).
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
                height: s.height,
            }),
            None if s.text.trim().is_empty() => {}
            None => return None,
        }
    }
    let by_height = matches!(out.as_slice(), [b] if b.width.is_none() && b.height.is_some());
    (out.len() >= 2 || by_height).then_some(out)
}

/// Blocks as [`Segment`]s: rows of badges and videos on their own,
/// everything between them as [`to_markdown`] makes it.
pub fn to_segments(blocks: &[Block]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut start = 0;
    let flush = |out: &mut Vec<Segment>, run: &[Block]| {
        let md = to_markdown(run);
        if !md.is_empty() {
            out.push(Segment::Markdown(md));
        }
    };
    let mut i = 0;
    while i < blocks.len() {
        let b = &blocks[i];
        let (seg, used) = match &b.kind {
            Kind::Video(v) => (Some(Segment::Video(v.clone())), 1),
            Kind::Row(_) => {
                // Consecutive rows of images are one grid.
                let rows: Vec<Vec<Vec<Badge>>> = blocks[i..].iter().map_while(image_row).collect();
                let n = rows.len();
                (Some(Segment::Grid(rows)).filter(|_| n > 0), n.max(1))
            }
            _ => (badge_row(b).map(Segment::Badges), 1),
        };
        if let Some(seg) = seg {
            flush(&mut out, &blocks[start..i]);
            out.push(seg);
            start = i + used;
        }
        i += used;
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

/// Words that mark a pack author's advice worth seeing before updating.
const ADVICE: [&str; 7] = [
    "fresh install",
    "backup",
    "back up",
    "new world",
    "world reset",
    "reset your world",
    "not compatible with existing worlds",
];

/// Lines of changelogs (as Markdown) with advice on how to update (a fresh
/// install, backing up worlds), as plain text, at most four. Hopper
/// desktop and the web panel show them above the pack's changelog.
pub fn advice<'a>(changelogs: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for md in changelogs {
        for line in md.lines() {
            let t = plain_line(line);
            let low = t.to_lowercase();
            if t.is_empty() || t.len() > 220 || !ADVICE.iter().any(|w| low.contains(w)) {
                continue;
            }
            // The same advice again, or inside a longer line: once.
            if let Some(i) = out.iter().position(|o| {
                let o = o.to_lowercase();
                low.contains(&o) || o.contains(&low)
            }) {
                if t.len() < out[i].len() {
                    out[i] = t;
                }
                continue;
            }
            out.push(t);
            if out.len() == 4 {
                return out;
            }
        }
    }
    out
}

/// A markdown line as plain text: marks out, "[x](url)" read as "x".
fn plain_line(line: &str) -> String {
    let mut t = String::new();
    let mut depth = 0usize;
    let mut prev = ' ';
    for c in line.chars() {
        if depth > 0 {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
        } else if c == '(' && prev == ']' {
            depth = 1;
        } else if !matches!(c, '*' | '_' | '#' | '>' | '[' | ']' | '`' | '\\') {
            t.push(c);
        }
        prev = c;
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ").trim_start_matches(['-', '•', ' ']).trim().to_string()
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
            Kind::Video(v) => {
                // The poster linking to the video, then a link that says so.
                if out.ends_with('\n') && !out.ends_with("\n\n") {
                    out.push('\n');
                }
                if let Some(t) = &v.thumb {
                    out.push_str(&format!("[![Video]({})]({})\n\n", url(t), url(&v.url)));
                }
                let label = match v.site() {
                    "video" => "▶ Watch the video".to_string(),
                    s => format!("▶ Watch on {s}"),
                };
                out.push_str(&format!("[{label}]({})\n\n", url(&v.url)));
            }
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

    /// Trimmed from Snappy Blocks' description: a 2x2 table of screenshots.
    #[test]
    fn link_icon_table_is_one_grid_of_small_unsized_images() {
        // Disrobe Sounds: an avatar and six pixel-art link icons, none with
        // a size, in a table of flex divs. The view shows them at their own
        // size, side by side; the parse must keep them apart from the text.
        let html = include_str!("richtext_fixtures/disrobe-sounds.html");
        let segs = to_segments(&parse(html));
        let Some(Segment::Grid(rows)) = segs.first() else { panic!("{:?}", segs.first()) };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0].len(), 1, "the avatar is its own cell");
        let icons = &rows[0][1];
        assert_eq!(icons.len(), 6);
        assert!(icons.iter().all(|b| b.width.is_none() && b.height.is_none() && b.link.is_some()));
        assert!(icons[0].link.as_deref().unwrap().contains("github.com"));
        // The banner below keeps its page-given "100%" (no pixel width), so
        // the view treats it as a hero.
        assert!(segs.iter().skip(1).any(|s| matches!(s, Segment::Markdown(m) if m.contains("banner.png"))));
    }

    #[test]
    fn image_table_is_one_grid() {
        let html = r#"<p>Intro</p><table><tbody>
<tr><td><img src="https://media.forgecdn.net/a.png" alt="a"></td><td><img src="https://media.forgecdn.net/b.png" alt="b"></td></tr>
<tr><td><img src="https://media.forgecdn.net/c.png" alt="c"></td><td><img src="https://media.forgecdn.net/d.png" alt="d"></td></tr>
</tbody></table><p>End</p>"#;
        let segs = to_segments(&parse(html));
        assert_eq!(segs.len(), 3, "{segs:?}");
        let Segment::Grid(rows) = &segs[1] else { panic!("{segs:?}") };
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.len() == 2 && r.iter().all(|c| c.len() == 1)));
        assert_eq!(rows[1][0][0].url, "https://media.forgecdn.net/c.png");
    }

    #[test]
    fn table_with_text_stays_markdown() {
        let html = "<table><tr><td>Name</td><td><img src=\"https://x/a.png\"></td></tr></table>";
        let segs = to_segments(&parse(html));
        assert!(matches!(segs.as_slice(), [Segment::Markdown(_)]), "{segs:?}");
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
<p>Text with <img src="https://cdn/i.png" width="16"> inline</p>"#,
        );
        assert_eq!(b[0].kind, Kind::Para);
        assert_eq!(b[0].spans[0].image.as_deref(), Some("https://cdn/x.png"));
        assert_eq!(b[0].spans[0].text, "Banner");
        assert_eq!(b[1].spans[0].link.as_deref(), Some("https://patreon.com/me"));
        assert_eq!(
            b[2].kind,
            Kind::Video(Video {
                url: "https://www.youtube.com/watch?v=zjnh7Rygd4M".into(),
                embed: Some("https://www.youtube.com/embed/zjnh7Rygd4M".into()),
                thumb: Some("https://img.youtube.com/vi/zjnh7Rygd4M/hqdefault.jpg".into()),
            })
        );
        assert_eq!(b[3].spans.len(), 3, "{:?}", b[3]);
        assert_eq!(b[3].spans[1].text, "image");
        assert_eq!(b[0].spans[0].width, Some(1315));
        assert_eq!(
            images(&b),
            vec![
                "https://cdn/x.png#w=1315",
                "https://i.imgur.com/p.png",
                "https://img.youtube.com/vi/zjnh7Rygd4M/hqdefault.jpg",
                "https://cdn/i.png#w=16"
            ]
        );
        let md = to_markdown(&b);
        assert!(md.contains("[![Banner](<https://cdn/x.png#w=1315>)](<https://cdn/x.png>)"), "{md}");
        assert!(md.contains("[![Patreon](<https://i.imgur.com/p.png>)](<https://patreon.com/me>)"), "{md}");
        assert!(md.contains("[![Video](<https://img.youtube.com/vi/zjnh7Rygd4M/hqdefault.jpg>)](<https://www.youtube.com/watch?v=zjnh7Rygd4M>)\n\n[▶ Watch on YouTube](<https://www.youtube.com/watch?v=zjnh7Rygd4M>)"), "{md}");
    }

    #[test]
    fn big_images_in_text_get_their_own_line() {
        // Blackey's Boss Battle: a bold name, the boss's picture, its blurb.
        let b = parse(
            r#"<div class="spoiler"><hr><strong>Ibel<img src="https://cdn/a.png"></strong>A lady who sips tea.<br><hr><p><strong>Blastray &amp; Dashblaze<img src="https://cdn/b.png"> <img src="https://cdn/c.png"></strong>Siblings.</p><h3>Head Chef <img src="https://cdn/d.png"></h3></div>"#,
        );
        let kinds: Vec<(&Kind, String, usize)> = b
            .iter()
            .map(|x| (&x.kind, text(x), x.spans.iter().filter(|s| s.image.is_some()).count()))
            .collect();
        assert_eq!(
            kinds,
            [
                (&Kind::Para, "Ibel".into(), 0),
                (&Kind::Para, "image".into(), 1),
                (&Kind::Para, "A lady who sips tea.".into(), 0),
                (&Kind::Rule, String::new(), 0),
                (&Kind::Para, "Blastray & Dashblaze".into(), 0),
                (&Kind::Para, "image image".into(), 2),
                (&Kind::Para, "Siblings.".into(), 0),
                (&Kind::Heading(3), "Head Chef".into(), 0),
                (&Kind::Para, "image".into(), 1),
            ]
        );
        assert!(b[0].spans[0].bold);
        // The pair sits side by side.
        assert!(matches!(&to_segments(&b)[..], [_, Segment::Badges(x), ..] if x.len() == 2));
    }

    #[test]
    fn video_urls() {
        let yt = |u| video_of(u).map(|v| v.url);
        let watch = Some("https://www.youtube.com/watch?v=abc_-1".to_string());
        assert_eq!(yt("https://youtu.be/abc_-1?t=3"), watch);
        assert_eq!(yt("https://m.youtube.com/watch?feature=x&v=abc_-1"), watch);
        assert_eq!(yt("https://www.youtube.com/shorts/abc_-1"), watch);
        assert_eq!(yt("//www.youtube-nocookie.com/embed/abc_-1?rel=0"), watch);
        assert_eq!(yt("https://www.youtube.com/channel/abc"), None);
        let v = video_of("https://player.vimeo.com/video/76979871?h=8272103f6e&title=0").unwrap();
        assert_eq!(v.url, "https://vimeo.com/76979871/8272103f6e");
        assert_eq!(v.embed.as_deref(), Some("https://player.vimeo.com/video/76979871?h=8272103f6e"));
        assert_eq!(v.site(), "Vimeo");
        assert_eq!(video_of("https://vimeo.com/76979871").unwrap().embed.as_deref(), Some("https://player.vimeo.com/video/76979871"));
        assert_eq!(video_of("https://vimeo.com/about"), None);
        let s = video_of("https://streamable.com/e/xyz12").unwrap();
        assert_eq!((s.url.as_str(), s.embed.as_deref()), ("https://streamable.com/xyz12", Some("https://streamable.com/e/xyz12")));
        let f = video_of("https://cdn.x/clip.WebM?v=2").unwrap();
        assert_eq!((f.url.as_str(), f.embed, f.thumb), ("https://cdn.x/clip.WebM?v=2", None, None));
        assert_eq!(video_of("https://cdn.x/clip.mp4.png"), None);
    }

    #[test]
    fn videos_become_their_own_segments() {
        let b = parse(
            r#"<p>Intro</p>
<iframe src="https://player.vimeo.com/video/123" width="640"></iframe>
<p><iframe src="https://streamable.com/e/abc"></iframe></p>
<p><a href="https://youtu.be/QQ">Trailer</a></p>
<p>See <a href="https://youtu.be/QQ">this</a> too</p>
<p><a href="https://vimeo.com/9"><img src="https://img/poster.png"></a></p>
<p>https://cdn.x/a.mp4</p>
<video controls poster="https://img/p.jpg"><source src="https://cdn.x/b.webm"><source src="https://cdn.x/b.mp4"></video>
<iframe src="https://example.com/widget"></iframe><p>End</p>"#,
        );
        let segs = to_segments(&b);
        let videos: Vec<&Video> = segs
            .iter()
            .filter_map(|s| match s {
                Segment::Video(v) => Some(v),
                _ => None,
            })
            .collect();
        let urls: Vec<&str> = videos.iter().map(|v| v.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://vimeo.com/123",
                "https://streamable.com/abc",
                "https://www.youtube.com/watch?v=QQ",
                "https://vimeo.com/9",
                "https://cdn.x/a.mp4",
                "https://cdn.x/b.webm"
            ]
        );
        // A linked image is the poster of a video without one.
        assert_eq!(videos[3].thumb.as_deref(), Some("https://img/poster.png"));
        assert_eq!(videos[5].thumb.as_deref(), Some("https://img/p.jpg"));
        // A video link inside a sentence stays a link; other iframes vanish.
        let text: String = segs
            .iter()
            .filter_map(|s| match s {
                Segment::Markdown(m) => Some(m.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("|");
        assert_eq!(text, "Intro|See [this](<https://youtu.be/QQ>) too|End");
        // Markdown descriptions: a lone link on its own line.
        let m = parse_markdown("Look:\n\nhttps://www.youtube.com/watch?v=QQ\n\n[![x](https://img/t.png)](https://streamable.com/zz)");
        assert!(matches!(&m[1].kind, Kind::Video(v) if v.url.ends_with("v=QQ")), "{m:?}");
        assert!(matches!(&m[2].kind, Kind::Video(v) if v.thumb.as_deref() == Some("https://img/t.png")), "{m:?}");
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
    fn changelog_markdown_in_html_is_parsed() {
        let b = parse_changelog(
            "<p># Changelog - 3.0.0</p><p>## Summary</p><p>Fixed **crash** and `code`</p><p>- one</p><p>- two &amp; three</p>",
        );
        assert_eq!(b[0].kind, Kind::Heading(1));
        assert_eq!(b[1].kind, Kind::Heading(2));
        assert!(b[2].spans.iter().any(|s| s.bold && s.text == "crash"));
        assert!(b[2].spans.iter().any(|s| s.code && s.text == "code"));
        assert_eq!(b[3].kind, Kind::Bullet(0));
        assert_eq!(b[4].spans.iter().map(|s| s.text.as_str()).collect::<String>(), "two & three");
        // The same typed with line breaks in one paragraph.
        let b = parse_changelog("# Title<br>- a<br>- b");
        assert_eq!(b[0].kind, Kind::Heading(1));
        assert_eq!(b.len(), 3);
    }

    const FLAT: &str = "# Changelog  All notable changes to this project. ## [5.1.0] - 2025-03-01  ### Added - Boulders for modded ores ([#28](https://github.com/x/y/pull/28)) - Config toggles for each ore ### Fixed - Crash on **world** load ([#30](https://github.com/x/y/pull/30))";

    fn flat_checks(b: &[Block]) {
        let kinds: Vec<_> = b.iter().map(|b| b.kind.clone()).collect();
        assert_eq!(kinds[0], Kind::Heading(1));
        assert_eq!(kinds[1], Kind::Para);
        assert_eq!(kinds[2], Kind::Heading(2));
        assert_eq!(kinds[3], Kind::Heading(3));
        assert_eq!(kinds[4], Kind::Bullet(0));
        assert_eq!(kinds[5], Kind::Bullet(0));
        assert_eq!(kinds[6], Kind::Heading(3));
        assert_eq!(kinds[7], Kind::Bullet(0));
        assert!(b[4].spans.iter().any(|s| s.link.as_deref() == Some("https://github.com/x/y/pull/28") && s.text == "#28"));
        assert!(b[7].spans.iter().any(|s| s.bold && s.text == "world"));
        assert!(b.iter().all(|b| b.spans.iter().all(|s| !s.code)), "nothing monospace");
    }

    #[test]
    fn changelog_with_lost_line_breaks_comes_back() {
        // As plain paragraph text, inside <pre>, inside <code>, and as Markdown.
        flat_checks(&parse_changelog(&format!("<p>{FLAT}</p>")));
        flat_checks(&parse_changelog(&format!("<pre>{FLAT}</pre>")));
        flat_checks(&parse_changelog(&format!("<p><code>{FLAT}</code></p>")));
        flat_checks(&parse_markdown_changelog(FLAT));
    }

    #[test]
    fn changelog_markdown_in_code_keeps_its_lines() {
        let md = "# Changelog\n\n## [5.1.0]\n\n### Added\n- Boulders\n- Toggles\n";
        for html in [format!("<pre>{md}</pre>"), format!("<pre><code>{md}</code></pre>")] {
            let b = parse_changelog(&html);
            assert_eq!(b[0].kind, Kind::Heading(1), "{html}");
            assert_eq!(b.iter().filter(|b| b.kind == Kind::Bullet(0)).count(), 2);
        }
        // Text with line breaks is never re-split.
        assert_eq!(restore_breaks(md), md);
        let spaced = "## A\n- Fixed a thing - Another thing";
        assert_eq!(restore_breaks(spaced), spaced);
    }

    #[test]
    fn real_code_blocks_stay_code() {
        let b = parse_changelog("<pre>fn main() {\n    println!(\"hi\");\n}</pre>");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].kind, Kind::Code);
        let b = parse_changelog("<p>Set <code>max = 3</code> in the config</p>");
        assert_eq!(b[0].kind, Kind::Para);
        // "C#" and a lone hyphen in prose are not marks.
        assert_eq!(restore_breaks("Fixed the C# bridge - Mostly"), "Fixed the C# bridge - Mostly");
    }

    #[test]
    fn changelog_real_html_and_plain_text_unchanged() {
        let html = "<h2>Fixes</h2><ul><li>a <b>b</b></li></ul>";
        assert_eq!(parse_changelog(html), parse(html));
        let plain = "<p>Fixed the C# bridge</p><p>Used `x` once</p>";
        assert_eq!(parse_changelog(plain), parse(plain));
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
        // Text between big images puts each on its own line.
        assert_eq!(text(&b[1]), "and");
        assert_eq!(b[2].spans[0].image.as_deref(), Some("https://img/s.png"));
        assert_eq!(b[3].kind, Kind::Numbered(0));
        assert_eq!(b[5].kind, Kind::Code);
        assert_eq!(text(&b[5]), "let x = 1;\n");
        let Kind::Row(r) = &b[7].kind else { panic!("{:?}", b[7]) };
        assert_eq!(r[0][0].text, "a|b");
        assert!(r[0][0].code);
        assert_eq!(b[8].spans[0].image.as_deref(), Some("https://img/h.png"));
        assert_eq!(b[8].spans[0].width, Some(200));
    }

    #[test]
    fn entities() {
        assert_eq!(
            decode_entities("a &amp; b &#8217; &#x2014; &bogus"),
            "a & b ’ — &bogus"
        );
        assert_eq!(decode_entities("Pok&eacute;mon &Ouml; &ntilde; &ccedil; &xacute;"), "Pokémon Ö ñ ç &xacute;");
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

    #[test]
    fn shortcodes_become_emoji_outside_code() {
        let b = parse("<h2>:sparkles: New Features</h2><p><code>:bug:</code> :bug: fixed</p>");
        assert_eq!(b[0].spans[0].text, "✨ New Features");
        let p: Vec<&str> = b[1].spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(p, [":bug:", " 🐛 fixed"]);
        assert!(to_markdown(&parse_markdown("## :wrench: Chores")).contains("🔧 Chores"));
    }

    /// Enchantment Outlines on CurseForge: the author's HTML shows escaped
    /// in its Markdown, with badge widths like "150.775" and "750/".
    #[test]
    fn escaped_html_reads_as_tags() {
        let html = concat!(
            "&lt;center&gt;<a href=\"https://www.curseforge.com/minecraft/texture-packs/x\"> <img src=\"https://a/1.png\" width=\"150.775\"> </a>",
            " <a href=\"/linkout?remoteUrl=https%253a%252f%252fmodrinth.com%252fmod%252fsodium\"> <img src=\"https://a/2.png\" width=\"207.2\"> </a>&lt;/center&gt;\n",
            "<p><img src=\"https://a/3.png\" width=\"750/\"></p>\n",
            "<p>&lt;details&gt; &lt;summary&gt;Accessibility&lt;/summary&gt; [Sodium Compatible](<a href=\"/linkout?remoteUrl=https%253a%252f%252fmodrinth.com%252fmod%252fsodium\" rel=\"nofollow\">https://modrinth.com/mod/sodium</a>) more</p>",
            "<p><code>&lt;center&gt;</code> stays</p>",
        );
        let b = parse(html);
        // All but the code line at the end.
        let all = to_markdown(&b[..b.len() - 1]);
        assert!(!all.contains("center"), "{all}");
        assert!(!all.contains("details"), "{all}");
        let badges = match &to_segments(&b)[0] {
            Segment::Badges(b) => b.clone(),
            s => panic!("{s:?}"),
        };
        assert_eq!(badges.iter().map(|b| b.width).collect::<Vec<_>>(), [Some(151), Some(207)]);
        assert_eq!(badges[1].link.as_deref(), Some("https://modrinth.com/mod/sodium"));
        assert!(b.iter().any(|b| b.spans.iter().any(|s| s.width == Some(750))));
        // The leftover Markdown link is the link, named as the author wrote.
        let link = b.iter().flat_map(|b| &b.spans).find(|s| s.text == "Sodium Compatible").unwrap();
        assert_eq!(link.link.as_deref(), Some("https://modrinth.com/mod/sodium"));
        assert!(all.contains("Accessibility"));
        // Code keeps what it says.
        assert!(b.iter().flat_map(|b| &b.spans).any(|s| s.code && s.text == "<center>"));
    }

    #[test]
    fn sizes_read_from_attributes() {
        assert_eq!(px_size("48"), Some(48));
        assert_eq!(px_size("48px"), Some(48));
        assert_eq!(px_size("150.775"), Some(151));
        assert_eq!(px_size("750/"), Some(750));
        assert_eq!(px_size("50%"), None);
        assert_eq!(px_size("0"), None);
        assert_eq!(style_px("border:0; width: 120px", "width"), Some(120));
    }

    /// Glowing Glints on Modrinth: badges sized by height only, in HTML
    /// inside Markdown.
    #[test]
    fn height_only_badges() {
        let md = "<p align=\"center\">\n  <a href=\"https://modrinth.com/mod/sodium\">\n    <img src=\"https://a/s_b*c.png\" height=\"48\">\n  </a>\n  <a href=\"https://modrinth.com/resourcepack/x\">\n    <img src=\"https://a/m.png\" height=\"48\">\n  </a>\n</p>\n\n<p align=\"center\">\n  <img src=\"https://a/one.png\" height=\"64\"/>\n</p>\n\n<details>\n<summary>Credits</summary>\n\n- Vanilla Tweaks: https://vanillatweaks.net/\n\n</details>";
        let b = parse_markdown(md);
        let segs = to_segments(&b);
        let Segment::Badges(row) = &segs[0] else { panic!("{segs:?}") };
        assert_eq!(row.len(), 2);
        assert_eq!(row[0].height, Some(48));
        // An address's `*` or `_` isn't emphasis.
        assert_eq!(row[0].url, "https://a/s_b*c.png");
        assert_eq!(row[0].link.as_deref(), Some("https://modrinth.com/mod/sodium"));
        let Segment::Badges(one) = &segs[1] else { panic!("{segs:?}") };
        assert_eq!(one[0].height, Some(64));
        let rest = to_markdown(&b);
        assert!(rest.contains("**Credits**") && !rest.contains("details"), "{rest}");
    }

    #[test]
    fn removed_embed_is_its_video() {
        let b = parse(concat!(
            "<p><span><span class=\"embed-removed\"> Embed Removed: <a href=\"https://elerium.curseforge.com/linkout?remoteUrl=",
            "https%3a%2f%2fwww.youtube-nocookie.com%2fembed%2fgEnrM4tDtmM\">https://www.youtube-nocookie.com/embed/gEnrM4tDtmM</a> </span></span> after</p>"
        ));
        assert!(matches!(&b[0].kind, Kind::Video(v) if v.url == "https://www.youtube.com/watch?v=gEnrM4tDtmM"), "{b:?}");
        assert!(!to_markdown(&b).contains("Embed Removed"));
        assert!(to_markdown(&b).contains("after"));
    }
}
