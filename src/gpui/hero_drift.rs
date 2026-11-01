//! The hero's moving backdrop: soft blobs in the pack art's own colors,
//! drifting slowly and nudged by what you do (tab switches, Play, server
//! power buttons, tasks starting and ending).
//!
//! Cheap by design: 12 frames a second at rest, full rate only for the
//! second or two after an action, and no frames at all while Hopper isn't
//! the focused window, after 20 s without input, with reduced motion, or
//! with Settings > Appearance > Still header on (today's blurred art then).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// After this long without mouse or keys, the drift settles and stops.
const IDLE: Duration = Duration::from_secs(20);

static STILL: AtomicBool = AtomicBool::new(false);
static JOBS: AtomicI32 = AtomicI32::new(0);

/// Settings > Appearance > Still header.
pub fn set_still(still: bool) {
    STILL.store(still, Ordering::Relaxed);
}

// ---- Colors ----

/// An art's colors: three vivid ones for the blobs and a dark base.
#[derive(Clone, Copy, Debug)]
struct Look {
    cols: [Hsla; 3],
    base: Hsla,
}

fn looks() -> &'static Mutex<HashMap<PathBuf, Look>> {
    static L: OnceLock<Mutex<HashMap<PathBuf, Look>>> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// Works out `art`'s colors (and the grain) ahead of the first draw. Call
/// it off the UI thread, next to the blur.
pub fn prepare(art: &Path) {
    let _ = grain();
    if looks().lock().is_ok_and(|m| m.contains_key(art)) {
        return;
    }
    let Some(img) = image::ImageReader::open(art)
        .ok()
        .and_then(|r| r.with_guessed_format().ok())
        .and_then(|r| r.decode().ok())
    else {
        return;
    };
    let look = palette(&img.resize_exact(64, 64, image::imageops::FilterType::Triangle).to_rgba8());
    if let Ok(mut m) = looks().lock() {
        m.insert(art.to_path_buf(), look);
    }
}

/// Colors weighted by how vivid they are, not by area: an icon is mostly
/// dark or white, its character is in the colorful bits.
fn palette(img: &image::RgbaImage) -> Look {
    let mut buckets = [[0f32; 4]; 12];
    let mut dark = [0f32; 4];
    for p in img.pixels() {
        if p[3] < 128 {
            continue;
        }
        let (r, g, b) = (p[0] as f32 / 255., p[1] as f32 / 255., p[2] as f32 / 255.);
        let mx = r.max(g).max(b);
        let mn = r.min(g).min(b);
        if mx < 0.3 {
            dark = [dark[0] + r, dark[1] + g, dark[2] + b, dark[3] + 1.];
        }
        if mx - mn < 1e-3 {
            continue;
        }
        let s = (mx - mn) / mx;
        let h = if mx == r {
            ((g - b) / (mx - mn)).rem_euclid(6.)
        } else if mx == g {
            (b - r) / (mx - mn) + 2.
        } else {
            (r - g) / (mx - mn) + 4.
        } / 6.;
        let w = s * s * mx;
        let k = ((h * 12.) as usize).min(11);
        buckets[k] = [buckets[k][0] + r * w, buckets[k][1] + g * w, buckets[k][2] + b * w, buckets[k][3] + w];
    }
    let mut order: Vec<usize> = (0..12).filter(|&k| buckets[k][3] > 0.).collect();
    order.sort_by(|a, b| buckets[*b][3].total_cmp(&buckets[*a][3]));
    let vivid = |k: usize| {
        let c = buckets[k];
        let mut h: Hsla = Rgba { r: c[0] / c[3], g: c[1] / c[3], b: c[2] / c[3], a: 1. }.into();
        // A little calmer than the icon itself.
        h.s = h.s.clamp(0.55, 0.95) * 0.75;
        h.l = h.l.clamp(0.38, 0.58);
        h
    };
    let first = order.first().map_or(hsla(0.07, 0.6, 0.5, 1.), |&k| vivid(k));
    let pick = |i: usize, shift: f32| {
        order.get(i).map_or(Hsla { h: (first.h + shift).rem_euclid(1.), ..first }, |&k| vivid(k))
    };
    let base = if dark[3] > 0. {
        let mut h: Hsla = Rgba { r: dark[0] / dark[3], g: dark[1] / dark[3], b: dark[2] / dark[3], a: 1. }.into();
        h.l = h.l.min(0.06);
        h
    } else {
        Hsla { l: 0.05, s: 0.4, ..first }
    };
    Look { cols: [first, pick(1, 0.06), pick(2, -0.05)], base }
}

/// A grey noise image laid faintly over the blobs, made once.
fn grain() -> Option<PathBuf> {
    static G: OnceLock<Option<PathBuf>> = OnceLock::new();
    G.get_or_init(|| {
        const W: u32 = 1920;
        const H: u32 = 240;
        let out = std::env::temp_dir().join("hopper").join("hero-grain.png");
        if out.is_file() {
            return Some(out);
        }
        let mut img = image::RgbaImage::new(W, H);
        let mut seed: u32 = 0x2545_f491;
        for px in img.pixels_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let v = (seed >> 24) as u8;
            *px = image::Rgba([v, v, v, 255]);
        }
        std::fs::create_dir_all(out.parent()?).ok()?;
        img.save(&out).ok()?;
        Some(out)
    })
    .clone()
}

