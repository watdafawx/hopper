//! The Accounts dialog (from the avatar in the title bar): Hopper's
//! Microsoft accounts, and the active one's skin, cape and name.
//!
//! `--mock-accounts` (or `HOPPER_MOCK_ACCOUNTS=1`) runs it against made-up
//! accounts in memory: no network, no Credential Manager, nothing saved.

use crate::skins::{self, View};
use crate::store::Store;
use crate::widgets;
use anyhow::{Context as _, bail};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::accounts::{Accounts, Saved};
use hopper::mcprofile::{self, Api, McProfile, NameChange, NameStatus, Variant};
use hopper::skinhistory::{self, History};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

/// The id the view uses for the old single sign-in before it is copied.
const LEGACY: &str = "legacy";

// ---- what the dialog asks for ---------------------------------------------

enum Op {
    Profile,
    NameInfo,
    Available(String),
    SkinUrl(Variant, String),
    Upload(Variant, Vec<u8>),
    Reset,
    Cape(Option<String>),
    Rename(String),
}

enum Done {
    /// The profile, and the history file of the skin it wears.
    Profile(McProfile, Option<String>),
    NameInfo(NameChange),
    Available(String, NameStatus),
}

/// Whether this run uses the made-up accounts.
pub fn mock() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("HOPPER_MOCK_ACCOUNTS").is_some()
            || std::env::args().any(|a| a == "--mock-accounts")
    })
}

fn history() -> Option<History> {
    if mock() {
        Some(History::at(mock_state().dir.join("history")))
    } else {
        History::open()
    }
}

/// Runs `op` for the active account (blocking).
fn run(op: Op) -> anyhow::Result<Done> {
    let done = if mock() { mock_run(op)? } else { live_run(op)? };
    Ok(match done {
        Done::Profile(p, _) => {
            // Keep the skin it wears, to switch back to later.
            let file = p.active_skin().and_then(|s| {
                let path = hopper::imgcache::fetch(&s.url).ok()?;
                let bytes = std::fs::read(path).ok()?;
                history()?.remember(&bytes, s.variant).ok().map(|e| e.file)
            });
            Done::Profile(p, file)
        }
        d => d,
    })
}

fn live_run(op: Op) -> anyhow::Result<Done> {
    let a = Accounts::load();
    let session = match a.active() {
        Some(s) => hopper::accounts::session(&s.id)?,
        // The old single sign-in: copied into the list on first use.
        None => hopper::accounts::active_session()?,
    };
    let api = Api::new(session.access_token.clone());
    let done = match op {
        Op::Profile => Done::Profile(api.profile()?, None),
        Op::NameInfo => Done::NameInfo(api.name_change()?),
        Op::Available(n) => Done::Available(n.clone(), api.name_available(&n)?),
        Op::SkinUrl(v, url) => Done::Profile(api.set_skin_url(v, &url)?, None),
        Op::Upload(v, png) => Done::Profile(api.upload_skin(v, &png)?, None),
        Op::Reset => Done::Profile(api.reset_skin()?, None),
        Op::Cape(Some(id)) => Done::Profile(api.show_cape(&id)?, None),
        Op::Cape(None) => Done::Profile(api.hide_cape()?, None),
        Op::Rename(n) => Done::Profile(api.change_name(&n)?, None),
    };
    if let Done::Profile(p, _) = &done {
        let skin = p.active_skin().map(|s| s.url.as_str());
        hopper::accounts::note_profile(&p.id, &p.name, skin)?;
    }
    Ok(done)
}

fn switch(id: &str) -> anyhow::Result<Accounts> {
    if mock() {
        let mut m = mock_state();
        m.accounts.set_active(id);
        return Ok(m.accounts.clone());
    }
    hopper::accounts::switch(id)?;
    Ok(Accounts::load())
}

fn sign_out(id: &str) -> anyhow::Result<Accounts> {
    if mock() {
        let mut m = mock_state();
        m.accounts.remove(id);
        return Ok(m.accounts.clone());
    }
    hopper::accounts::sign_out(id)?;
    Ok(Accounts::load())
}

/// Makes `id` the account games start with.
pub fn switch_account(store: &Entity<Store>, id: String, window: &mut Window, cx: &mut App) {
    match switch(&id) {
        Ok(list) => {
            let name = list.get(&id).map(|a| a.name.clone()).unwrap_or_default();
            store.update(cx, |s, cx| s.set_accounts(list, cx));
            window.push_notification(Notification::info(format!("Games now start as {name}.")), cx);
        }
        Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
    }
}

/// Asks, then signs `a` out of Hopper.
pub fn ask_sign_out(store: Entity<Store>, a: Saved, window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, move |alert, _, _| {
        let (store, a) = (store.clone(), a.clone());
        alert
            .title(format!("Sign out of {}?", a.name))
            .description(
                "Hopper forgets this account's sign-in on this PC. The Minecraft account itself doesn't change, and you can add it again any time.",
            )
            .confirm()
            .ok_text("Sign out")
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| {
                match sign_out(&a.id) {
                    Ok(list) => {
                        store.update(cx, |s, cx| {
                            s.set_accounts(list, cx);
                            if !mock() && s.accounts().active.is_none() {
                                s.update_settings(|st| st.signed_in_as = None, cx);
                            }
                        });
                        window.push_notification(Notification::info(format!("Signed out of {}.", a.name)), cx);
                    }
                    Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
                }
                true
            })
    });
}

/// Signs in another Microsoft account.
pub fn add_account(store: &Entity<Store>, window: &mut Window, cx: &mut App) {
    if mock() {
        let list = mock_add();
        store.update(cx, |s, cx| s.set_accounts(list, cx));
        return;
    }
    crate::dialogs::open_sign_in(store.clone(), window, cx);
}

