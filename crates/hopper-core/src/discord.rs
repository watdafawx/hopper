//! What Hopper shows on Discord: the player's current activity turned into
//! Rich Presence fields, with the privacy rules applied. The app sends the
//! result to the Discord app on the same PC; nothing here talks to Discord.
//!
//! Everything sent is seen by every friend and every server the player is
//! in, so only public names go out: never search text, paths, addresses,
//! other players or account details.

use serde::{Deserialize, Serialize};

/// Hopper's Discord application. Public: Discord shows it in the app list.
pub const APP_ID: &str = "1567875545579126866";

/// Where "Get Hopper" goes.
pub const SITE: &str = "https://watdafawx.github.io/hopper/";

/// Discord rejects a whole update when one text is longer than this.
const MAX_TEXT: usize = 128;
/// Button labels: 1 to 32 characters.
const MAX_LABEL: usize = 32;
/// Button and image links.
const MAX_URL: usize = 512;

/// The Discord switches in Settings › Discord.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Show anything at all.
    pub enabled: bool,
    /// The pack's name and icon (off: "Playing Minecraft").
    pub show_pack: bool,
    /// What kind of thing is browsed, and the public project open.
    pub show_browsing: bool,
    /// The open server's name and player count on the Servers page.
    pub show_server: bool,
    /// A Join button, with the address typed in `share_address`.
    pub join_button: bool,
    pub share_address: String,
    /// Clear the status while Hopper is just open.
    pub hide_idle: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            enabled: true,
            show_pack: true,
            show_browsing: true,
            show_server: false,
            join_button: false,
            share_address: String::new(),
            hide_idle: false,
        }
    }
}

/// A modpack (or instance) as it may be shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pack {
    pub name: String,
    /// The pack's version ("5.2"), if it's from a site.
    pub version: String,
    /// Its icon on CurseForge or Modrinth. Local icons never go out.
    pub icon_url: Option<String>,
    /// Its page on CurseForge or Modrinth.
    pub page_url: Option<String>,
    pub mods: usize,
}

/// What kind of project Browse lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mods,
    Modpacks,
    ResourcePacks,
    Shaders,
    DataPacks,
    Worlds,
}

impl Kind {
    fn plural(self) -> &'static str {
        match self {
            Kind::Mods => "mods",
            Kind::Modpacks => "modpacks",
            Kind::ResourcePacks => "resource packs",
            Kind::Shaders => "shaders",
            Kind::DataPacks => "data packs",
            Kind::Worlds => "worlds",
        }
    }

    fn one(self) -> &'static str {
        match self {
            Kind::Mods => "Mod",
            Kind::Modpacks => "Modpack",
            Kind::ResourcePacks => "Resource pack",
            Kind::Shaders => "Shader",
            Kind::DataPacks => "Data pack",
            Kind::Worlds => "World",
        }
    }
}

fn site(modrinth: bool) -> &'static str {
    if modrinth { "Modrinth" } else { "CurseForge" }
}

/// What the player is doing in Hopper, as the app sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    /// Hopper is open, nothing in particular.
    Idle,
    Browsing { kind: Kind, modrinth: bool },
    /// A project's page is open (from CurseForge or Modrinth).
    Project { kind: Kind, modrinth: bool, name: String, version: String, icon_url: Option<String>, page_url: Option<String> },
    Installing { pack: Pack, update: bool, percent: Option<u8> },
    Playing { pack: Pack, game_version: String, loader: String },
    /// The Servers page; `open` is the server picked, with its players.
    Servers { count: usize, running: usize, open: Option<OpenServer> },
    Configs { pack: Pack },
    Crash { pack: Pack },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenServer {
    pub name: String,
    pub running: bool,
    pub online: u32,
    pub max: u32,
}

impl Doing {
    /// Changes worth sending at once instead of waiting out the limit:
    /// starting or stopping a game, or an install starting or ending.
    pub fn big_change(&self, before: &Doing) -> bool {
        let bucket = |d: &Doing| match d {
            Doing::Playing { .. } => 1,
            Doing::Installing { .. } => 2,
            _ => 0,
        };
        bucket(self) != bucket(before)
    }
}

