//! "Relay through a VPS" setup on a paired Hopper Server (design/vps-relay.md,
//! B8): the web dialog's five steps (Before you start, Connect, Check,
//! Install, Test) as a desktop dialog. The setup runs on the panel; this
//! only drives its `/vps-relay/setup*` routes and shows what they say, with
//! the web dialog's words. The ssh key, password and passphrase are typed
//! here, sent once over the paired (pinned TLS) connection and cleared from
//! the boxes as soon as the panel has them; nothing is saved on this PC.

use crate::relay_card::{mark, running_mark};
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::form::Field;
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hopperd::{Client, Remote};
use hopper::relay_view::Mark;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Before,
    Connect,
    Check,
    Run,
    Test,
}

/// How the owner signs in to the VPS.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SignIn {
    Saved,
    Key,
    Password,
    NewKey,
}

/// The VPS's host key as the panel read it (nothing was sent to the VPS),
/// waiting for the owner to check it against the provider.
#[derive(Clone)]
struct HostCheck {
    host_key: String,
    /// The key saved for this same address, "" if none.
    known: String,
    /// It differs from the saved one: the VPS was reinstalled, or something
    /// else answers there.
    changed: bool,
}

/// What a sign-in attempt came to.
enum Signed {
    /// The panel wants the owner to check this host key first.
    Check(HostCheck),
    /// Signed in; the panel's state with its first check.
    In(Value),
}

/// Run in the VPS's console: prints every host key it has.
const CHECK_CMD: &str = "for f in /etc/ssh/ssh_host_*_key.pub; do ssh-keygen -lf \"$f\"; done";

/// A question asked in place of the buttons.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ask {
    Remove,
    Forget,
}

pub struct RelaySetup {
    remote: Remote,
    /// The panel's name in Settings, for the words ("Hopper Server sends the game…").
    server_id: String,
    server_name: String,
    st: Option<Value>,
    stage: Stage,
    busy: Option<&'static str>,
    err: String,
    show_cmds: bool,
    sign: SignIn,
    key_name: String,
    host_check: Option<HostCheck>,
    reinstalled: bool,
    show_how: bool,
    ask: Option<Ask>,
    ask_tick: bool,
    test: Option<Value>,
    host: Entity<InputState>,
    ssh_port: Entity<InputState>,
    user: Entity<InputState>,
    key: Entity<TextareaState>,
    passphrase: Entity<InputState>,
    password: Entity<InputState>,
    game_port: Entity<InputState>,
    tunnel_port: Entity<InputState>,
    public_address: Entity<InputState>,
    polling: bool,
}

/// A string field of a JSON object, "" when missing.
fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or_default().to_string()
}

fn rows(v: &Value) -> Vec<Value> {
    v.get("rows").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The mark in front of a row: the ones of the card, plus the setup's
/// still-running, skipped and waiting steps.
fn row_mark(state: &str, cx: &App) -> AnyElement {
    match state {
        "ok" | "done" => mark(Mark::Ok, cx).into_any_element(),
        "warn" => mark(Mark::Warn, cx).into_any_element(),
        "fail" => mark(Mark::Fail, cx).into_any_element(),
        "run" => running_mark(cx).into_any_element(),
        other => div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.))
            .text_color(cx.theme().muted_foreground)
            .child(Icon::new(if other == "skip" { IconName::Minus } else { IconName::Circle }).size(px(9.)))
            .into_any_element(),
    }
}

/// Opens the setup for the server `server_id` on `remote`.
pub fn open(_store: Entity<Store>, remote: Remote, server_name: String, window: &mut Window, cx: &mut App) {
    let server_id = remote.id.clone();
    let form = cx.new(|cx| RelaySetup::new(remote, server_id, server_name, window, cx));
    window.open_dialog(cx, move |dialog, window, _| {
        dialog
            .title("Relay through a VPS")
            .w(rems(40.).to_pixels(window.rem_size()))
            .margin_top(rems(widgets::DIALOG_TOP).to_pixels(window.rem_size()))
            .child(form.clone())
    });
}

