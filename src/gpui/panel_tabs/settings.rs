//! A Hopper Server server's Settings tab, like the panel's: a section list
//! on the left (with "Find a setting"), one page of settings on the right.
//! server.properties fields, memory and GC flags, and the address players
//! join by wait in one save bar; start files are edited in place.

use super::{Load, Target};
use crate::store::Store;
use crate::widgets;
use crate::widgets::Clickable as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hopperd::Client;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

/// The pages: id, label, icon, and groups of server.properties keys on
/// each (a heading, keys). Same order and keys as the panel's Settings.
const PAGES: &[(&str, &str, IconName, &[(&str, &[&str])])] = &[
    (
        "general",
        "General",
        IconName::SlidersHorizontal,
        &[("Server list", &["motd"]), ("Game", &["difficulty", "gamemode", "hardcore", "pvp", "enable-command-block"])],
    ),
    (
        "players",
        "Players & access",
        IconName::Users,
        &[("Players", &["max-players", "spawn-protection", "allow-flight"]), ("Who can join", &["online-mode", "white-list"])],
    ),
    ("world", "World", IconName::Map, &[("World", &["level-name", "level-seed"]), ("Performance", &["view-distance", "simulation-distance"])]),
    ("network", "Network & address", IconName::Globe, &[("Network", &["server-port"])]),
    ("java", "Memory & Java", IconName::MemoryStick, &[]),
    ("files", "Start files", IconName::FileCode, &[]),
    ("all", "All properties", IconName::ListTree, &[]),
];

const UNITS: &[(&str, &str)] =
    &[("view-distance", "chunks"), ("simulation-distance", "chunks"), ("spawn-protection", "blocks"), ("max-players", "players")];

// ---- what the panel sends ----------------------------------------------------

#[derive(Clone)]
enum Kind {
    Bool,
    Int,
    Enum(Vec<String>),
    Text,
}

/// A server.properties key Hopper knows (from `GET .../properties` specs).
#[derive(Clone)]
struct Spec {
    key: String,
    label: String,
    kind: Kind,
    description: String,
    default: String,
}

#[derive(Clone)]
struct Preset {
    id: String,
    label: String,
    supported: bool,
}

/// `GET .../jvm`.
#[derive(Clone)]
struct Jvm {
    memory_mb: Option<u32>,
    preset: String,
    flags: Vec<String>,
    presets: Vec<Preset>,
    java: Option<String>,
    java_major: Option<u64>,
    host_mb: u64,
    files: Vec<String>,
}

/// `GET .../address`.
#[derive(Clone)]
struct Addr {
    addresses: Vec<String>,
    sleep: u32,
    port: u64,
    router_port: u64,
}

/// `GET /router`.
#[derive(Clone)]
struct Router {
    enabled: bool,
    domain: String,
    port: u64,
    listening: Option<u64>,
    error: Option<String>,
}

#[derive(Clone)]
struct Data {
    specs: Vec<Spec>,
    /// What the form started with: the file's values, else the defaults.
    orig: BTreeMap<String, String>,
    jvm: Option<Jvm>,
    addr: Option<Addr>,
    router: Option<Router>,
    /// stopped, starting, running, stopping, crashed.
    state: String,
}

impl Data {
    fn spec(&self, k: &str) -> Option<&Spec> {
        self.specs.iter().find(|s| s.key == k)
    }

    fn alive(&self) -> bool {
        matches!(self.state.as_str(), "running" | "starting" | "stopping")
    }
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_string()
}

fn parse_specs(v: &Value) -> Vec<Spec> {
    v["specs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let key = x["key"].as_str()?.to_string();
                    let kind = match x["kind"].as_str().unwrap_or_default() {
                        "bool" => Kind::Bool,
                        "int" => Kind::Int,
                        "enum" => Kind::Enum(
                            x["options"]
                                .as_array()
                                .map(|o| o.iter().filter_map(|o| o.as_str().map(String::from)).collect())
                                .unwrap_or_default(),
                        ),
                        _ => Kind::Text,
                    };
                    Some(Spec {
                        label: x["label"].as_str().unwrap_or(&key).to_string(),
                        description: s(x, "description"),
                        default: s(x, "default"),
                        key,
                        kind,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_jvm(v: &Value) -> Jvm {
    Jvm {
        memory_mb: v["memory_mb"].as_u64().map(|m| m as u32),
        preset: s(v, "preset"),
        flags: v["flags"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect()).unwrap_or_default(),
        presets: v["presets"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|p| Preset { id: s(p, "id"), label: s(p, "label"), supported: p["supported"].as_bool().unwrap_or(true) })
                    .collect()
            })
            .unwrap_or_default(),
        java: v["java"].as_str().map(String::from),
        java_major: v["java_major"].as_u64(),
        host_mb: v["host_memory_mb"].as_u64().unwrap_or(0),
        files: v["files"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect()).unwrap_or_default(),
    }
}

fn parse_addr(v: &Value) -> Addr {
    Addr {
        addresses: v["addresses"].as_array().map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).collect()).unwrap_or_default(),
        sleep: v["sleep_after_minutes"].as_u64().unwrap_or(0) as u32,
        port: v["port"].as_u64().unwrap_or(0),
        router_port: v["router_port"].as_u64().unwrap_or(0),
    }
}

fn parse_router(v: &Value) -> Router {
    let c = &v["config"];
    Router {
        enabled: c["enabled"].as_bool().unwrap_or(false),
        domain: s(c, "domain"),
        port: c["port"].as_u64().unwrap_or(0),
        listening: v["status"]["listening"].as_u64(),
        error: v["status"]["error"].as_str().map(String::from),
    }
}