/// The fields Discord gets. Plain data, so it can be tested and compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    pub details: String,
    pub state: Option<String>,
    /// An uploaded asset's key or an https image link.
    pub large_image: String,
    pub large_text: String,
    pub small_image: Option<String>,
    pub small_text: Option<String>,
    /// Unix seconds.
    pub start: u64,
    /// (label, link), at most two.
    pub buttons: Vec<(String, String)>,
    /// What the member list says: false = the app's name ("Playing
    /// Hopper"), true = the details ("Playing All the Mods 10").
    pub show_details: bool,
}

/// The timer: when the game started while playing, else when Hopper opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Times {
    pub app_started: u64,
    pub game_started: Option<u64>,
}

/// The activity for what the player is doing, or `None` to show nothing.
pub fn activity(doing: &Doing, prefs: &Prefs, times: Times) -> Option<Activity> {
    if !prefs.enabled {
        return None;
    }
    let hopper = || ("hopper".to_string(), "Hopper, a Minecraft launcher".to_string());
    let mut a = Activity {
        details: String::new(),
        state: None,
        large_image: hopper().0,
        large_text: hopper().1,
        small_image: None,
        small_text: None,
        start: times.app_started,
        buttons: vec![("Get Hopper".into(), SITE.into())],
        show_details: false,
    };
    let small = |a: &mut Activity, key: &str, text: &str| {
        a.small_image = Some(key.into());
        a.small_text = Some(text.into());
    };
    // A pack name, or None when hidden or unsafe to show.
    let pack_name = |p: &Pack| if prefs.show_pack { safe_text(&p.name) } else { None };
    match doing {
        Doing::Idle => {
            if prefs.hide_idle {
                return None;
            }
            a.details = "In the launcher".into();
        }
        Doing::Browsing { kind, modrinth } => {
            browsing(&mut a, *kind, *modrinth, prefs);
            small(&mut a, "browsing", "Browsing");
        }
        Doing::Project { kind, modrinth, name, version, icon_url, page_url } => {
            match safe_text(name).filter(|_| prefs.show_browsing) {
                Some(n) => {
                    a.details = format!("Looking at {n}");
                    a.state = Some(format!("{} on {}", kind.one(), site(*modrinth)));
                    if let Some(icon) = icon_url.as_deref().and_then(safe_url) {
                        a.large_image = icon;
                        a.large_text = with_version(&n, version);
                    }
                    if let Some(page) = page_url.as_deref().and_then(safe_url) {
                        a.buttons.push((format!("View on {}", site(*modrinth)), page));
                    }
                    small(&mut a, "browsing", "Browsing in Hopper");
                }
                None => {
                    browsing(&mut a, *kind, *modrinth, prefs);
                    small(&mut a, "browsing", "Browsing");
                }
            }
        }
        Doing::Installing { pack, update, percent } => {
            let verb = if *update { "Updating" } else { "Installing" };
            match pack_name(pack) {
                Some(n) => {
                    a.details = format!("{verb} {n}");
                    pack_art(&mut a, pack, &n);
                }
                None => a.details = format!("{verb} a modpack"),
            }
            // 5% steps: fewer updates, and the number still moves.
            a.state = percent.map(|p| format!("{}% done", p.min(100) / 5 * 5));
            small(&mut a, "installing", verb);
        }
        Doing::Playing { pack, game_version, loader } => {
            a.start = times.game_started.unwrap_or(times.app_started);
            match pack_name(pack) {
                Some(n) => {
                    a.details = n.clone();
                    pack_art(&mut a, pack, &n);
                    if let Some(page) = pack.page_url.as_deref().and_then(safe_url) {
                        a.buttons.push(("View pack".into(), page));
                    }
                }
                None => a.details = "Playing Minecraft".into(),
            }
            a.state = safe_text(&version_line(game_version, loader));
            a.show_details = true;
            small(&mut a, "playing", "Launched with Hopper");
        }
        Doing::Servers { count, running, open } => {
            a.large_image = "hopperd".into();
            a.large_text = "Hopper Server".into();
            small(&mut a, "server", "Managing servers");
            match open.as_ref().filter(|_| prefs.show_server).and_then(|o| safe_text(&o.name).map(|n| (o, n))) {
                Some((o, n)) => {
                    if o.running {
                        a.details = format!("Running {n}");
                        a.state = (o.max > 0).then(|| format!("{}/{} players", o.online, o.max));
                        if let Some(join) = join_link(prefs, &n) {
                            a.buttons.push((label(&format!("Join {n}"), "Join server"), join));
                        }
                    } else {
                        a.details = format!("{n} is stopped");
                    }
                }
                None => {
                    a.details = match count {
                        1 => "Managing a server".into(),
                        n => format!("Managing {n} servers"),
                    };
                    a.state = (*running > 0).then(|| format!("{running} running"));
                }
            }
        }
        Doing::Configs { pack } => {
            a.details = "Editing configs".into();
            a.state = pack_name(pack);
            small(&mut a, "configs", "Config editor");
        }
        Doing::Crash { pack } => {
            a.details = "Fixing a crash".into();
            a.state = pack_name(pack);
            small(&mut a, "crash", "Crash help");
        }
    }
    Some(fit(a))
}

