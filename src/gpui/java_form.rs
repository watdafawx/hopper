//! Memory, Java and argument fields, shared by an instance's settings and
//! the global defaults.

use crate::store::Store;
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::form::{Field, Form};
use gpui_kit::component::input::{Editor, Input, InputEvent, InputState};
use crate::args_editor::{self, ArgsEditor};
use gpui_kit::component::slider::{Slider, SliderEvent, SliderState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::java::{JavaInstall, PRESETS, recommended_major};
use hopper::jvm_args::{self, Control, Options, Pick, Target};
use hopper::jvm_editor;
use hopper::settings::JavaPreset;
use std::rc::Rc;

/// What the preset name box is for.
#[derive(Clone, Copy, PartialEq)]
enum Naming {
    New,
    Rename(usize),
}

pub struct JavaForm {
    store: Entity<Store>,
    /// Two thumbs: -Xms (start) and -Xmx (end), in GB.
    memory: Entity<SliderState>,
    memory_mb: u32,
    /// -Xms in MB; 0 leaves it to Hopper (256 MB).
    xms_mb: u32,
    /// Slider minimum in GB: 0 is "Default" for an instance.
    min_gb: f32,
    java_path: String,
    args: ArgsEditor,
    args_text: String,
    /// Minecraft version, for the recommended Java (None for the defaults).
    game_version: Option<String>,
    /// What 0 memory / blank arguments fall back to (None: no fallback).
    fallback_memory: Option<u32>,
    /// The generator panel is open.
    generator: bool,
    /// The arguments shown one by one, each explained on hover or click.
    show_flags: bool,
    /// The flag whose explanation shows (clicked).
    explained: Option<String>,
    /// The saved presets show as a list to rename, update or delete.
    managing: bool,
    /// The Java the arguments are for when the form doesn't pick it (a
    /// server's Settings, which shows its own Java row).
    target: Option<Target>,
    naming: Option<Naming>,
    name: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl JavaForm {
    pub fn new(
        store: Entity<Store>,
        fallback_memory: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let min = if fallback_memory.is_some() { 0. } else { 1. };
        let memory = cx.new(|_| {
            SliderState::new()
                .min(min)
                .max(32.)
                .step(0.5)
                .default_value((min, min))
        });
        let args = ArgsEditor::new(
            if fallback_memory.is_some() {
                "Blank uses the default arguments from Settings"
            } else {
                "Extra Java arguments"
            },
            window,
            cx,
        );
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Preset name"));
        let subs = vec![
            cx.subscribe_in(&name, window, |this, _, e: &InputEvent, window, cx| {
                if matches!(e, InputEvent::PressEnter { .. }) {
                    this.commit_name(window, cx);
                }
            }),
            cx.subscribe_in(&memory, window, |this, _, e: &SliderEvent, window, cx| {
                if let SliderEvent::Change(v) = e {
                    this.memory_moved(v.start(), v.end(), window, cx);
                }
            }),
            cx.subscribe_in(&args.state, window, |this, _, e: &InputEvent, window, cx| match e {
                InputEvent::Change => {
                    // The box shows one flag per line; the saved value has them on one.
                    this.args_text = this.args.saved(cx);
                    cx.notify();
                }
                InputEvent::Blur => this.args.tidy(window, cx),
                _ => {}
            }),
        ];
        store.update(cx, |s, _| s.want_javas());
        Self {
            store,
            memory,
            memory_mb: 0,
            xms_mb: 0,
            min_gb: min,
            java_path: String::new(),
            args,
            args_text: String::new(),
            game_version: None,
            fallback_memory,
            generator: false,
            show_flags: false,
            explained: None,
            managing: false,
            target: None,
            naming: None,
            name,
            _subscriptions: subs,
        }
    }

    /// A server's slider: 1 GB up to `max_gb`, in place of the instance one.
    /// Call before [`Self::set`].
    pub fn set_max_gb(&mut self, max_gb: f32, window: &mut Window, cx: &mut Context<Self>) {
        let min = self.min_gb;
        self.memory = cx.new(|_| SliderState::new().min(min).max(max_gb.max(min + 1.)).step(0.5).default_value((min, min)));
        self._subscriptions.push(cx.subscribe_in(&self.memory, window, |this, _, e: &SliderEvent, window, cx| {
            if let SliderEvent::Change(v) = e {
                this.memory_moved(v.start(), v.end(), window, cx);
            }
        }));
    }

    /// The Java the arguments are for, when the form's own Java row isn't
    /// shown (a server's: its panel says which).
    pub fn set_target(&mut self, target: Option<Target>, cx: &mut Context<Self>) {
        self.target = target;
        cx.notify();
    }

    /// Loads values to edit.
    pub fn set(
        &mut self,
        memory_mb: u32,
        java_path: &str,
        args: &str,
        game_version: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.memory_mb = memory_mb;
        self.xms_mb = hopper::jvm_args::memory(args).0.unwrap_or(0).min(memory_mb);
        self.java_path = java_path.to_string();
        self.args_text = args.to_string();
        self.game_version = game_version;
        self.memory.update(cx, |s, cx| {
            let xmx = memory_mb as f32 / 1024.;
            let xms = if self.xms_mb == 0 { self.min_gb } else { self.xms_mb as f32 / 1024. };
            s.set_value((xms.min(xmx), xmx), window, cx)
        });
        self.args.show(args, window, cx);
        // Loading changes nothing: the value stays as saved until edited.
        self.args_text = args.to_string();
        cx.notify();
    }

    /// (memory MB, Java path, arguments)
    pub fn values(&self) -> (u32, String, String) {
        (
            self.memory_mb,
            self.java_path.clone(),
            self.args_text.trim().to_string(),
        )
    }

    fn set_args(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.args.show(text, window, cx);
        self.args_text = jvm_editor::compact(text);
        cx.notify();
    }

    /// The slider moved: -Xms and -Xmx in the arguments follow it, so what
    /// the box shows is what Java gets.
    fn memory_moved(&mut self, start: f32, end: f32, window: &mut Window, cx: &mut Context<Self>) {
        let mb = |gb: f32| (gb * 1024.).round() as u32;
        self.memory_mb = mb(end);
        // A thumb at the very start sets nothing: Hopper's 256 MB start,
        // and for an instance at 0 the default memory.
        self.xms_mb = if start <= self.min_gb { 0 } else { mb(start) };
        let xmx = (self.memory_mb > 0).then_some(self.memory_mb);
        let xms = (self.xms_mb > 0 && xmx.is_some()).then_some(self.xms_mb);
        let had = hopper::jvm_args::memory(&self.args_text);
        // Arguments left blank (falling back to the defaults) stay blank
        // until there's a start size to keep.
        if had == (xms, xmx) || (self.args_text.trim().is_empty() && xms.is_none()) {
            cx.notify();
            return;
        }
        let args = hopper::jvm_args::set_memory(&self.args_text, xms, xmx);
        self.set_args(&args, window, cx);
    }

    /// Rebuilds the arguments after a pick on a generator row (the same
    /// rebuild Hopper Server's panel runs).
    fn edit(&mut self, target: Target, id: &str, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        let args = jvm_args::regenerate(&self.args_text, target, id, pick);
        if args != self.args_text {
            self.set_args(&args, window, cx);
        }
    }

    fn presets(&self, cx: &App) -> Vec<JavaPreset> {
        self.store.read(cx).settings().java_presets.clone()
    }

    fn edit_presets(&self, f: impl FnOnce(&mut Vec<JavaPreset>), cx: &mut Context<Self>) {
        self.store
            .update(cx, |s, cx| s.update_settings(|st| f(&mut st.java_presets), cx));
    }

    fn start_naming(&mut self, naming: Naming, window: &mut Window, cx: &mut Context<Self>) {
        let value = match naming {
            Naming::New => String::new(),
            Naming::Rename(i) => self.presets(cx).get(i).map(|p| p.name.clone()).unwrap_or_default(),
        };
        self.naming = Some(naming);
        self.name.update(cx, |s, cx| {
            s.set_value(value, window, cx);
            s.focus(window, cx);
        });
        cx.notify();
    }

    /// Saves the name box: a new preset of the current arguments (replacing
    /// one of the same name), or a rename.
    fn commit_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let Some(naming) = self.naming else { return };
        if name.is_empty() {
            return;
        }
        let args = self.args_text.trim().to_string();
        self.edit_presets(
            |ps| match naming {
                Naming::New => match ps.iter_mut().find(|p| p.name.eq_ignore_ascii_case(&name)) {
                    Some(p) => p.args = args,
                    None => ps.push(JavaPreset { name, args }),
                },
                Naming::Rename(i) => {
                    if let Some(p) = ps.get_mut(i) {
                        p.name = name;
                    }
                }
            },
            cx,
        );
        self.naming = None;
        self.name.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    fn picked(&self, javas: Option<&[JavaInstall]>) -> Option<JavaInstall> {
        if self.java_path.is_empty() {
            return None;
        }
        javas
            .and_then(|js| {
                js.iter()
                    .find(|j| {
                        j.exe
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&self.java_path)
                    })
                    .cloned()
            })
            .or_else(|| hopper::java::inspect(std::path::Path::new(&self.java_path)))
    }

    /// The Memory, Java and Java arguments fields.
    fn fields(&self, rem: Pixels, cx: &mut Context<Self>) -> Vec<Field> {
        let [memory, java, args] = self.parts(rem, cx);
        vec![
            Field::new().label("Memory").child(memory),
            Field::new().label("Java").items_start().child(java),
            Field::new()
                .label("Java arguments")
                .items_start()
                .child(args),
        ]
    }

    /// The memory slider, the Java picker and the arguments box, for a
    /// page that lays them out itself (Settings).
    pub fn parts(&self, rem: Pixels, cx: &mut Context<Self>) -> [AnyElement; 3] {
        let theme = cx.theme();
        // Best first for this Minecraft (client side), when it's known.
        let javas = self.store.read(cx).javas().map(|j| {
            match self.game_version.as_deref() {
                Some(gv) => {
                    let req = hopper::java::requirement(gv, hopper::java::Side::Client);
                    hopper::java::rank(j, &req, hopper::java::Side::Client)
                        .into_iter()
                        .map(|r| r.install)
                        .collect()
                }
                None => j.to_vec(),
            }
        });
        let want = self.game_version.as_deref().map(recommended_major);
        // The first one that fits gets the star.
        let best = want.and_then(|w| {
            javas.iter().flatten().find(|j| j.major >= w && j.is_64bit).map(|j| j.exe.clone())
        });
        let none_fit = want.is_some() && best.is_none() && javas.is_some();
        let picked = self.picked(javas.as_deref());

        let gb = self.memory_mb as f32 / 1024.;
        let memory_note = match self.fallback_memory {
            Some(def) if self.memory_mb == 0 => {
                format!("Default ({:.1} GB)", def as f32 / 1024.)
            }
            _ if self.xms_mb > 0 => {
                format!("{:.1} to {gb:.1} GB", self.xms_mb as f32 / 1024.)
            }
            _ => format!("{gb:.1} GB"),
        };
        let memory = h_flex()
            .w_full()
            .max_w(rems(FIELD_W))
            // A floor for where its parent is sized by it (Settings page
            // row control): the slider would collapse otherwise.
            .min_w(rems(24.))
            .gap_4()
            .child(div().flex_1().min_w_0().child(Slider::new(&self.memory)))
            .child(
                div()
                    .flex_none()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(memory_note),
            );

        let current = match &picked {
            None if self.java_path.is_empty() => "Launcher's own Java (recommended)".to_string(),
            Some(j) => j.label(),
            None => self.java_path.clone(),
        };
        let mut items = vec![(
            "Launcher's own Java (picks the right version)".to_string(),
            self.java_path.is_empty(),
        )];
        let exes: Vec<String> = javas
            .iter()
            .flatten()
            .map(|j| j.exe.to_string_lossy().into_owned())
            .collect();
        for j in javas.iter().flatten() {
            let star = if best.as_ref() == Some(&j.exe) { "  ★" } else { "" };
            items.push((
                format!("{}{star}", j.label()),
                j.exe
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&self.java_path),
            ));
        }
        let view = cx.entity();
        let pick = {
            let view = view.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                let path = if ix == 0 {
                    String::new()
                } else {
                    exes.get(ix - 1).cloned().unwrap_or_default()
                };
                view.update(cx, |this, cx| {
                    this.java_path = path;
                    cx.notify();
                });
            })
        };
        let java_warning: Option<AnyElement> = match (want, &picked) {
            (Some(want), Some(j)) if j.major < want => Some(
                h_flex()
                    .gap_1p5()
                    .text_sm()
                    .text_color(theme.warning)
                    .child(Icon::new(IconName::TriangleAlert).small())
                    .child(format!(
                        "This Minecraft needs Java {want} or newer; this is Java {}.",
                        j.major
                    ))
                    .into_any_element(),
            ),
            (Some(want), Some(j)) if j.major > want + 4 => Some(
                widgets::caption(
                    format!("Java {want} is the usual choice; newer may not work with every mod."),
                    cx,
                )
                .into_any_element(),
            ),
            _ => None,
        };
        let detecting = javas.is_none();
        let java = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .w_full().max_w(rems(FIELD_W))
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(widgets::picker("java", current, items, rem, pick)),
                    )
                    .when_some(want, |r, major| {
                        r.children(java_get(major, false, true, none_fit, cx))
                    })
                    .child(
                        Button::new("java-browse")
                            .outline()
                            .small()
                            .label("Browse…")
                            .on_click(cx.listener(|_, _, _, cx| {
                                let paths = cx.prompt_for_paths(PathPromptOptions {
                                    files: true,
                                    directories: false,
                                    multiple: false,
                                    prompt: Some("Use this Java".into()),
                                });
                                cx.spawn(async move |this, cx| {
                                    if let Ok(Ok(Some(p))) = paths.await
                                        && let Some(p) = p.into_iter().next()
                                    {
                                        this.update(cx, |this, cx| {
                                            this.java_path = p.to_string_lossy().into_owned();
                                            cx.notify();
                                        })
                                        .ok();
                                    }
                                })
                                .detach();
                            })),
                    ),
            )
            .when(detecting, |r| {
                r.child(widgets::caption("Looking for Java installs", cx).text_xs())
            })
            .children(java_warning);

        // The Java the arguments are for: the picked one, else the
        // launcher's own (never GraalVM).
        let target = match &picked {
            _ if self.target.is_some() => self.target.unwrap_or_default(),
            Some(j) => Target::of(j),
            None if self.java_path.is_empty() => Target {
                major: want,
                ..Target::default()
            },
            None => Target::default(),
        };
        let slider = if self.memory_mb > 0 {
            jvm_editor::Memory::Slider { xmx_mb: self.memory_mb }
        } else {
            jvm_editor::Memory::Free
        };
        self.args.refresh(args_editor::Context { target, memory: slider }, cx);
        let mut warnings = jvm_args::conflicts(&self.args_text, target);
        // Launching uses the slider's -Xmx; one typed in the box is dropped.
        if let (_, Some(xmx)) = jvm_args::memory(&self.args_text)
            && xmx != self.memory_mb
        {
            warnings.push(format!(
                "-Xmx{} here isn't used: the slider's maximum is. Move the slider to update it.",
                jvm_args::format_mb(xmx)
            ));
        }

        let saved = self.presets(cx);
        let presets = h_flex()
            .gap_1()
            .flex_wrap()
            .child(widgets::caption("Presets", cx).text_xs().pr_1())
            .children(PRESETS.iter().map(|(name, args)| {
                Button::new(SharedString::from(format!("preset:{name}")))
                    .xsmall()
                    .ghost()
                    .label(*name)
                    .tooltip(*args)
                    .on_click(cx.listener(move |this, _, w, cx| this.set_args(args, w, cx)))
            }))
            .children(saved.iter().enumerate().map(|(i, p)| {
                let args = p.args.clone();
                Button::new(("saved-preset", i))
                    .xsmall()
                    .outline()
                    .label(p.name.clone())
                    .tooltip(if p.args.is_empty() { "No arguments".to_string() } else { p.args.clone() })
                    .on_click(cx.listener(move |this, _, w, cx| this.set_args(&args, w, cx)))
            }))
            .child(
                Button::new("preset-save")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Plus)
                    .label("Save as preset")
                    .tooltip("Saves the arguments above under a name")
                    .on_click(cx.listener(|this, _, w, cx| this.start_naming(Naming::New, w, cx))),
            )
            .when(!saved.is_empty(), |r| {
                r.child(
                    Button::new("preset-manage")
                        .xsmall()
                        .ghost()
                        .icon(IconName::Pencil)
                        .label("Edit presets")
                        .selected(self.managing)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.managing = !this.managing;
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("preset-clear")
                    .xsmall()
                    .ghost()
                    .label("Clear")
                    .on_click(cx.listener(|this, _, w, cx| this.set_args("", w, cx))),
            );

        let naming = self.naming.map(|n| {
            h_flex()
                .gap_2()
                .child(div().w(rems(16.)).child(Input::new(&self.name).small()))
                .child(
                    Button::new("name-save")
                        .small()
                        .primary()
                        .label(match n {
                            Naming::New => "Save",
                            Naming::Rename(_) => "Rename",
                        })
                        .on_click(cx.listener(|this, _, w, cx| this.commit_name(w, cx))),
                )
                .child(
                    Button::new("name-cancel")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.naming = None;
                            cx.notify();
                        })),
                )
                .when(n == Naming::New, |r| {
                    r.child(widgets::caption("A preset with the same name is replaced.", cx).text_xs())
                })
        });

        let manage = (self.managing && !saved.is_empty()).then(|| {
            v_flex()
                .max_w(rems(40.))
                .gap_1()
                .children(saved.iter().enumerate().map(|(i, p)| {
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .w(rems(9.))
                                .flex_none()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .truncate()
                                .child(p.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(widgets::caption(p.args.clone(), cx).text_xs()),
                        )
                        .child(
                            Button::new(("preset-rename", i))
                                .xsmall()
                                .ghost()
                                .label("Rename")
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.start_naming(Naming::Rename(i), w, cx)
                                })),
                        )
                        .child(
                            Button::new(("preset-update", i))
                                .xsmall()
                                .ghost()
                                .label("Use current")
                                .tooltip("Replaces this preset's arguments with the ones above")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let args = this.args_text.trim().to_string();
                                    this.edit_presets(
                                        |ps| {
                                            if let Some(p) = ps.get_mut(i) {
                                                p.args = args;
                                            }
                                        },
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            Button::new(("preset-delete", i))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Trash)
                                .tooltip("Delete this preset")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.edit_presets(
                                        |ps| {
                                            if i < ps.len() {
                                                ps.remove(i);
                                            }
                                        },
                                        cx,
                                    );
                                    if this.naming == Some(Naming::Rename(i)) {
                                        this.naming = None;
                                    }
                                })),
                        )
                }))
        });

        // Every flag in use, each explained on hover or click.
        let tokens = jvm_args::split(&self.args_text);
        let n_flags = tokens.len();
        // The text box already shows them: one by one only on request.
        let flags = (self.show_flags && !tokens.is_empty()).then(|| {
            h_flex()
                .max_w(rems(40.))
                .flex_wrap()
                .gap_1()
                .children(tokens.iter().enumerate().map(|(i, t)| {
                    let known = jvm_args::explain(t);
                    let tip = known
                        .clone()
                        .unwrap_or_else(|| "Hopper doesn't know this flag. It's kept as it is.".into());
                    let token = t.clone();
                    Button::new(("flag", i))
                        .xsmall()
                        .map(|b| if known.is_some() { b.outline() } else { b.ghost() })
                        .label(t.clone())
                        .tooltip(tip)
                        .selected(self.explained.as_deref() == Some(t.as_str()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.explained = if this.explained.as_deref() == Some(token.as_str()) {
                                None
                            } else {
                                Some(token.clone())
                            };
                            cx.notify();
                        }))
                }))
        });
        let explained = self
            .explained
            .as_ref()
            .filter(|t| tokens.contains(t))
            .map(|t| {
                let text = jvm_args::explain(t)
                    .unwrap_or_else(|| "Hopper doesn't know this flag. It's kept as it is.".into());
                h_flex()
                    .max_w(rems(40.))
                    .gap_2()
                    .items_start()
                    .text_sm()
                    .child(Icon::new(IconName::Info).small().text_color(cx.theme().muted_foreground))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(t.clone()),
                            )
                            .child(div().text_color(cx.theme().muted_foreground).child(text)),
                    )
            });

        let generator = self.generator.then(|| self.generator_panel(target, cx));

        let args = v_flex()
            .gap_2()
            .child({
                // Fits the flags up to 12 rows, then scrolls; or the height
                // dragged on its bottom edge.
                let h = self.args.height(self.store.read(cx).settings().java_args_rows, cx);
                let editor = Editor::new(&self.args.state).h_full();
                self.args.tab_accepts(
                    self.args.with_handle(editor, h, |this: &mut Self| &mut this.args, &self.store, cx).max_w(rems(FIELD_W)),
                )
            })
            .child(
                h_flex()
                    .w_full().max_w(rems(FIELD_W))
                    .items_start()
                    .gap_2()
                    .child(presets.flex_1().min_w_0())
                    .when(n_flags > 0, |r| {
                        r.child(
                            widgets::caption(format!("{n_flags} flag{}", if n_flags == 1 { "" } else { "s" }), cx)
                                .text_xs()
                                .flex_none()
                                .pt_0p5(),
                        )
                    }),
            )
            .children(naming)
            .children(manage)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("generator")
                            .small()
                            .map(|b| if self.generator { b.primary() } else { b.outline() })
                            .icon(IconName::SlidersHorizontal)
                            .label("Generator")
                            .tooltip("Pick options and Hopper writes the arguments")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.generator = !this.generator;
                                cx.notify();
                            })),
                    )
                    .when(n_flags > 0, |r| {
                        r.child(
                            Button::new("explain-flags")
                                .small()
                                .ghost()
                                .icon(if self.show_flags { IconName::ChevronDown } else { IconName::ChevronRight })
                                .label("Explain each")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_flags = !this.show_flags;
                                    this.explained = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        widgets::caption(
                            "The memory slider sets -Xms (left) and -Xmx (right).",
                            cx,
                        )
                            .text_xs(),
                    ),
            )
            .children(generator)
            .children(flags)
            .children(explained)
            .children(warnings.into_iter().map(|w| {
                h_flex()
                    .max_w(rems(40.))
                    .gap_1p5()
                    .items_start()
                    .text_sm()
                    .text_color(cx.theme().warning)
                    .child(Icon::new(IconName::TriangleAlert).small())
                    .child(div().flex_1().min_w_0().child(w))
            }));

        [
            memory.into_any_element(),
            java.into_any_element(),
            args.into_any_element(),
        ]
    }

    /// The generator: options with a sentence each. Every change rewrites
    /// the arguments at once; flags it doesn't cover stay after its own.
    /// The rows come from hopper-core, shared with Hopper Server's panel.
    fn generator_panel(&self, target: Target, cx: &mut Context<Self>) -> AnyElement {
        let o = Options::parse(&self.args_text);
        let mut section = None;
        let mut rows: Vec<AnyElement> = Vec::new();
        for row in o.rows(target) {
            if row.section != section {
                section = row.section;
                if let Some(head) = section {
                    rows.push(
                        div()
                            .pt_2()
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().muted_foreground)
                            .child(head.to_uppercase())
                            .into_any_element(),
                    );
                }
            }
            let id = row.id;
            let control = match row.control {
                Control::Switch { on } => Switch::new(SharedString::from(format!("gen-{id}")))
                    .checked(on)
                    .on_click(cx.listener(move |this, v: &bool, w, cx| this.edit(target, id, Pick::On(*v), w, cx)))
                    .into_any_element(),
                Control::Choice { labels, selected } => segmented(
                    SharedString::from(format!("gen-{id}")),
                    labels,
                    selected,
                    cx.listener(move |this, ix: &usize, w, cx| this.edit(target, id, Pick::Index(*ix), w, cx)),
                )
                .into_any_element(),
            };
            rows.push(gen_row(row.label, row.note, control, cx));
        }
        let kept = o.extra.len();
        v_flex()
            .max_w(rems(40.))
            .gap_2()
            .p_3()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(cx.theme().border)
            .children(rows)
            .when(kept > 0, |r| {
                r.child(
                    widgets::caption(
                        format!(
                            "{kept} other flag{} kept as {}: {}",
                            if kept == 1 { "" } else { "s" },
                            if kept == 1 { "it is" } else { "they are" },
                            o.extra.join(" ")
                        ),
                        cx,
                    )
                    .text_xs(),
                )
            })
            .into_any_element()
    }
}

