//! A server's Settings tab: server.properties as a form, and how its JVM
//! starts (memory, the Java arguments with their generator, and for servers
//! Hopper hosts, which Java). Memory and arguments use the instance form.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::Field;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hosting::properties::{COMMON, PropKind};
use crate::java_form::JavaForm;
use hopper::hosting::{JvmArgs, Properties};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

pub struct ServerSettings {
    store: Entity<Store>,
    root: Option<PathBuf>,
    /// As read from disk.
    props: Properties,
    jvm: JvmArgs,
    /// The form's values, by key.
    values: HashMap<&'static str, String>,
    /// Text and number fields, by key.
    inputs: HashMap<&'static str, Entity<InputState>>,
    /// Memory (-Xms/-Xmx) and the Java arguments, as an instance has them.
    java_form: Entity<JavaForm>,
    /// The arguments line the form loaded with (memory first).
    args_orig: String,
    /// A java.exe picked for a hosted server.
    java: Option<PathBuf>,
    /// The hosted server's schedule, as read, and as edited.
    schedule: hopper::hosting::Schedule,
    sched: hopper::hosting::Schedule,
    restart_at: Entity<InputState>,
    warn_at: Entity<InputState>,
    max_crashes: Entity<InputState>,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl ServerSettings {
    pub fn new(store: Entity<Store>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let java_form = cx.new(|cx| {
            let mut f = JavaForm::new(store.clone(), None, window, cx);
            let max = hopper::jvm_args::memory_max_mb(hopper::hosting::local_panel::total_memory() / (1024 * 1024));
            f.set_max_gb(max as f32 / 1024.0, window, cx);
            f
        });
        let mut inputs = HashMap::new();
        let mut subs = vec![cx.observe(&java_form, |_, _, cx| cx.notify())];
        for spec in COMMON {
            if matches!(spec.kind, PropKind::String | PropKind::Int { .. }) {
                let input = cx.new(|cx| InputState::new(window, cx).placeholder(spec.default));
                let key = spec.key;
                subs.push(cx.subscribe(&input, move |this, state, e: &InputEvent, cx| {
                    if matches!(e, InputEvent::Change) {
                        this.values.insert(key, state.read(cx).value().to_string());
                        cx.notify();
                    }
                }));
                inputs.insert(spec.key, input);
            }
        }
        let restart_at = cx.new(|cx| InputState::new(window, cx).placeholder("Off (e.g. 04:00)"));
        let warn_at = cx.new(|cx| InputState::new(window, cx).placeholder("10, 5, 1"));
        let max_crashes = cx.new(|cx| InputState::new(window, cx).placeholder("3"));
        for i in [&restart_at, &warn_at, &max_crashes] {
            subs.push(cx.subscribe(i, |this, _, e: &InputEvent, cx| {
                if matches!(e, InputEvent::Change) {
                    this.read_schedule_inputs(cx);
                    cx.notify();
                }
            }));
        }
        Self {
            schedule: Default::default(),
            sched: Default::default(),
            restart_at,
            warn_at,
            max_crashes,
            store,
            root: None,
            props: Properties::default(),
            jvm: JvmArgs::default(),
            values: HashMap::new(),
            inputs,
            java_form,
            args_orig: String::new(),
            java: None,
            error: None,
            _subscriptions: subs,
        }
    }

    /// Shows (and reads) the server in `root`.
    pub fn show(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if self.root.as_ref() == Some(&root) {
            return;
        }
        self.root = Some(root);
        self.reload(window, cx);
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        self.error = None;
        self.props = Properties::load(&root.join("server.properties")).unwrap_or_default();
        self.jvm = JvmArgs::load(&root).unwrap_or_default();
        self.java = None;
        self.values.clear();
        for spec in COMMON {
            let v = self.props.get(spec.key).unwrap_or(spec.default).to_string();
            if let Some(input) = self.inputs.get(spec.key) {
                let v = v.clone();
                input.update(cx, |s, cx| s.set_value(v, window, cx));
            }
            self.values.insert(spec.key, v);
        }
        self.schedule = hopper::hosting::Schedule::load(&root);
        self.sched = self.schedule.clone();
        let (r, w, m) = (
            self.sched.daily_restart.clone().unwrap_or_default(),
            self.sched.warn_minutes.iter().map(|m| m.to_string()).collect::<Vec<_>>().join(", "),
            self.sched.max_crash_restarts.to_string(),
        );
        self.restart_at.update(cx, |s, cx| s.set_value(r, window, cx));
        self.warn_at.update(cx, |s, cx| s.set_value(w, window, cx));
        self.max_crashes.update(cx, |s, cx| s.set_value(m, window, cx));
        let xmx = self.jvm.xmx_mb.unwrap_or(4096);
        let line = self.jvm.flags.iter().map(|f| hopper::hosting::jvm::quote(f)).collect::<Vec<_>>().join(" ");
        self.args_orig = hopper::jvm_args::set_memory(&line, self.jvm.xms_mb, Some(xmx));
        let (args, target) = (self.args_orig.clone(), self.target());
        self.java_form.update(cx, |f, cx| {
            f.set_target(Some(target), cx);
            f.set(xmx, "", &args, None, window, cx);
        });
        cx.notify();
    }

    /// The schedule fields into `sched` (validated on save).
    fn read_schedule_inputs(&mut self, cx: &App) {
        let r = self.restart_at.read(cx).value().trim().to_string();
        self.sched.daily_restart = (!r.is_empty()).then_some(r);
        self.sched.warn_minutes = self
            .warn_at
            .read(cx)
            .value()
            .split([',', ' '])
            .filter_map(|p| p.trim().parse().ok())
            .collect();
        if let Ok(n) = self.max_crashes.read(cx).value().trim().parse() {
            self.sched.max_crash_restarts = n;
        }
    }

    /// Memory MB and the arguments line (memory first) the form has now.
    fn form_args(&self, cx: &App) -> (u32, String) {
        let (mb, _, args) = self.java_form.read(cx).values();
        (mb, args)
    }

    fn changed(&self, cx: &App) -> bool {
        let props = COMMON.iter().any(|s| {
            self.values.get(s.key).map(String::as_str) != Some(self.props.get(s.key).unwrap_or(s.default))
        });
        props
            || self.sched != self.schedule
            || {
                let (mb, args) = self.form_args(cx);
                Some(mb) != self.jvm.xmx_mb.or(Some(4096)) || args != self.args_orig
            }
            || self.java.is_some()
    }

    /// The Java the generator writes for: the one picked, else the one the
    /// start script runs, else Java 21 (what Hopper gives a new server).
    fn target(&self) -> hopper::jvm_args::Target {
        let java = self.java.clone().or_else(|| {
            let root = self.root.as_ref()?;
            hopper::hosting::script::launch_command(root).ok().map(|l| l.java)
        });
        java.and_then(|j| hopper::java::inspect(&j))
            .map(|j| hopper::jvm_args::Target::of(&j))
            .unwrap_or(hopper::jvm_args::Target { major: Some(21), ..Default::default() })
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        // Check every value first; save nothing if one is wrong.
        for spec in COMMON {
            if let Some(v) = self.values.get(spec.key)
                && let Err(e) = spec.validate(v)
            {
                self.error = Some(e);
                cx.notify();
                return;
            }
        }
        let mut props = self.props.clone();
        for spec in COMMON {
            if let Some(v) = self.values.get(spec.key) {
                let on_disk = self.props.get(spec.key);
                // Leave keys the file doesn't have alone unless changed.
                if on_disk != Some(v.as_str()) && !(on_disk.is_none() && v == spec.default) {
                    props.set(spec.key, v.trim());
                }
            }
        }
        // The form's line into the file: -Xms / -Xmx from the slider, the
        // rest one per line (comments kept).
        let (mb, args) = self.form_args(cx);
        let typed = JvmArgs::parse(&hopper::jvm_args::without_memory(&args));
        let mut jvm = self.jvm.clone();
        jvm.flags = typed.flags;
        jvm.set_memory_range(hopper::jvm_args::memory(&args).0, mb);
        let java = self.java.clone();
        let hosted = self.store.read(cx).is_hosted(&root);
        if hosted && let Err(e) = self.sched.validate() {
            self.error = Some(e);
            cx.notify();
            return;
        }
        let sched = (hosted && self.sched != self.schedule).then(|| self.sched.clone());
        let r = (|| -> anyhow::Result<()> {
            if let Some(s) = &sched {
                s.save(&root)?;
            }
            props.save(&root.join("server.properties"))?;
            jvm.save(&root)?;
            if let Some(j) = java {
                let bat = root.join("run.bat");
                let text = std::fs::read_to_string(&bat)?;
                let java = hopper::hosting::script::console_java(&j);
                let new = hopper::hosting::script::rewrite_java(&text, &java)
                    .ok_or_else(|| anyhow::anyhow!("run.bat has no java line to change"))?;
                std::fs::write(&bat, new)?;
            }
            Ok(())
        })();
        match r {
            Ok(()) => {
                let running = self
                    .store
                    .read(cx)
                    .panel_stats(&root)
                    .is_some_and(|p| p.running);
                window.push_notification(
                    Notification::success(if running {
                        "Saved. Restart the server to use the new settings."
                    } else {
                        "Saved."
                    }),
                    cx,
                );
                self.reload(window, cx);
            }
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                cx.notify();
            }
        }
    }

