//! A server's Settings tab: server.properties as a form, and how its JVM
//! starts (memory, GC flags and, for servers Hopper hosts, which Java).

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::Field;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::hosting::properties::{COMMON, PropKind};
use hopper::hosting::{JvmArgs, JvmPreset, Properties};
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
    memory: Entity<SliderState>,
    preset: JvmPreset,
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
        let memory = cx.new(|_| SliderState::new().min(1.).max(32.).step(0.5));
        let mut inputs = HashMap::new();
        let mut subs = vec![cx.subscribe(&memory, |_, _, _: &SliderEvent, cx| cx.notify())];
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
            memory,
            preset: JvmPreset::Aikar,
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
        self.preset = self.jvm.preset();
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
        let gb = self.jvm.xmx_mb.unwrap_or(4096) as f32 / 1024.0;
        self.memory
            .update(cx, |s, cx| s.set_value(gb.clamp(1.0, 32.0), window, cx));
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

    fn memory_mb(&self, cx: &App) -> u32 {
        (self.memory.read(cx).value().start() * 1024.0).round() as u32
    }

    fn changed(&self, cx: &App) -> bool {
        let props = COMMON.iter().any(|s| {
            self.values.get(s.key).map(String::as_str) != Some(self.props.get(s.key).unwrap_or(s.default))
        });
        props
            || self.sched != self.schedule
            || Some(self.memory_mb(cx)) != self.jvm.xmx_mb
            || self.preset != self.jvm.preset()
            || self.java.is_some()
    }

    /// The Java major this server runs, for flag presets.
    fn java_major(&self, cx: &App) -> u32 {
        let Some(root) = &self.root else { return 21 };
        let java = self.java.clone().or_else(|| {
            hopper::hosting::script::launch_command(root)
                .ok()
                .map(|l| PathBuf::from(l.java))
        });
        java.and_then(|j| hopper::java::inspect(&j))
            .map(|j| j.major)
            .or_else(|| {
                let store = self.store.read(cx);
                store
                    .server(root)
                    .and_then(|s| store.paired(s))
                    .map(|i| hopper::java::recommended_major(&i.game_version))
            })
            .unwrap_or(21)
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
        let mut jvm = self.jvm.clone();
        jvm.set_memory(self.memory_mb(cx));
        if self.preset != jvm.preset() && self.preset != JvmPreset::Custom {
            jvm.set_preset(self.preset, self.java_major(cx));
        }
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
        let major = self.java_major(cx);

        let props = crate::java_form::settings_form(rem)
            .children(COMMON.iter().map(|spec| self.field(spec, rem, cx)));

        let presets: Vec<(String, bool)> = JvmPreset::ALL
            .iter()
            .map(|p| {
                let label = if p.supported(major) {
                    p.label().to_string()
                } else {
                    format!("{} (needs Java 21+)", p.label())
                };
                (label, *p == self.preset)
            })
            .collect();
        let view = cx.entity();
        let preset_picker = widgets::picker(
            "jvm-preset",
            if self.preset == JvmPreset::Custom {
                "Custom flags".to_string()
            } else {
                self.preset.label().to_string()
            },
            presets,
            rem,
            Rc::new(move |ix, _, cx| {
                if let Some(p) = JvmPreset::ALL.get(ix) {
                    view.update(cx, |this, cx| {
                        this.preset = *p;
                        cx.notify();
                    });
                }
            }),
        );
        let gb = self.memory.read(cx).value().start();
        let mut jvm = crate::java_form::settings_form(rem)
            .child(
                Field::new()
                    .label("Memory")
                    .description("The server's -Xms and -Xmx. Modded servers want 6 to 10 GB.")
                    .child(
                        h_flex()
                            .gap_3()
                            .child(div().w_64().child(Slider::new(&self.memory)))
                            .child(div().text_sm().child(format!("{gb:.1} GB"))),
                    ),
            )
            .child(
                Field::new()
                    .label("GC flags")
                    .description("Aikar's flags suit almost every server.")
                    .child(div().w_64().child(preset_picker)),
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
                            cx.notify();
                        })
                    })
                },
                cx,
            ));
        }

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