impl Render for JavaForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let fields = self.fields(rem, cx);
        settings_form(rem).children(fields)
    }
}

/// The width of a settings form's field column in rems: inputs, pickers
/// and the arguments box all end at the same edge.
pub const FIELD_W: f32 = 40.;

/// A settings form's whole width (label, gap, field) in rems: pages centre
/// their forms in a column this wide, like the Settings page.
pub const FORM_W: f32 = 50.;

/// The label column and rhythm every settings form shares, so stacked
/// forms line up.
pub fn settings_form(rem: Pixels) -> Form {
    Form::horizontal()
        .large()
        .label_width(rems(9.).to_pixels(rem))
}

/// One generator option: what it is and a sentence on the left, its
/// control on the right.
fn gen_row(label: &str, note: String, control: AnyElement, cx: &App) -> AnyElement {
    h_flex()
        .gap_4()
        .justify_between()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(label.to_string()))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).child(note)),
        )
        .child(div().flex_none().child(control))
        .into_any_element()
}

fn segmented(
    id: impl Into<ElementId>,
    labels: Vec<String>,
    selected: Option<usize>,
    on_pick: impl Fn(&usize, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    TabBar::new(id)
        .segmented()
        .xsmall()
        .when_some(selected, |t, i| t.selected_index(i))
        .children(labels.into_iter().map(|l| Tab::new().label(l)))
        .on_click(on_pick)
}

/// A button that downloads a Java ("java-21" Temurin, "graalvm-25"): it
/// shows the download's progress, repainting only itself while it runs.
pub struct JavaGet {
    store: Entity<Store>,
    key: String,
    major: u32,
    graal: bool,
    label: SharedString,
    tooltip: SharedString,
    primary: std::rc::Rc<std::cell::Cell<bool>>,
    small: bool,
    _tick: Option<Task<()>>,
}

impl JavaGet {
    fn start(&mut self, cx: &mut Context<Self>) {
        let (major, graal) = (self.major, self.graal);
        self.store.update(cx, |s, cx| {
            if graal {
                s.download_graalvm(major)
            } else {
                s.download_java(major)
            }
            cx.notify();
        });
        self.watch(cx);
    }

    fn watch(&mut self, cx: &mut Context<Self>) {
        if self._tick.is_some() {
            return;
        }
        self._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
                let more = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        let more = this.store.read(cx).java_download(&this.key).is_some();
                        if !more {
                            this._tick = None;
                        }
                        more
                    })
                    .unwrap_or(false);
                if !more {
                    break;
                }
            }
        }));
    }
}