// ---- the made-up accounts -------------------------------------------------

struct Mock {
    accounts: Accounts,
    profiles: HashMap<String, McProfile>,
    names: HashMap<String, NameChange>,
    dir: PathBuf,
    made: u32,
}

fn mock_state() -> MutexGuard<'static, Mock> {
    static M: OnceLock<Mutex<Mock>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(make_mock()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn mock_file(dir: &std::path::Path, name: &str, img: &image::RgbaImage) -> String {
    let path = dir.join(name);
    let _ = std::fs::write(&path, hopper::skinrender::png_bytes(img));
    path.to_string_lossy().to_string()
}

fn mock_skin(id: &str, url: String, variant: Variant) -> mcprofile::Skin {
    mcprofile::Skin {
        id: id.into(),
        state: "ACTIVE".into(),
        url,
        variant,
        texture_key: id.into(),
        alias: None,
    }
}

fn make_mock() -> Mock {
    use hopper::skinrender::{sample_cape, sample_skin};
    let dir = std::env::temp_dir().join(format!("hopper-mock-accounts-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let main_skin = mock_file(&dir, "main.png", &sample_skin(false, [38, 120, 200]));
    let alt_skin = mock_file(&dir, "alt.png", &sample_skin(true, [70, 160, 90]));
    mock_file(&dir, "steve.png", &sample_skin(false, [0, 170, 170]));
    let cape = |name: &str, c: [u8; 3]| mock_file(&dir, &format!("cape-{name}.png"), &sample_cape(c));
    let capes = vec![
        mcprofile::Cape {
            id: "c1".into(),
            state: "ACTIVE".into(),
            url: cape("migrator", [92, 172, 230]),
            alias: "Migrator".into(),
        },
        mcprofile::Cape {
            id: "c2".into(),
            state: "INACTIVE".into(),
            url: cape("pan", [236, 156, 60]),
            alias: "Pan".into(),
        },
        mcprofile::Cape {
            id: "c3".into(),
            state: "INACTIVE".into(),
            url: cape("vanilla", [214, 214, 214]),
            alias: "Vanilla".into(),
        },
    ];
    let main = McProfile {
        id: "a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
        name: "PixelPioneer".into(),
        skins: vec![mock_skin("s1", main_skin.clone(), Variant::Classic)],
        capes,
    };
    let alt = McProfile {
        id: "0f9e8d7c6b5a49382716051423324150".into(),
        name: "FawxBuilds".into(),
        skins: vec![mock_skin("s2", alt_skin.clone(), Variant::Slim)],
        capes: Vec::new(),
    };
    let ten_days_ago = hopper::time::now() - 10 * 86_400;
    let iso = |t: i64| {
        let d = hopper::time::date(t);
        // dd/mm/yyyy -> yyyy-mm-dd
        format!("{}-{}-{}T12:00:00Z", &d[6..10], &d[3..5], &d[0..2])
    };
    let mut accounts = Accounts::default();
    accounts.upsert_active(&alt.id, &alt.name);
    accounts.upsert_active(&main.id, &main.name);
    accounts.note_profile(&main.id, &main.name, Some(&main_skin));
    accounts.note_profile(&alt.id, &alt.name, Some(&alt_skin));
    let names = HashMap::from([
        (
            main.id.clone(),
            NameChange {
                changed_at: Some(iso(ten_days_ago)),
                created_at: Some("2014-06-01T12:00:00Z".into()),
                name_change_allowed: false,
            },
        ),
        (
            alt.id.clone(),
            NameChange {
                changed_at: None,
                created_at: Some("2021-03-01T12:00:00Z".into()),
                name_change_allowed: true,
            },
        ),
    ]);
    let profiles = HashMap::from([(main.id.clone(), main), (alt.id.clone(), alt)]);
    Mock {
        accounts,
        profiles,
        names,
        dir,
        made: 0,
    }
}

/// The mock account list (for the store).
pub fn mock_accounts() -> Accounts {
    mock_state().accounts.clone()
}

/// Adds a made-up account (the mock's "Add account").
fn mock_add() -> Accounts {
    let mut m = mock_state();
    m.made += 1;
    let n = m.made;
    let id = format!("{n:032x}");
    let name = format!("NewAlt{n}");
    let url = mock_file(
        &m.dir.clone(),
        &format!("new{n}.png"),
        &hopper::skinrender::sample_skin(false, [180, 60, 60]),
    );
    m.profiles.insert(
        id.clone(),
        McProfile {
            id: id.clone(),
            name: name.clone(),
            skins: vec![mock_skin(&format!("n{n}"), url.clone(), Variant::Classic)],
            capes: Vec::new(),
        },
    );
    m.names.insert(
        id.clone(),
        NameChange {
            name_change_allowed: true,
            ..Default::default()
        },
    );
    m.accounts.upsert_active(&id, &name);
    m.accounts.note_profile(&id, &name, Some(&url));
    m.accounts.clone()
}

fn mock_run(op: Op) -> anyhow::Result<Done> {
    std::thread::sleep(Duration::from_millis(300));
    let mut m = mock_state();
    let id = m.accounts.active.clone().context("Not signed in.")?;
    let dir = m.dir.clone();
    m.made += 1;
    let made = m.made;
    let Mock {
        profiles,
        names,
        accounts,
        ..
    } = &mut *m;
    let p = profiles.get_mut(&id).context("no such profile")?;
    let set_skin = |p: &mut McProfile, url: String, v: Variant| {
        p.skins = vec![mock_skin(&format!("m{made}"), url, v)];
    };
    match op {
        Op::Profile => {}
        Op::NameInfo => return Ok(Done::NameInfo(names.get(&id).cloned().unwrap_or_default())),
        Op::Available(n) => {
            mcprofile::validate_name(&n).map_err(anyhow::Error::msg)?;
            let l = n.to_lowercase();
            let s = if l.contains("taken") || l == "notch" || l == "steve" {
                NameStatus::Taken
            } else if l.contains("bad") {
                NameStatus::NotAllowed
            } else {
                NameStatus::Available
            };
            return Ok(Done::Available(n, s));
        }
        Op::SkinUrl(v, url) => {
            // The mock can't download; a local texture keeps its picture.
            let local = std::path::Path::new(&url).is_file();
            let url = if local {
                url
            } else {
                mock_file(
                    &dir,
                    &format!("url{made}.png"),
                    &hopper::skinrender::sample_skin(v.is_slim(), [150, 90, 200]),
                )
            };
            set_skin(p, url, v);
        }
        Op::Upload(v, png) => {
            mcprofile::check_skin_png(&png)?;
            let path = dir.join(format!("upload{made}.png"));
            std::fs::write(&path, &png)?;
            set_skin(p, path.to_string_lossy().to_string(), v);
        }
        Op::Reset => set_skin(p, dir.join("steve.png").to_string_lossy().to_string(), Variant::Classic),
        Op::Cape(which) => {
            for c in &mut p.capes {
                c.state = if Some(&c.id) == which.as_ref() { "ACTIVE" } else { "INACTIVE" }.into();
            }
        }
        Op::Rename(n) => {
            mcprofile::validate_name(&n).map_err(anyhow::Error::msg)?;
            if !names.get(&id).is_some_and(|c| c.name_change_allowed) {
                bail!("Couldn't change your name: Mojang doesn't allow a name change yet (one every 30 days).");
            }
            p.name = n.clone();
            names.insert(
                id.clone(),
                NameChange {
                    changed_at: Some(hopper::time::now_iso()),
                    created_at: None,
                    name_change_allowed: false,
                },
            );
        }
    }
    let skin = p.active_skin().map(|s| s.url.clone());
    accounts.note_profile(&id, &p.name.clone(), skin.as_deref());
    Ok(Done::Profile(p.clone(), None))
}

// ---- heads ------------------------------------------------------------------

/// An account's face: drawn from its skin when it is cached, else the
/// public avatar, else a placeholder.
pub fn head(store: &Store, a: &Saved, size: Rems, cx: &App) -> AnyElement {
    let drawn = a
        .skin_url
        .as_deref()
        .and_then(|u| store.image(u))
        .and_then(|p| skins::image(p, View::Head));
    let theme = cx.theme();
    let frame = div()
        .size(size)
        .flex_none()
        .rounded(theme.radius)
        .overflow_hidden()
        .bg(theme.muted);
    if let Some(img_data) = drawn {
        return frame.child(img(img_data).size_full()).into_any_element();
    }
    if let Some(p) = store.image(&a.fallback_avatar()) {
        return frame.child(img(p.to_path_buf()).size_full()).into_any_element();
    }
    frame
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.muted_foreground)
        .child(Icon::new(IconName::UserRound).small())
        .into_any_element()
}

// ---- the dialog -------------------------------------------------------------

enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(t) => Some(t),
            _ => None,
        }
    }
}