fn browsing(a: &mut Activity, kind: Kind, modrinth: bool, prefs: &Prefs) {
    if prefs.show_browsing {
        a.details = format!("Browsing {}", kind.plural());
        a.state = Some(format!("on {}", site(modrinth)));
    } else {
        a.details = "Browsing".into();
    }
}

fn pack_art(a: &mut Activity, pack: &Pack, name: &str) {
    if let Some(icon) = pack.icon_url.as_deref().and_then(safe_url) {
        a.large_image = icon;
    }
    let mut t = with_version(name, &pack.version);
    if pack.mods > 0 {
        t.push_str(&format!(" · {} mods", pack.mods));
    }
    a.large_text = t;
}

fn with_version(name: &str, version: &str) -> String {
    match safe_text(version) {
        Some(v) if !name.contains(&v) => format!("{name} · {v}"),
        _ => name.to_string(),
    }
}

/// "Minecraft 1.21.1 · NeoForge" from "1.21.1" and "neoforge-21.1.77".
pub fn version_line(game_version: &str, loader: &str) -> String {
    let name = loader.split(['-', ' ']).next().unwrap_or("").to_lowercase();
    let pretty = match name.as_str() {
        "neoforge" => "NeoForge",
        "forge" => "Forge",
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        _ => "",
    };
    match (game_version.trim(), pretty) {
        ("", "") => "Minecraft".into(),
        ("", l) => format!("Minecraft · {l}"),
        (v, "") => format!("Minecraft {v}"),
        (v, l) => format!("Minecraft {v} · {l}"),
    }
}

/// The Join page on Hopper's site, with the address the player typed for
/// sharing. Only with the switch on and an address that is a host name,
/// never a bare IP.
fn join_link(prefs: &Prefs, server: &str) -> Option<String> {
    if !prefs.join_button {
        return None;
    }
    let addr = prefs.share_address.trim();
    if addr.is_empty() || looks_like_ip(addr) || !addr.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')) {
        return None;
    }
    let url = format!("{SITE}join.html?address={}&name={}", enc(addr), enc(server));
    (url.len() <= MAX_URL).then_some(url)
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Text that may be shown, or `None` when it's empty or looks private: a
/// file path, an IP address, a link or an email.
pub fn safe_text(s: &str) -> Option<String> {
    let t = s.trim();
    if t.chars().count() < 2 {
        return None;
    }
    let lower = t.to_lowercase();
    let pathlike = t.contains('\\')
        || t.contains(":/")
        || (t.len() > 2 && t.as_bytes()[1] == b':' && t.as_bytes()[0].is_ascii_alphabetic())
        || t.starts_with('/')
        || t.starts_with('~');
    let linklike = lower.contains("http") || lower.contains("www.") || t.contains('@');
    if pathlike || linklike || has_ip(t) {
        return None;
    }
    Some(t.to_string())
}

