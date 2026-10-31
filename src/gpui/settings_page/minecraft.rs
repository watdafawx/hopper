//! Settings > Minecraft: the in-game options Hopper writes into every
//! instance before it starts (see `hopper::mcoptions`). Each option is a
//! row with a switch in front: off, the pack decides. A lock beside the
//! value sets it every start instead of once.

use super::*;
use gpui_kit::component::Selectable as _;
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use hopper::mcoptions::{self as mco, Kind, McOptions, McValue, Spec, SPECS};

/// The page's own state for the Minecraft category.
pub(super) struct Mc {
    sliders: HashMap<&'static str, Entity<SliderState>>,
    show_keys: bool,
    /// The action waiting for a key press.
    capture: Option<String>,
    /// When a mouse button was just bound, so the click that follows
    /// doesn't start listening again.
    bound_at: Option<Instant>,
    focus: FocusHandle,
    /// The instance to copy from (its folder).
    import_from: Option<std::path::PathBuf>,
    import_note: Option<Result<String, String>>,
    _subs: Vec<Subscription>,
}

/// A slider's range and step, for the options that have one.
fn range(kind: Kind) -> Option<(f32, f32, f32)> {
    match kind {
        Kind::Volume => Some((0., 100., 1.)),
        Kind::Sensitivity => Some((0., 200., 1.)),
        Kind::Fov => Some((30., 110., 1.)),
        Kind::Number { min, max } => Some((min as f32, max as f32, 1.)),
        Kind::MaxFps => Some((10., 260., 10.)),
        _ => None,
    }
}

/// Languages offered, by Minecraft's code.
const LANGS: &[(&str, &str)] = &[
    ("en_us", "English (US)"),
    ("en_gb", "English (UK)"),
    ("de_de", "Deutsch"),
    ("fr_fr", "Français"),
    ("es_es", "Español (España)"),
    ("es_mx", "Español (México)"),
    ("it_it", "Italiano"),
    ("nl_nl", "Nederlands"),
    ("pt_br", "Português (Brasil)"),
    ("pt_pt", "Português (Portugal)"),
    ("pl_pl", "Polski"),
    ("cs_cz", "Čeština"),
    ("sv_se", "Svenska"),
    ("no_no", "Norsk"),
    ("da_dk", "Dansk"),
    ("fi_fi", "Suomi"),
    ("hu_hu", "Magyar"),
    ("tr_tr", "Türkçe"),
    ("ru_ru", "Русский"),
    ("uk_ua", "Українська"),
    ("ja_jp", "日本語"),
    ("ko_kr", "한국어"),
    ("zh_cn", "简体中文"),
    ("zh_tw", "繁體中文"),
];

/// The settings entry of an option (`"mc-fov"`).
fn row_id(key: &str) -> &'static str {
    ENTRIES
        .iter()
        .find(|e| e.id.strip_prefix("mc-") == Some(key))
        .map(|e| e.id)
        .unwrap_or_else(|| panic!("no settings entry for {key}"))
}

/// Changes one option, adding it (with its default value) if it was never set.
fn edit(store: &Entity<Store>, spec: &'static Spec, f: impl FnOnce(&mut McValue), cx: &mut App) {
    store.update(cx, |s, cx| {
        s.update_settings(
            |st| {
                let v = st
                    .mc_options
                    .values
                    .entry(spec.key.to_string())
                    .or_insert_with(|| McValue { value: spec.default.to_string(), ..Default::default() });
                f(v);
            },
            cx,
        )
    });
}

impl Mc {
    pub(super) fn new(s: &hopper::settings::Settings, cx: &mut Context<SettingsPage>) -> Self {
        let mut mc = Mc {
            sliders: HashMap::new(),
            show_keys: false,
            capture: None,
            bound_at: None,
            focus: cx.focus_handle(),
            import_from: None,
            import_note: None,
            _subs: Vec::new(),
        };
        mc.build_sliders(&s.mc_options, cx);
        mc
    }