enum NameCheck {
    Empty,
    Same,
    Invalid(String),
    Checking,
    Done(String, NameStatus),
    Failed(String),
}

pub struct AccountsView {
    store: Entity<Store>,
    /// Skin, Cape or Name.
    tab: usize,
    /// Whose profile is shown (an account id, or [`LEGACY`]).
    for_id: Option<String>,
    /// Bumped per account shown; late answers for another are dropped.
    generation: u64,
    profile: Load<McProfile>,
    name_info: Load<NameChange>,
    /// The history file of the skin worn now.
    current_file: Option<String>,
    history: Vec<skinhistory::Entry>,
    busy: Option<SharedString>,
    back: bool,
    show_url: bool,
    name_input: Entity<InputState>,
    url_input: Entity<InputState>,
    name_check: NameCheck,
    _check: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

impl AccountsView {
    fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder("New name"));
        let url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://…/skin.png"));
        let subs = vec![
            cx.observe(&store, |this, store, cx| {
                let target = Self::target(&store, cx);
                if target != this.for_id
                    && !(this.for_id.as_deref() == Some(LEGACY) && target.is_some())
                {
                    this.load(cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(&name_input, window, |this, _, e: &InputEvent, _, cx| {
                if matches!(e, InputEvent::Change) {
                    this.check_name_soon(cx);
                }
            }),
            cx.subscribe_in(&url_input, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.apply_url(window, cx);
                }
            }),
        ];
        let mut view = Self {
            store,
            tab: 0,
            for_id: None,
            generation: 0,
            profile: Load::Loading,
            name_info: Load::Loading,
            current_file: None,
            history: history().map(|h| h.list()).unwrap_or_default(),
            busy: None,
            back: false,
            show_url: false,
            name_input,
            url_input,
            name_check: NameCheck::Empty,
            _check: None,
            _subs: subs,
        };
        view.load(cx);
        view
    }

    /// The account to show: the active one, or the old sign-in.
    fn target(store: &Entity<Store>, cx: &App) -> Option<String> {
        let s = store.read(cx);
        s.accounts()
            .active
            .clone()
            .or_else(|| s.legacy_login().then(|| LEGACY.to_string()))
    }

    /// Reads the shown account's profile and name details.
    fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.for_id = Self::target(&self.store, cx);
        self.name_check = NameCheck::Empty;
        if self.for_id.is_none() {
            self.profile = Load::Failed(String::new());
            self.name_info = Load::Failed(String::new());
            cx.notify();
            return;
        }
        self.profile = Load::Loading;
        self.name_info = Load::Loading;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let r = cx.background_spawn(async { run(Op::Profile) }).await;
            let ok = r.is_ok();
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match r {
                    Ok(d) => this.apply(d, cx),
                    Err(e) => this.profile = Load::Failed(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
            if !ok {
                return;
            }
            let r = cx.background_spawn(async { run(Op::NameInfo) }).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match r {
                    Ok(d) => this.apply(d, cx),
                    Err(e) => this.name_info = Load::Failed(format!("{e:#}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn apply(&mut self, done: Done, cx: &mut Context<Self>) {
        match done {
            Done::Profile(p, file) => {
                self.for_id = Some(p.id.clone());
                self.current_file = file;
                self.history = history().map(|h| h.list()).unwrap_or_default();
                let urls: Vec<String> = p
                    .skins
                    .iter()
                    .map(|s| s.url.clone())
                    .chain(p.capes.iter().map(|c| c.url.clone()))
                    .collect();
                self.profile = Load::Ready(p);
                self.store.update(cx, |s, cx| {
                    for u in &urls {
                        s.want_image(u);
                    }
                    let list = if mock() {
                        mock_accounts()
                    } else {
                        Accounts::load()
                    };
                    s.set_accounts(list, cx);
                });
            }
            Done::NameInfo(n) => self.name_info = Load::Ready(n),
            Done::Available(name, status) => {
                if self.name_input.read(cx).value().trim() == name {
                    self.name_check = NameCheck::Done(name, status);
                }
            }
        }
    }

    /// Runs a change in the background, telling how it went.
    fn change(
        &mut self,
        doing: &'static str,
        op: Op,
        success: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(doing.into());
        let generation = self.generation;
        let renamed = matches!(op, Op::Rename(_));
        cx.spawn_in(window, async move |this, cx| {
            let r = cx.background_spawn(async move { run(op) }).await;
            let r = match r {
                Ok(d) if renamed => {
                    let info = cx.background_spawn(async { run(Op::NameInfo) }).await;
                    Ok((d, info.ok()))
                }
                Ok(d) => Ok((d, None)),
                Err(e) => Err(e),
            };
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                if this.generation == generation {
                    match r {
                        Ok((d, info)) => {
                            this.apply(d, cx);
                            if let Some(i) = info {
                                this.apply(i, cx);
                            }
                            if renamed {
                                this.name_input.update(cx, |s, cx| s.set_value("", window, cx));
                                this.name_check = NameCheck::Empty;
                            }
                            window.push_notification(Notification::success(success), cx);
                        }
                        Err(e) => {
                            window.push_notification(Notification::error(format!("{e:#}")), cx)
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn switch_to(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        switch_account(&self.store, id, window, cx);
    }

    fn ask_sign_out(&mut self, a: Saved, window: &mut Window, cx: &mut Context<Self>) {
        ask_sign_out(self.store.clone(), a, window, cx);
    }

    fn add_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        add_account(&self.store, window, cx);
    }

    // ---- skin -----------------------------------------------------------

    fn active_skin(&self) -> Option<mcprofile::Skin> {
        self.profile.ready()?.active_skin().cloned()
    }

    fn pick_skin_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Use this skin".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let read = cx
                .background_spawn(async move {
                    let bytes = std::fs::read(&path)
                        .with_context(|| format!("Couldn't read {}", path.display()))?;
                    mcprofile::check_skin_png(&bytes)?;
                    let img = image::load_from_memory(&bytes)?.to_rgba8();
                    let v = if hopper::skinrender::looks_slim(&img) {
                        Variant::Slim
                    } else {
                        Variant::Classic
                    };
                    anyhow::Ok((bytes, v))
                })
                .await;
            this.update_in(cx, |this, window, cx| match read {
                Ok((bytes, v)) => this.change(
                    "Uploading your skin",
                    Op::Upload(v, bytes),
                    format!("Skin uploaded ({} arms).", v.label().to_lowercase()),
                    window,
                    cx,
                ),
                Err(e) => window.push_notification(Notification::error(format!("{e:#}")), cx),
            })
            .ok();
        })
        .detach();
    }

    fn apply_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.url_input.read(cx).value().trim().to_string();
        if url.is_empty() {
            return;
        }
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            window.push_notification(
                Notification::error("A skin URL starts with http:// or https://."),
                cx,
            );
            return;
        }
        let v = self.active_skin().map(|s| s.variant).unwrap_or_default();
        self.show_url = false;
        self.url_input.update(cx, |s, cx| s.set_value("", window, cx));
        self.change(
            "Changing your skin",
            Op::SkinUrl(v, url),
            "Skin changed.".into(),
            window,
            cx,
        );
    }

    fn set_variant(&mut self, v: Variant, window: &mut Window, cx: &mut Context<Self>) {
        let Some(skin) = self.active_skin() else { return };
        if skin.variant == v {
            return;
        }
        self.change(
            "Changing the arms",
            Op::SkinUrl(v, skin.url),
            format!("Your skin now has {} arms.", v.label().to_lowercase()),
            window,
            cx,
        );
    }

    fn ask_reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let view = view.clone();
            alert
                .title("Reset to the default skin?")
                .description(
                    "Mojang puts a default skin on this account. Your current skin stays in Recent skins, so you can put it back.",
                )
                .confirm()
                .ok_text("Reset skin")
                .on_ok(move |_, window, cx| {
                    view.update(cx, |this, cx| {
                        this.change(
                            "Resetting your skin",
                            Op::Reset,
                            "Skin reset to the default.".into(),
                            window,
                            cx,
                        )
                    });
                    true
                })
        });
    }

    fn wear(&mut self, e: skinhistory::Entry, window: &mut Window, cx: &mut Context<Self>) {
        let Some(h) = history() else { return };
        match std::fs::read(h.path(&e)) {
            Ok(bytes) => self.change(
                "Uploading your skin",
                Op::Upload(e.variant, bytes),
                "Skin changed.".into(),
                window,
                cx,
            ),
            Err(err) => window.push_notification(Notification::error(format!("{err:#}")), cx),
        }
    }

    fn forget(&mut self, file: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(h) = history() else { return };
        if let Err(e) = h.forget(&file) {
            window.push_notification(Notification::error(format!("{e:#}")), cx);
        }
        self.history = h.list();
        cx.notify();
    }

    // ---- name -------------------------------------------------------------

    fn check_name_soon(&mut self, cx: &mut Context<Self>) {
        let name = self.name_input.read(cx).value().trim().to_string();
        let current = self.profile.ready().map(|p| p.name.clone()).unwrap_or_default();
        self._check = None;
        self.name_check = if name.is_empty() {
            NameCheck::Empty
        } else if name == current {
            NameCheck::Same
        } else if let Err(e) = mcprofile::validate_name(&name) {
            NameCheck::Invalid(e)
        } else {
            NameCheck::Checking
        };
        cx.notify();
        if !matches!(self.name_check, NameCheck::Checking) {
            return;
        }
        let generation = self.generation;
        self._check = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let n = name.clone();
            let r = cx.background_spawn(async move { run(Op::Available(n)) }).await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                match r {
                    Ok(d) => this.apply(d, cx),
                    Err(e) => {
                        if this.name_input.read(cx).value().trim() == name {
                            this.name_check = NameCheck::Failed(format!("{e:#}"));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn ask_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let NameCheck::Done(name, NameStatus::Available) = &self.name_check else {
            return;
        };
        let name = name.clone();
        let old = self.profile.ready().map(|p| p.name.clone()).unwrap_or_default();
        let until = hopper::time::date(hopper::time::now() + mcprofile::NAME_COOLDOWN_DAYS * 86_400);
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, cx| {
            let (view, name) = (view.clone(), name.clone());
            let theme = cx.theme();
            alert
                .title(format!("Change your name to {name}?"))
                .description(
                    v_flex()
                        .gap_2()
                        .text_sm()
                        .child(format!(
                            "Mojang allows one name change every 30 days. After this you can't change it again until {until}."
                        ))
                        .child(format!(
                            "{old} is given up: someone else may take it once Mojang releases it, and you may not get it back."
                        ))
                        .child(
                            div()
                                .text_color(theme.muted_foreground)
                                .child("Servers that know you by name (whitelists, ops, bans) follow your account, but some plugins may not."),
                        ),
                )
                .confirm()
                .ok_text("Change name")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let n = name.clone();
                    view.update(cx, |this, cx| {
                        this.change(
                            "Changing your name",
                            Op::Rename(n.clone()),
                            format!("Your name is now {n}."),
                            window,
                            cx,
                        )
                    });
                    true
                })
        });
    }
}

// ---- rendering ----------------------------------------------------------------

/// A profile id with dashes, as Minecraft shows UUIDs.
fn dashed(id: &str) -> String {
    if id.len() != 32 {
        return id.to_string();
    }
    format!("{}-{}-{}-{}-{}", &id[..8], &id[8..12], &id[12..16], &id[16..20], &id[20..])
}

/// A muted line that wraps.
fn note(text: &'static str, cx: &App) -> Div {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn section_title(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_semibold()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_uppercase())
}

impl AccountsView {
    fn account_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let accounts = store.accounts().clone();
        let legacy = store.legacy_login() && accounts.list.is_empty();
        let legacy_name = store.settings().signed_in_as.clone();
        let busy = self.busy.is_some();
        let mut list: Vec<Saved> = accounts.list.clone();
        // Active first, then by last use.
        list.sort_by(|a, b| {
            let act = |s: &Saved| accounts.active.as_deref() == Some(s.id.as_str());
            act(b).cmp(&act(a)).then(b.last_used.cmp(&a.last_used))
        });
        let rows: Vec<AnyElement> = list
            .into_iter()
            .map(|a| {
                let active = accounts.active.as_deref() == Some(a.id.as_str());
                let head = head(store, &a, rems(2.25), cx);
                let sub = if active {
                    "Starts your games".to_string()
                } else {
                    a.last_used
                        .as_deref()
                        .map(|t| format!("Used {}", hopper::time::relative(t).to_lowercase()))
                        .unwrap_or_else(|| "Not used yet".into())
                };
                let (id1, id2) = (a.id.clone(), a.clone());
                h_flex()
                    .id(SharedString::from(format!("acct:{}", a.id)))
                    .group("acct-row")
                    .gap_2p5()
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .when(active, |r| r.bg(theme.primary.opacity(0.12)))
                    .when(!active, |r| r.hover(|s| s.bg(theme.list_hover)))
                    .child(head)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_1p5()
                                    .child(div().text_sm().font_semibold().truncate().child(a.name.clone()))
                                    .when(active, |r| {
                                        r.child(Tag::primary().outline().xsmall().child("Active"))
                                    }),
                            )
                            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(sub)),
                    )
                    .when(!active, |r| {
                        r.child(
                            Button::new(SharedString::from(format!("switch:{}", a.id)))
                                .xsmall()
                                .outline()
                                .label("Switch")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.switch_to(id1.clone(), window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("signout:{}", a.id)))
                            .xsmall()
                            .ghost()
                            .icon(IconName::LogOut)
                            .tooltip("Sign out")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ask_sign_out(id2.clone(), window, cx)
                            })),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex()
            .w(rems(17.))
            .flex_none()
            .gap_1()
            .pr_4()
            .border_r_1()
            .border_color(theme.border)
            .child(section_title("Microsoft accounts", cx).px_2().pb_1())
            .children(rows)
            .when(legacy, |r| {
                r.child(
                    h_flex()
                        .gap_2p5()
                        .px_2()
                        .py_1p5()
                        .rounded(theme.radius)
                        .bg(theme.primary.opacity(0.12))
                        .child(
                            div()
                                .size(rems(2.25))
                                .rounded(theme.radius)
                                .bg(theme.muted)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(IconName::UserRound).small()),
                        )
                        .child(
                            v_flex()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_semibold()
                                        .child(legacy_name.unwrap_or_else(|| "Saved sign-in".into())),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Saved sign-in"),
                                ),
                        ),
                )
            })
            .child(
                div().pt_2().child(
                    Button::new("add-account")
                        .small()
                        .outline()
                        .w_full()
                        .icon(IconName::UserPlus)
                        .label("Add account")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, window, cx| this.add_account(window, cx))),
                ),
            )
            .child(
                div()
                    .px_1()
                    .pt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("The active account starts games when Hopper launches them directly (Settings → Playing & Java)."),
            )
    }

    fn header(&self, p: &McProfile, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let id = p.id.clone();
        h_flex()
            .gap_3()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_xl().font_semibold().child(p.name.clone()))
                    .child(
                        h_flex()
                            .gap_1()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(dashed(&p.id))
                            .child(
                                Button::new("copy-uuid")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::Copy)
                                    .tooltip("Copy UUID")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(dashed(&id)))
                                    }),
                            ),
                    ),
            )
            .when_some(self.busy.clone(), |r, b| {
                r.child(
                    h_flex()
                        .gap_2()
                        .px_2p5()
                        .py_1()
                        .rounded_full()
                        .bg(theme.muted)
                        .text_xs()
                        .child(Spinner::new().xsmall())
                        .child(b),
                )
            })
            .child(
                Button::new("reload-profile")
                    .small()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .tooltip("Read the profile again")
                    .disabled(self.busy.is_some())
                    .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
            )
    }

    fn skin_tab(&self, p: &McProfile, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let skin = p.active_skin().cloned();
        let variant = skin.as_ref().map(|s| s.variant).unwrap_or_default();
        let cape = p
            .active_cape()
            .and_then(|c| store.image(&c.url))
            .map(|p| p.to_path_buf());
        let texture = skin.as_ref().and_then(|s| store.image(&s.url)).map(|p| p.to_path_buf());
        let drawn = texture.as_ref().and_then(|t| {
            skins::image(
                t,
                View::Body {
                    slim: variant.is_slim(),
                    back: self.back,
                    cape: cape.clone(),
                },
            )
        });
        let busy = self.busy.is_some();
        let preview = v_flex()
            .flex_none()
            .w(rems(12.))
            .items_center()
            .gap_3()
            .py_4()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.5))
            .child(
                div()
                    .w(rems(7.))
                    .h(rems(14.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|d| match drawn {
                        Some(i) => d.child(img(i).size_full()),
                        None => d.child(Spinner::new()),
                    }),
            )
            .child(
                TabBar::new("skin-side")
                    .segmented()
                    .xsmall()
                    .selected_index(usize::from(self.back))
                    .child(Tab::new().label("Front"))
                    .child(Tab::new().label("Back"))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.back = *ix == 1;
                        cx.notify();
                    })),
            );
        let controls = v_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(
                v_flex()
                    .gap_1p5()
                    .child(section_title("Arms", cx))
                    .child(
                        TabBar::new("variant")
                            .segmented()
                            .small()
                            .selected_index(usize::from(variant.is_slim()))
                            .child(Tab::new().label("Classic (4 px)").disabled(busy))
                            .child(Tab::new().label("Slim (3 px)").disabled(busy))
                            .on_click(cx.listener(|this, ix: &usize, window, cx| {
                                let v = if *ix == 1 { Variant::Slim } else { Variant::Classic };
                                this.set_variant(v, window, cx)
                            })),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(section_title("Change skin", cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("upload-skin")
                                    .small()
                                    .primary()
                                    .icon(IconName::Upload)
                                    .label("Upload skin…")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.pick_skin_file(window, cx)
                                    })),
                            )
                            .child(
                                Button::new("skin-url")
                                    .small()
                                    .outline()
                                    .icon(IconName::Link)
                                    .label("Use skin from URL")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_url = !this.show_url;
                                        if this.show_url {
                                            this.url_input.update(cx, |s, cx| s.focus(window, cx));
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("reset-skin")
                                    .small()
                                    .ghost()
                                    .icon(IconName::RotateCcw)
                                    .label("Reset skin")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.ask_reset(window, cx)
                                    })),
                            ),
                    )
                    .when(self.show_url, |r| {
                        r.child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().child(Input::new(&self.url_input).small()))
                                .child(
                                    Button::new("apply-url")
                                        .small()
                                        .primary()
                                        .label("Use")
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.apply_url(window, cx)
                                        })),
                                ),
                        )
                    })
                    .child(note(
                        "A skin is a 64×64 (or old 64×32) PNG. Slim arms are guessed from the file; switch them above.",
                        cx,
                    )),
            );
        let recent = self.recent_skins(window, cx);
        v_flex()
            .gap_5()
            .child(h_flex().gap_5().items_start().child(preview).child(controls))
            .child(recent)
            .into_any_element()
    }

    fn recent_skins(&self, _: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let busy = self.busy.is_some();
        let Some(h) = history() else {
            return div().into_any_element();
        };
        let tiles: Vec<AnyElement> = self
            .history
            .iter()
            .map(|e| {
                let current = self.current_file.as_deref() == Some(e.file.as_str());
                let drawn = skins::image(
                    &h.path(e),
                    View::Body {
                        slim: e.variant.is_slim(),
                        back: false,
                        cape: None,
                    },
                );
                let (wear, forget) = (e.clone(), e.file.clone());
                div()
                    .id(SharedString::from(format!("hist:{}", e.file)))
                    .group("hist")
                    .relative()
                    .w(rems(4.5))
                    .h(rems(7.))
                    .p_1p5()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius)
                    .border_1()
                    .map(|d| {
                        if current {
                            d.border_color(theme.primary).bg(theme.primary.opacity(0.1))
                        } else {
                            d.border_color(theme.border)
                                .cursor_pointer()
                                .hover(|s| s.bg(theme.list_hover))
                        }
                    })
                    .when_some(drawn, |d, i| d.child(img(i).h_full().w(rems(2.75))))
                    .when(current, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top_1()
                                .right_1()
                                .text_color(theme.primary)
                                .child(Icon::new(IconName::CircleCheck).xsmall()),
                        )
                    })
                    .when(!current && !busy, |d| {
                        d.tooltip(|w, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("Wear this skin").build(w, cx)
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.wear(wear.clone(), window, cx)
                        }))
                        .child(
                            div()
                                .absolute()
                                .top_0p5()
                                .right_0p5()
                                .invisible()
                                .group_hover("hist", |s| s.visible())
                                .child(
                                    Button::new(SharedString::from(format!("forget:{}", e.file)))
                                        .xsmall()
                                        .ghost()
                                        .icon(IconName::X)
                                        .tooltip("Remove from this list")
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.forget(forget.clone(), window, cx)
                                        })),
                                ),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        v_flex()
            .gap_1p5()
            .child(
                h_flex()
                    .gap_2()
                    .child(section_title("Recent skins", cx))
                    .child(widgets::caption("Kept on this PC; click one to wear it again.", cx)),
            )
            .map(|r| {
                if tiles.is_empty() {
                    r.child(widgets::caption("Skins you wear show up here.", cx))
                } else {
                    r.child(h_flex().gap_2().flex_wrap().children(tiles))
                }
            })
            .into_any_element()
    }

    fn cape_tab(&self, p: &McProfile, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let store = self.store.read(cx);
        let busy = self.busy.is_some();
        if p.capes.is_empty() {
            return v_flex()
                .py_10()
                .items_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child(Icon::new(IconName::Ban).large())
                .child(div().font_semibold().text_color(theme.foreground).child("No capes"))
                .child(widgets::caption("This account doesn't own any capes.", cx))
                .into_any_element();
        }
        let drawn: Vec<_> = p
            .capes
            .iter()
            .map(|c| store.image(&c.url).and_then(|path| skins::image(path, View::Cape)))
            .collect();
        let none_active = p.active_cape().is_none();
        let tile = |id: String, label: String, active: bool, body: AnyElement, cx: &mut Context<Self>| {
            let which = (id != "none").then(|| id.clone());
            let label2 = label.clone();
            v_flex()
                .id(SharedString::from(format!("cape:{id}")))
                .w(rems(7.5))
                .gap_1p5()
                .p_2()
                .items_center()
                .rounded(theme.radius_lg)
                .border_1()
                .map(|d| {
                    if active {
                        d.border_color(theme.primary).bg(theme.primary.opacity(0.1))
                    } else {
                        d.border_color(theme.border)
                            .when(!busy, |d| d.cursor_pointer().hover(|s| s.bg(theme.list_hover)))
                    }
                })
                .child(
                    div()
                        .w_full()
                        .h(rems(8.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius)
                        .bg(theme.muted.opacity(0.6))
                        .child(body),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .text_sm()
                        .when(active, |r| {
                            r.text_color(theme.primary)
                                .font_semibold()
                                .child(Icon::new(IconName::Check).xsmall())
                        })
                        .child(label),
                )
                .when(!active && !busy, |d| {
                    d.on_click(cx.listener(move |this, _, window, cx| {
                        let ok = match &which {
                            Some(_) => format!("Now wearing the {label2} cape."),
                            None => "Cape hidden.".to_string(),
                        };
                        this.change(
                            if which.is_some() { "Changing your cape" } else { "Hiding your cape" },
                            Op::Cape(which.clone()),
                            ok,
                            window,
                            cx,
                        )
                    }))
                })
                .into_any_element()
        };
        let mut tiles = vec![tile(
            "none".into(),
            "No cape".into(),
            none_active,
            Icon::new(IconName::Ban)
                .large()
                .text_color(theme.muted_foreground)
                .into_any_element(),
            cx,
        )];
        for (c, drawn) in p.capes.iter().zip(drawn) {
            let body = match drawn {
                Some(i) => img(i).w(rems(3.75)).h(rems(6.)).into_any_element(),
                None => Spinner::new().into_any_element(),
            };
            let label = if c.alias.is_empty() { "Cape".to_string() } else { c.alias.clone() };
            tiles.push(tile(c.id.clone(), label, c.is_active(), body, cx));
        }
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(section_title("Your capes", cx))
                    .child(widgets::caption("Pick the one others see; the skin's Back view shows it on you.", cx)),
            )
            .child(h_flex().gap_2().flex_wrap().children(tiles))
            .into_any_element()
    }

    fn name_tab(&self, p: &McProfile, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let busy = self.busy.is_some();
        let now = hopper::time::now();
        let (eligible, line, icon, color) = match &self.name_info {
            Load::Loading => (true, "Checking when you can change it…".to_string(), None, theme.muted_foreground),
            Load::Failed(e) => (true, e.clone(), Some(IconName::TriangleAlert), theme.warning),
            Load::Ready(n) if n.name_change_allowed => {
                (true, n.describe(now), Some(IconName::CircleCheck), theme.success)
            }
            Load::Ready(n) => (false, n.describe(now), Some(IconName::Clock), theme.warning),
        };
        let (status, status_color): (Option<AnyElement>, Hsla) = match &self.name_check {
            NameCheck::Empty => (None, theme.muted_foreground),
            NameCheck::Same => (Some(div().child("That's your name now.").into_any_element()), theme.muted_foreground),
            NameCheck::Invalid(e) => (Some(div().child(e.clone()).into_any_element()), theme.danger),
            NameCheck::Checking => (
                Some(
                    h_flex()
                        .gap_1p5()
                        .child(Spinner::new().xsmall())
                        .child("Checking…")
                        .into_any_element(),
                ),
                theme.muted_foreground,
            ),
            NameCheck::Done(n, s) => {
                let (icon, color) = match s {
                    NameStatus::Available => (IconName::CircleCheck, theme.success),
                    _ => (IconName::CircleX, theme.danger),
                };
                let text = match s {
                    NameStatus::Available => format!("{n} is available"),
                    other => other.describe().to_string(),
                };
                (
                    Some(
                        h_flex()
                            .gap_1p5()
                            .child(Icon::new(icon).xsmall())
                            .child(text)
                            .into_any_element(),
                    ),
                    color,
                )
            }
            NameCheck::Failed(e) => (Some(div().child(e.clone()).into_any_element()), theme.danger),
        };
        let can = eligible
            && !busy
            && matches!(self.name_check, NameCheck::Done(_, NameStatus::Available));
        v_flex()
            .gap_5()
            .child(
                v_flex()
                    .gap_1()
                    .child(section_title("Current name", cx))
                    .child(div().text_2xl().font_semibold().child(p.name.clone()))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_sm()
                            .text_color(color)
                            .when_some(icon, |r, i| r.child(Icon::new(i).small()))
                            .child(line),
                    ),
            )
            .child(
                v_flex()
                    .gap_1p5()
                    .child(section_title("New name", cx))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().w(rems(16.)).child(Input::new(&self.name_input).small().disabled(busy)))
                            .child(
                                Button::new("change-name")
                                    .small()
                                    .primary()
                                    .icon(IconName::Pencil)
                                    .label("Change name…")
                                    .disabled(!can)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.ask_rename(window, cx)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .min_h(rems(1.25))
                            .text_sm()
                            .text_color(status_color)
                            .children(status),
                    )
                    .child(
                        note(
                            "3 to 16 letters, digits or _. Availability is checked as you type; nothing changes until you confirm.",
                            cx,
                        ),
                    ),
            )
            .into_any_element()
    }

    fn empty_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .py_16()
            .child(
                div()
                    .size(rems(3.5))
                    .rounded_full()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(IconName::UserRound).large()),
            )
            .child(div().text_lg().font_semibold().child("No Microsoft account yet"))
            .child(
                div()
                    .max_w(rems(24.))
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Add one to start games through Hopper and to change your skin, cape and name."),
            )
            .child(
                Button::new("add-first")
                    .primary()
                    .icon(IconName::UserPlus)
                    .label("Add account")
                    .on_click(cx.listener(|this, _, window, cx| this.add_account(window, cx))),
            )
            .into_any_element()
    }
}