// ---- Motion ----

/// The drift's state. Actions move the targets; the shown values ease
/// toward them, so a click bends the motion instead of restarting it.
#[derive(Default)]
struct Drift {
    phase: f32,
    energy: f32,
    push: f32,
    swell: f32,
    energy_to: f32,
    push_to: f32,
    swell_to: f32,
    last: Option<Instant>,
}

impl Drift {
    fn kick(&mut self, n: f32, dir: f32) {
        self.energy_to = (self.energy_to + n * 1.5).min(4.);
        self.push_to = (self.push_to + dir * 0.06).clamp(-0.12, 0.12);
        self.swell_to = (self.swell_to + 0.6).min(1.);
    }

    /// Moves `dt` seconds on; `busy` while a task runs.
    fn step(&mut self, dt: f32, busy: bool) {
        self.energy_to *= (-dt / 0.9).exp();
        self.push_to *= (-dt / 0.8).exp();
        self.swell_to *= (-dt / 0.7).exp();
        let k = 1. - (-dt / 0.3).exp();
        self.energy += (self.energy_to - self.energy) * k;
        self.push += (self.push_to - self.push) * k;
        self.swell += (self.swell_to - self.swell) * k;
        self.phase += dt * (1. + self.energy * 7. + if busy { 2. } else { 0. });
    }

    /// Drops any boost (paused: it shouldn't fire when you come back).
    fn calm(&mut self) {
        *self = Drift { phase: self.phase, ..Default::default() };
    }

    fn boosted(&self) -> bool {
        self.energy > 0.05 || self.push.abs() > 0.002 || self.swell > 0.02
    }
}

fn drift() -> &'static Mutex<Drift> {
    static D: OnceLock<Mutex<Drift>> = OnceLock::new();
    D.get_or_init(Default::default)
}

/// An action: `n` how much (tab 1, Play or a server button 1.6), `dir`
/// which way it pushes the blobs (-1 left, 1 right, 0 none).
pub fn kick(n: f32, dir: f32) {
    if let Ok(mut d) = drift().lock() {
        d.kick(n, dir);
    }
}

pub fn job_started() {
    JOBS.fetch_add(1, Ordering::Relaxed);
    kick(1.2, 0.);
}

pub fn job_ended() {
    JOBS.fetch_sub(1, Ordering::Relaxed);
    kick(2.2, 0.);
}

/// Three slow waves at irrational speed ratios: smooth, never repeats.
fn wander(t: f32, k: usize) -> f32 {
    const R: [f32; 5] = [1., std::f32::consts::SQRT_2, 1.618_034, std::f32::consts::E / 2., std::f32::consts::PI / 2.3];
    let kf = k as f32;
    ((t * 0.11 * R[k % 5] + kf).sin()
        + (t * 0.067 * R[(k + 2) % 5] + 2.1 * kf).sin() * 0.6
        + (t * 0.029 * R[(k + 4) % 5] + 0.7 * kf).sin() * 0.4)
        / 2.
}

/// Whether Hopper owns the foreground window and someone touched mouse
/// or keys in the last [`IDLE`]. Asked of Windows directly, so it is right
/// even when nothing in the app redrew on a focus change.
fn awake() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        let mut pid = 0u32;
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid));
        if pid != std::process::id() {
            return false;
        }
        let mut li = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if GetLastInputInfo(&mut li).as_bool() {
            let now = windows::Win32::System::SystemInformation::GetTickCount();
            return Duration::from_millis(now.wrapping_sub(li.dwTime) as u64) < IDLE;
        }
        return true;
    }
    #[allow(unreachable_code)]
    true
}

/// Where the drift is now: (phase, push, swell), advanced to this moment.
fn tick() -> (f32, f32, f32) {
    let Ok(mut d) = drift().lock() else { return (0., 0., 0.) };
    let now = Instant::now();
    let dt = d.last.map_or(0., |l| now.duration_since(l).as_secs_f32().min(0.1));
    d.last = Some(now);
    if awake() {
        d.step(dt, JOBS.load(Ordering::Relaxed) > 0);
    } else {
        d.calm();
    }
    (d.phase, d.push, d.swell)
}

// ---- Drawing ----

/// Blob spots: (x, y, w, h) as fractions of the header, and a color.
const SPOTS: [(f32, f32, f32, f32, usize); 5] = [
    (-0.04, -0.25, 0.3, 1.1, 0),
    (0.2, -0.05, 0.26, 1.0, 1),
    (0.45, -0.35, 0.3, 1.2, 2),
    (0.72, -0.1, 0.28, 1.0, 0),
    (0.9, 0.2, 0.2, 0.9, 1),
];