    /// One slider per option that has one, at its saved value. Moving one
    /// turns its option on.
    fn build_sliders(&mut self, o: &McOptions, cx: &mut Context<SettingsPage>) {
        self.sliders.clear();
        self._subs.clear();
        for spec in SPECS {
            let Some((min, max, step)) = range(spec.kind) else { continue };
            let v = mco::shown(spec.kind, &o.value(spec).value).clamp(min, max);
            let slider = cx.new(|_| SliderState::new().min(min).max(max).step(step).default_value(v));
            self._subs.push(cx.subscribe(&slider, move |this: &mut SettingsPage, _, e: &SliderEvent, cx| {
                if let SliderEvent::Change(v) = e {
                    let raw = mco::stored(spec.kind, v.start());
                    edit(
                        &this.store,
                        spec,
                        |e| {
                            e.value = raw;
                            e.on = true;
                        },
                        cx,
                    );
                }
            }));
            self.sliders.insert(spec.key, slider);
        }
    }
}

impl SettingsPage {
    pub(super) fn minecraft(&mut self, s: &hopper::settings::Settings, window: &mut Window, cx: &mut Context<Self>) -> Vec<Block> {
        let o = &s.mc_options;
        let rem = window.rem_size();
        let mut blocks = vec![Block::card(
            None,
            vec![
                Row::new("mc-enabled").control(self.switch("mc-enabled", o.enabled, |st, v| st.mc_options.enabled = v, cx)),
                self.mc_import_row(rem, cx),
            ],
        )];
        for group in [mco::SOUND, mco::VIEW, mco::VIDEO, mco::START] {
            let rows = SPECS.iter().filter(|sp| sp.group == group).map(|sp| self.mc_row(sp, o, rem, cx)).collect();
            blocks.push(Block::card(Some(group), rows));
        }
        blocks.push(Block::card(Some("Controls"), vec![self.mc_keys_row(o, cx)]));
        blocks
    }

    /// "Every start" (locked) or "First start" for one option.
    fn mc_lock(&self, id: SharedString, always: bool, on_toggle: impl Fn(&mut Self, &mut Context<Self>) + 'static, cx: &mut Context<Self>) -> Button {
        Button::new(id)
            .ghost()
            .small()
            .icon(if always { IconName::Lock } else { IconName::LockOpen })
            .selected(always)
            .tooltip(if always {
                "Every start: set again each time the game starts"
            } else {
                "First start: set once, then what you change in game stays"
            })
            .on_click(cx.listener(move |this, _, _, cx| on_toggle(this, cx)))
    }

    fn mc_row(&self, spec: &'static Spec, o: &McOptions, rem: Pixels, cx: &mut Context<Self>) -> Row {
        let id = row_id(spec.key);
        let v = o.value(spec);
        let on = Switch::new(SharedString::from(format!("{id}-on"))).checked(v.on).on_click(cx.listener(move |this, on: &bool, _, cx| {
            let on = *on;
            edit(&this.store, spec, |e| e.on = on, cx);
        }));
        let store = self.store.clone();
        let pick = |labels: Vec<String>, values: Vec<String>, current: &str| {
            let ix = values.iter().position(|v| v == current.trim().trim_matches('"'));
            let label = ix.map(|i| labels[i].clone()).unwrap_or_else(|| current.trim().trim_matches('"').to_string());
            let store = store.clone();
            div().w(rems(11.)).child(widgets::picker(
                SharedString::from(format!("{id}-pick")),
                label,
                labels.iter().enumerate().map(|(i, l)| (l.clone(), Some(i) == ix)).collect(),
                rem,
                Rc::new(move |i, _, cx| {
                    if let Some(val) = values.get(i).cloned() {
                        edit(
                            &store,
                            spec,
                            |e| {
                                e.value = val;
                                e.on = true;
                            },
                            cx,
                        );
                    }
                }),
            ))
            .into_any_element()
        };
        let value: Option<AnyElement> = match spec.kind {
            Kind::Fixed(_) => None,
            Kind::GuiScale => Some(pick(
                std::iter::once("Auto".to_string()).chain((1..=6).map(|n| n.to_string())).collect(),
                (0..=6).map(|n| n.to_string()).collect(),
                &v.value,
            )),
            Kind::Bool => Some(pick(vec!["On".into(), "Off".into()], vec!["true".into(), "false".into()], &v.value)),
            Kind::Lang => {
                let mut langs: Vec<(String, String)> = LANGS.iter().map(|(c, n)| (c.to_string(), n.to_string())).collect();
                let cur = v.value.trim().trim_matches('"').to_string();
                if !cur.is_empty() && !langs.iter().any(|(c, _)| *c == cur) {
                    langs.push((cur.clone(), cur));
                }
                Some(pick(
                    langs.iter().map(|(_, n)| n.clone()).collect(),
                    langs.iter().map(|(c, _)| c.clone()).collect(),
                    &v.value,
                ))
            }
            _ => self.mc.sliders.get(spec.key).map(|s| {
                h_flex()
                    .gap_3()
                    .child(div().w(rems(11.)).child(Slider::new(s)))
                    .child(div().w(rems(4.5)).text_sm().text_right().child(mco::label(spec.kind, &v.value)))
                    .into_any_element()
            }),
        };
        let lock = (!matches!(spec.kind, Kind::Fixed(_))).then(|| {
            self.mc_lock(
                SharedString::from(format!("{id}-lock")),
                v.always,
                move |this, cx| edit(&this.store, spec, |e| e.always = !e.always, cx),
                cx,
            )
        });
        let control = (value.is_some() || lock.is_some()).then(|| {
            h_flex()
                .gap_2()
                .when(!v.on, |d| d.opacity(0.45))
                .children(value)
                .children(lock)
        });
        Row::new(id).lead(on).muted(!v.on).when_some(control, |r, c| r.control(c))
    }