impl Render for AccountsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let height = (window.viewport_size().height * 0.78).min(rems(38.).to_pixels(window.rem_size()));
        let list = self.account_list(cx);
        let right: AnyElement = if self.for_id.is_none() {
            self.empty_state(cx)
        } else {
            match &self.profile {
                Load::Loading => v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Spinner::new())
                    .child(widgets::caption("Reading your Minecraft profile…", cx))
                    .into_any_element(),
                Load::Failed(e) => v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_color(theme.danger)
                            .child(Icon::new(IconName::TriangleAlert))
                            .child(div().max_w(rems(28.)).text_sm().child(e.clone())),
                    )
                    .child(
                        Button::new("retry-profile")
                            .small()
                            .outline()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.load(cx))),
                    )
                    .into_any_element(),
                Load::Ready(p) => {
                    let p = p.clone();
                    let header = self.header(&p, cx);
                    let body = match self.tab {
                        1 => self.cape_tab(&p, cx),
                        2 => self.name_tab(&p, cx),
                        _ => self.skin_tab(&p, window, cx),
                    };
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_3()
                        .child(header)
                        .child(
                            TabBar::new("account-tabs")
                                .underline()
                                .small()
                                .selected_index(self.tab)
                                .child(Tab::new().label("Skin"))
                                .child(Tab::new().label("Cape"))
                                .child(Tab::new().label("Name"))
                                .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                    this.tab = *ix;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .id("account-body")
                                .flex_1()
                                .min_h_0()
                                .pr_2()
                                .overflow_y_scrollbar()
                                .child(div().pt_1().pb_2().child(body)),
                        )
                        .into_any_element()
                }
            }
        };
        h_flex()
            .h(height)
            .items_start()
            .child(list)
            .child(v_flex().h_full().flex_1().min_w_0().pl_5().child(right))
    }
}

/// Opens the Accounts dialog.
pub fn open(store: Entity<Store>, window: &mut Window, cx: &mut App) {
    store.update(cx, |s, _| s.reload_accounts());
    let view = cx.new(|cx| AccountsView::new(store, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Accounts")
            .w(rems(58.).to_pixels(window.rem_size()))
            .child(view.clone())
    });
}

#[cfg(test)]
mod tests {
    use super::dashed;

    #[test]
    fn uuids_get_dashes() {
        assert_eq!(
            dashed("069a79f444e94726a5befca90e38aaf5"),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
        assert_eq!(dashed("short"), "short");
    }
}
