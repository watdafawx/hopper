//! An instance's Performance tab: how each recorded run went, and the runs
//! side by side by Java arguments (one row per preset).

use crate::store::{Msg, Store, StoreEvent, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::perf::advisor::{self, Advice, Change, Fix, Severity, Tone};
use hopper::perf::{Group, Session};
use std::collections::HashSet;
use std::rc::Rc;

/// A group and the name it shows under.
struct Labeled {
    group: Group,
    label: String,
}

impl std::ops::Deref for Labeled {
    type Target = Group;
    fn deref(&self) -> &Group {
        &self.group
    }
}
use std::path::PathBuf;

/// Runs shorter than this don't count in the comparison.
const MIN_RUN_SECS: u64 = 60;

/// A change made from a piece of advice, kept so it can be reverted.
struct Applied {
    id: u64,
    root: PathBuf,
    text: String,
    before: Change,
    after: Change,
}

pub struct PerfView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    sessions: Vec<Session>,
    loading: bool,
    /// The PC's RAM, MB.
    ram_mb: u64,
    /// Changes made this session, newest last.
    applied: Vec<Applied>,
    next_id: u64,
    /// Advice with its "Why" open.
    why: HashSet<&'static str>,
    _sub: Subscription,
}

fn card(cx: &App) -> Div {
    let theme = cx.theme();
    v_flex()
        .gap_3()
        .p_4()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.border)
}

fn fmt(v: Option<f64>, digits: usize, unit: &str) -> String {
    v.map_or("–".into(), |v| format!("{v:.digits$}{unit}"))
}

