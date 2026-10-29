//! A Hopper Server server's Schedule tab: daily restart, crash restarts,
//! scheduled backups and start-up. Same fields as the panel's Schedule page.

use super::{Load, Target, load_state};
use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::Field;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hosting::Schedule;
use hopper::hosting::backup::Scope;
use serde_json::Value;
use std::rc::Rc;

/// What `GET /servers/{id}/schedule` answers.
struct Loaded {
    schedule: Schedule,
    next_restart: Option<String>,
    gave_up: Option<String>,
    last_backup: Option<i64>,
}

fn parse(v: &Value) -> Loaded {
    Loaded {
        schedule: v.get("schedule").and_then(|s| serde_json::from_value(s.clone()).ok()).unwrap_or_default(),
        next_restart: v.get("next_restart").and_then(Value::as_str).map(str::to_string),
        gave_up: v.get("gave_up").and_then(Value::as_str).map(str::to_string),
        last_backup: v.get("last_backup").and_then(Value::as_i64),
    }
}

pub struct ScheduleTab {
    store: Entity<Store>,
    target: Option<Target>,
    data: Load<Loaded>,
    /// As on the panel, and as edited.
    saved: Schedule,
    sched: Schedule,
    restart_at: Entity<InputState>,
    warn_at: Entity<InputState>,
    max_crashes: Entity<InputState>,
    backup_hours: Entity<InputState>,
    backup_keep: Entity<InputState>,
    saving: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ScheduleTab {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mk = |window: &mut Window, cx: &mut Context<Self>, ph: &'static str| {
            cx.new(|cx| InputState::new(window, cx).placeholder(ph))
        };
        let restart_at = mk(window, cx, "Off (e.g. 04:00)");
        let warn_at = mk(window, cx, "10, 5, 1");
        let max_crashes = mk(window, cx, "3");
        let backup_hours = mk(window, cx, "0");
        let backup_keep = mk(window, cx, "12");
        let mut subs = Vec::new();
        for (i, input) in [&restart_at, &warn_at, &max_crashes, &backup_hours, &backup_keep].into_iter().enumerate() {
            subs.push(cx.subscribe(input, move |this, state, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    let text = state.read(cx).value().trim().to_string();
                    this.edit(i, &text);
                    cx.notify();
                }
            }));
        }
        Self {
            store,
            target: None,
            data: Load::Waiting,
            saved: Schedule::default(),
            sched: Schedule::default(),
            restart_at,
            warn_at,
            max_crashes,
            backup_hours,
            backup_keep,
            saving: false,
            error: None,
            _subscriptions: subs,
        }
    }

    /// A text field changed: into the edited schedule (numbers that don't
    /// parse are left as they were).
    fn edit(&mut self, field: usize, text: &str) {
        let n = text.parse::<u32>().ok();
        match field {
            0 => self.sched.daily_restart = (!text.is_empty()).then(|| text.to_string()),
            1 => {
                self.sched.warn_minutes =
                    text.split([',', ' ']).filter_map(|p| p.trim().parse().ok()).filter(|n| (1..1440).contains(n)).collect()
            }
            2 => {
                if let Some(n) = n {
                    self.sched.max_crash_restarts = n;
                }
            }
            3 => {
                if let Some(n) = n {
                    self.sched.backup_every_hours = n;
                }
            }
            _ => {
                if let Some(n) = n {
                    self.sched.backup_keep = n;
                }
            }
        }
    }

    /// Shows `target`'s Schedule (again when it's the same server: a refresh).
    pub fn show(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        if self.target.as_ref() != Some(&target) {
            self.data = Load::Waiting;
        }
        self.target = Some(target);
        self.error = None;
        self.fetch(window, cx);
        cx.notify();
    }

    /// Reads the schedule from the panel and puts it in the form.
    fn fetch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let path = t.sp("/schedule");
        let client = t.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { client.get(&path).map_err(|e| format!("{e:#}")) })
                .await;
            this.update_in(cx, |v, window, cx| {
                if v.target.as_ref() != Some(&t) {
                    return;
                }
                match r {
                    Ok(json) => {
                        let l = parse(&json);
                        v.saved = l.schedule.clone();
                        v.sched = l.schedule.clone();
                        v.fill_inputs(window, cx);
                        v.data = Load::Ready(l);
                    }
                    Err(e) => v.data = Load::Failed(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn fill_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let s = self.sched.clone();
        let warn = s.warn_minutes.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(", ");
        let vals = [
            s.daily_restart.clone().unwrap_or_default(),
            warn,
            s.max_crash_restarts.to_string(),
            s.backup_every_hours.to_string(),
            s.backup_keep.to_string(),
        ];
        for (input, v) in [&self.restart_at, &self.warn_at, &self.max_crashes, &self.backup_hours, &self.backup_keep]
            .into_iter()
            .zip(vals)
        {
            input.update(cx, |st, cx| st.set_value(v, window, cx));
        }
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sched = self.saved.clone();
        self.error = None;
        self.fill_inputs(window, cx);
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let mut s = self.sched.clone();
        s.backup_keep = s.backup_keep.max(1);
        if let Err(e) = s.validate() {
            self.error = Some(e);
            cx.notify();
            return;
        }
        let Ok(body) = serde_json::to_value(&s) else { return };
        self.saving = true;
        self.error = None;
        let path = t.sp("/schedule");
        let client = t.client.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_spawn(async move { client.put(&path, body).map_err(|e| format!("{e:#}")) })
                .await;
            this.update_in(cx, |v, window, cx| {
                v.saving = false;
                match r {
                    Ok(_) => v.fetch(window, cx),
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn section(title: &'static str, form: impl IntoElement) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
            .child(form)
    }
}

impl Render for ScheduleTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full();
        if let Some(s) = load_state(&self.data, cx) {
            return root.p_4().child(s).into_any_element();
        }
        let Some(l) = self.data.ready() else { return root.into_any_element() };
        let rem = window.rem_size();
        let theme = cx.theme().clone();
        let changed = self.sched != self.saved;
        let form = || crate::java_form::settings_form(rem);

        let next = l.next_restart.clone();
        let restart = form()
            .child(
                Field::new()
                    .label("Daily restart")
                    .description(match next {
                        Some(n) if self.sched.daily_restart == self.saved.daily_restart => {
                            format!("Next: {n}. Players get warnings in chat first.")
                        }
                        _ => "A time (HH:MM, the server PC's clock) to restart every day, or empty for none.".to_string(),
                    })
                    .child(div().w_32().child(Input::new(&self.restart_at).small())),
            )
            .child(
                Field::new()
                    .label("Warn before")
                    .description("Minutes before the restart to warn players, comma separated.")
                    .child(div().w_48().child(Input::new(&self.warn_at).small())),
            );

        let crashes = form().child(
            Field::new()
                .label("After a crash")
                .description("Stops trying after the limit within an hour.")
                .child(
                    h_flex()
                        .gap_3()
                        .child(
                            Switch::new("sched-crash-restart")
                                .label("Start it again, up to")
                                .checked(self.sched.auto_restart_on_crash)
                                .on_change(cx.listener(|this, v: &bool, _, cx| {
                                    this.sched.auto_restart_on_crash = *v;
                                    cx.notify();
                                })),
                        )
                        .child(div().w_16().child(Input::new(&self.max_crashes).small()))
                        .child(div().text_sm().child("times an hour")),
                ),
        );

        let view = cx.entity();
        let scopes = [("Worlds", Scope::Worlds), ("Everything", Scope::Full)];
        let items: Vec<(String, bool)> =
            scopes.iter().map(|(n, s)| (n.to_string(), *s == self.sched.backup_scope)).collect();
        let current = scopes.iter().find(|(_, s)| *s == self.sched.backup_scope).map_or("Worlds", |(n, _)| *n);
        let last = l.last_backup.map(|t| {
            let ago = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64)
                - t)
                .max(0);
            match ago {
                0..=3599 => format!("{} min ago", ago / 60),
                3600..=86399 => format!("{} h ago", ago / 3600),
                _ => format!("{} days ago", ago / 86400),
            }
        });
        let backups = form()
            .child(
                Field::new()
                    .label("Back up every")
                    .description(match last {
                        Some(a) => format!("Hours, while the server runs. 0: off. Last scheduled backup {a}."),
                        None => "Hours, while the server runs. 0: off.".to_string(),
                    })
                    .child(div().w_24().child(Input::new(&self.backup_hours).small())),
            )
            .child(
                Field::new()
                    .label("Backups kept")
                    .description("Older scheduled backups are deleted.")
                    .child(div().w_24().child(Input::new(&self.backup_keep).small())),
            )
            .child(
                Field::new().label("What they hold").child(div().w_48().child(widgets::picker(
                    "sched-backup-scope",
                    current.to_string(),
                    items,
                    rem,
                    Rc::new(move |ix, _, cx| {
                        if let Some((_, s)) = scopes.get(ix) {
                            view.update(cx, |this, cx| {
                                this.sched.backup_scope = *s;
                                cx.notify();
                            });
                        }
                    }),
                ))),
            );

        let startup = form().child(
            Field::new().label("With the panel").child(
                Switch::new("sched-start-with-panel")
                    .label("Start this server when the panel starts")
                    .checked(self.sched.start_with_hopper)
                    .on_change(cx.listener(|this, v: &bool, _, cx| {
                        this.sched.start_with_hopper = *v;
                        cx.notify();
                    })),
            ),
        );

        let gave_up = l.gave_up.clone();
        let saving = self.saving;
        root.child(
            div().id("schedule-scroll").size_full().overflow_y_scrollbar().child(
                v_flex()
                    .gap_6()
                    .p_4()
                    .pb_6()
                    .when_some(gave_up, |c, g| {
                        let place = self.target.as_ref().map(|t| format!("{}#{}", t.remote.panel, t.remote.id)).unwrap_or_default();
                        let key = hopper::settings::notice_key(&place, "restart-gave-up", &g);
                        let body = widgets::notice_bar(theme.warning, IconName::TriangleAlert, cx).child(div().text_sm().child(g));
                        c.children(widgets::closable_notice(body, key, widgets::Closed::Chip, "Automatic restarts stopped", theme.warning, &self.store, cx))
                    })
                    .child(Self::section("Daily restart", restart))
                    .child(Self::section("Crashes", crashes))
                    .child(Self::section("Backups", backups))
                    .child(Self::section("Start-up", startup))
                    .child(
                        h_flex()
                            .gap_3()
                            .child(
                                Button::new("schedule-save")
                                    .primary()
                                    .icon(IconName::Save)
                                    .label("Save")
                                    .loading(saving)
                                    .disabled(!changed || saving)
                                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                            )
                            .child(
                                Button::new("schedule-discard")
                                    .ghost()
                                    .icon(IconName::RotateCcw)
                                    .label("Discard")
                                    .disabled(!changed || saving)
                                    .on_click(cx.listener(|this, _, window, cx| this.discard(window, cx))),
                            )
                            .when_some(self.error.clone(), |r, e| {
                                r.child(div().text_sm().text_color(theme.danger).child(e))
                            }),
                    ),
            ),
        )
        .into_any_element()
    }
}