/// "4 GB", "1.5 GB", "512 MB".
fn gb(mb: u64) -> String {
    if mb >= 1024 {
        if mb % 1024 == 0 { format!("{} GB", mb / 1024) } else { format!("{:.1} GB", mb as f64 / 1024.0) }
    } else {
        format!("{mb} MB")
    }
}

/// A PNG's width and height from its header.
fn png_size(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let n = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    Some((n(16), n(20)))
}

/// A start file open for editing.
struct Editor {
    path: String,
    area: Option<Entity<TextareaState>>,
    modified: Option<u64>,
    error: Option<String>,
    /// It changed on the panel since it was opened: the next save overwrites.
    force: bool,
    saving: bool,
}

pub struct SettingsTab {
    #[allow(dead_code)]
    store: Entity<Store>,
    target: Option<Target>,
    /// Bumped on every load, so a late answer for an old one is dropped.
    ticket: u64,
    data: Load<Data>,
    page: usize,
    search: Entity<InputState>,
    /// The form's values, by key.
    values: BTreeMap<String, String>,
    /// Text and number fields, by key.
    inputs: HashMap<String, Entity<InputState>>,
    memory: Option<Entity<SliderState>>,
    /// What the memory slider started at.
    mem_start: u32,
    preset: String,
    addr_inputs: Vec<Entity<InputState>>,
    sleep_on: bool,
    sleep_min: Entity<InputState>,
    /// DNS check results by address: (points here, note).
    dns: Option<HashMap<String, (bool, String)>>,
    checking: bool,
    editor: Option<Editor>,
    saving: bool,
    error: Option<String>,
    form_subs: Vec<Subscription>,
    _subs: Vec<Subscription>,
}