fn gb(mb: f64) -> String {
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

fn duration(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

/// "4–8 GB", "8 GB".
fn memory(xms: Option<u32>, xmx: Option<u32>) -> String {
    let g = |mb: u32| {
        if mb % 1024 == 0 {
            format!("{}", mb / 1024)
        } else {
            format!("{:.1}", mb as f64 / 1024.0)
        }
    };
    match (xms, xmx) {
        (Some(a), Some(b)) if a != b => format!("{}–{} GB", g(a), g(b)),
        (_, Some(b)) => format!("{} GB", g(b)),
        _ => String::new(),
    }
}

/// Column widths in rems, first the arguments.
const COLS: [(&str, f32, &str); 8] = [
    ("Runs", 5.5, "Runs over a minute, and time played"),
    ("Avg FPS", 4.5, "Average frames per second after loading"),
    ("1% low", 4.5, "FPS of the slowest 1% of frames"),
    ("p99 frame", 5.0, "99% of frames were at least this fast"),
    ("Worst GC", 5.0, "Longest garbage collection pause"),
    ("p99 GC", 4.5, "99% of GC pauses were at most this long"),
    ("Peak heap", 5.0, "Most Java heap in use"),
    ("Load", 4.0, "Start until the main menu (median)"),
];

impl PerfView {
    pub fn new(store: Entity<Store>, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let _sub = cx.subscribe(&store, |this, _, e: &StoreEvent, cx| {
            if let StoreEvent::SessionSaved(root) = e
                && this.root.as_ref() == Some(root)
            {
                this.reload(cx);
            }
        });
        Self {
            store,
            root: None,
            sessions: Vec::new(),
            loading: false,
            ram_mb: advisor::system_ram_mb(),
            applied: Vec::new(),
            next_id: 1,
            why: HashSet::new(),
            _sub,
        }
    }

    pub fn show_for(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.sessions.clear();
        }
        self.root = Some(root);
        self.reload(cx);
    }

    /// Reads the instance's sessions off the UI thread.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let (Some(root), Some(base)) = (self.root.clone(), hopper::perf::sessions_dir()) else {
            return;
        };
        self.loading = true;
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let mut sessions = cx
                .background_spawn(async move { hopper::perf::load_all(&base, &r) })
                .await;
            // The per-second samples are only drawn as a small line.
            for s in &mut sessions {
                let step = s.samples.len().div_ceil(120).max(1);
                if step > 1 {
                    s.samples = s.samples.iter().step_by(step).cloned().collect();
                }
            }
            this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    this.sessions = sessions;
                    this.loading = false;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn delete(&mut self, id: String, cx: &mut Context<Self>) {
        let (Some(root), Some(base)) = (self.root.clone(), hopper::perf::sessions_dir()) else {
            return;
        };
        if let Err(e) = hopper::perf::delete(&base, &root, &id) {
            log::warn!("deleting session: {e:#}");
        }
        self.sessions.retain(|s| s.id != id);
        cx.notify();
    }

    /// Asks before deleting a run: its numbers can't come back.
    fn ask_delete(&mut self, id: String, what: String, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (view, id) = (view.clone(), id.clone());
            alert
                .title("Delete this run?")
                .description(format!("{what}. Its numbers leave the comparison and can't be brought back."))
                .button_props(
                    gpui_kit::component::dialog::DialogButtonProps::default()
                        .ok_text("Delete")
                        .ok_variant(gpui_kit::component::button::ButtonVariant::Danger)
                        .show_cancel(true),
                )
                .on_ok(move |_, _, cx| {
                    view.update(cx, |v, cx| v.delete(id.clone(), cx));
                    true
                })
        });
    }

    /// The memory limit the instance runs with: -Xmx, else the setting,
    /// else Hopper's default.
    fn effective_mb(inst: &hopper::instances::Instance, default_mb: u32) -> u32 {
        hopper::jvm_args::memory(&inst.settings.java_args)
            .1
            .unwrap_or(if inst.settings.memory_mb > 0 { inst.settings.memory_mb } else { default_mb })
    }

    fn setup(&self, cx: &App) -> Option<advisor::Setup> {
        let store = self.store.read(cx);
        let inst = store.instance(self.root.as_ref()?)?;
        Some(advisor::Setup {
            xmx_mb: Self::effective_mb(inst, store.settings().default_memory_mb),
            args: inst.settings.java_args.clone(),
            ram_mb: self.ram_mb,
        })
    }

    /// Saves new memory and arguments the way the Settings page does.
    fn save_settings(&self, root: &PathBuf, c: &Change, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let store = self.store.read(cx);
        if let Some(why) = store.edit_hold_reason(root) {
            window.push_notification(Notification::error(format!("Can't change settings now: {why}.")), cx);
            return false;
        }
        let Some(mut inst) = store.instance(root).cloned() else {
            window.push_notification(Notification::error("That instance is gone."), cx);
            return false;
        };
        inst.settings.memory_mb = c.memory_mb;
        inst.settings.java_args = c.java_args.clone();
        match inst.save() {
            Ok(()) => {
                let r = root.clone();
                store.spawn(move || Msg::InstanceChanged(r));
                true
            }
            Err(e) => {
                window.push_notification(Notification::error(format!("{e:#}")), cx);
                false
            }
        }
    }

    /// Applies a piece of advice, then offers Undo.
    fn apply(&mut self, fix: Fix, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        let store = self.store.read(cx);
        let Some(inst) = store.instance(&root) else { return };
        let xmx = Self::effective_mb(inst, store.settings().default_memory_mb);
        let before = Change { memory_mb: inst.settings.memory_mb, java_args: inst.settings.java_args.clone() };
        let after = advisor::apply(&fix, xmx, &before.java_args, self.ram_mb);
        if after == before || !self.save_settings(&root, &after, window, cx) {
            return;
        }
        let (text, toast) = match &fix {
            Fix::SetMemory { .. } => (
                format!("Memory {} → {}", advisor::gb_text(xmx), advisor::gb_text(after.memory_mb)),
                format!("Memory is now {} (was {}).", advisor::gb_text(after.memory_mb), advisor::gb_text(xmx)),
            ),
            Fix::SwitchPreset { name, .. } => (format!("Preset {name}"), format!("Switched to preset {name}.")),
        };
        let id = self.next_id;
        self.next_id += 1;
        self.applied.push(Applied { id, root, text, before, after });
        let view = cx.entity();
        let undone = Rc::new(std::cell::Cell::new(false));
        let note = Notification::success(toast).action(move |_, _, cx| {
            let (view, undone) = (view.clone(), undone.clone());
            Button::new("perf-undo")
                .label("Undo")
                .disabled(undone.get())
                .on_click(cx.listener(move |note, _, window, cx| {
                    if !undone.replace(true) {
                        view.update(cx, |v, cx| v.revert(id, window, cx));
                    }
                    note.dismiss(window, cx);
                }))
        })
        // Revert under "Applied changes" keeps the undo.
        .autohide(true);
        window.push_notification(note, cx);
        cx.notify();
    }

    /// Puts an applied change back, unless the settings moved on since.
    fn revert(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pos) = self.applied.iter().position(|a| a.id == id) else { return };
        let a = self.applied.remove(pos);
        cx.notify();
        let now = self
            .store
            .read(cx)
            .instance(&a.root)
            .map(|i| (i.settings.memory_mb, i.settings.java_args.trim().to_string()));
        if now != Some((a.after.memory_mb, a.after.java_args.trim().to_string())) {
            window.push_notification(
                Notification::warning("The settings were changed since, so Hopper left them as they are."),
                cx,
            );
            return;
        }
        if self.save_settings(&a.root, &a.before, window, cx) {
            window.push_notification(Notification::success(format!("Reverted: {}.", a.text)), cx);
        }
    }

    fn advice_card(&self, i: usize, a: &Advice, primary: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (color, icon) = match a.severity {
            Severity::High => (theme.danger, IconName::TriangleAlert),
            Severity::Warning => (theme.warning, IconName::TriangleAlert),
            Severity::Tip => (theme.primary, IconName::Sparkles),
            Severity::Info => (theme.muted_foreground, IconName::Info),
        };
        let tone = |t: Tone| match t {
            Tone::Bad => theme.danger,
            Tone::Warn => theme.warning,
            Tone::Good => theme.success,
            Tone::Plain => theme.muted_foreground,
        };
        let open = self.why.contains(a.id);
        let id = a.id;
        let apply = a.fix.clone().map(|fix| {
            let label = match &fix {
                Fix::SetMemory { to_mb } => format!("Apply: {}", advisor::gb_text(*to_mb)),
                Fix::SwitchPreset { .. } => "Switch preset".to_string(),
            };
            let b = Button::new(("perf-apply", i)).small().label(label);
            let b = if primary { b.primary() } else { b.outline() };
            b.on_click(cx.listener(move |this, _, window, cx| this.apply(fix.clone(), window, cx)))
        });
        let chips: Vec<_> = a.evidence.iter().map(|e| widgets::chip(tone(e.tone)).child(e.text.clone())).collect();
        let theme = cx.theme();
        widgets::row_selected_in(
            v_flex().id(("perf-advice", i)).gap_2().px_3().py_2p5().rounded(theme.radius_lg),
            color,
        )
        .child(
            h_flex()
                .gap_3()
                .items_start()
                .child(div().pt_0p5().child(Icon::new(icon).size_4().text_color(color)))
                // Capped so Why and Apply stay by the text on wide windows.
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .max_w(rems(48.))
                        .gap_1()
                        .child(div().text_sm().font_semibold().child(a.title.clone()))
                        .child(div().text_sm().text_color(theme.muted_foreground).child(a.detail.clone()))
                        .child(h_flex().gap_1p5().flex_wrap().children(chips))
                        .when_some(a.note.clone(), |c, n| c.child(div().text_xs().text_color(theme.muted_foreground).child(n))),
                )
                .child(
                    Button::new(("perf-why", i))
                        .ghost()
                        .small()
                        .label(if open { "Hide" } else { "Why" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.why.remove(id) {
                                this.why.insert(id);
                            }
                            cx.notify();
                        })),
                )
                .children(apply),
        )
        .when(open, |c| {
            c.child(
                v_flex()
                    .gap_1()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("Rule: {}", a.rule)))
                    .when(a.series.len() > 1, |c| {
                        c.child(div().text_xs().text_color(theme.muted_foreground).child(a.series_label.clone())).child(
                            div().h(rems(3.)).w_full().child(widgets::sparkline(
                                a.series.clone(),
                                if a.series_label.contains('%') { 100.0 } else { 0.0 },
                                theme.primary,
                            )),
                        )
                    }),
            )
        })
        .into_any_element()
    }

    /// The advice cards, or why there are none.
    fn advice_section(&self, n_runs: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let setup = self.setup(cx)?;
        let presets = self.store.read(cx).settings().java_presets.clone();
        let report = advisor::advise(&self.sessions, &presets, &setup);
        let mine: Vec<&Applied> = self.applied.iter().filter(|a| Some(&a.root) == self.root.as_ref()).collect();
        let mut out = v_flex().gap_2();
        if !report.advice.is_empty() {
            let first_fix = report.advice.iter().position(|a| a.fix.is_some());
            let cards: Vec<AnyElement> = report
                .advice
                .iter()
                .enumerate()
                .map(|(i, a)| self.advice_card(i, a, first_fix == Some(i), cx))
                .collect();
            out = out
                .child(
                    h_flex()
                        .gap_2()
                        .child(div().font_semibold().child("Advice"))
                        .child(widgets::chip(cx.theme().primary).child(report.advice.len().to_string()))
                        .child(widgets::caption(format!("from {} runs", report.runs), cx).text_xs()),
                )
                .children(cards);
        } else if !report.enough && n_runs > 0 && mine.is_empty() {
            out = out.child(widgets::empty_state(
                IconName::Sparkles,
                "Play a few more times for advice",
                Some(
                    format!(
                        "Hopper needs at least {} runs over a minute to judge memory and settings. {} counted so far.",
                        advisor::MIN_RUNS,
                        report.runs
                    )
                    .into(),
                ),
                cx,
            ));
        } else if report.enough && mine.is_empty() {
            out = out.child(
                h_flex()
                    .gap_2()
                    .child(Icon::new(IconName::CircleCheck).size_4().text_color(cx.theme().success))
                    .child(widgets::caption("Nothing to fix. Memory and settings look right for your recent runs.", cx)),
            );
        }
        if !mine.is_empty() {
            let rows: Vec<AnyElement> = mine
                .iter()
                .rev()
                .map(|a| {
                    let id = a.id;
                    let theme = cx.theme();
                    h_flex()
                        .id(("perf-applied", a.id as usize))
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .items_center()
                        .hover(|s| s.bg(theme.list_hover))
                        .child(Icon::new(IconName::CircleCheck).size_4().text_color(theme.success))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .max_w(rems(48.))
                                .child(div().text_sm().font_semibold().child(a.text.clone()))
                                .child(
                                    div().text_xs().text_color(theme.muted_foreground).child(
                                        "Changed in this instance's Java settings. The next run shows if it helped.",
                                    ),
                                ),
                        )
                        .child(
                            Button::new(("perf-revert", a.id as usize))
                                .outline()
                                .small()
                                .icon(IconName::Undo2)
                                .label("Revert")
                                .on_click(cx.listener(move |this, _, window, cx| this.revert(id, window, cx))),
                        )
                        .into_any_element()
                })
                .collect();
            out = out.child(card(cx).gap_1().child(div().font_semibold().pb_1().child("Applied changes")).children(rows));
        }
        Some(out.into_any_element())
    }

    fn header_row(cx: &App) -> Div {
        let theme = cx.theme();
        h_flex()
            .gap_2()
            .px_2()
            .text_xs()
            .font_semibold()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w(rems(8.)).child("JAVA ARGUMENTS"))
            .children(COLS.iter().enumerate().map(|(i, (name, w, tip))| {
                let tip = SharedString::from(*tip);
                div()
                    .id(("perf-col", i))
                    .w(rems(*w))
                    .flex_none()
                    .text_right()
                    .child(name.to_uppercase())
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
                    })
            }))
    }

    /// Which group is best in each column (index), when there are several.
    fn best(groups: &[Group]) -> [Option<usize>; 8] {
        let pick = |f: &dyn Fn(&Group) -> Option<f64>, higher: bool| -> Option<usize> {
            if groups.len() < 2 {
                return None;
            }
            groups
                .iter()
                .enumerate()
                .filter_map(|(i, g)| f(g).map(|v| (i, v)))
                .max_by(|a, b| if higher { a.1.total_cmp(&b.1) } else { b.1.total_cmp(&a.1) })
                .map(|(i, _)| i)
        };
        [
            None,
            pick(&|g| g.avg_fps, true),
            pick(&|g| g.low1_fps, true),
            pick(&|g| g.frame_p99_ms, false),
            pick(&|g| g.worst_pause_ms, false),
            pick(&|g| g.p99_pause_ms, false),
            None,
            pick(&|g| g.load_secs, false),
        ]
    }

    fn group_row(i: usize, g: &Labeled, best: &[Option<usize>; 8], cx: &App) -> AnyElement {
        let theme = cx.theme();
        let cells = [
            format!("{} · {}", g.runs, duration(g.seconds)),
            fmt(g.avg_fps, 0, ""),
            fmt(g.low1_fps, 0, ""),
            fmt(g.frame_p99_ms, 1, " ms"),
            fmt(g.worst_pause_ms, 0, " ms"),
            fmt(g.p99_pause_ms, 1, " ms"),
            g.peak_heap_mb.map_or("–".into(), gb),
            fmt(g.load_secs, 0, "s"),
        ];
        let mem = memory(g.xms_mb, g.xmx_mb);
        let name = if mem.is_empty() { g.label.clone() } else { format!("{} · {mem}", g.label) };
        let args = g.args.join(" ");
        h_flex()
            .id(("perf-group", i))
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(theme.radius)
            .hover(|s| s.bg(theme.list_hover))
            .child(
                v_flex()
                    .flex_1()
                    .min_w(rems(8.))
                    .child(div().text_sm().font_semibold().truncate().child(name))
                    .child(widgets::caption(args, cx).text_xs().truncate()),
            )
            .children(cells.into_iter().enumerate().map(|(c, text)| {
                div()
                    .w(rems(COLS[c].1))
                    .flex_none()
                    .text_right()
                    .text_sm()
                    .when(best[c] == Some(i), |d| d.text_color(theme.success).font_semibold())
                    .child(text)
            }))
            .into_any_element()
    }

    fn session_row(&self, s: &Session, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let presets = &self.store.read(cx).settings().java_presets;
        let name = hopper::perf::args_label(&s.args, presets);
        let mem = memory(s.xms_mb, s.xmx_mb);
        // No end recorded: the game is running now, or it stopped without
        // a clean exit (killed or crashed hard).
        let running_now = self
            .root
            .as_ref()
            .and_then(|r| self.store.read(cx).running(r))
            .is_some_and(|r| s.started + 5 >= r.started);
        let when = format!(
            "{} · {}",
            hopper::time::relative_secs(s.started as i64),
            match (s.ended, running_now) {
                (0, true) => "running now".to_string(),
                (0, false) => "length unknown: the game didn't close cleanly".to_string(),
                _ => duration(s.seconds()),
            }
        );
        let what = format!("{name}, {}", hopper::time::relative_secs(s.started as i64));
        let mut facts: Vec<String> = Vec::new();
        match &s.frames {
            Some(f) => facts.push(format!(
                "{:.0} FPS · 1% low {:.0} · p99 {:.1} ms · worst {:.0} ms",
                f.avg_fps, f.low1_fps, f.p99_ms, f.max_ms
            )),
            None if !s.frames_note.is_empty() => facts.push(s.frames_note.clone()),
            None => {}
        }
        if let Some(g) = s.gc.as_ref().filter(|g| g.pauses > 0) {
            facts.push(format!(
                "{} GC · {} pauses, worst {:.0} ms · heap {}",
                if g.collector.is_empty() { "GC" } else { &g.collector },
                g.pauses,
                g.max_pause_ms,
                gb(g.peak_heap_mb)
            ));
        }
        let mut sys = vec![format!("CPU {:.0}%", s.cpu_avg), format!("RAM {}", gb(s.ram_peak_mb))];
        if let Some(g) = s.gpu_avg {
            sys.push(format!("GPU {g:.0}%"));
        }
        if let Some(v) = s.vram_peak_mb {
            sys.push(format!("VRAM {}", gb(v)));
        }
        if let Some(l) = s.load_secs {
            sys.push(format!("loaded in {l:.0}s"));
        }
        if !s.java.is_empty() {
            sys.push(format!("Java {}", s.java));
        }
        facts.push(sys.join(" · "));
        let fps: Vec<f32> = s.samples.iter().map(|x| x.fps.unwrap_or(0.0)).collect();
        let has_fps = fps.iter().any(|v| *v > 0.0);
        let id = s.id.clone();
        h_flex()
            .id(SharedString::from(format!("perf-run:{}", s.id)))
            .gap_4()
            .px_2()
            .py_2()
            .rounded(theme.radius)
            .hover(|st| st.bg(theme.list_hover))
            // Capped so the graph and Delete stay by the run on wide windows.
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .max_w(rems(48.))
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_sm().font_semibold().child(if mem.is_empty() {
                                name
                            } else {
                                format!("{name} · {mem}")
                            }))
                            .child(widgets::caption(when, cx).text_xs())
                            .when(s.crashed, |r| r.child(widgets::pill("Crashed", theme.danger, false)))
                            .when(!s.pack.is_empty(), |r| {
                                r.child(widgets::caption(s.pack.clone(), cx).text_xs().truncate())
                            }),
                    )
                    .children(facts.into_iter().map(|f| widgets::caption(f, cx).text_xs())),
            )
            .when(has_fps, |r| {
                r.child(div().w(rems(10.)).flex_none().child(widgets::sparkline(fps, 0.0, theme.primary)))
            })
            .child(
                Button::new(SharedString::from(format!("perf-del:{}", s.id)))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Trash)
                    .tooltip("Delete this run")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.ask_delete(id.clone(), what.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }
}