impl Render for JavaGet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.store.read(cx).java_download(&self.key);
        if running.is_some() {
            // Opened while a download already runs (another page started it).
            self.watch(cx);
        }
        let progress = running.as_ref().and_then(|j| j.progress);
        let b = Button::new(SharedString::from(format!("get:{}", self.key)))
            .map(|b| if self.small { b.small() } else { b })
            .map(|b| if self.primary.get() && running.is_none() { b.primary() } else { b.outline() })
            .icon(IconName::Download)
            .tooltip(self.tooltip.clone());
        let b = match (&running, progress) {
            (Some(_), Some(p)) => b.loading(true).disabled(true).label(format!("Downloading {}%", (p * 100.0).floor() as u32)),
            (Some(_), None) => b.loading(true).disabled(true).label("Downloading…"),
            (None, _) => b.label(self.label.clone()).on_click(cx.listener(|this, _, _, cx| this.start(cx))),
        };
        widgets::progress_fill(b, progress, cx)
    }
}

/// Every [`JavaGet`], made at start ([`init_java_gets`]) so any page can
/// show one without making it: by (key, wording for a page that needs it).
struct JavaGets(std::collections::HashMap<(String, bool), Entity<JavaGet>>);
impl Global for JavaGets {}

/// Makes the Java download buttons: Temurin 8–25 in both wordings, GraalVM.
pub fn init_java_gets(store: &Entity<Store>, cx: &mut App) {
    let mut map = std::collections::HashMap::new();
    let mut add = |major: u32, graal: bool, need: bool, cx: &mut App| {
        let key = if graal { format!("graalvm-{major}") } else { format!("java-{major}") };
        let (label, tooltip): (SharedString, SharedString) = match (graal, need) {
            (true, _) => (format!("GraalVM {major}").into(), "Oracle GraalVM: often faster for big modpacks".into()),
            (false, true) => (format!("Download Java {major}").into(), "Eclipse Temurin, into Hopper's own folder".into()),
            (false, false) => (format!("Java {major}").into(), "Eclipse Temurin".into()),
        };
        let e = cx.new(|_| JavaGet {
            store: store.clone(),
            key: key.clone(),
            major,
            graal,
            label,
            tooltip,
            primary: std::rc::Rc::new(std::cell::Cell::new(false)),
            small: true,
            _tick: None,
        });
        map.insert((key, need), e);
    };
    for m in [8u32, 11, 16, 17, 21, 25] {
        add(m, false, false, cx);
        add(m, false, true, cx);
    }
    for &m in hopper::javadl::GRAALVM_MAJORS {
        add(m, true, false, cx);
    }
    cx.set_global(JavaGets(map));
}

/// The download button for Temurin `major` (or GraalVM when `graal`);
/// `need`: worded for a page that needs that Java, filled in when `primary`.
pub fn java_get(major: u32, graal: bool, need: bool, primary: bool, cx: &App) -> Option<Entity<JavaGet>> {
    let key = if graal { format!("graalvm-{major}") } else { format!("java-{major}") };
    let e = cx.try_global::<JavaGets>()?.0.get(&(key, need))?.clone();
    e.read(cx).primary.set(primary);
    Some(e)
}