impl SettingsTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find a setting"));
        let sleep_min = cx.new(|cx| InputState::new(window, cx).placeholder("15"));
        let notify = |_: &mut Self, _: Entity<InputState>, e: &InputEvent, cx: &mut Context<Self>| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        };
        let subs = vec![cx.subscribe(&search, notify), cx.subscribe(&sleep_min, notify)];
        Self {
            store,
            target: None,
            ticket: 0,
            data: Load::Waiting,
            page: 0,
            search,
            values: BTreeMap::new(),
            inputs: HashMap::new(),
            memory: None,
            mem_start: 0,
            preset: String::new(),
            addr_inputs: Vec::new(),
            sleep_on: false,
            sleep_min,
            dns: None,
            checking: false,
            editor: None,
            saving: false,
            error: None,
            form_subs: Vec::new(),
            _subs: subs,
        }
    }

    /// Shows `target`'s Settings (again when it's the same server: a refresh).
    pub fn show(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() != Some(&target) {
            self.data = Load::Waiting;
            self.editor = None;
            self.page = 0;
        }
        self.target = Some(target);
        self.load(window, cx);
    }

    /// Runs `work` off the UI thread with the panel's client, then `done`
    /// with a window (fields need one to be made).
    fn work<T: Send + 'static>(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl FnOnce(&Client) -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(t) = &self.target else { return };
        super::call_in(window, cx, t, work, done);
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        self.ticket += 1;
        let ticket = self.ticket;
        let (props, jvm, addr, summary) = (t.sp("/properties"), t.sp("/jvm"), t.sp("/address"), t.sp(""));
        self.work(
            window,
            cx,
            move |c| {
                let p = c.get(&props)?;
                Ok((p, c.get(&jvm).ok(), c.get(&addr).ok(), c.get("/router").ok(), c.get(&summary).ok()))
            },
            move |this, r, window, cx| {
                if this.ticket != ticket {
                    return;
                }
                match r {
                    Ok((p, jvm, addr, router, summary)) => {
                        let specs = parse_specs(&p);
                        let mut orig: BTreeMap<String, String> = p["values"]
                            .as_object()
                            .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string())).collect())
                            .unwrap_or_default();
                        for sp in &specs {
                            orig.entry(sp.key.clone()).or_insert_with(|| sp.default.clone());
                        }
                        let data = Data {
                            specs,
                            orig,
                            jvm: jvm.as_ref().map(parse_jvm),
                            addr: addr.as_ref().map(parse_addr),
                            router: router.as_ref().map(parse_router),
                            state: summary.as_ref().map(|v| s(v, "state")).unwrap_or_default(),
                        };
                        this.data = Load::Ready(data);
                        this.error = None;
                        this.dns = None;
                        this.reset_form(window, cx);
                    }
                    Err(e) => {
                        if this.data.ready().is_none() {
                            this.data = Load::Failed(e);
                        } else {
                            this.error = Some(e);
                        }
                    }
                }
            },
        );
    }

    /// The form back to what the panel has.
    fn reset_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.data.ready() else { return };
        self.values = d.orig.clone();
        let mut subs = Vec::new();
        let mut inputs = HashMap::new();
        for k in d.orig.keys() {
            if matches!(d.spec(k).map(|s| &s.kind), Some(Kind::Bool | Kind::Enum(_))) {
                continue;
            }
            let v = d.orig.get(k).cloned().unwrap_or_default();
            let placeholder = d.spec(k).map(|s| s.default.clone()).unwrap_or_default();
            let input = cx.new(|cx| {
                let mut st = InputState::new(window, cx).placeholder(placeholder);
                st.set_value(v, window, cx);
                st
            });
            let key = k.clone();
            subs.push(cx.subscribe(&input, move |this, st, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.values.insert(key.clone(), st.read(cx).value().to_string());
                    cx.notify();
                }
            }));
            inputs.insert(k.clone(), input);
        }
        self.inputs = inputs;

        self.memory = None;
        if let Some(j) = &d.jvm {
            let host = if j.host_mb > 0 { j.host_mb } else { 65536 };
            let max_mb = host.saturating_sub(1024).min(65536).max(2048);
            let max = (max_mb as f32 / 1024.0 * 2.0).floor() / 2.0;
            let start = (j.memory_mb.unwrap_or(4096) as f32 / 1024.0).clamp(1.0, max);
            self.mem_start = (start * 1024.0).round() as u32;
            let slider = cx.new(|_| SliderState::new().min(1.).max(max).step(0.5).default_value(start));
            subs.push(cx.subscribe(&slider, |_, _, _: &SliderEvent, cx| cx.notify()));
            self.memory = Some(slider);
            self.preset = j.preset.clone();
        }

        let domain = d.router.as_ref().map(|r| r.domain.clone()).unwrap_or_default();
        let (addrs, sleep) = d.addr.as_ref().map_or((Vec::new(), 0), |a| (a.addresses.clone(), a.sleep));
        self.sleep_on = sleep > 0;
        let mins = if sleep > 0 { sleep } else { 15 }.to_string();
        self.sleep_min.update(cx, |s, cx| s.set_value(mins, window, cx));
        self.addr_inputs.clear();
        let shown: Vec<String> = if addrs.is_empty() { vec![String::new()] } else { addrs };
        for a in shown {
            let suffix = format!(".{domain}");
            let v = if !domain.is_empty() && a.ends_with(&suffix) { a[..a.len() - suffix.len()].to_string() } else { a };
            self.push_addr(v, &mut subs, window, cx);
        }
        self.form_subs = subs;
        cx.notify();
    }

    fn push_addr(&mut self, v: String, subs: &mut Vec<Subscription>, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut st = InputState::new(window, cx).placeholder("survival.example.com");
            st.set_value(v, window, cx);
            st
        });
        subs.push(cx.subscribe(&input, |_, _, e: &InputEvent, cx| {
            if matches!(e, InputEvent::Change) {
                cx.notify();
            }
        }));
        self.addr_inputs.push(input);
    }

    // ---- what changed ------------------------------------------------------------

    fn domain(&self) -> String {
        self.data.ready().and_then(|d| d.router.as_ref()).map(|r| r.domain.clone()).unwrap_or_default()
    }

    /// An address field's address: a bare name gets the domain.
    fn full_address(v: &str, domain: &str) -> String {
        let v = v.trim();
        if !domain.is_empty() && !v.is_empty() && !v.contains('.') {
            format!("{}.{domain}", v.to_lowercase())
        } else {
            v.to_string()
        }
    }

    fn addresses(&self, cx: &App) -> Vec<String> {
        let domain = self.domain();
        let mut out: Vec<String> = Vec::new();
        for i in &self.addr_inputs {
            let a = Self::full_address(&i.read(cx).value(), &domain);
            if !a.is_empty() && !out.contains(&a) {
                out.push(a);
            }
        }
        out
    }

    fn sleep_minutes(&self, cx: &App) -> u32 {
        if !self.sleep_on {
            return 0;
        }
        self.sleep_min.read(cx).value().trim().parse::<u32>().unwrap_or(1).max(1)
    }

    fn mem_mb(&self, cx: &App) -> Option<u32> {
        self.memory.as_ref().map(|m| (m.read(cx).value().start() * 1024.0).round() as u32)
    }

    fn changed_props(&self) -> Vec<String> {
        let Some(d) = self.data.ready() else { return Vec::new() };
        self.values
            .iter()
            .filter(|(k, v)| d.orig.get(*k).map(String::as_str).unwrap_or_default() != v.as_str())
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn mem_changed(&self, cx: &App) -> bool {
        self.mem_mb(cx).is_some_and(|m| m != self.mem_start)
    }

    fn preset_changed(&self) -> bool {
        self.data.ready().and_then(|d| d.jvm.as_ref()).is_some_and(|j| j.preset != self.preset)
    }

    fn addr_changed(&self, cx: &App) -> (bool, bool) {
        let Some(a) = self.data.ready().and_then(|d| d.addr.as_ref()) else { return (false, false) };
        (self.addresses(cx) != a.addresses, self.sleep_minutes(cx) != a.sleep)
    }

    /// Names of everything unsaved, for the save bar.
    fn changes(&self, cx: &App) -> Vec<String> {
        let mut out = self.changed_props();
        if self.mem_changed(cx) {
            out.push("memory".into());
        }
        if self.preset_changed() {
            out.push("GC flags".into());
        }
        let (addr, sleep) = self.addr_changed(cx);
        if addr {
            out.push("address".into());
        }
        if sleep {
            out.push("sleep".into());
        }
        out
    }

    // ---- saving -------------------------------------------------------------------

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let props: serde_json::Map<String, Value> = self
            .changed_props()
            .into_iter()
            .map(|k| {
                let v = self.values.get(&k).cloned().unwrap_or_default();
                (k, Value::String(v))
            })
            .collect();
        let mut jvm = serde_json::Map::new();
        if self.mem_changed(cx)
            && let Some(m) = self.mem_mb(cx)
        {
            jvm.insert("memory_mb".into(), json!(m));
        }
        if self.preset_changed() && self.preset != "custom" {
            jvm.insert("preset".into(), json!(self.preset));
        }
        let (a, sl) = self.addr_changed(cx);
        let addr = (a || sl).then(|| json!({ "addresses": self.addresses(cx), "sleep_after_minutes": self.sleep_minutes(cx) }));
        let (pp, jp, ap) = (t.sp("/properties"), t.sp("/jvm"), t.sp("/address"));
        self.saving = true;
        self.error = None;
        cx.notify();
        self.work(
            window,
            cx,
            move |c| {
                let mut restart = false;
                if !props.is_empty() {
                    let r = c.put(&pp, json!({ "values": props }))?;
                    restart |= r["restart_needed"].as_bool().unwrap_or(false);
                }
                if !jvm.is_empty() {
                    let r = c.put(&jp, Value::Object(jvm))?;
                    restart |= r["restart_needed"].as_bool().unwrap_or(false);
                }
                if let Some(b) = addr {
                    c.put(&ap, b)?;
                }
                Ok(restart)
            },
            |this, r, window, cx| {
                this.saving = false;
                match r {
                    Ok(restart) => {
                        window.push_notification(
                            Notification::success(if restart { "Saved. Restart the server to apply." } else { "Saved." }),
                            cx,
                        );
                        this.load(window, cx);
                    }
                    Err(e) => this.error = Some(e),
                }
            },
        );
    }

    fn move_port(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let (path, join) = (t.sp("/address/move-port"), self.data.ready().and_then(|d| d.addr.as_ref()).map_or(0, |a| a.router_port));
        self.work(
            window,
            cx,
            move |c| c.post(&path, json!({})),
            move |this, r, window, cx| match r {
                Ok(v) => {
                    let port = v["port"].as_u64().map_or_else(|| "a new port".to_string(), |p| format!("port {p}"));
                    window.push_notification(
                        Notification::success(format!("It now uses {port}. Players still join on {join}.")),
                        cx,
                    );
                    this.load(window, cx);
                }
                Err(e) => this.error = Some(e),
            },
        );
    }

    fn check_dns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.checking = true;
        cx.notify();
        self.work(
            window,
            cx,
            |c| c.post("/router/check", json!({})),
            |this, r, _, _| {
                this.checking = false;
                match r {
                    Ok(v) => {
                        this.dns = Some(
                            v["dns"]
                                .as_array()
                                .map(|a| {
                                    a.iter()
                                        .map(|d| (s(d, "name"), (d["ok"].as_bool().unwrap_or(false), s(d, "note"))))
                                        .collect()
                                })
                                .unwrap_or_default(),
                        )
                    }
                    Err(e) => this.error = Some(e),
                }
            },
        );
    }

    /// The DNS result for an address: its own, else its domain's wildcard.
    fn dns_of(&self, a: &str) -> Option<(bool, String)> {
        let dns = self.dns.as_ref()?;
        let domain = self.domain();
        dns.get(a)
            .or_else(|| (!domain.is_empty() && a.ends_with(&format!(".{domain}"))).then(|| dns.get(&format!("*.{domain}"))).flatten())
            .cloned()
    }

    fn pick_icon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Server icon (64 × 64 PNG)".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path): Option<PathBuf> = paths.into_iter().next() else { return };
            let client = t.client.clone();
            let id = t.id().to_string();
            let r = cx
                .background_spawn(async move {
                    let bytes = std::fs::read(&path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
                    let size = png_size(&bytes).ok_or_else(|| "The server icon has to be a PNG (64 × 64).".to_string())?;
                    client.upload(&id, &path, "server-icon.png").map_err(|e| format!("{e:#}"))?;
                    Ok::<_, String>(size)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                match r {
                    Ok((w, h)) => {
                        if (w, h) != (64, 64) {
                            window.push_notification(
                                Notification::warning(format!("That picture is {w} × {h}; Minecraft shows only 64 × 64 icons.")),
                                cx,
                            );
                        }
                        window.push_notification(
                            Notification::success("Server icon changed. The server list shows it after a restart."),
                            cx,
                        );
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- start files ----------------------------------------------------------------

    fn open_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        self.editor = Some(Editor { path: path.clone(), area: None, modified: None, error: None, force: false, saving: false });
        cx.notify();
        let url = t.fp(&format!("/text?path={}", super::q(&path)));
        self.work(
            window,
            cx,
            move |c| c.get(&url),
            move |this, r, window, cx| {
                let Some(ed) = this.editor.as_mut().filter(|e| e.path == path) else { return };
                match r {
                    Ok(v) => {
                        let text = s(&v, "content");
                        ed.modified = v["modified"].as_u64();
                        ed.area = Some(cx.new(|cx| {
                            let mut st = TextareaState::new(window, cx).auto_grow(6, 24);
                            st.set_value(text, window, cx);
                            st
                        }));
                    }
                    Err(e) => ed.error = Some(e),
                }
            },
        );
    }

    fn save_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let Some(ed) = self.editor.as_mut() else { return };
        let Some(area) = &ed.area else { return };
        let body = json!({
            "path": ed.path, "content": area.read(cx).value().to_string(),
            "modified": ed.modified, "force": ed.force,
        });
        ed.saving = true;
        ed.error = None;
        let url = t.fp("/text");
        cx.notify();
        self.work(
            window,
            cx,
            move |c| c.put(&url, body),
            |this, r, window, cx| {
                let Some(ed) = this.editor.as_mut() else { return };
                ed.saving = false;
                match r {
                    Ok(v) => match v["result"].as_str().unwrap_or_default() {
                        "conflict" => {
                            ed.error = Some("It changed on the panel since you opened it. Save again to overwrite it.".into());
                            ed.force = true;
                        }
                        "invalid" => ed.error = Some(s(&v, "error")),
                        _ => {
                            let name = ed.path.clone();
                            this.editor = None;
                            let running = this.data.ready().is_some_and(|d| d.alive());
                            window.push_notification(
                                Notification::success(if running {
                                    format!("Saved {name}. Restart the server to apply.")
                                } else {
                                    format!("Saved {name}.")
                                }),
                                cx,
                            );
                            this.load(window, cx);
                        }
                    },
                    Err(e) => ed.error = Some(e),
                }
            },
        );
    }

    // ---- pieces ----------------------------------------------------------------------

    /// One setting: its label, description and raw key on the left, its
    /// control on the right; tinted while it has an unsaved change.
    fn row(label: AnyElement, desc: Option<String>, key: Option<&str>, control: AnyElement, dirty: bool, cx: &App) -> Div {
        let theme = cx.theme();
        h_flex()
            .gap_4()
            .px_2()
            .py_2p5()
            .border_b_1()
            .border_color(theme.border)
            .when(dirty, |r| r.bg(theme.primary.opacity(0.08)).rounded(theme.radius))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(label)
                    .when(desc.is_some() || key.is_some(), |c| {
                        c.child(
                            h_flex()
                                .flex_wrap()
                                .gap_x_1p5()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .when_some(desc, |r, d| r.child(d))
                                .when_some(key, |r, k| r.child(div().font_family(theme.mono_font_family.clone()).child(k.to_string()))),
                        )
                    }),
            )
            .child(h_flex().flex_none().gap_2().justify_end().child(control))
    }

    fn label(text: impl Into<SharedString>) -> AnyElement {
        div().text_sm().font_medium().child(text.into()).into_any_element()
    }

    fn heading(text: &str, cx: &App) -> Div {
        div()
            .pt_5()
            .pb_1()
            .px_2()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(text.to_uppercase())
    }

    /// A dropdown of `items` (label, checked, enabled).
    fn dropdown(
        id: ElementId,
        label: String,
        items: Vec<(String, bool, bool)>,
        rem: Pixels,
        on_pick: Rc<dyn Fn(usize, &mut Window, &mut App)>,
    ) -> impl IntoElement {
        Button::new(id).outline().small().w_full().label(label).dropdown_caret(true).dropdown_menu(
            move |menu: PopupMenu, _, _| {
                let mut menu = menu.min_w(rems(12.).to_pixels(rem)).max_h(rems(24.).to_pixels(rem)).scrollable(true);
                for (ix, (l, checked, enabled)) in items.iter().enumerate() {
                    let f = on_pick.clone();
                    menu = menu.item(
                        PopupMenuItem::new(l.clone())
                            .checked(*checked)
                            .disabled(!*enabled)
                            .on_click(move |_, w, cx| f(ix, w, cx)),
                    );
                }
                menu
            },
        )
    }

    /// The field for server.properties key `k`.
    fn field(&self, k: &str, rem: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let Some(d) = self.data.ready() else { return div().into_any_element() };
        let value = self.values.get(k).cloned().unwrap_or_default();
        let dirty = d.orig.get(k).map(String::as_str).unwrap_or_default() != value;
        let theme = cx.theme().clone();
        let input = |w: Rems| match self.inputs.get(k) {
            Some(i) => div().w(w).child(Input::new(i).small()).into_any_element(),
            None => div().into_any_element(),
        };
        let Some(spec) = d.spec(k).cloned() else {
            let label = div().text_sm().font_family(theme.mono_font_family.clone()).child(k.to_string()).into_any_element();
            return Self::row(label, None, None, input(rems(20.)), dirty, cx).into_any_element();
        };
        let key = spec.key.clone();
        let control = match &spec.kind {
            Kind::Bool => Switch::new(ElementId::Name(format!("prop:{key}").into()))
                .checked(value.trim() == "true")
                .on_click(cx.listener(move |this, v: &bool, _, cx| {
                    this.values.insert(key.clone(), v.to_string());
                    cx.notify();
                }))
                .into_any_element(),
            Kind::Enum(options) => {
                let items = options.iter().map(|o| (o.clone(), *o == value, true)).collect();
                let (view, opts) = (cx.entity(), options.clone());
                div()
                    .w_48()
                    .child(Self::dropdown(
                        ElementId::Name(format!("prop:{key}").into()),
                        value.clone(),
                        items,
                        rem,
                        Rc::new(move |ix, _, cx| {
                            if let Some(o) = opts.get(ix).cloned() {
                                let key = key.clone();
                                view.update(cx, |this, cx| {
                                    this.values.insert(key, o);
                                    cx.notify();
                                });
                            }
                        }),
                    ))
                    .into_any_element()
            }
            Kind::Int => h_flex()
                .gap_2()
                .child(input(rems(7.)))
                .when_some(UNITS.iter().find(|(u, _)| *u == k).map(|(_, u)| *u), |r, u| {
                    r.child(div().w(rems(3.)).text_xs().text_color(theme.muted_foreground).child(u))
                })
                .into_any_element(),
            Kind::Text => input(rems(20.)),
        };
        Self::row(Self::label(spec.label.clone()), Some(spec.description.clone()), Some(&spec.key), control, dirty, cx)
            .into_any_element()
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let searching = !self.search.read(cx).value().trim().is_empty();
        let all = self.all_keys().len();
        v_flex()
            .w(rems(13.))
            .flex_none()
            .h_full()
            .gap_0p5()
            .px_3()
            .py_3()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .pb_2()
                    .child(Input::new(&self.search).small().prefix(Icon::new(IconName::Search).small()).cleanable(true)),
            )
            .children(PAGES.iter().enumerate().map(|(i, (id, label, icon, _))| {
                let active = !searching && i == self.page;
                let label = if *id == "all" && all > 0 { format!("{label} {all}") } else { label.to_string() };
                h_flex()
                    .id(ElementId::Name(format!("settings-page:{id}").into()))
                    .h(rems(2.))
                    .px_2p5()
                    .gap_2p5()
                    .rounded(theme.radius)
                    .text_sm()
                    .clickable(cx)
                    .when(active, |r| widgets::row_selected(r, cx).font_medium())
                    .when(!active, |r| r.hover(|s| widgets::row_hover(s, cx)))
                    .child(Icon::new(icon.clone()).small().text_color(if active { theme.primary } else { theme.muted_foreground }))
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.page = i;
                        this.search.update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    }))
            }))
    }

    /// Every key: the known ones and whatever else the file has.
    fn all_keys(&self) -> Vec<String> {
        let Some(d) = self.data.ready() else { return Vec::new() };
        let set: BTreeSet<String> = d.specs.iter().map(|s| s.key.clone()).chain(self.values.keys().cloned()).collect();
        set.into_iter().collect()
    }

    fn icon_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let control = Button::new("settings-icon")
            .outline()
            .small()
            .icon(IconName::ImageUp)
            .label("Change…")
            .on_click(cx.listener(|this, _, window, cx| this.pick_icon(window, cx)))
            .into_any_element();
        Self::row(
            Self::label("Icon"),
            Some("server-icon.png in the server folder, 64 × 64 PNG".into()),
            None,
            control,
            false,
            cx,
        )
        .into_any_element()
    }

    fn java_page(&self, d: &Data, rem: Pixels, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Some(j) = d.jvm.clone() else {
            return vec![widgets::caption("The panel didn't say how this server starts.", cx).into_any_element()];
        };
        let mut out = vec![Self::heading("Memory", cx).into_any_element()];
        if let Some(m) = &self.memory {
            let mb = self.mem_mb(cx).unwrap_or(0);
            let host = if j.host_mb > 0 { format!(" This PC has {}.", gb(j.host_mb)) } else { String::new() };
            out.push(
                Self::row(
                    Self::label("Memory"),
                    Some(format!("The server's -Xms and -Xmx. Modded servers want 6 to 10 GB.{host}")),
                    None,
                    h_flex()
                        .gap_3()
                        .child(div().w_64().child(Slider::new(m)))
                        .child(div().w(rems(4.)).text_sm().font_semibold().child(gb(mb as u64)))
                        .into_any_element(),
                    self.mem_changed(cx),
                    cx,
                )
                .into_any_element(),
            );
        }
        out.push(Self::heading("Java", cx).into_any_element());
        let java = j.java.clone().unwrap_or_else(|| "java on PATH".into());
        out.push(
            Self::row(
                Self::label("Java"),
                Some(match j.java_major {
                    Some(m) => format!("Java {m}: the one the server starts with"),
                    None => "Version unknown: the one the server starts with".into(),
                }),
                None,
                div()
                    .max_w(rems(24.))
                    .truncate()
                    .text_sm()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .child(java)
                    .into_any_element(),
                false,
                cx,
            )
            .into_any_element(),
        );
        let label = if self.preset == "custom" {
            format!("Custom flags ({})", j.flags.len())
        } else {
            j.presets.iter().find(|p| p.id == self.preset).map_or_else(|| self.preset.clone(), |p| p.label.clone())
        };
        let items = j
            .presets
            .iter()
            .map(|p| {
                let l = if p.supported { p.label.clone() } else { format!("{} (needs a newer Java)", p.label) };
                (l, p.id == self.preset, p.supported)
            })
            .collect();
        let (view, ids) = (cx.entity(), j.presets.iter().map(|p| p.id.clone()).collect::<Vec<_>>());
        out.push(
            Self::row(
                Self::label("GC flags"),
                Some("How Java frees memory. Aikar's flags suit almost every server.".into()),
                None,
                div()
                    .w_64()
                    .child(Self::dropdown(
                        "settings-gc".into(),
                        label,
                        items,
                        rem,
                        Rc::new(move |ix, _, cx| {
                            if let Some(id) = ids.get(ix).cloned() {
                                view.update(cx, |this, cx| {
                                    this.preset = id;
                                    cx.notify();
                                });
                            }
                        }),
                    ))
                    .into_any_element(),
                self.preset_changed(),
                cx,
            )
            .into_any_element(),
        );
        out
    }

    fn files_page(&self, d: &Data, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Some(j) = d.jvm.clone() else {
            return vec![widgets::caption("The panel didn't say how this server starts.", cx).into_any_element()];
        };
        let mut out = vec![Self::heading("Files that start the server", cx).into_any_element()];
        if j.files.is_empty() {
            out.push(
                div()
                    .px_2()
                    .child(widgets::caption("No run.bat, start.bat or user_jvm_args.txt; the server starts from its jar.", cx))
                    .into_any_element(),
            );
        }
        for (i, f) in j.files.iter().enumerate() {
            let open = self.editor.as_ref().is_some_and(|e| &e.path == f);
            let path = f.clone();
            let control = if open {
                div().into_any_element()
            } else {
                Button::new(("settings-edit-file", i))
                    .outline()
                    .small()
                    .icon(IconName::FileCode)
                    .label("Edit")
                    .on_click(cx.listener(move |this, _, window, cx| this.open_file(path.clone(), window, cx)))
                    .into_any_element()
            };
            let label = div().text_sm().font_medium().font_family(theme.mono_font_family.clone()).child(f.clone()).into_any_element();
            let desc = if f == "user_jvm_args.txt" { "Java arguments: memory and flags" } else { "The start script" };
            out.push(Self::row(label, Some(desc.into()), None, control, false, cx).into_any_element());
            if open && let Some(ed) = &self.editor {
                let mut col = v_flex().gap_2().px_2().py_3();
                match &ed.area {
                    Some(a) => col = col.child(Textarea::new(a)),
                    None if ed.error.is_none() => col = col.child(widgets::skeleton_rows(3, rems(1.5), cx)),
                    None => {}
                }
                col = col
                    .when_some(ed.error.clone(), |c, e| c.child(div().text_sm().text_color(theme.danger).child(e)))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("settings-file-save")
                                    .primary()
                                    .small()
                                    .icon(IconName::Save)
                                    .label(if ed.force { "Save anyway" } else { "Save" })
                                    .disabled(ed.area.is_none() || ed.saving)
                                    .on_click(cx.listener(|this, _, window, cx| this.save_file(window, cx))),
                            )
                            .child(
                                Button::new("settings-file-cancel")
                                    .ghost()
                                    .small()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.editor = None;
                                        cx.notify();
                                    })),
                            ),
                    );
                out.push(col.into_any_element());
            }
        }
        out.push(Self::heading(&format!("Java flags ({})", j.flags.len()), cx).into_any_element());
        out.push(
            div()
                .mx_2()
                .px_3()
                .py_2p5()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_xs()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.muted_foreground)
                .child(if j.flags.is_empty() { "None".to_string() } else { j.flags.join("\n") })
                .into_any_element(),
        );
        out
    }

    /// The Address card: addresses, sleeping, and the router's state.
    fn address(&self, d: &Data, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let mut out = vec![Self::heading("Address players join by", cx).into_any_element()];
        let (Some(a), Some(router)) = (d.addr.clone(), d.router.clone()) else {
            out.push(div().px_2().child(widgets::caption("This panel doesn't have server addresses.", cx)).into_any_element());
            return out;
        };
        let alive = d.alive();
        let mut col = v_flex().gap_2().px_2();
        if router.enabled && a.port == a.router_port {
            col = col.child(
                widgets::notice_bar(theme.warning, IconName::TriangleAlert, cx)
                    .child(
                        div().flex_1().text_sm().child(format!(
                            "This server uses port {}, the port players join on. Give it its own port so the router can take {}.",
                            a.port, a.port
                        )),
                    )
                    .when(alive, |r| r.child(widgets::caption("Stop it first.", cx).flex_none()))
                    .child(
                        Button::new("settings-move-port")
                            .outline()
                            .small()
                            .icon(IconName::ArrowRightLeft)
                            .label("Move it")
                            .disabled(alive)
                            .on_click(cx.listener(|this, _, window, cx| this.move_port(window, cx))),
                    ),
            );
        }
        let suffix = if router.domain.is_empty() { String::new() } else { format!(".{}", router.domain) };
        let n = self.addr_inputs.len();
        for (i, input) in self.addr_inputs.iter().enumerate() {
            let v = input.read(cx).value().to_string();
            let bare = !suffix.is_empty() && !v.contains('.');
            let full = Self::full_address(&v, &router.domain);
            let dns = self.dns_of(&full);
            col = col.child(
                h_flex()
                    .gap_2()
                    .child(div().when(bare, |d| d.w(rems(10.))).when(!bare, |d| d.flex_1().min_w_0()).child(Input::new(input).small()))
                    .when(bare, |r| {
                        r.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(suffix.clone()),
                        )
                    })
                    .child(div().flex_1())
                    .when_some(dns, |r, (ok, note)| {
                        r.child(widgets::chip(if ok { theme.success } else { theme.danger }).child(if ok { "✓ DNS".to_string() } else { note }))
                    })
                    .when(n > 1 || !v.is_empty(), |r| {
                        r.child(
                            Button::new(("settings-addr-remove", i))
                                .ghost()
                                .small()
                                .icon(IconName::X)
                                .tooltip("Remove")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if this.addr_inputs.len() > 1 {
                                        this.addr_inputs.remove(i);
                                    } else if let Some(f) = this.addr_inputs.first() {
                                        f.update(cx, |s, cx| s.set_value("", window, cx));
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        col = col
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("settings-addr-add")
                            .ghost()
                            .small()
                            .icon(IconName::Plus)
                            .label("Add another address")
                            .on_click(cx.listener(|this, _, window, cx| {
                                let mut subs = std::mem::take(&mut this.form_subs);
                                this.push_addr(String::new(), &mut subs, window, cx);
                                this.form_subs = subs;
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .when(!a.addresses.is_empty(), |r| {
                        r.child(
                            Button::new("settings-dns")
                                .ghost()
                                .small()
                                .icon(IconName::SearchCheck)
                                .label(if self.checking { "Checking…" } else { "Check DNS" })
                                .disabled(self.checking)
                                .on_click(cx.listener(|this, _, window, cx| this.check_dns(window, cx))),
                        )
                    }),
            )
            .child(
                h_flex()
                    .gap_2()
                    .pt_1()
                    .child(Switch::new("settings-sleep").label("Sleep when empty for").checked(self.sleep_on).on_click(
                        cx.listener(|this, v: &bool, _, cx| {
                            this.sleep_on = *v;
                            cx.notify();
                        }),
                    ))
                    .child(div().w(rems(4.5)).child(Input::new(&self.sleep_min).small().disabled(!self.sleep_on)))
                    .child(div().text_sm().child("min")),
            )
            .child(widgets::caption(
                if self.sleep_on {
                    "Joining starts it again; the server list shows \"Sleeping\" meanwhile."
                } else {
                    "Off: it runs until someone stops it."
                },
                cx,
            ));
        col = if !router.enabled {
            col.child(widgets::caption("Server addresses are off. Turn them on in the panel's Access & devices.", cx))
        } else if let Some(e) = router.error {
            col.child(div().text_sm().text_color(theme.danger).child(e))
        } else if let Some(p) = router.listening {
            col.child(widgets::caption(
                format!(
                    "Players join on port {p}.{}",
                    if router.domain.is_empty() { " Set a domain in the panel's Access & devices to type only names." } else { "" }
                ),
                cx,
            ))
        } else {
            col.child(widgets::caption(format!("The router starts on port {} once a server has an address.", router.port), cx))
        };
        out.push(col.into_any_element());
        out
    }

    fn save_bar(&self, changes: &[String], alive: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let n = changes.len();
        let list = format!(
            "{n} unsaved change{}: {}{}",
            if n == 1 { "" } else { "s" },
            changes.iter().take(3).cloned().collect::<Vec<_>>().join(", "),
            if n > 3 { "…" } else { "" }
        );
        h_flex()
            .flex_none()
            .gap_3()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().truncate().child(list))
                    .when(alive, |c| c.child(widgets::caption("The server is running: restart it to apply.", cx))),
            )
            .child(
                Button::new("settings-discard")
                    .outline()
                    .small()
                    .label("Discard")
                    .disabled(self.saving)
                    .on_click(cx.listener(|this, _, window, cx| this.reset_form(window, cx))),
            )
            .child(
                Button::new("settings-save")
                    .primary()
                    .small()
                    .icon(IconName::Save)
                    .label(format!("Save {n}"))
                    .disabled(self.saving)
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
            )
    }
}

impl Render for SettingsTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let theme = cx.theme().clone();
        if let Some(e) = super::load_state(&self.data, cx) {
            return div().size_full().p_4().child(e).into_any_element();
        }
        let Some(d) = self.data.ready().cloned() else {
            return div().into_any_element();
        };
        let query = self.search.read(cx).value().trim().to_lowercase();
        let mut body: Vec<AnyElement> = Vec::new();
        if !query.is_empty() {
            let hits: Vec<String> = self
                .all_keys()
                .into_iter()
                .filter(|k| {
                    let sp = d.spec(k);
                    format!("{k} {} {}", sp.map_or("", |s| &s.label), sp.map_or("", |s| &s.description))
                        .to_lowercase()
                        .contains(&query)
                })
                .collect();
            let raw = self.search.read(cx).value().trim().to_string();
            body.push(
                div()
                    .px_2()
                    .pb_1()
                    .text_lg()
                    .font_semibold()
                    .child(format!("{} setting{} matching “{raw}”", hits.len(), if hits.len() == 1 { "" } else { "s" }))
                    .into_any_element(),
            );
            if hits.is_empty() {
                body.push(div().px_2().child(widgets::caption("Nothing matches. Try the key name, like view-distance.", cx)).into_any_element());
            }
            for k in hits {
                body.push(self.field(&k, rem, cx));
            }
        } else {
            let (id, label, _, groups) = PAGES[self.page.min(PAGES.len() - 1)];
            body.push(div().px_2().text_lg().font_semibold().child(label).into_any_element());
            for (heading, keys) in groups {
                body.push(Self::heading(heading, cx).into_any_element());
                for k in *keys {
                    body.push(self.field(k, rem, cx));
                }
                if id == "general" && *heading == "Server list" {
                    body.push(self.icon_row(cx));
                }
            }
            match id {
                "network" => body.extend(self.address(&d, cx)),
                "java" => body.extend(self.java_page(&d, rem, cx)),
                "files" => body.extend(self.files_page(&d, cx)),
                "all" => {
                    body.push(
                        div()
                            .px_2()
                            .pt_2()
                            .child(widgets::caption("Every key in server.properties. Keys without a description here are shown by name.", cx))
                            .into_any_element(),
                    );
                    for k in self.all_keys() {
                        body.push(self.field(&k, rem, cx));
                    }
                }
                _ => {}
            }
        }
        let changes = self.changes(cx);
        let alive = self.data.ready().is_some_and(|d| d.alive());
        let bar = (!changes.is_empty()).then(|| self.save_bar(&changes, alive, cx));

        h_flex()
            .size_full()
            .items_start()
            .child(self.nav(cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(
                        div()
                            .id("settings-tab-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scrollbar()
                            .child(
                                v_flex()
                                    .w_full()
                                    .max_w(rems(54.))
                                    .px_6()
                                    .py_4()
                                    .when_some(self.error.clone(), |c, e| {
                                        c.child(div().px_2().pb_2().text_sm().text_color(theme.danger).child(e))
                                    })
                                    .children(body),
                            ),
                    )
                    .children(bar),
            )
            .into_any_element()
    }
}