    fn field(&self, spec: &'static hopper::hosting::PropSpec, rem: Pixels, cx: &mut Context<Self>) -> Field {
        let value = self.values.get(spec.key).cloned().unwrap_or_default();
        let control: AnyElement = match spec.kind {
            PropKind::Bool => Switch::new(spec.key)
                .checked(value.trim() == "true")
                .on_change(cx.listener(move |this, v: &bool, _, cx| {
                    this.values.insert(spec.key, v.to_string());
                    cx.notify();
                }))
                .into_any_element(),
            PropKind::Enum(options) => {
                let items = options.iter().map(|o| (o.to_string(), *o == value)).collect();
                let view = cx.entity();
                div()
                    .w_48()
                    .child(widgets::picker(
                        spec.key,
                        value.clone(),
                        items,
                        rem,
                        Rc::new(move |ix, _, cx| {
                            if let Some(o) = options.get(ix) {
                                view.update(cx, |this, cx| {
                                    this.values.insert(spec.key, o.to_string());
                                    cx.notify();
                                });
                            }
                        }),
                    ))
                    .into_any_element()
            }
            PropKind::Int { .. } => match self.inputs.get(spec.key) {
                Some(i) => div().w_32().child(Input::new(i).small()).into_any_element(),
                None => div().into_any_element(),
            },
            PropKind::String => match self.inputs.get(spec.key) {
                Some(i) => div().w_96().child(Input::new(i).small()).into_any_element(),
                None => div().into_any_element(),
            },
        };
        Field::new()
            .label(spec.label)
            .description(spec.description)
            .child(control)
    }
}

