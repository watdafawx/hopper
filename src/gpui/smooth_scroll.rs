//! Smooth scrolling (Settings › Appearance), for every scrollable list and
//! panel in the window at once.
//!
//! A mouse wheel arrives as notches (line deltas) that jump the list by a
//! few lines each. With smooth scrolling on, the window catches each notch
//! before any list sees it and plays its distance back as small pixel
//! steps over the next frames, easing out. Notches that come in quickly add
//! up, so a fast spin glides further instead of stuttering. Touchpads and
//! precise wheels already send pixels and smooth themselves, so they pass
//! straight through, as does Ctrl + wheel (zoom). Off, or when Windows asks
//! for less motion, every notch passes through as before.
//!
//! Frames are asked for only while a glide is under way, so an idle window
//! draws nothing.

use gpui_kit::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static ON: AtomicBool = AtomicBool::new(true);

/// Settings › Appearance › Smooth scrolling.
pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

/// How long a glide takes, from the last notch to standing still, whatever
/// the frame rate.
const GLIDE: Duration = Duration::from_millis(150);

struct Glide {
    /// The whole distance of this glide, in pixels (positive is up / left,
    /// as GPUI counts wheel deltas)...
    total: Point<f32>,
    /// ...and how much of it is scrolled so far.
    done: Point<f32>,
    start: Instant,
    modifiers: Modifiers,
    /// A frame callback is queued.
    pumping: bool,
}

thread_local! {
    static STATE: RefCell<Glide> = RefCell::new(Glide {
        total: Point::default(),
        done: Point::default(),
        start: Instant::now(),
        modifiers: Modifiers::default(),
        pumping: false,
    });
}

/// A notch's distance on top of what's still left to scroll. A notch the
/// other way drops what's left of the old direction, so reversing feels
/// immediate.
fn add(left: f32, d: f32) -> f32 {
    if d != 0. && left != 0. && left.signum() != d.signum() { d } else { left + d }
}

/// How far along a glide is after `t` of its time (0 to 1): quick at
/// first, settling gently (ease-out cubic).
fn ease(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

/// An invisible element for the root view: while painted, it catches wheel
/// notches for the whole window.
pub fn catcher() -> impl IntoElement {
    canvas(
        |_, _, _| {},
        |_, _, window, _| {
            // A notch is this many pixels per line, as the lists count it.
            let line = f32::from(window.line_height());
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || !ON.load(Ordering::Relaxed)
                    || cx.reduce_motion()
                    || e.modifiers.control
                    || e.modifiers.platform
                {
                    return;
                }
                let ScrollDelta::Lines(lines) = e.delta else { return };
                cx.stop_propagation();
                let start = STATE.with(|g| {
                    let mut g = g.borrow_mut();
                    // Each notch starts a new glide over what's left plus it.
                    let left = if g.pumping { point(g.total.x - g.done.x, g.total.y - g.done.y) } else { Point::default() };
                    g.total = point(add(left.x, lines.x * line), add(left.y, lines.y * line));
                    g.done = Point::default();
                    g.start = Instant::now();
                    g.modifiers = e.modifiers;
                    !std::mem::replace(&mut g.pumping, true)
                });
                if start {
                    window.on_next_frame(frame);
                }
            });
        },
    )
    .absolute()
    .size_0()
}

/// One frame of a glide: scrolls the part due by now where the mouse is,
/// and asks for the next frame until the glide's time is up.
fn frame(window: &mut Window, cx: &mut App) {
    let (delta, modifiers, more) = STATE.with(|g| {
        let mut g = g.borrow_mut();
        let t = g.start.elapsed().as_secs_f32() / GLIDE.as_secs_f32();
        let p = ease(t);
        let at = point(g.total.x * p, g.total.y * p);
        let d = point(at.x - g.done.x, at.y - g.done.y);
        g.done = at;
        let more = t < 1.;
        g.pumping = more;
        (d, g.modifiers, more)
    });
    if delta.x != 0. || delta.y != 0. {
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: window.mouse_position(),
                delta: ScrollDelta::Pixels(point(px(delta.x), px(delta.y))),
                modifiers,
                touch_phase: TouchPhase::Moved,
            }),
            cx,
        );
    }
    if more {
        window.on_next_frame(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::{add, ease};

    #[test]
    fn notches_add_up_and_reversing_drops_the_rest() {
        assert_eq!(add(30., 60.), 90.);
        assert_eq!(add(-30., -60.), -90.);
        assert_eq!(add(30., -60.), -60.);
        assert_eq!(add(0., -60.), -60.);
    }

    #[test]
    fn a_glide_eases_out_and_lands_exactly() {
        assert_eq!(ease(0.), 0.);
        assert_eq!(ease(1.), 1.);
        assert_eq!(ease(2.), 1.);
        // Quick at first: a 60 fps frame (of 150 ms) already moves over a
        // quarter of the way, so there's no lag before it starts.
        assert!(ease(16. / 150.) > 0.25);
        // Always moving forward, slower towards the end.
        let steps: Vec<f32> = (0..=10).map(|i| ease(i as f32 / 10.)).collect();
        for w in steps.windows(3) {
            assert!(w[1] > w[0] && w[1] - w[0] > w[2] - w[1]);
        }
    }
}