impl RelaySetup {
    fn new(remote: Remote, server_id: String, server_name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = |placeholder: &str, value: &str, masked: bool, window: &mut Window, cx: &mut Context<Self>| {
            let (p, v) = (placeholder.to_string(), value.to_string());
            cx.new(|cx| InputState::new(window, cx).masked(masked).placeholder(p).default_value(v))
        };
        let host = input("203.0.113.10", "", false, window, cx);
        let ssh_port = input("22", "22", false, window, cx);
        let user = input("ubuntu, debian or root", "", false, window, cx);
        let passphrase = input("Only if the key has one", "", true, window, cx);
        let password = input("", "", true, window, cx);
        let game_port = input("25565", "25565", false, window, cx);
        let tunnel_port = input("25566", "25566", false, window, cx);
        let public_address = input("Optional: a name like play.example.com", "", false, window, cx);
        let key = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 6)
                .placeholder("Paste the key (-----BEGIN …) or choose a file")
        });
        let mut this = Self {
            remote,
            server_id,
            server_name,
            st: None,
            stage: Stage::Before,
            busy: None,
            err: String::new(),
            show_cmds: false,
            sign: SignIn::Key,
            key_name: String::new(),
            host_check: None,
            reinstalled: false,
            show_how: false,
            ask: None,
            ask_tick: false,
            test: None,
            host,
            ssh_port,
            user,
            key,
            passphrase,
            password,
            game_port,
            tunnel_port,
            public_address,
            polling: false,
        };
        this.load(window, cx);
        this
    }

    // ---- what the panel said --------------------------------------------------

    fn null(&self) -> Value {
        Value::Null
    }

    fn login(&self) -> Value {
        self.st.as_ref().and_then(|v| v.get("login").cloned()).filter(|l| l.is_object()).unwrap_or_else(|| self.null())
    }

    fn has_login(&self) -> bool {
        self.login().is_object()
    }

    fn check(&self) -> Value {
        self.st.as_ref().and_then(|v| v.get("check").cloned()).filter(|c| c.is_object()).unwrap_or_else(|| self.null())
    }

    fn run(&self) -> Value {
        self.st.as_ref().and_then(|v| v.get("run").cloned()).filter(|c| c.is_object()).unwrap_or_else(|| self.null())
    }

    fn running(&self) -> bool {
        self.run()["running"].as_bool().unwrap_or(false)
    }

    fn removing(&self) -> bool {
        self.run()["kind"].as_str() == Some("remove")
    }

    fn failed(&self) -> bool {
        let r = self.run();
        r.is_object() && !self.running() && r["error"].as_str().is_some_and(|e| !e.is_empty())
    }

    fn finished(&self) -> bool {
        self.run().is_object() && !self.running() && !self.failed()
    }

    fn stopped(&self) -> bool {
        self.check()["stop"].as_bool().unwrap_or(false)
    }

    fn paired(&self) -> bool {
        self.st.as_ref().is_some_and(|v| v["paired"]["address"].as_str().is_some_and(|a| !a.is_empty()))
    }

    fn secrets_cleared(&self, window: &mut Window, cx: &mut Context<Self>) {
        // Nothing secret stays in a box once the panel has it.
        self.key.update(cx, |k, cx| k.set_value("", window, cx));
        self.passphrase.update(cx, |k, cx| k.set_value("", window, cx));
        self.password.update(cx, |k, cx| k.set_value("", window, cx));
    }

    fn port(&self, i: &Entity<InputState>, cx: &App) -> u64 {
        i.read(cx).value().trim().parse::<u64>().unwrap_or(0)
    }

    /// Where players join, said when the relay runs.
    fn join_at(&self, cx: &App) -> String {
        let st = self.st.as_ref();
        let public = st.map(|v| s(&v["paired"], "public_address")).unwrap_or_default();
        if !public.is_empty() {
            return public;
        }
        let l = self.login();
        let game = self.check()["ports"]["game"].as_u64().unwrap_or(0);
        let _ = cx;
        format!("{}{}", s(&l, "host"), if game != 0 && game != 25565 { format!(":{game}") } else { String::new() })
    }

    fn can_connect(&self, cx: &App) -> bool {
        let filled = |i: &Entity<InputState>| !i.read(cx).value().trim().is_empty();
        filled(&self.host)
            && filled(&self.user)
            && match self.sign {
                SignIn::Key => !self.key.read(cx).value().trim().is_empty(),
                SignIn::Password => !self.password.read(cx).value().is_empty(),
                SignIn::NewKey => self.st.as_ref().is_some_and(|v| !s(v, "new_public").is_empty()),
                SignIn::Saved => false,
            }
    }

    fn ok_to_install(&self, cx: &App) -> bool {
        let c = self.check();
        c["ok"].as_bool().unwrap_or(false)
            && self.port(&self.game_port, cx) == c["ports"]["game"].as_u64().unwrap_or(0)
            && self.port(&self.tunnel_port, cx) == c["ports"]["tunnel"].as_u64().unwrap_or(0)
    }

    // ---- calls ------------------------------------------------------------------

    /// One call to the panel off the UI thread; `done` gets its answer, or
    /// the error is shown in the dialog.
    fn act<T: Send + 'static>(
        &mut self,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl FnOnce(&Client) -> anyhow::Result<T> + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        if self.busy.is_some() {
            return;
        }
        let client = match Client::for_remote(&self.remote) {
            Ok(c) => c,
            Err(e) => {
                self.err = format!("{e:#}");
                cx.notify();
                return;
            }
        };
        self.busy = Some(what);
        self.err.clear();
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx.background_spawn(async move { work(&client).map_err(|e| format!("{e:#}")) }).await;
            this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match r {
                    Ok(v) => done(this, v, window, cx),
                    Err(e) => this.err = e,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Reads the setup's state; the first time it also picks the stage the
    /// dialog opens on: a run in progress or just ended, a saved login to
    /// check, or what to rent first.
    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.act("load", window, cx, |c| c.get("/vps-relay/setup"), |this, st, window, cx| {
            this.st = Some(st);
            let l = this.login();
            if l.is_object() {
                this.host.update(cx, |i, cx| i.set_value(s(&l, "host"), window, cx));
                this.ssh_port.update(cx, |i, cx| i.set_value(l["port"].as_u64().filter(|p| *p != 0).unwrap_or(22).to_string(), window, cx));
                this.user.update(cx, |i, cx| i.set_value(s(&l, "user"), window, cx));
                this.sign = SignIn::Saved;
            }
            let st = this.st.clone().unwrap_or_default();
            let game = st["check"]["ports"]["game"].as_u64().or(st["ports"]["game"].as_u64()).unwrap_or(25565);
            let tunnel = st["check"]["ports"]["tunnel"].as_u64().or(st["ports"]["tunnel"].as_u64()).unwrap_or(25566);
            this.game_port.update(cx, |i, cx| i.set_value(game.to_string(), window, cx));
            this.tunnel_port.update(cx, |i, cx| i.set_value(tunnel.to_string(), window, cx));
            this.public_address.update(cx, |i, cx| i.set_value(s(&st["paired"], "public_address"), window, cx));
            this.stage = if this.run().is_object() {
                Stage::Run
            } else if l.is_object() {
                Stage::Check
            } else {
                Stage::Before
            };
            if this.stage == Stage::Check && !this.check().is_object() {
                this.run_check(window, cx);
            }
            this.follow_run(cx);
        });
    }

    /// Follows a run while it goes (the panel keeps going if this closes).
    fn follow_run(&mut self, cx: &mut Context<Self>) {
        if !self.running() || self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(700)).await;
                let Ok(remote) = this.update(cx, |t, _| t.remote.clone()) else { return };
                let r = cx.background_spawn(async move { Client::for_remote(&remote)?.get("/vps-relay/setup") }).await;
                let go = this
                    .update(cx, |t, cx| {
                        if let Ok(v) = r {
                            t.st = Some(v);
                        }
                        let go = t.running();
                        t.polling = go;
                        cx.notify();
                        go
                    })
                    .unwrap_or(false);
                if !go {
                    return;
                }
            }
        })
        .detach();
    }

    fn to_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.stage = Stage::Connect;
        self.err.clear();
        let h = self.host.clone();
        h.update(cx, |i, cx| i.focus(window, cx));
        cx.notify();
    }

    fn new_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.act("newkey", window, cx, |c| c.post("/vps-relay/setup/new-key", json!({})), |this, r, _, _| {
            if let Some(st) = this.st.as_mut() {
                st["new_public"] = r["public_key"].clone();
            }
        });
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sign_in(None, window, cx);
    }

    /// The owner checked the host key shown and confirms it.
    fn confirm_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let confirmed = self.host_check.clone();
        self.sign_in(confirmed, window, cx);
    }

    /// Signs in. Without a confirmed host key the panel refuses before it
    /// sends anything to the VPS; this then asks it to read the key (still
    /// sending nothing) and shows it for the owner to check first.
    fn sign_in(&mut self, confirmed: Option<HostCheck>, window: &mut Window, cx: &mut Context<Self>) {
        let text = |i: &Entity<InputState>| i.read(cx).value().trim().to_string();
        let (host, user) = (text(&self.host), text(&self.user));
        let port = text(&self.ssh_port).parse::<u64>().ok().filter(|p| *p != 0).unwrap_or(22);
        let mut body = json!({ "host": host, "port": port, "user": user });
        match self.sign {
            SignIn::Saved => {
                self.err = "Pick how to sign in.".into();
                cx.notify();
                return;
            }
            SignIn::Key => {
                body["mode"] = json!("key");
                body["key"] = json!(self.key.read(cx).value().to_string());
                body["passphrase"] = json!(self.passphrase.read(cx).value().to_string());
            }
            SignIn::Password => {
                body["mode"] = json!("password");
                body["password"] = json!(self.password.read(cx).value().to_string());
            }
            SignIn::NewKey => body["mode"] = json!("new"),
        }
        if let Some(c) = &confirmed {
            body["host_key"] = json!(c.host_key);
            body["trust_new"] = json!(c.changed);
        }
        let probe = json!({ "host": host, "port": port });
        let ports = json!({ "game_port": self.port(&self.game_port, cx), "tunnel_port": self.port(&self.tunnel_port, cx) });
        self.act(
            "connect",
            window,
            cx,
            move |c| {
                if let Err(e) = c.post_wait("/vps-relay/setup/connect", body, Duration::from_secs(90)) {
                    let m = format!("{e:#}").to_lowercase();
                    if confirmed.is_none() && (m.contains("host key first") || m.contains("host key changed")) {
                        let r = c.post_wait("/vps-relay/setup/host-key", probe, Duration::from_secs(40))?;
                        let (host_key, known) = (s(&r, "host_key"), s(&r, "known"));
                        let changed = !known.is_empty() && known != host_key;
                        return Ok(Signed::Check(HostCheck { host_key, known, changed }));
                    }
                    return Err(e);
                }
                let mut st = c.get("/vps-relay/setup")?;
                st["check"] = c.post_wait("/vps-relay/setup/check", ports, Duration::from_secs(120))?;
                Ok(Signed::In(st))
            },
            |this, out, window, cx| match out {
                Signed::Check(h) => {
                    this.host_check = Some(h);
                    this.reinstalled = false;
                    this.show_how = false;
                }
                Signed::In(st) => {
                    this.secrets_cleared(window, cx);
                    this.host_check = None;
                    this.reinstalled = false;
                    this.key_name.clear();
                    this.st = Some(st);
                    this.stage = Stage::Check;
                    cx.notify();
                }
            },
        );
    }

    fn run_check(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = json!({ "game_port": self.port(&self.game_port, cx), "tunnel_port": self.port(&self.tunnel_port, cx) });
        self.act("check", window, cx, move |c| c.post_wait("/vps-relay/setup/check", body, Duration::from_secs(120)), |this, r, _, _| {
            if let Some(st) = this.st.as_mut() {
                st["check"] = r;
            }
        });
    }

    fn install(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let body = json!({
            "server": self.server_id,
            "game_port": self.port(&self.game_port, cx),
            "tunnel_port": self.port(&self.tunnel_port, cx),
            "public_address": self.public_address.read(cx).value().trim().to_string(),
        });
        self.act("install", window, cx, move |c| c.post("/vps-relay/setup/install", body), |this, st, _, cx| {
            this.st = Some(st);
            this.stage = Stage::Run;
            this.show_cmds = false;
            this.follow_run(cx);
        });
    }

    fn remove(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask = None;
        self.ask_tick = false;
        self.act("remove", window, cx, |c| c.post("/vps-relay/setup/remove", json!({})), |this, st, _, cx| {
            this.st = Some(st);
            this.stage = Stage::Run;
            this.show_cmds = false;
            this.follow_run(cx);
        });
    }

    fn forget(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ask = None;
        self.act("forget", window, cx, |c| c.post("/vps-relay/setup/forget", json!({})), |this, st, _, _| {
            this.st = Some(st);
            this.sign = SignIn::Key;
            this.stage = Stage::Connect;
        });
    }

    fn run_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = Client::server_path(&self.server_id, "/vps-relay/test");
        self.stage = Stage::Test;
        self.test = None;
        self.act("test", window, cx, move |c| c.post_wait(&path, json!({}), Duration::from_secs(60)), |this, r, _, _| {
            this.test = Some(r);
        });
    }

    /// Back to Check from a finished run (Run again).
    fn again(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.err.clear();
        self.stage = Stage::Check;
        self.run_check(window, cx);
    }

    fn pick_key_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("The VPS's private key".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let read = cx
                .background_spawn(async move {
                    let meta = std::fs::metadata(&path)?;
                    // A private key is a few KB; anything big is the wrong file.
                    if meta.len() > 64 * 1024 {
                        anyhow::bail!("That file is too big to be an ssh key. Pick the private key (like id_ed25519), not the .pub one.");
                    }
                    Ok((std::fs::read_to_string(&path)?, path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                match read {
                    Ok((text, name)) => {
                        this.key.update(cx, |k, cx| k.set_value(text, window, cx));
                        this.key_name = name;
                    }
                    Err(e) => this.err = format!("{e:#}"),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- pieces -----------------------------------------------------------------

    fn stepper(&self, cx: &App) -> Div {
        let theme = cx.theme();
        let at = match self.stage {
            Stage::Before => 1,
            Stage::Connect => 2,
            Stage::Check => 3,
            Stage::Run => 4,
            Stage::Test => 5,
        };
        let fourth = if self.stage == Stage::Run && self.removing() { "Remove" } else { "Install" };
        let names = ["Before you start", "Connect", "Check", fourth, "Test"];
        let test_ok = self.test.as_ref().is_some_and(|t| t["ok"].as_bool().unwrap_or(false));
        h_flex().gap_3().flex_wrap().children(names.iter().enumerate().map(|(i, name)| {
            let n = i + 1;
            let done = n < at || (n == 4 && at == 4 && self.finished()) || (n == 5 && test_ok);
            let bad = n == 3 && at == 3 && self.stopped();
            let on = n == at && !done;
            let (bg, fg) = if done {
                (theme.success.opacity(0.22), crate::theme::chip_ink(theme.success))
            } else if bad {
                (theme.danger.opacity(0.22), crate::theme::chip_ink(theme.danger))
            } else if on {
                (theme.primary, theme.primary_foreground)
            } else {
                (theme.muted_foreground.opacity(0.18), theme.muted_foreground)
            };
            h_flex()
                .gap_1p5()
                .items_center()
                .text_xs()
                .text_color(if on || done { theme.foreground } else { theme.muted_foreground })
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(18.))
                        .rounded_full()
                        .bg(bg)
                        .text_color(fg)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(if done {
                            Icon::new(IconName::Check).size(px(11.)).into_any_element()
                        } else if bad {
                            "!".into_any_element()
                        } else {
                            n.to_string().into_any_element()
                        }),
                )
                .child(*name)
        }))
    }

    fn who(&self, text: impl IntoElement, change: Option<AnyElement>) -> Div {
        h_flex().gap_2().items_center().text_sm().child(div().flex_1().min_w_0().child(text)).children(change)
    }

    fn err_note(&self, cx: &App) -> Option<Div> {
        (!self.err.is_empty()).then(|| {
            let theme = cx.theme();
            div()
                .px_3()
                .py_2()
                .rounded(theme.radius)
                .text_xs()
                .bg(theme.danger.opacity(widgets::alpha::TINT))
                .child(self.err.clone())
        })
    }

    fn note(&self, text: impl Into<SharedString>, good: bool, cx: &App) -> Div {
        let theme = cx.theme();
        div()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .text_xs()
            .bg(if good { theme.success.opacity(widgets::alpha::TINT) } else { theme.foreground.opacity(widgets::alpha::HOVER) })
            .child(text.into())
    }

    fn host_key(&self, cx: &App) -> Option<Div> {
        let l = self.login();
        let k = s(&l, "host_key");
        (!k.is_empty()).then(|| {
            h_flex()
                .gap_1p5()
                .items_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(Icon::new(IconName::KeyRound).size(px(12.)))
                .child("Host key")
                .child(div().font_family("Consolas").min_w_0().truncate().child(k))
        })
    }

    fn commands(&self, list: Vec<String>, cx: &mut Context<Self>) -> Option<Div> {
        if list.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let show = self.show_cmds;
        Some(
            v_flex()
                .gap_1()
                .child(
                    h_flex().child(
                        Button::new("relay-cmds")
                            .ghost()
                            .xsmall()
                            .label(if show { "Hide the commands" } else { "Show the commands" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_cmds = !this.show_cmds;
                                cx.notify();
                            })),
                    ),
                )
                .when(show, |c| {
                    c.child(
                        div()
                            .px_3()
                            .py_2()
                            .rounded(theme.radius)
                            .bg(theme.muted)
                            .text_xs()
                            .font_family("Consolas")
                            .child(list.join("\n")),
                    )
                }),
        )
    }

    /// The list of check, run or test rows.
    fn row_list(&self, items: Vec<(String, String, Option<String>)>, stale: bool, cx: &App) -> Div {
        let theme = cx.theme();
        v_flex()
            .gap_1p5()
            .when(stale, |c| c.opacity(0.6))
            .children(items.into_iter().map(|(state, text, detail)| {
                let wait = matches!(state.as_str(), "wait" | "skip");
                h_flex()
                    .gap_2()
                    .items_start()
                    .text_sm()
                    .when(wait, |r| r.text_color(theme.muted_foreground))
                    .child(row_mark(&state, cx))
                    .child(div().min_w_0().flex_1().child(text))
                    .children(detail.filter(|d| !d.is_empty()).map(|d| div().flex_none().max_w(rems(14.)).truncate().text_xs().text_color(theme.muted_foreground).child(d)))
            }))
    }

    fn input_row(&self, label: &'static str, child: impl IntoElement) -> Field {
        Field::new().label(label).child(div().w(rems(26.)).child(child))
    }

    // ---- stages -------------------------------------------------------------------

    fn before(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let need = |k: &'static str, v: &'static str| {
            h_flex()
                .gap_3()
                .items_start()
                .text_sm()
                .child(div().w(rems(4.5)).flex_none().text_color(theme.muted_foreground).child(k))
                .child(div().flex_1().min_w_0().child(v))
        };
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(theme.muted_foreground).child("Players join a small rented server (a VPS) instead of this PC. Hopper Server sends the game to it over one connection it opens itself, so nothing needs opening at home."))
            .child(
                v_flex()
                    .gap_1p5()
                    .child(need("System", "Debian 12 or 13, or Ubuntu 22.04 or 24.04. Nothing else works yet."))
                    .child(need("Size", "2 GB memory or more (4 GB keeps a bigger cache), 2 GB free disk, x64 or arm64"))
                    .child(need("Address", "A public IPv4 address"))
                    .child(need("Ports", "Open in the provider's firewall: 25565 TCP players, 25566 TCP Hopper Server, 24454 UDP voice chat (only with a voice mod)"))
                    .child(need("Login", "ssh as a user with sudo (cloud images' default user, or root)")),
            )
            .child(widgets::fine_print("Any provider works. Pick one near your players: their ping goes to the VPS, then on to Hopper Server.", cx))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("rs-cancel").outline().label("Cancel").on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("rs-to-connect")
                            .primary()
                            .label("I have one: connect")
                            .on_click(cx.listener(|this, _, window, cx| this.to_connect(window, cx))),
                    ),
            )
    }

    /// A host key in a box, with a Copy button when `copy` names it.
    fn key_box(&self, label: &'static str, key: String, copy: Option<&'static str>, cx: &App) -> Div {
        let theme = cx.theme();
        let k = key.clone();
        v_flex().gap_1().child(widgets::fine_print(label, cx)).child(
            h_flex()
                .gap_2()
                .items_center()
                .px_3()
                .py_1p5()
                .rounded(theme.radius)
                .border_1()
                .border_color(theme.border)
                .child(div().flex_1().min_w_0().text_xs().font_family("Consolas").child(key))
                .children(copy.map(|id| {
                    Button::new(id).small().outline().icon(IconName::Copy).label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(k.clone())))
                })),
        )
    }

    /// The step between Connect and signing in: the owner checks the host
    /// key the VPS showed, with nothing sent to it yet.
    fn host_check_stage(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let _ = window;
        let Some(hc) = self.host_check.clone() else { return div() };
        let busy = self.busy.is_some();
        let theme = cx.theme().clone();
        let host = self.host.read(cx).value().trim().to_string();
        let changed = hc.changed;
        let show_how = self.show_how;
        v_flex()
            .gap_3()
            .child(if changed {
                self.note(
                    "This VPS shows a different host key than before. That is normal if you reinstalled it. If you didn't, something else may be answering at this address: don't continue.",
                    false,
                    cx,
                )
                .bg(theme.danger.opacity(widgets::alpha::TINT))
            } else {
                div().text_sm().text_color(theme.muted_foreground).child(format!(
                    "Hopper reached {host}. Before it signs in, make sure this host key matches the one your VPS provider shows (often on the VPS's page or in its console). If it doesn't, something else is answering at this address: stop and check the address."
                ))
            })
            .child(self.key_box(if changed { "Host key now" } else { "Host key" }, hc.host_key.clone(), Some("rs-copy-hk"), cx))
            .when(changed, |c| c.child(self.key_box("Saved before", hc.known.clone(), None, cx)))
            .child(widgets::fine_print("Nothing has been sent to the VPS yet: the sign-in only goes out after you confirm.", cx))
            .child(
                h_flex().child(
                    Button::new("rs-how")
                        .ghost()
                        .xsmall()
                        .label(if show_how { "Hide how to check" } else { "How do I check this?" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_how = !this.show_how;
                            cx.notify();
                        })),
                ),
            )
            .when(show_how, |c| {
                c.child(
                    v_flex()
                        .gap_1p5()
                        .child(widgets::fine_print("Open the VPS's web console (your provider's page has one) and run this. One of the lines it prints is this key:", cx))
                        .child(div().px_3().py_2().rounded(theme.radius).bg(theme.foreground.opacity(widgets::alpha::HOVER)).text_xs().font_family("Consolas").child(CHECK_CMD))
                        .child(h_flex().child(
                            Button::new("rs-copy-cmd")
                                .small()
                                .outline()
                                .icon(IconName::Copy)
                                .label("Copy command")
                                .on_click(|_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(CHECK_CMD.to_string()))),
                        )),
                )
            })
            .when(changed, |c| {
                c.child(
                    Checkbox::new("rs-reinstalled")
                        .label("I reinstalled the VPS: trust this new host key")
                        .checked(self.reinstalled)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.reinstalled = *v;
                            cx.notify();
                        })),
                )
            })
            .children(self.err_note(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("rs-hk-back")
                            .ghost()
                            .label("Back")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.host_check = None;
                                this.err.clear();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(Button::new("rs-hk-cancel").outline().label("Cancel").disabled(busy).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("rs-hk-go")
                            .primary()
                            .label(if self.busy == Some("connect") {
                                "Signing in…"
                            } else if changed {
                                "Trust the new key and sign in"
                            } else {
                                "It matches: sign in"
                            })
                            .loading(self.busy == Some("connect"))
                            .disabled(busy || (changed && !self.reinstalled))
                            .on_click(cx.listener(|this, _, window, cx| this.confirm_key(window, cx))),
                    ),
            )
    }

    fn connect_stage(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let rem = window.rem_size();
        let busy = self.busy.is_some();
        let has_login = self.has_login();
        let login = self.login();
        let sign = self.sign;
        let mut items: Vec<(SignIn, &'static str)> = Vec::new();
        if has_login {
            items.push((SignIn::Saved, "Saved key"));
        }
        items.extend([(SignIn::Key, "Key file"), (SignIn::Password, "Password"), (SignIn::NewKey, "New key")]);
        let selected = items.iter().position(|(m, _)| *m == sign).unwrap_or(0);
        let modes: Vec<SignIn> = items.iter().map(|(m, _)| *m).collect();
        let public = self.st.as_ref().map(|v| s(v, "new_public")).unwrap_or_default();
        let note = if sign == SignIn::Password {
            "The password is used once, to put a new key on the VPS, and isn't kept. Hopper Server keeps that key for repair and removal."
        } else {
            "Hopper Server keeps the key, readable by the owner only, to repair or remove the relay later. The user needs sudo."
        };
        let can = self.can_connect(cx);
        let theme = cx.theme().clone();
        v_flex()
            .gap_3()
            .child(div().text_sm().text_color(theme.muted_foreground).child("Players join the VPS; Hopper Server sends the game to it over one connection it opens itself, so nothing needs opening at home. Hopper signs in to the VPS over ssh to set it up."))
            .child(
                crate::java_form::settings_form(rem)
                    .child(
                        Field::new().label("VPS address").child(
                            h_flex()
                                .gap_2()
                                .w(rems(26.))
                                .child(div().flex_1().child(Input::new(&self.host)))
                                .child(div().w(rems(6.)).child(Input::new(&self.ssh_port).prefix(div().text_xs().text_color(theme.muted_foreground).child("port")))),
                        ),
                    )
                    .child(self.input_row("ssh user", Input::new(&self.user)))
                    .child(
                        Field::new().label("Sign in with").child(
                            widgets::Segmented::new("rs-sign")
                                .selected_index(selected)
                                .children(items.iter().map(|(_, l)| widgets::SegItem::new().label(*l)))
                                .on_click(cx.listener(move |this, ix: &usize, window, cx| {
                                    this.sign = modes[*ix];
                                    this.err.clear();
                                    if this.sign == SignIn::NewKey && this.st.as_ref().is_none_or(|v| s(v, "new_public").is_empty()) {
                                        this.new_key(window, cx);
                                    }
                                    cx.notify();
                                })),
                        ),
                    )
                    .when(sign == SignIn::Key, |f| {
                        f.child(Field::new().label("Private key").child(
                            v_flex()
                                .gap_1p5()
                                .w(rems(26.))
                                .child(Textarea::new(&self.key))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(
                                            Button::new("rs-key-file")
                                                .small()
                                                .outline()
                                                .label("Choose file…")
                                                .on_click(cx.listener(|this, _, window, cx| this.pick_key_file(window, cx))),
                                        )
                                        .when(!self.key_name.is_empty(), |r| r.child(widgets::caption(self.key_name.clone(), cx).text_xs())),
                                ),
                        ))
                        .child(self.input_row("Passphrase", Input::new(&self.passphrase)))
                    })
                    .when(sign == SignIn::Password, |f| f.child(self.input_row("Password", Input::new(&self.password))))
                    .when(sign == SignIn::NewKey, |f| {
                        f.child(Field::new().label("Public key").child(if public.is_empty() {
                            div().w(rems(26.)).child(widgets::fine_print("Making a key…", cx)).into_any_element()
                        } else {
                            let p = public.clone();
                            v_flex()
                                .gap_1p5()
                                .w(rems(26.))
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_start()
                                        .child(div().flex_1().min_w_0().text_xs().font_family("Consolas").child(public.clone()))
                                        .child(
                                            Button::new("rs-copy-pub")
                                                .small()
                                                .outline()
                                                .icon(IconName::Copy)
                                                .label("Copy")
                                                .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(p.clone()))),
                                        ),
                                )
                                .child(widgets::fine_print("Add it to the VPS first: your cloud's ssh keys page, or a line in ~/.ssh/authorized_keys. Then press Connect.", cx))
                                .into_any_element()
                        }))
                    })
                    .when(sign == SignIn::Saved, |f| {
                        f.child(Field::new().label("").child(
                            div().w(rems(26.)).child(widgets::fine_print(
                                format!("The key Hopper Server saved for {}@{}. To change it, pick another way.", s(&login, "user"), s(&login, "host")),
                                cx,
                            )),
                        ))
                    }),
            )
            .children(self.host_key(cx))
            .child(self.note(note, false, cx).text_color(theme.muted_foreground))
            .children(self.err_note(cx))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("rs-back")
                            .ghost()
                            .label("Back")
                            .disabled(busy)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stage = if has_login { Stage::Check } else { Stage::Before };
                                if has_login {
                                    this.sign = SignIn::Saved;
                                }
                                this.err.clear();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(Button::new("rs-cancel").outline().label("Cancel").disabled(busy).on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(
                        Button::new("rs-connect")
                            .primary()
                            .label(if self.busy == Some("connect") { "Connecting…" } else { "Connect" })
                            .loading(self.busy == Some("connect"))
                            .disabled(busy || !can)
                            .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                    ),
            )
    }

    fn check_stage(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let rem = window.rem_size();
        let busy = self.busy.is_some();
        let login = self.login();
        let check = self.check();
        let stopped = self.stopped();
        let port_busy = |name: &str, i: &Entity<InputState>, cx: &App| -> bool {
            let v = i.read(cx).value().trim().parse::<u64>().unwrap_or(0);
            rows(&check).iter().find(|r| r["id"] == "ports").is_some_and(|r| {
                let _ = name;
                r["busy"].as_array().is_some_and(|b| b.iter().any(|x| x.as_u64() == Some(v)))
            })
        };
        let _ = port_busy;
        let version = self.st.as_ref().map(|v| s(v, "version")).unwrap_or_default();
        let source_file = self.st.as_ref().is_some_and(|v| s(v, "source") == "file");
        let paired = self.paired();
        let ok_install = self.ok_to_install(cx);
        let cmds = check["commands"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
        let theme = cx.theme().clone();
        let port_txt = match login["port"].as_u64() {
            Some(p) if p != 0 && p != 22 => format!(" (port {p})"),
            _ => String::new(),
        };
        v_flex()
            .gap_3()
            .child(self.who(
                div().child(format!("Signed in to {}@{}{port_txt}", s(&login, "user"), s(&login, "host"))).font_family("Consolas"),
                Some(
                    Button::new("rs-change")
                        .ghost()
                        .small()
                        .label("Change")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.err.clear();
                            if this.sign == SignIn::Saved {
                                this.sign = SignIn::Key;
                            }
                            this.stage = Stage::Connect;
                            cx.notify();
                        }))
                        .into_any_element(),
                ),
            ))
            .children(self.host_key(cx))
            .child(if self.busy == Some("check") && !check.is_object() {
                h_flex().gap_2().items_center().text_xs().text_color(theme.muted_foreground).child(running_mark(cx)).child("Checking the VPS…").into_any_element()
            } else if check.is_object() {
                let mut items: Vec<(String, String, Option<String>)> = rows(&check).iter().map(|r| (s(r, "state"), s(r, "text"), None)).collect();
                if stopped {
                    items.push(("skip".into(), "Memory, disk, sudo and ports: not checked".into(), None));
                }
                self.row_list(items, self.busy == Some("check"), cx).into_any_element()
            } else {
                div().into_any_element()
            })
            .when(stopped, |c| {
                c.child(self.note(
                    "Reinstall the VPS with Debian 12 or 13, or Ubuntu 22.04 or 24.04 (on the provider's page this is often called Rebuild or Reinstall), then connect again. Nothing was changed on the VPS.",
                    false,
                    cx,
                ).bg(theme.danger.opacity(widgets::alpha::TINT)))
            })
            .when(!stopped, |c| {
                c.child(
                    crate::java_form::settings_form(rem)
                        .child(
                            Field::new().label("Players' port").child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .w(rems(26.))
                                    .child(div().w(rems(7.)).child(Input::new(&self.game_port)))
                                    .child(div().text_sm().text_color(theme.muted_foreground).child("Tunnel port"))
                                    .child(div().w(rems(7.)).child(Input::new(&self.tunnel_port))),
                            ),
                        )
                        .child(self.input_row("Players join at", Input::new(&self.public_address))),
                )
                .child(widgets::fine_print(
                    format!(
                        "Installs Hopper Server {version} in relay mode on the VPS, as its own service and user, and opens the two ports in its firewall. {}",
                        if source_file { "Test build: the program comes from this PC." } else { "The program comes from the Hopper Server release on GitHub, checked against its checksum." }
                    ),
                    cx,
                ))
            })
            .children(self.err_note(cx))
            .children(self.commands(cmds, cx))
            .child(match self.ask {
                Some(a) => self.ask_block(a, cx).into_any_element(),
                None => h_flex()
                    .gap_2()
                    .when(self.has_login() && paired, |r| {
                        r.child(
                            Button::new("rs-remove")
                                .ghost()
                                .label("Remove the relay…")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.ask = Some(Ask::Remove);
                                    this.ask_tick = false;
                                    cx.notify();
                                })),
                        )
                    })
                    .when(self.has_login(), |r| {
                        r.child(
                            Button::new("rs-forget")
                                .ghost()
                                .label("Forget this VPS")
                                .tooltip("Forget the VPS here without connecting to it (it was deleted)")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    // Nothing on the VPS changes: say so before the login is gone.
                                    if this.paired() {
                                        this.ask = Some(Ask::Forget);
                                        cx.notify();
                                    } else {
                                        this.forget(window, cx);
                                    }
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("rs-check")
                            .outline()
                            .label(if self.busy == Some("check") { "Checking…" } else { "Check again" })
                            .loading(self.busy == Some("check"))
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.run_check(window, cx))),
                    )
                    .when(!stopped, |r| {
                        r.child(
                            Button::new("rs-install")
                                .primary()
                                .label(if self.busy == Some("install") { "Starting…" } else if paired { "Run again" } else { "Install" })
                                .loading(self.busy == Some("install"))
                                .disabled(busy || !ok_install)
                                .tooltip(if ok_install {
                                    ""
                                } else if check["ok"].as_bool().unwrap_or(false) {
                                    "The ports changed: check again first"
                                } else {
                                    "Fix what the check found first"
                                })
                                .on_click(cx.listener(|this, _, window, cx| this.install(window, cx))),
                        )
                    })
                    .into_any_element(),
            })
    }

    /// The question in place of the buttons: remove the relay (a tick box
    /// is the deliberate step), or forget the VPS.
    fn ask_block(&self, a: Ask, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let host = s(&self.login(), "host");
        let (title, body, ok) = match a {
            Ask::Remove => (
                "Remove the relay from the VPS?",
                format!("Stops the relay on {host}, deletes its program, keys and user, takes out the firewall rules it added and disconnects Hopper Server from it. Players can't join through the VPS any more. Nothing else on the VPS changes."),
                "Remove the relay",
            ),
            Ask::Forget => (
                "Forget this VPS?",
                format!("Disconnects Hopper Server and forgets the login for {host}, without connecting to it. Whatever the setup put on the VPS stays there; use Remove the relay to take it off."),
                "Forget it",
            ),
        };
        let ready = a == Ask::Forget || self.ask_tick;
        v_flex()
            .gap_2()
            .px_3()
            .py_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(if a == Ask::Remove { theme.danger.opacity(0.5) } else { theme.border })
            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(div().text_sm().text_color(theme.muted_foreground).child(body))
            .when(a == Ask::Remove, |c| {
                c.child(
                    Checkbox::new("rs-remove-tick")
                        .label("Yes, remove it")
                        .checked(self.ask_tick)
                        .on_change(cx.listener(|this, v: &bool, _, cx| {
                            this.ask_tick = *v;
                            cx.notify();
                        })),
                )
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(Button::new("rs-ask-cancel").outline().label("Cancel").on_click(cx.listener(|this, _, _, cx| {
                        this.ask = None;
                        cx.notify();
                    })))
                    .child(
                        Button::new("rs-ask-ok")
                            .when(a == Ask::Remove, |b| b.danger())
                            .when(a == Ask::Forget, |b| b.primary())
                            .label(ok)
                            .disabled(!ready || self.busy.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| match a {
                                Ask::Remove => this.remove(window, cx),
                                Ask::Forget => this.forget(window, cx),
                            })),
                    ),
            )
    }

    fn run_stage(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.busy.is_some();
        let run = self.run();
        let login = self.login();
        let running = self.running();
        let removing = self.removing();
        let failed = self.failed();
        let finished = self.finished();
        let steps = run["steps"].as_array().cloned().unwrap_or_default();
        let done = steps.iter().filter(|x| !matches!(x["state"].as_str(), Some("wait" | "run"))).count();
        let pct = if steps.is_empty() { 0.0 } else { done as f32 / steps.len() as f32 * 100.0 };
        let cmds = run["commands"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
        let items = steps.iter().map(|x| (s(x, "state"), s(x, "title"), Some(s(x, "detail")))).collect();
        let has_login = self.has_login();
        let at = if s(&login, "user").is_empty() { s(&login, "host") } else { format!("{}@{}", s(&login, "user"), s(&login, "host")) };
        let at = if at.is_empty() { "the VPS".to_string() } else { at };
        let join = self.join_at(cx);
        v_flex()
            .gap_3()
            .child(self.who(
                div().child(format!("{} {at}", if removing { "Removing the relay from" } else { "Setting up" })).font_family("Consolas"),
                None,
            ))
            .child(self.row_list(items, false, cx))
            .child(gpui_kit::component::progress::Progress::new("rs-bar").value(pct))
            .child(if failed {
                self.note(s(&run, "error"), false, cx).bg(cx.theme().danger.opacity(widgets::alpha::TINT)).into_any_element()
            } else if finished && removing {
                self.note("The relay is off the VPS: no relay user, service, files or firewall rules are left, and Hopper Server is disconnected.", true, cx).into_any_element()
            } else if finished && run["changed"].as_u64() == Some(0) {
                self.note("Everything was already right. Nothing changed.", true, cx).into_any_element()
            } else if finished {
                self.note(format!("The relay is running. Players join {join}. Test it next: Hopper checks the connection and asks the VPS for this server the way a player's game does."), true, cx).into_any_element()
            } else {
                widgets::fine_print("You can close this: it keeps going, and Hopper Server's task list shows how far it got.", cx).into_any_element()
            })
            .children(self.err_note(cx))
            .children(self.commands(cmds, cx))
            .child(match self.ask {
                Some(a) => self.ask_block(a, cx).into_any_element(),
                None => h_flex()
                    .gap_2()
                    .when(!running && !removing && has_login, |r| {
                        r.child(Button::new("rs-remove2").ghost().label("Remove the relay…").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                            this.ask = Some(Ask::Remove);
                            this.ask_tick = false;
                            cx.notify();
                        })))
                    })
                    .when(!running && removing && failed && has_login, |r| {
                        r.child(Button::new("rs-remove3").ghost().label("Try again").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                            this.ask = Some(Ask::Remove);
                            this.ask_tick = false;
                            cx.notify();
                        })))
                    })
                    .child(div().flex_1())
                    .when(!running && !removing && has_login, |r| {
                        r.child(Button::new("rs-again").outline().label("Run again").disabled(busy).on_click(cx.listener(|this, _, window, cx| this.again(window, cx))))
                    })
                    .when(!running && removing && !has_login, |r| {
                        r.child(Button::new("rs-setup-again").outline().label("Set up again").disabled(busy).on_click(cx.listener(|this, _, _, cx| {
                            this.sign = SignIn::Key;
                            this.stage = Stage::Connect;
                            cx.notify();
                        })))
                    })
                    .when(finished && !removing, |r| {
                        r.child(Button::new("rs-close").outline().label("Close").on_click(|_, window, cx| window.close_dialog(cx)))
                            .child(Button::new("rs-test").primary().label("Test").disabled(busy).on_click(cx.listener(|this, _, window, cx| this.run_test(window, cx))))
                    })
                    .when(!(finished && !removing), |r| {
                        r.child(
                            Button::new("rs-done")
                                .when(!running, |b| b.primary())
                                .when(running, |b| b.outline())
                                .label(if running { "Close" } else { "Done" })
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                    })
                    .into_any_element(),
            })
    }

    fn test_stage(&self, cx: &mut Context<Self>) -> Div {
        let busy = self.busy.is_some();
        let testing = self.busy == Some("test");
        let has_login = self.has_login();
        let join = self.join_at(cx);
        let ok = self.test.as_ref().is_some_and(|t| t["ok"].as_bool().unwrap_or(false));
        v_flex()
            .gap_3()
            .child(self.who(div().child(format!("Testing the relay for {}", if self.server_name.is_empty() { &self.server_id } else { &self.server_name })), None))
            .child(match &self.test {
                None => h_flex()
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(running_mark(cx))
                    .child("Connecting to the relay and asking the VPS like a player's game would…")
                    .into_any_element(),
                Some(t) => self
                    .row_list(rows(t).iter().map(|r| (s(r, "state"), s(r, "text"), None)).collect(), testing, cx)
                    .into_any_element(),
            })
            .children(self.test.as_ref().map(|_| {
                if ok {
                    self.note(format!("It works. Players join {join}; the server's Overview shows the relay and can test it again any time."), true, cx)
                } else {
                    self.note("Something on the way doesn't work yet: the rows above say what and what to do. Fix it, then test again.", false, cx)
                        .bg(cx.theme().danger.opacity(widgets::alpha::TINT))
                }
            }))
            .children(self.err_note(cx))
            .child(
                h_flex()
                    .gap_2()
                    .when(has_login, |r| {
                        r.child(Button::new("rs-setup-again2").ghost().label("Run the setup again").disabled(busy).on_click(cx.listener(|this, _, window, cx| this.again(window, cx))))
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("rs-test-again")
                            .outline()
                            .label(if testing { "Testing…" } else { "Test again" })
                            .loading(testing)
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.run_test(window, cx))),
                    )
                    .child(
                        Button::new("rs-test-done")
                            .when(ok, |b| b.primary())
                            .when(!ok, |b| b.outline())
                            .label("Done")
                            .disabled(testing)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    ),
            )
    }
}

impl Render for RelaySetup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let body = if self.st.is_none() {
            if self.err.is_empty() {
                h_flex().gap_2().items_center().text_xs().text_color(cx.theme().muted_foreground).child(running_mark(cx)).child("Loading…").into_any_element()
            } else {
                v_flex()
                    .gap_3()
                    .children(self.err_note(cx))
                    .child(h_flex().justify_end().child(Button::new("rs-close0").outline().label("Close").on_click(|_, window, cx| window.close_dialog(cx))))
                    .into_any_element()
            }
        } else {
            match self.stage {
                Stage::Before => self.before(cx).into_any_element(),
                Stage::Connect if self.host_check.is_some() => self.host_check_stage(window, cx).into_any_element(),
                Stage::Connect => self.connect_stage(window, cx).into_any_element(),
                Stage::Check => self.check_stage(window, cx).into_any_element(),
                Stage::Run => self.run_stage(cx).into_any_element(),
                Stage::Test => self.test_stage(cx).into_any_element(),
            }
        };
        v_flex()
            .id("relay-setup")
            .gap_4()
            .max_h(widgets::dialog_room(window) - rem * 3.)
            .overflow_y_scroll()
            .child(self.stepper(cx))
            .child(body)
    }
}