impl Render for ServerSettings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let hosted = self.store.read(cx).is_hosted(&root);
        let changed = self.changed(cx);

        let props = crate::java_form::settings_form(rem)
            .children(COMMON.iter().map(|spec| self.field(spec, rem, cx)));

        let [memory, _, args] = self.java_form.update(cx, |f, cx| f.parts(rem, cx));
        let mut jvm = crate::java_form::settings_form(rem).child(
            Field::new()
                .label("Memory")
                .description("The left end sets -Xms, the right end -Xmx. Modded servers want 6 to 10 GB.")
                .child(memory),
        );
        if hosted {
            // The Java run.bat starts, unless another was just picked.
            let current = self.java.clone().or_else(|| {
                hopper::hosting::script::launch_command(&root).ok().map(|l| PathBuf::from(l.java))
            });
            let store = self.store.read(cx);
            let game = hopper::hosting::HostedMeta::load(&root)
                .map(|m| m.minecraft)
                .or_else(|| store.server(&root).and_then(|s| store.paired(s)).map(|i| i.game_version.clone()));
            jvm = jvm.child(crate::host_dialog::java_field(
                &self.store,
                current,
                game,
                rem,
                {
                    let view = cx.entity();
                    Rc::new(move |java: PathBuf, _: &mut Window, cx: &mut App| {
                        view.update(cx, |this, cx| {
                            this.java = Some(java);
                            let target = this.target();
                            this.java_form.update(cx, |f, cx| f.set_target(Some(target), cx));
                            cx.notify();
                        })
                    })
                },
                cx,
            ));
        }
        jvm = jvm.child(Field::new().label("Java arguments").items_start().child(args));

        let schedule = hosted.then(|| {
            let next = hopper::hosting::schedule::describe_next_restart(
                &self.sched,
                hopper::hosting::schedule::LocalTime::now(),
            );
            crate::java_form::settings_form(rem)
                .child(
                    Field::new()
                        .label("Daily restart")
                        .description(match next {
                            Some(n) => format!("Next: {n}. Players get warnings in chat first."),
                            None => "A time (HH:MM) to restart every day, or empty for none.".to_string(),
                        })
                        .child(div().w_32().child(Input::new(&self.restart_at).small())),
                )
                .child(
                    Field::new()
                        .label("Warn before")
                        .description("Minutes before the restart to warn players.")
                        .child(div().w_48().child(Input::new(&self.warn_at).small())),
                )
                .child(
                    Field::new().label("After a crash").child(
                        h_flex()
                            .gap_3()
                            .child(
                                Switch::new("crash-restart")
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
                )
                .child(
                    Field::new()
                        .label("With Hopper")
                        .child(
                            Switch::new("start-with-hopper")
                                .label("Start this server when Hopper starts")
                                .checked(self.sched.start_with_hopper)
                                .on_change(cx.listener(|this, v: &bool, _, cx| {
                                    this.sched.start_with_hopper = *v;
                                    cx.notify();
                                })),
                        ),
                )
        });

        v_flex()
            .gap_6()
            .pb_6()
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("server.properties"))
                    .child(props),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Java"))
                    .child(jvm),
            )
            .when_some(schedule, |c, form| {
                c.child(
                    v_flex()
                        .gap_3()
                        .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Schedule"))
                        .child(form),
                )
            })
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        Button::new("server-settings-save")
                            .primary()
                            .label("Save")
                            .disabled(!changed)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    )
                    .child(
                        Button::new("server-settings-reload")
                            .ghost()
                            .icon(IconName::RotateCcw)
                            .label("Undo changes")
                            .disabled(!changed)
                            .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
                    )
                    .when_some(self.error.clone(), |r, e| {
                        r.child(div().text_sm().text_color(theme.danger).child(e))
                    }),
            )
            .into_any_element()
    }
}