    /// "Copy from an instance": a picker of instances that have options,
    /// and Import.
    fn mc_import_row(&self, rem: Pixels, cx: &mut Context<Self>) -> Row {
        let list: Vec<(std::path::PathBuf, String)> = self
            .store
            .read(cx)
            .instances()
            .iter()
            .filter(|i| i.root.join("options.txt").is_file())
            .map(|i| (i.root.clone(), i.name.clone()))
            .collect();
        if list.is_empty() {
            return Row::new("mc-import").note(Some(widgets::caption("None of your instances has been started yet.", cx).into_any_element()));
        }
        let ix = self.mc.import_from.as_ref().and_then(|p| list.iter().position(|(r, _)| r == p)).unwrap_or(0);
        let roots: Vec<_> = list.iter().map(|(r, _)| r.clone()).collect();
        let view = cx.entity().downgrade();
        let picker = div().w(rems(13.)).child(widgets::picker(
            "mc-import-from",
            list[ix].1.clone(),
            list.iter().enumerate().map(|(i, (_, n))| (n.clone(), i == ix)).collect(),
            rem,
            Rc::new(move |i, _, cx| {
                let root = roots.get(i).cloned();
                view.update(cx, |this, cx| {
                    this.mc.import_from = root;
                    this.mc.import_note = None;
                    cx.notify();
                })
                .ok();
            }),
        ));
        let (root, name) = list[ix].clone();
        let note = self.mc.import_note.as_ref().map(|n| match n {
            Ok(t) => status_line(IconName::CircleCheck, t.clone(), cx.theme().success),
            Err(e) => status_line(IconName::TriangleAlert, e.clone(), cx.theme().danger),
        });
        Row::new("mc-import")
            .control(
                h_flex().gap_2().child(picker).child(
                    Button::new("mc-import-go").outline().small().icon(IconName::Import).label("Import").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.mc.import_note = Some(match hopper::mcoptions::read_instance(&root) {
                                Ok((values, keys)) => {
                                    let (nv, nk) = (values.len(), keys.len());
                                    let from = name.clone();
                                    this.store.update(cx, |s, cx| s.update_settings(|st| st.mc_options.import(&from, values, keys), cx));
                                    let o = this.store.read(cx).settings().mc_options.clone();
                                    this.mc.build_sliders(&o, cx);
                                    Ok(format!("Copied {nv} options and {nk} key bindings from {name}."))
                                }
                                Err(e) => Err(format!("{e:#}")),
                            });
                            cx.notify();
                        },
                    )),
                ),
            )
            .note(note)
    }

    fn mc_keys_row(&self, o: &McOptions, cx: &mut Context<Self>) -> Row {
        let n = o.keys.len();
        let desc = if n == 0 {
            "Click a key below to change it. Keys you don't set stay the pack's.".to_string()
        } else {
            let from = if o.keys_from.is_empty() { String::new() } else { format!(", copied from {}", o.keys_from) };
            format!("{n} keys set{from}. Keys of mods a pack doesn't have are left out.")
        };
        let on = Switch::new("mc-keys-on").checked(o.keys_on).on_click(cx.listener(|this, on: &bool, _, cx| {
            let on = *on;
            this.store.update(cx, |s, cx| s.update_settings(|st| st.mc_options.keys_on = on, cx));
        }));
        let lock = self.mc_lock(
            "mc-keys-lock".into(),
            o.keys_always,
            |this, cx| this.store.update(cx, |s, cx| s.update_settings(|st| st.mc_options.keys_always = !st.mc_options.keys_always, cx)),
            cx,
        );
        let control = h_flex()
            .gap_2()
            .when(n > 0, |r| {
                r.child(Button::new("mc-keys-reset").ghost().small().label("Clear").tooltip("Forget every key set here").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.store.update(cx, |s, cx| {
                            s.update_settings(
                                |st| {
                                    st.mc_options.keys.clear();
                                    st.mc_options.keys_from.clear();
                                },
                                cx,
                            )
                        });
                    },
                )))
            })
            .child(
                Button::new("mc-keys-show")
                    .outline()
                    .small()
                    .icon(IconName::Keyboard)
                    .label(if self.mc.show_keys { "Hide keys" } else { "Show keys" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.mc.show_keys = !this.mc.show_keys;
                        this.mc.capture = None;
                        cx.notify();
                    })),
            )
            .child(lock);
        Row::new("mc-keys")
            .desc(desc)
            .lead(on)
            .muted(!o.keys_on)
            .control(control)
            .when(self.mc.show_keys, |r| r.below(self.mc_key_grid(o, cx)))
    }

    /// Every vanilla action, then the mods' ones that were copied in, each
    /// with its key: click it, then press a key or a mouse button.
    fn mc_key_grid(&self, o: &McOptions, cx: &mut Context<Self>) -> AnyElement {
        let vanilla: Vec<(String, Option<String>, String)> = mco::DEFAULT_KEYS
            .iter()
            .map(|(a, d)| (a.to_string(), o.keys.get(*a).cloned(), d.to_string()))
            .collect();
        let mut mods: Vec<(String, Option<String>, String)> = o
            .keys
            .iter()
            .filter(|(a, _)| !mco::DEFAULT_KEYS.iter().any(|(d, _)| d == a))
            .map(|(a, k)| (a.clone(), Some(k.clone()), String::new()))
            .collect();
        // By mod, then by name.
        mods.sort_by_cached_key(|(a, _, _)| (mco::action_mod(a).to_lowercase(), mco::action_label(a)));
        let grid = |cells: Vec<AnyElement>| div().grid().grid_cols(2).gap_x_6().gap_y_0p5().children(cells);
        let vanilla: Vec<AnyElement> = vanilla.into_iter().map(|c| self.mc_key_cell(c, cx)).collect();
        let n_mods = mods.len();
        let mods: Vec<AnyElement> = mods.into_iter().map(|c| self.mc_key_cell(c, cx)).collect();
        v_flex()
            .gap_2()
            .track_focus(&self.mc.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                let Some(action) = this.mc.capture.clone() else { return };
                cx.stop_propagation();
                if e.keystroke.key == "escape" {
                    this.mc.capture = None;
                } else if let Some(k) = mco::key_from_name(&e.keystroke.key) {
                    this.mc_set_key(action, k, cx);
                }
                cx.notify();
            }))
            .on_modifiers_changed(cx.listener(|this, e: &ModifiersChangedEvent, _, cx| {
                let Some(action) = this.mc.capture.clone() else { return };
                let m = e.modifiers;
                let name = if m.shift {
                    "shift"
                } else if m.control {
                    "control"
                } else if m.alt {
                    "alt"
                } else {
                    return;
                };
                if let Some(k) = mco::key_from_name(name) {
                    this.mc_set_key(action, k, cx);
                }
            }))
            .child(grid(vanilla))
            .when(n_mods > 0, |d| {
                d.child(
                    div()
                        .pt_2()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("Mod keys ({n_mods}): set only in packs that have the mod, from their second start")),
                )
                .child(grid(mods))
            })
            .into_any_element()
    }

    /// One action and its key.
    fn mc_key_cell(&self, (action, set, default): (String, Option<String>, String), cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let waiting = self.mc.capture.as_deref() == Some(action.as_str());
        let shown = if waiting { "Press a key…".to_string() } else { mco::key_label(set.as_deref().unwrap_or(&default)) };
        let start = {
            let a = action.clone();
            cx.listener(move |this: &mut Self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>| {
                if this.mc.bound_at.is_some_and(|t| t.elapsed() < Duration::from_millis(600)) {
                    return;
                }
                this.mc.capture = Some(a.clone());
                window.focus(&this.mc.focus, cx);
                cx.notify();
            })
        };
        let chip = div()
            .id(SharedString::from(format!("mc-key-{action}")))
            .clickable(cx)
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded(theme.radius)
            .text_xs()
            .map(|d| {
                if waiting {
                    d.bg(theme.primary.opacity(0.2)).text_color(theme.primary)
                } else if set.is_some() {
                    d.bg(theme.foreground.opacity(0.1))
                } else {
                    d.bg(theme.foreground.opacity(0.05)).text_color(theme.muted_foreground)
                }
            })
            .child(shown)
            .on_click(start)
            .on_mouse_down(MouseButton::Left, self.mc_mouse(&action, "key.mouse.left", cx))
            .on_mouse_down(MouseButton::Right, self.mc_mouse(&action, "key.mouse.right", cx))
            .on_mouse_down(MouseButton::Middle, self.mc_mouse(&action, "key.mouse.middle", cx))
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Back), self.mc_mouse(&action, "key.mouse.4", cx))
            .on_mouse_down(MouseButton::Navigate(NavigationDirection::Forward), self.mc_mouse(&action, "key.mouse.5", cx));
        let clear = set.is_some().then(|| {
            let a = action.clone();
            Button::new(SharedString::from(format!("mc-key-x-{action}")))
                .ghost()
                .xsmall()
                .icon(IconName::X)
                .tooltip("Leave this key to the pack")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let a = a.clone();
                    this.store.update(cx, |s, cx| {
                        s.update_settings(
                            |st| {
                                st.mc_options.keys.remove(&a);
                            },
                            cx,
                        )
                    });
                }))
        });
        let m = mco::action_mod(&action);
        h_flex()
            .gap_2()
            .px_2()
            .py_0p5()
            .rounded(theme.radius)
            .hover(|d| d.bg(theme.foreground.opacity(0.04)))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(div().min_w_0().text_sm().truncate().child(mco::action_label(&action)))
                    .when(!m.is_empty(), |d| d.child(widgets::chip(theme.muted_foreground).child(m.to_string()))),
            )
            .children(clear)
            .child(chip)
            .into_any_element()
    }

    /// Binds a mouse button while `action` listens for a key.
    fn mc_mouse(&self, action: &str, key: &'static str, cx: &mut Context<Self>) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static + use<> {
        let a = action.to_string();
        let view = cx.entity().downgrade();
        move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
            view.update(cx, |this, cx| this.mc_bind_mouse(&a, key, cx)).ok();
        }
    }

    fn mc_bind_mouse(&mut self, action: &str, key: &str, cx: &mut Context<Self>) {
        if self.mc.capture.as_deref() != Some(action) {
            return;
        }
        self.mc.bound_at = Some(Instant::now());
        self.mc_set_key(action.to_string(), key.to_string(), cx);
        cx.stop_propagation();
    }

    fn mc_set_key(&mut self, action: String, key: String, cx: &mut Context<Self>) {
        self.mc.capture = None;
        self.store.update(cx, |s, cx| {
            s.update_settings(
                |st| {
                    st.mc_options.keys.insert(action, key);
                    st.mc_options.keys_on = true;
                },
                cx,
            )
        });
        cx.notify();
    }
}