/// An https link to show as an image or open from a button. Local files,
/// plain http and anything else never go out.
pub fn safe_url(s: &str) -> Option<String> {
    let t = s.trim();
    let host = t.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    if host.is_empty() || host.contains('@') || looks_like_ip(host.split(':').next().unwrap_or(host)) || host.starts_with("localhost") {
        return None;
    }
    (t.len() <= MAX_URL && !t.chars().any(char::is_whitespace)).then(|| t.to_string())
}

fn looks_like_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok() || s.split(':').next().is_some_and(|h| h.parse::<std::net::Ipv4Addr>().is_ok())
}

fn has_ip(s: &str) -> bool {
    s.split(|c: char| !(c.is_ascii_digit() || c == '.')).any(|w| w.parse::<std::net::Ipv4Addr>().is_ok())
}

fn label(want: &str, fallback: &str) -> String {
    if want.chars().count() <= MAX_LABEL { want.to_string() } else { fallback.to_string() }
}

fn cut(s: &str) -> String {
    if s.chars().count() <= MAX_TEXT {
        return s.to_string();
    }
    let mut t: String = s.chars().take(MAX_TEXT - 1).collect();
    t.push('…');
    t
}

/// Keeps every field inside Discord's limits, so no update is rejected.
fn fit(mut a: Activity) -> Activity {
    a.details = cut(&a.details);
    a.state = a.state.map(|s| cut(&s));
    a.large_text = cut(&a.large_text);
    a.small_text = a.small_text.map(|s| cut(&s));
    a.buttons.truncate(2);
    for b in &mut a.buttons {
        b.0 = label(&b.0, "Open");
    }
    a.buttons.retain(|(_, u)| u.len() <= MAX_URL);
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Times = Times { app_started: 1000, game_started: Some(2000) };

    fn atm() -> Pack {
        Pack {
            name: "All the Mods 10".into(),
            version: "5.2".into(),
            icon_url: Some("https://media.forgecdn.net/avatars/atm10.png".into()),
            page_url: Some("https://www.curseforge.com/minecraft/modpacks/all-the-mods-10".into()),
            mods: 491,
        }
    }

    fn act(d: &Doing) -> Activity {
        activity(d, &Prefs::default(), T).unwrap()
    }

    #[test]
    fn idle_shows_launcher_and_get_hopper() {
        let a = act(&Doing::Idle);
        assert_eq!(a.details, "In the launcher");
        assert_eq!(a.state, None);
        assert_eq!(a.large_image, "hopper");
        assert_eq!(a.start, 1000);
        assert_eq!(a.buttons, vec![("Get Hopper".to_string(), SITE.to_string())]);
        assert!(!a.show_details);
    }

    #[test]
    fn off_or_hidden_idle_shows_nothing() {
        let off = Prefs { enabled: false, ..Prefs::default() };
        assert_eq!(activity(&Doing::Idle, &off, T), None);
        assert_eq!(activity(&Doing::Playing { pack: atm(), game_version: "1.21.1".into(), loader: "neoforge".into() }, &off, T), None);
        let hide = Prefs { hide_idle: true, ..Prefs::default() };
        assert_eq!(activity(&Doing::Idle, &hide, T), None);
        assert!(activity(&Doing::Browsing { kind: Kind::Mods, modrinth: true }, &hide, T).is_some());
    }

    #[test]
    fn browsing_names_kind_and_site_only() {
        let a = act(&Doing::Browsing { kind: Kind::Modpacks, modrinth: false });
        assert_eq!(a.details, "Browsing modpacks");
        assert_eq!(a.state.as_deref(), Some("on CurseForge"));
        assert_eq!(a.small_image.as_deref(), Some("browsing"));
        let quiet = Prefs { show_browsing: false, ..Prefs::default() };
        let a = activity(&Doing::Browsing { kind: Kind::Mods, modrinth: true }, &quiet, T).unwrap();
        assert_eq!(a.details, "Browsing");
        assert_eq!(a.state, None);
    }

    #[test]
    fn project_page_shows_icon_and_link() {
        let d = Doing::Project {
            kind: Kind::Mods,
            modrinth: true,
            name: "Create".into(),
            version: "6.0.6".into(),
            icon_url: Some("https://cdn.modrinth.com/data/create/icon.png".into()),
            page_url: Some("https://modrinth.com/mod/create".into()),
        };
        let a = act(&d);
        assert_eq!(a.details, "Looking at Create");
        assert_eq!(a.state.as_deref(), Some("Mod on Modrinth"));
        assert_eq!(a.large_image, "https://cdn.modrinth.com/data/create/icon.png");
        assert_eq!(a.large_text, "Create · 6.0.6");
        assert_eq!(a.buttons[1], ("View on Modrinth".to_string(), "https://modrinth.com/mod/create".to_string()));
        let quiet = Prefs { show_browsing: false, ..Prefs::default() };
        let a = activity(&d, &quiet, T).unwrap();
        assert_eq!(a.details, "Browsing");
        assert_eq!(a.large_image, "hopper");
        assert_eq!(a.buttons.len(), 1);
    }

    #[test]
    fn installing_rounds_progress_to_five() {
        let a = act(&Doing::Installing { pack: atm(), update: false, percent: Some(47) });
        assert_eq!(a.details, "Installing All the Mods 10");
        assert_eq!(a.state.as_deref(), Some("45% done"));
        assert_eq!(a.small_image.as_deref(), Some("installing"));
        assert!(a.large_image.starts_with("https://"));
        let a = act(&Doing::Installing { pack: atm(), update: true, percent: None });
        assert_eq!(a.details, "Updating All the Mods 10");
        assert_eq!(a.state, None);
    }

    #[test]
    fn playing_shows_pack_in_member_list() {
        let a = act(&Doing::Playing { pack: atm(), game_version: "1.21.1".into(), loader: "neoforge-21.1.77".into() });
        assert_eq!(a.details, "All the Mods 10");
        assert_eq!(a.state.as_deref(), Some("Minecraft 1.21.1 · NeoForge"));
        assert_eq!(a.large_text, "All the Mods 10 · 5.2 · 491 mods");
        assert_eq!(a.small_image.as_deref(), Some("playing"));
        assert_eq!(a.start, 2000);
        assert!(a.show_details);
        assert_eq!(a.buttons[1].0, "View pack");
    }

    #[test]
    fn hidden_pack_says_minecraft() {
        let p = Prefs { show_pack: false, ..Prefs::default() };
        let a = activity(&Doing::Playing { pack: atm(), game_version: "1.20.1".into(), loader: "forge".into() }, &p, T).unwrap();
        assert_eq!(a.details, "Playing Minecraft");
        assert_eq!(a.large_image, "hopper");
        assert_eq!(a.buttons.len(), 1);
        let a = activity(&Doing::Configs { pack: atm() }, &p, T).unwrap();
        assert_eq!(a.state, None);
    }

    #[test]
    fn local_icons_never_go_out() {
        let mut p = atm();
        p.icon_url = Some(r"C:\Users\mikey\curseforge\Instances\ATM10\profileImage.png".into());
        p.page_url = Some("file:///C:/x".into());
        let a = act(&Doing::Playing { pack: p, game_version: "1.21.1".into(), loader: "neoforge".into() });
        assert_eq!(a.large_image, "hopper");
        assert_eq!(a.buttons.len(), 1);
    }

    #[test]
    fn private_looking_names_are_dropped() {
        for bad in [r"C:\Users\mikey\pack", "/home/mikey/pack", "192.168.50.3", "join 10.0.0.5 now", "https://evil.example", "me@mail.com", "x"] {
            assert_eq!(safe_text(bad), None, "{bad}");
        }
        assert_eq!(safe_text(" All the Mods 10 ").as_deref(), Some("All the Mods 10"));
        assert_eq!(safe_text("Create: Above and Beyond").as_deref(), Some("Create: Above and Beyond"));
        let mut p = atm();
        p.name = r"D:\Games\secret".into();
        let a = act(&Doing::Crash { pack: p });
        assert_eq!(a.details, "Fixing a crash");
        assert_eq!(a.state, None);
    }

    #[test]
    fn urls_must_be_public_https() {
        assert!(safe_url("https://cdn.modrinth.com/a.png").is_some());
        for bad in ["http://cdn.modrinth.com/a.png", "https://192.168.50.3/a.png", "https://localhost:8080/x", "file:///C:/a.png", "C:\\a.png", "https://user@host/x", "https://a b"] {
            assert_eq!(safe_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn servers_hide_names_by_default() {
        let open = Some(OpenServer { name: "TNP-LL8".into(), running: true, online: 3, max: 20 });
        let a = act(&Doing::Servers { count: 6, running: 2, open: open.clone() });
        assert_eq!(a.details, "Managing 6 servers");
        assert_eq!(a.state.as_deref(), Some("2 running"));
        assert_eq!(a.large_image, "hopperd");
        let show = Prefs { show_server: true, ..Prefs::default() };
        let a = activity(&Doing::Servers { count: 6, running: 2, open: open.clone() }, &show, T).unwrap();
        assert_eq!(a.details, "Running TNP-LL8");
        assert_eq!(a.state.as_deref(), Some("3/20 players"));
        assert_eq!(a.buttons.len(), 1, "no Join without the switch");
        let a = act(&Doing::Servers { count: 1, running: 0, open: None });
        assert_eq!(a.details, "Managing a server");
        assert_eq!(a.state, None);
    }

    #[test]
    fn join_needs_switch_and_a_host_name() {
        let open = Some(OpenServer { name: "Cozy SMP".into(), running: true, online: 1, max: 10 });
        let d = Doing::Servers { count: 1, running: 1, open };
        let mut p = Prefs { show_server: true, join_button: true, share_address: "play.example.com".into(), ..Prefs::default() };
        let a = activity(&d, &p, T).unwrap();
        assert_eq!(a.buttons[1].0, "Join Cozy SMP");
        assert_eq!(a.buttons[1].1, format!("{SITE}join.html?address=play.example.com&name=Cozy%20SMP"));
        for bad in ["192.168.50.3", "192.168.50.3:25565", "", "a b", "x/../y"] {
            p.share_address = bad.into();
            assert_eq!(activity(&d, &p, T).unwrap().buttons.len(), 1, "{bad}");
        }
    }

    #[test]
    fn long_text_and_labels_fit_discord() {
        let mut p = atm();
        p.name = "A".repeat(300);
        p.page_url = None;
        let a = act(&Doing::Installing { pack: p, update: false, percent: None });
        assert_eq!(a.details.chars().count(), 128);
        assert!(a.large_text.chars().count() <= 128);
        let open = Some(OpenServer { name: "A very long server name for friends".into(), running: true, online: 0, max: 0 });
        let p = Prefs { show_server: true, join_button: true, share_address: "mc.example.com".into(), ..Prefs::default() };
        let a = activity(&Doing::Servers { count: 1, running: 1, open }, &p, T).unwrap();
        assert_eq!(a.buttons[1].0, "Join server");
        assert!(a.buttons.iter().all(|(l, _)| l.chars().count() <= 32));
    }

    #[test]
    fn version_lines() {
        assert_eq!(version_line("1.21.1", "neoforge-21.1.77"), "Minecraft 1.21.1 · NeoForge");
        assert_eq!(version_line("1.20.1", "fabric-0.16"), "Minecraft 1.20.1 · Fabric");
        assert_eq!(version_line("1.21", ""), "Minecraft 1.21");
        assert_eq!(version_line("", ""), "Minecraft");
    }

    #[test]
    fn big_changes_flush_at_once() {
        let play = Doing::Playing { pack: atm(), game_version: String::new(), loader: String::new() };
        let inst = Doing::Installing { pack: atm(), update: false, percent: Some(10) };
        let inst2 = Doing::Installing { pack: atm(), update: false, percent: Some(50) };
        assert!(play.big_change(&Doing::Idle));
        assert!(Doing::Idle.big_change(&play));
        assert!(inst.big_change(&Doing::Idle));
        assert!(!inst2.big_change(&inst));
        assert!(!Doing::Browsing { kind: Kind::Mods, modrinth: false }.big_change(&Doing::Idle));
    }
}