/// One frame: the blobs (round box shadows, blurred on the GPU), the
/// aurora band behind the title, and the grain.
fn frame(l: &Look, (t, push, swell): (f32, f32, f32)) -> Div {
    let blobs = SPOTS.iter().enumerate().map(|(i, &(x, y, w, h, ci))| {
        let sc = (1. + wander(t, i + 10) * 0.18) * (1. + swell * 0.1);
        div()
            .absolute()
            .left(relative(x + wander(t, i) * 0.09 + push * (1. - 0.15 * i as f32)))
            .top(relative(y + wander(t, i + 5) * 0.28))
            .w(relative(w * sc))
            .h(relative(h * sc))
            .rounded_full()
            .shadow(vec![BoxShadow::new(px(0.), px(0.), l.cols[ci]).blur_radius(px(56.))])
    });
    let shift = wander(t, 20) * 0.08;
    let band = (0..5).map(move |i| {
        div()
            .absolute()
            .left(relative(0.1 + 0.085 * i as f32 + shift + wander(t, 30 + i) * 0.02))
            .top(relative(0.38 + wander(t, 40 + i) * 0.05))
            .w(relative(0.09))
            .h(relative(0.2))
            .rounded_full()
            .shadow(vec![BoxShadow::new(px(0.), px(0.), l.cols[i % 3].opacity(0.45)).blur_radius(px(30.))])
    });
    div()
        .absolute()
        .inset_0()
        .bg(l.base)
        .children(blobs)
        .children(band)
        .children(grain().map(|p| img(p).absolute().inset_0().size_full().object_fit(ObjectFit::Fill).opacity(0.05)))
}

/// The moving backdrop for `art`, or `None` for today's blur (Still header
/// on, or the colors aren't worked out yet). `key` names the page, so each
/// hero keeps its own frame pump.
pub fn backdrop(art: Option<&Path>, key: &str, cx: &App) -> Option<AnyElement> {
    if STILL.load(Ordering::Relaxed) {
        return None;
    }
    let mut l = *looks().lock().ok()?.get(art?)?;
    if !cx.theme().mode.is_dark() {
        // Light themes: pale pastel blobs on a near-white base.
        l.base = Hsla { s: 0.3, l: 0.95, ..l.base };
        l.cols = l.cols.map(|c| Hsla { s: c.s * 0.7, l: 0.72, ..c });
    }
    if cx.reduce_motion() {
        return Some(frame(&l, (0., 0., 0.)).into_any_element());
    }
    let now = tick();
    // Paused: one still frame and no pump. Each pumped frame re-renders
    // this, so the check here is what stops it; focus changes and input
    // redraw the window, which starts it again.
    if !awake() {
        return Some(frame(&l, now).into_any_element());
    }
    let boosted = drift().lock().is_ok_and(|d| d.boosted()) || JOBS.load(Ordering::Relaxed) > 0;
    let mut a = Animation::new(Duration::from_secs(1)).repeat();
    if !boosted {
        a = a.with_max_fps(12.);
    }
    Some(
        div()
            .absolute()
            .inset_0()
            .with_animation(ElementId::Name(format!("hero-drift:{key}").into()), a, move |el, _| {
                el.child(frame(&l, tick()))
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Drift, palette, wander};

    #[test]
    fn wander_stays_in_range_and_never_lines_up() {
        for k in 0..45 {
            for i in 0..2000 {
                assert!(wander(i as f32 * 0.37, k).abs() <= 1.0);
            }
        }
        // No loop within the first ten minutes at rest.
        let start: Vec<f32> = (0..5).map(|k| wander(0., k)).collect();
        for s in 1..600 {
            let now: Vec<f32> = (0..5).map(|k| wander(s as f32, k)).collect();
            assert!(start.iter().zip(&now).any(|(a, b)| (a - b).abs() > 0.01), "repeats at {s}s");
        }
    }

    #[test]
    fn palette_picks_the_vivid_color_over_area() {
        // Mostly near-black and white, with a red corner.
        let img = image::RgbaImage::from_fn(64, 64, |x, y| match (x < 16 && y < 16, x % 2 == 0) {
            (true, _) => image::Rgba([220, 40, 30, 255]),
            (false, true) => image::Rgba([12, 12, 14, 255]),
            (false, false) => image::Rgba([250, 250, 250, 255]),
        });
        let l = palette(&img);
        let red = l.cols[0];
        assert!(red.h < 0.05 || red.h > 0.95, "hue {}", red.h);
        assert!(l.base.l <= 0.06);
    }

    #[test]
    fn a_kick_eases_in_and_fades_out() {
        let mut d = Drift::default();
        d.kick(1., 1.);
        d.step(1. / 60., false);
        // Eases in: not at the target after one frame.
        assert!(d.push > 0. && d.push < 0.01, "push {}", d.push);
        for _ in 0..60 {
            d.step(1. / 60., false);
        }
        let peak = d.push;
        assert!(peak > 0.01);
        for _ in 0..600 {
            d.step(1. / 60., false);
        }
        assert!(!d.boosted() && d.push < peak * 0.05);
        // Two quick clicks build on each other instead of restarting.
        let mut d = Drift::default();
        d.kick(1., 1.);
        d.step(0.1, false);
        let one = d.push_to;
        d.kick(1., 1.);
        assert!(d.push_to > one);
    }
}
