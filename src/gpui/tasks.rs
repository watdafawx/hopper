//! The tasks button in the title bar, beside the account, on every page: a
//! badge while jobs run, a popover listing every job, and a file that flies
//! into it when a job starts.

use crate::widgets::Clickable as _;
use crate::widgets;
use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

/// How long a file takes to fly into the button.
const FLIGHT: Duration = Duration::from_millis(700);

pub struct TasksButton {
    store: Entity<Store>,
    /// The popover is open.
    pub open: bool,
    /// Where the button was last painted, in window coordinates.
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Bumped when a flying file lands, to pulse the button.
    pulse: u64,
    /// Jobs already seen, so only new ones fly.
    seen: HashSet<u64>,
    started: bool,
    /// Files in the air: (job id, where they took off).
    flights: Vec<(u64, Point<Pixels>)>,
    _subs: [Subscription; 2],
}

impl TasksButton {
    pub fn new(store: Entity<Store>, cx: &mut Context<Self>) -> Self {
        let ticks = store.read(cx).job_ticks().clone();
        let subs = [
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&ticks, |_, _, cx| cx.notify()),
        ];
        Self {
            store,
            open: false,
            bounds: Rc::new(Cell::new(None)),
            pulse: 0,
            seen: HashSet::new(),
            started: false,
            flights: Vec::new(),
            _subs: subs,
        }
    }

    fn toggle(&mut self, cx: &mut Context<Self>) {
        self.open = !self.open;
        cx.notify();
    }

    /// The popover and any flying files, drawn over the whole window.
    /// `root` must be the window-sized root, positioned at the origin.
    pub fn overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let jobs = self.store.read(cx).jobs().snapshot();
        // A new job takes off from the pointer, which is still on whatever
        // started it. Jobs there at launch don't fly.
        for j in &jobs {
            if self.seen.insert(j.id) && self.started && !j.done {
                self.flights.push((j.id, window.mouse_position()));
                let id = j.id;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(FLIGHT).await;
                    let _ = this.update(cx, |t, cx| {
                        t.flights.retain(|f| f.0 != id);
                        t.pulse += 1;
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        self.started = true;
        let Some(b) = self.bounds.get() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let theme = cx.theme();
        let target = b.center() - point(rems(0.875).to_pixels(window.rem_size()), rems(0.875).to_pixels(window.rem_size()));
        for &(id, from) in &self.flights {
            let from = from - point(px(14.), px(14.));
            out.push(
                div()
                    .absolute()
                    .left(from.x)
                    .top(from.y)
                    .size(rems(1.75))
                    .rounded(theme.radius)
                    .bg(theme.primary)
                    .text_color(theme.primary_foreground)
                    .shadow_lg()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::ListChecks).small())
                    .with_animation(
                        ElementId::Name(format!("fly:{id}").into()),
                        Animation::new(FLIGHT).with_easing(ease_in_out),
                        move |el, t| {
                            // An arc: up and over, shrinking as it lands.
                            let x = from.x + (target.x - from.x) * t;
                            let lift = px(120.) * (std::f32::consts::PI * t).sin();
                            let y = from.y + (target.y - from.y) * t - lift;
                            el.left(x).top(y).opacity(1.0 - 0.6 * t * t)
                        },
                    )
                    .into_any_element(),
            );
        }
        if self.open {
            out.push(self.popover(b, window, cx));
        }
        out
    }

    fn popover(&self, b: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let store = self.store.clone();
        let jobs = store.read(cx).jobs().snapshot();
        let me = cx.entity();
        // Under its button, right edges lined up, inside the window.
        let place = widgets::anchored_popover(b, window);
        let rem = window.rem_size();
        // The list's room: the window under the button, less the heading.
        let list_h = (rem * 26.).min(place.max_h - rem * 3.5).max(rem * 4.);
        let lines = jobs.len() + jobs.iter().map(|j| j.notes.len()).sum::<usize>();
        let rows = jobs.iter().rev().map(|j| {
            let id = j.id;
            let store = store.clone();
            let state: AnyElement = if !j.done {
                gpui_kit::component::spinner::Spinner::new().small().into_any_element()
            } else if j.error.is_some() {
                Icon::new(IconName::CircleX).small().text_color(theme.danger).into_any_element()
            } else {
                Icon::new(IconName::CircleCheck).small().text_color(theme.success).into_any_element()
            };
            // A finished comparison opens its review on click.
            let review = j.error.is_none() && crate::pack_update::has_review(id, j.done);
            let (store_r, me_s) = (store.clone(), me.clone());
            let me = me_s.clone();
            v_flex()
                .id(ElementId::Name(format!("task-row-{id}").into()))
                .gap_1()
                .py_1p5()
                .when(review, |r| {
                    r.clickable(cx)
                        .rounded(theme.radius)
                        .hover(|s| widgets::row_hover(s, cx))
                        .on_click(move |_, window, cx| {
                            me.update(cx, |t, cx| {
                                t.open = false;
                                cx.notify();
                            });
                            crate::pack_update::reopen(store_r.clone(), id, window, cx)
                        })
                })
                .child(
                    h_flex()
                        .gap_3()
                        .child(div().flex_none().child(state))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_sm().font_medium().truncate().child(j.title.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .truncate()
                                        .text_color(if j.error.is_some() { theme.danger } else { theme.muted_foreground })
                                        .child(j.error.clone().unwrap_or(j.detail.clone())),
                                ),
                        )
                        .child({
                            // Labelled actions: Cancel while it runs; Dismiss
                            // (and Copy details on a failure) after.
                            let done = j.done;
                            let detail = j.error.clone().map(|e| format!("{}: {e}", j.title));
                            let store_c = store.clone();
                            // A snapshot this task took to undo it.
                            let snap = done.then(|| hopper::snapshots::of_job(id)).flatten();
                            let store_s = store.clone();
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .when_some(snap, |r, (root, snap)| {
                                    r.child(
                                        Button::new(ElementId::Name(format!("task-snap-{id}").into()))
                                            .icon(IconName::Archive)
                                            .xsmall()
                                            .ghost()
                                            .label("View snapshot")
                                            .tooltip("The snapshot taken first: compare it with now, or restore it")
                                            .on_click(move |_, window, cx| {
                                                cx.stop_propagation();
                                                me_s.update(cx, |t, cx| {
                                                    t.open = false;
                                                    cx.notify();
                                                });
                                                crate::backups_view::open_at(
                                                    store_s.clone(),
                                                    root.clone(),
                                                    Some(snap.clone()),
                                                    window,
                                                    cx,
                                                )
                                            }),
                                    )
                                })
                                .when_some(detail, |r, d| {
                                    r.child(
                                        Button::new(ElementId::Name(format!("task-copy-{id}").into()))
                                            .icon(IconName::Copy)
                                            .xsmall()
                                            .ghost()
                                            .label("Copy details")
                                            .on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                cx.write_to_clipboard(ClipboardItem::new_string(d.clone()))
                                            }),
                                    )
                                })
                                .child(
                                    Button::new(ElementId::Name(format!("task-x-{id}").into()))
                                        .xsmall()
                                        .ghost()
                                        .label(if done { "Dismiss" } else { "Cancel" })
                                        .tooltip(if done { "Remove from the list" } else { "Stops at the next safe point" })
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            store_c.update(cx, |s, cx| {
                                                if done {
                                                    s.jobs().dismiss(id);
                                                } else {
                                                    s.jobs().cancel(id);
                                                }
                                                cx.notify();
                                            })
                                        }),
                                )
                        }),
                )
                .when(!j.done, |c| {
                    c.child(
                        div().pl(rems(2.0)).child(
                            Progress::new(ElementId::Name(format!("task-bar-{id}").into()))
                                .loading(j.progress.is_none())
                                .value(j.progress.unwrap_or(0.) * 100.),
                        ),
                    )
                })
                .children(j.notes.iter().map(|n| note(n, &theme)))
        });
        let store2 = self.store.clone();
        let bounds = self.bounds.clone();
        let panel = v_flex()
            .id("tasks-popover")
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .shadow_xl()
            .occlude()
            .on_mouse_down_out(cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                // The button toggles it itself.
                if bounds.get().is_some_and(|b| b.contains(&e.position)) {
                    return;
                }
                this.open = false;
                cx.notify();
            }))
            .child(
                h_flex()
                    .px_4()
                    .pt_3()
                    .pb_2()
                    .gap_2()
                    .child(div().text_sm().font_semibold().child("Tasks"))
                    .child(div().flex_1())
                    .when(jobs.iter().any(|j| j.done), |r| {
                        r.child(
                            Button::new("clear-finished")
                                .xsmall()
                                .ghost()
                                .label("Clear finished")
                                .on_click(move |_, _, cx| {
                                    store2.update(cx, |s, cx| {
                                        s.jobs().clear_finished();
                                        cx.notify();
                                    })
                                }),
                        )
                    }),
            )
            .child(
                div()
                    .id("tasks-list")
                    // A set height once long: a max height alone doesn't scroll.
                    .map(|d| if lines > 7 { d.h(list_h) } else { d.max_h(list_h) })
                    .px_4()
                    .pb_3()
                    .overflow_y_scrollbar()
                    .child(v_flex().children(rows).when(jobs.is_empty(), |c| {
                        c.child(
                            div()
                                .py_6()
                                .text_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Nothing running. Downloads and installs show up here."),
                        )
                    })),
            );
        // 22-30rem by content.
        widgets_pop(place.apply(panel, 22., 30., window))
    }
}