impl Render for PerfView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let advice = self.advice_section(self.sessions.len(), cx);
        let Some(root) = self.root.clone() else {
            return div().into_any_element();
        };
        let recording = self.store.read(cx).settings().record_sessions;
        let presets = self.store.read(cx).settings().java_presets.clone();
        let groups = hopper::perf::compare(&self.sessions, &presets, MIN_RUN_SECS);
        let best = Self::best(&groups);
        let groups: Vec<Labeled> = groups
            .into_iter()
            .map(|g| Labeled { label: hopper::perf::args_label(&g.args, &presets), group: g })
            .collect();
        let rows: Vec<AnyElement> = groups
            .iter()
            .enumerate()
            .map(|(i, g)| Self::group_row(i, g, &best, cx))
            .collect();
        let runs: Vec<AnyElement> = self
            .sessions
            .clone()
            .iter()
            .map(|s| self.session_row(s, cx))
            .collect();
        let folder = hopper::perf::sessions_dir().map(|b| hopper::perf::instance_dir(&b, &root));
        let n = self.sessions.len();
        v_flex()
            .id("perf")
            .size_full()
            .px_6()
            .pb_6()
            .gap_4()
            .overflow_y_scrollbar()
            .child(
                h_flex()
                    .gap_2()
                    .child(widgets::caption(
                        format!("{n} run{} recorded", if n == 1 { "" } else { "s" }),
                        cx,
                    ))
                    .child(div().flex_1())
                    .when_some(folder, |r, dir| {
                        r.child(
                            Button::new("perf-folder")
                                .outline()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("Open folder")
                                .on_click(move |_, _, _| {
                                    let _ = std::fs::create_dir_all(&dir);
                                    open_path(&dir)
                                }),
                        )
                    }),
            )
            .when(!recording, |r| {
                r.child(
                    card(cx).child(widgets::caption(
                        "Recording is off. Turn on Settings → Java → Record how games run.",
                        cx,
                    )),
                )
            })
            .when_some(advice, |r, a| r.child(a))
            .when(!rows.is_empty(), |r| {
                r.child(
                    card(cx)
                        .gap_1()
                        .child(div().font_semibold().pb_1().child("By Java arguments"))
                        .child(Self::header_row(cx))
                        .children(rows),
                )
            })
            .when(!runs.is_empty(), |r| {
                r.child(
                    card(cx)
                        .gap_1()
                        .child(div().font_semibold().pb_1().child("Runs"))
                        .children(runs),
                )
            })
            .when(n == 0 && self.loading, |r| r.child(widgets::skeleton_rows(3, rems(3.), cx)))
            .when(n == 0 && !self.loading, |r| {
                r.child(widgets::empty_state(
                    IconName::Activity,
                    "No runs yet",
                    Some("Play this instance and each run shows up here: FPS, frame times, GC pauses, load time.".into()),
                    cx,
                ))
            })
            .into_any_element()
    }
}
