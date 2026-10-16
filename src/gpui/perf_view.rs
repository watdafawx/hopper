//! An instance's Performance tab: how each recorded run went, and the runs
//! side by side by Java arguments (one row per preset).

use crate::store::{Store, StoreEvent, open_path};
use crate::widgets;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::perf::{Group, Session};

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

pub struct PerfView {
    store: Entity<Store>,
    root: Option<PathBuf>,
    sessions: Vec<Session>,
    loading: bool,
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
        let when = format!(
            "{} · {}",
            hopper::time::relative_secs(s.started as i64),
            if s.ended == 0 { "still running or cut short".into() } else { duration(s.seconds()) }
        );
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
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
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
                    .on_click(cx.listener(move |this, _, _, cx| this.delete(id.clone(), cx))),
            )
            .into_any_element()
    }
}

impl Render for PerfView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let theme = cx.theme();
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
            .when(n == 0 && !self.loading, |r| {
                r.child(
                    v_flex()
                        .py_10()
                        .gap_2()
                        .items_center()
                        .child(Icon::new(IconName::Activity).large().text_color(theme.muted_foreground))
                        .child(widgets::caption(
                            "Play this instance and each run shows up here: FPS, frame times, GC pauses, load time.",
                            cx,
                        )),
                )
            })
            .into_any_element()
    }
}