/// A note under a job, wrapping, with any link in it clickable.
fn note(text: &str, theme: &gpui_kit::component::Theme) -> AnyElement {
    let url = text
        .find("https://")
        .map(|i| text[i..].split_whitespace().next().unwrap_or("").to_string());
    v_flex()
        .pl(rems(2.0))
        .text_xs()
        .text_color(theme.warning)
        .child(
            h_flex()
                .gap_1p5()
                .items_start()
                .child(Icon::new(IconName::TriangleAlert).xsmall().flex_none())
                .child(div().flex_1().min_w_0().child(text.to_string())),
        )
        .when_some(url, |r, u| {
            let u2 = u.clone();
            r.child(
                Button::new(SharedString::from(format!("task-note:{u}")))
                    .link()
                    .xsmall()
                    .label("Open link")
                    .on_click(move |_, _, cx| cx.open_url(&u2)),
            )
        })
        .into_any_element()
}

/// A quick fade and lift as the popover opens.
fn widgets_pop(panel: Stateful<Div>) -> AnyElement {
    panel
        .with_animation(
            "tasks-pop",
            Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
            |el, t| el.opacity(t),
        )
        .into_any_element()
}

impl Render for TasksButton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let jobs = self.store.read(cx).jobs().snapshot();
        let running: Vec<_> = jobs.iter().filter(|j| !j.done).collect();
        let attention = jobs
            .iter()
            .filter(|j| j.done && (j.error.is_some() || !j.notes.is_empty()))
            .count();
        let failed = jobs.iter().any(|j| j.done && j.error.is_some());
        let dialog = window.has_active_dialog(cx);
        let theme = cx.theme();
        let bounds = self.bounds.clone();
        let (primary, open) = (theme.primary, self.open);
        div()
            .id("tasks-button")
            .relative()
            // Fits the title bar with its badge, which stays inside.
            .size(rems(1.75))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(theme.radius)
            .clickable(cx)
            .text_color(if running.is_empty() { theme.muted_foreground } else { theme.primary })
            .when(open, |d| d.bg(theme.list_active))
            .hover(|s| s.bg(theme.list_hover))
            // None under a dialog, where it could stay stuck.
            .when(!open && !dialog, |d| d.tooltip(|w, cx| gpui_kit::component::tooltip::Tooltip::new("Tasks").build(w, cx)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
            .child(
                canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(Icon::new(IconName::ListChecks).small())
            // A spinning ring while anything runs.
            .when(!running.is_empty(), |d| {
                d.child(
                    div().absolute().inset_0().child(
                        Icon::new(IconName::LoaderCircle)
                            .size_full()
                            .text_color(primary)
                            .with_animation(
                                "tasks-ring",
                                Animation::new(Duration::from_millis(900)).repeat(),
                                |i, t| i.transform(Transformation::rotate(percentage(t))),
                            ),
                    ),
                )
            })
            // A failure shows "!", else the count running (or needing a look).
            .when(failed || !running.is_empty() || attention > 0, |d| {
                let (n, bg) = if failed {
                    ("!".to_string(), theme.danger)
                } else if !running.is_empty() {
                    (running.len().to_string(), theme.primary)
                } else {
                    (attention.to_string(), theme.warning)
                };
                d.child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .right(px(-5.))
                        .min_w(px(13.))
                        .h(px(13.))
                        .px(px(3.))
                        .rounded_full()
                        .bg(bg)
                        .text_color(theme.primary_foreground)
                        .text_xs()
                        .font_semibold()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(n),
                )
            })
            // A ring that swells and fades when a file lands.
            .when(self.pulse > 0, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(theme.radius)
                        .border_2()
                        .border_color(primary)
                        .with_animation(
                            ElementId::Name(format!("tasks-pulse-{}", self.pulse).into()),
                            Animation::new(Duration::from_millis(450)).with_easing(ease_out_quint()),
                            move |el, t| el.opacity(1.0 - t).border_color(primary.opacity(1.0 - t)),
                        ),
                )
            })
    }
}
