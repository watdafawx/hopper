//! The tasks button in the title bar, beside the account, on every page: a
//! badge while jobs run, a popover listing every job, and a file that flies
//! into it when a job starts.

use crate::store::Store;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, StyledExt as _, h_flex, v_flex};
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
                    .child(Icon::new(IconName::Download).small())
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
        let view = window.viewport_size();
        let gap = px(8.);
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
            let (store_r, me) = (store.clone(), me.clone());
            v_flex()
                .id(ElementId::Name(format!("task-row-{id}").into()))
                .gap_1()
                .py_1p5()
                .when(review, |r| {
                    r.cursor_pointer()
                        .rounded(theme.radius)
                        .hover(|s| s.bg(theme.list_hover))
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
                        .child(
                            Button::new(ElementId::Name(format!("task-x-{id}").into()))
                                .icon(IconName::X)
                                .xsmall()
                                .ghost()
                                .tooltip(if j.done { "Dismiss" } else { "Cancel (stops at the next safe point)" })
                                .on_click({
                                    let done = j.done;
                                    move |_, _, cx| {
                                        store.update(cx, |s, cx| {
                                            if done {
                                                s.jobs().dismiss(id);
                                            } else {
                                                s.jobs().cancel(id);
                                            }
                                            cx.notify();
                                        })
                                    }
                                }),
                        ),
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
            .absolute()
            .w(rems(30.))
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
            // Beside a button low in the window, below one at the top.
            .map(|p| {
                if b.center().y > view.height / 2. {
                    p.left(b.right() + gap).bottom(view.height - b.bottom())
                } else {
                    p.top(b.bottom() + gap).right(view.width - b.right())
                }
            })
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
                    .when(lines > 7, |d| d.h(rems(26.)))
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
        widgets_pop(panel)
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
        .child(div().w_full().child(text.to_string()))
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let jobs = self.store.read(cx).jobs().snapshot();
        let running: Vec<_> = jobs.iter().filter(|j| !j.done).collect();
        let attention = jobs
            .iter()
            .filter(|j| j.done && (j.error.is_some() || !j.notes.is_empty()))
            .count();
        let failed = jobs.iter().any(|j| j.done && j.error.is_some());
        // Overall progress of what runs, for the bar under the icon.
        let progress = (!running.is_empty()).then(|| {
            running.iter().map(|j| j.progress.unwrap_or(0.)).sum::<f32>() / running.len() as f32
        });
        let theme = cx.theme();
        let tip: SharedString = match (running.len(), attention) {
            (0, 0) => "Tasks".into(),
            (0, n) => format!("{n} task{} need{} a look", if n == 1 { "" } else { "s" }, if n == 1 { "s" } else { "" }).into(),
            // Short: a long tooltip spills over the page below the bar.
            (n, _) => format!("{n} task{} running", if n == 1 { "" } else { "s" }).into(),
        };
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
            .cursor_pointer()
            .text_color(if running.is_empty() { theme.muted_foreground } else { theme.primary })
            .when(open, |d| d.bg(theme.list_active))
            .hover(|s| s.bg(theme.list_hover))
            .when(!open, |d| {
                d.tooltip(move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx))
            })
            .on_click(cx.listener(|this, _, _, cx| this.toggle(cx)))
            .child(
                canvas(move |b, _, _| bounds.set(Some(b)), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(Icon::new(IconName::Download).small())
            .when_some(progress, |d, p| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(2.))
                        .left(rems(0.375))
                        .right(rems(0.375))
                        .h(px(2.))
                        .rounded_full()
                        .bg(theme.muted)
                        .child(div().h_full().rounded_full().bg(theme.primary).w(relative(p.clamp(0.04, 1.)))),
                )
            })
            .when(!running.is_empty() || attention > 0, |d| {
                let n = if running.is_empty() { attention } else { running.len() };
                let bg = if !running.is_empty() {
                    theme.primary
                } else if failed {
                    theme.danger
                } else {
                    theme.warning
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
                        .text_size(px(10.))
                        .font_semibold()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(n.to_string()),
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
