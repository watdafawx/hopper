//! A small software 3D renderer for the Minecraft player model: the skin
//! (both layers, classic or slim arms), a cape or an elytra, posed and
//! animated, seen through an orbiting camera. It draws textured boxes with a
//! depth buffer and supersampling into an RGBA (or BGRA) buffer, so the UI
//! only has to show one image per frame.

use image::RgbaImage;
use std::f32::consts::{FRAC_PI_2, PI, TAU};

// ---- what to draw -------------------------------------------------------------

/// How the player stands or moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Pose {
    /// Standing, breathing and glancing around.
    Idle,
    #[default]
    Walk,
    Run,
    Sneak,
    CrouchWalk,
    Jump,
    Swim,
    /// Flying with spread elytra wings.
    Glide,
    Wave,
    Dance,
    LookAround,
    Attack,
    Bow,
    Sit,
    Sleep,
    /// Arms straight out, still: for checking a skin.
    TPose,
    /// Every other pose in turn.
    Cycle,
}

impl Pose {
    pub const ALL: [Pose; 17] = [
        Pose::Idle,
        Pose::Walk,
        Pose::Run,
        Pose::Sneak,
        Pose::CrouchWalk,
        Pose::Jump,
        Pose::Swim,
        Pose::Glide,
        Pose::Wave,
        Pose::Dance,
        Pose::LookAround,
        Pose::Attack,
        Pose::Bow,
        Pose::Sit,
        Pose::Sleep,
        Pose::TPose,
        Pose::Cycle,
    ];

    /// The poses "Cycle all" goes through, and how long each lasts.
    const CYCLE: [Pose; 15] = [
        Pose::Walk,
        Pose::Idle,
        Pose::Run,
        Pose::Sneak,
        Pose::CrouchWalk,
        Pose::Jump,
        Pose::Swim,
        Pose::Glide,
        Pose::Wave,
        Pose::Dance,
        Pose::LookAround,
        Pose::Attack,
        Pose::Bow,
        Pose::Sit,
        Pose::Sleep,
    ];
    const CYCLE_SECS: f32 = 4.0;

    pub fn label(self) -> &'static str {
        match self {
            Pose::Idle => "Idle",
            Pose::Walk => "Walk",
            Pose::Run => "Run",
            Pose::Sneak => "Sneak",
            Pose::CrouchWalk => "Crouch walk",
            Pose::Jump => "Jump",
            Pose::Swim => "Swim",
            Pose::Glide => "Elytra glide",
            Pose::Wave => "Wave",
            Pose::Dance => "Dance",
            Pose::LookAround => "Look around",
            Pose::Attack => "Attack",
            Pose::Bow => "Draw a bow",
            Pose::Sit => "Sit",
            Pose::Sleep => "Sleep",
            Pose::TPose => "T-pose",
            Pose::Cycle => "Cycle all",
        }
    }

    /// Whether it moves at all (a T-pose stands still).
    pub fn animated(self) -> bool {
        self != Pose::TPose
    }

    /// The pose shown at `t` seconds: itself, or the current one of a cycle.
    pub fn at(self, t: f32) -> Pose {
        if self != Pose::Cycle {
            return self;
        }
        let n = Self::CYCLE.len();
        Self::CYCLE[(t / Self::CYCLE_SECS) as usize % n]
    }
}

/// The scene behind the player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Backdrop {
    /// Transparent: the page shows through.
    None,
    #[default]
    Day,
    Night,
}

impl Backdrop {
    pub const ALL: [Backdrop; 3] = [Backdrop::None, Backdrop::Day, Backdrop::Night];

    pub fn label(self) -> &'static str {
        match self {
            Backdrop::None => "None",
            Backdrop::Day => "Day Sky",
            Backdrop::Night => "Night Sky",
        }
    }
}

/// What the camera sees.
pub struct Scene<'a> {
    pub skin: &'a RgbaImage,
    pub slim: bool,
    /// A cape texture, worn when there is no elytra.
    pub cape: Option<&'a RgbaImage>,
    /// An elytra texture (a cape texture works too): drawn instead of the cape.
    pub elytra: Option<&'a RgbaImage>,
    pub pose: Pose,
    /// Seconds of animation.
    pub time: f32,
}

/// An orbit around the player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Turn around the vertical axis, in radians; positive shows the
    /// player's right side.
    pub yaw: f32,
    /// Tilt, in radians; positive looks down from above.
    pub pitch: f32,
    /// 1 fits the player in the view.
    pub zoom: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: 0.5,
            pitch: 0.12,
            zoom: 1.0,
        }
    }
}

impl Camera {
    pub const MIN_ZOOM: f32 = 0.5;
    pub const MAX_ZOOM: f32 = 3.0;
    pub const MAX_PITCH: f32 = 1.2;

    /// Turns by a mouse drag of `dx`, `dy` logical pixels.
    pub fn drag(&mut self, dx: f32, dy: f32) {
        self.yaw = (self.yaw - dx * 0.01).rem_euclid(TAU);
        self.pitch = (self.pitch + dy * 0.01).clamp(-Self::MAX_PITCH, Self::MAX_PITCH);
    }

    /// Zooms by scroll `steps` (positive zooms in).
    pub fn scroll(&mut self, steps: f32) {
        self.zoom = (self.zoom * 1.1f32.powf(steps)).clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
    }
}

// ---- small linear algebra -----------------------------------------------------

type V3 = [f32; 3];

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: V3, s: f32) -> V3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 0.0 { scale(a, 1.0 / l) } else { a }
}

/// A 3×3 rotation, rows.
#[derive(Clone, Copy)]
struct M3([[f32; 3]; 3]);

impl M3 {

    fn rx(a: f32) -> M3 {
        let (s, c) = a.sin_cos();
        M3([[1., 0., 0.], [0., c, -s], [0., s, c]])
    }

    fn ry(a: f32) -> M3 {
        let (s, c) = a.sin_cos();
        M3([[c, 0., s], [0., 1., 0.], [-s, 0., c]])
    }

    fn rz(a: f32) -> M3 {
        let (s, c) = a.sin_cos();
        M3([[c, -s, 0.], [s, c, 0.], [0., 0., 1.]])
    }

    /// A rotation given as Minecraft's model angles (x, y, z), which apply
    /// x first. Minecraft's model space has y down and the front at -z;
    /// here y is up and the front is +z (a half turn about x), which keeps
    /// x angles and flips y and z ones.
    fn mc([x, y, z]: V3) -> M3 {
        M3::rz(-z).mul(M3::ry(-y)).mul(M3::rx(x))
    }

    fn mul(self, o: M3) -> M3 {
        let mut r = [[0.0; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v = (0..3).map(|k| self.0[i][k] * o.0[k][j]).sum();
            }
        }
        M3(r)
    }

    fn apply(self, v: V3) -> V3 {
        [dot(self.0[0], v), dot(self.0[1], v), dot(self.0[2], v)]
    }
}

/// Rotation then translation.
#[derive(Clone, Copy)]
struct Xf {
    m: M3,
    t: V3,
}

impl Xf {
    fn at(t: V3, m: M3) -> Xf {
        Xf { m, t }
    }

    fn apply(self, v: V3) -> V3 {
        add(self.m.apply(v), self.t)
    }
}

// ---- the model ------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum TexId {
    Skin,
    Cape,
    Elytra,
}

/// One textured face, in model space (y up, front +z, player's right -x,
/// feet at y = 0, 32 units = the player's height).
struct Quad {
    v: [V3; 4],
    /// Texture pixel coordinates at each corner.
    uv: [[f32; 2]; 4],
    /// The face's texture rectangle (min u, min v, max u, max v), to keep
    /// sampling inside it.
    clamp: [f32; 4],
    tex: TexId,
    /// Base skin layer: no transparency, back faces hidden.
    solid: bool,
}

/// A Minecraft-style box: `uv` is the texture corner of its unfolded faces,
/// `size` its texture size (w, h, d), `from` its low corner in part space.
struct Cuboid {
    tex: TexId,
    /// Texture pixels per unit (2 for a 128 wide skin).
    ts: f32,
    uv: (f32, f32),
    size: V3,
    from: V3,
    inflate: f32,
    mirror: bool,
    solid: bool,
}

impl Cuboid {
    fn emit(&self, xf: Xf, out: &mut Vec<Quad>) {
        let [w, h, d] = self.size;
        let (u, v) = self.uv;
        let g = self.inflate;
        let [x0, y0, z0] = sub(self.from, [g, g, g]);
        let [x1, y1, z1] = add(add(self.from, self.size), [g, g, g]);
        let (dx, dy, dz) = (x1 - x0, y1 - y0, z1 - z0);
        // Texture rectangles of the unfolded box.
        let right = (u, v + d, d, h);
        let front = (u + d, v + d, w, h);
        let left = (u + d + w, v + d, d, h);
        let back = (u + d + w + d, v + d, w, h);
        let top = (u + d, v, w, d);
        let bottom = (u + d + w, v, w, d);
        let (right, left) = if self.mirror { (left, right) } else { (right, left) };
        // (texture rect, origin, u axis, v axis); the outward normal is v × u.
        let faces = [
            (front, [x0, y1, z1], [dx, 0., 0.], [0., -dy, 0.]),
            (back, [x1, y1, z0], [-dx, 0., 0.], [0., -dy, 0.]),
            (right, [x0, y1, z0], [0., 0., dz], [0., -dy, 0.]),
            (left, [x1, y1, z1], [0., 0., -dz], [0., -dy, 0.]),
            (top, [x0, y1, z0], [dx, 0., 0.], [0., 0., dz]),
            (bottom, [x0, y0, z1], [dx, 0., 0.], [0., 0., -dz]),
        ];
        for ((ru, rv, rw, rh), o, ua, va) in faces {
            let s = self.ts;
            let (mut u0, mut u1) = (ru * s, (ru + rw) * s);
            let (v0, v1) = (rv * s, (rv + rh) * s);
            if self.mirror {
                std::mem::swap(&mut u0, &mut u1);
            }
            let corners = [o, add(o, ua), add(add(o, ua), va), add(o, va)];
            out.push(Quad {
                v: corners.map(|c| xf.apply(c)),
                uv: [[u0, v0], [u1, v0], [u1, v1], [u0, v1]],
                clamp: [u0.min(u1), v0, u0.max(u1), v1],
                tex: self.tex,
                solid: self.solid,
            });
        }
    }
}

/// One moment of a pose: joint angles (Minecraft's x, y, z model angles)
/// and offsets (model units, y up, front +z).
#[derive(Clone, Copy)]
struct Rig {
    head: V3,
    body: V3,
    r_arm: V3,
    l_arm: V3,
    r_leg: V3,
    l_leg: V3,
    head_off: V3,
    body_off: V3,
    arm_off: V3,
    leg_off: V3,
    /// The cape's swing back from the body.
    cape: f32,
    /// Elytra wings: tilt back (x) and spread (z, negative opens the left wing).
    wings: [f32; 2],
    /// The whole player turned (x, y, z in radians, about `root_pivot`)...
    root: V3,
    root_pivot: V3,
    /// ...and moved up.
    lift: f32,
    /// Where the camera looks.
    center: V3,
    /// Zoom that fits the pose in view (lying poses are longer than tall).
    fit: f32,
}

impl Default for Rig {
    fn default() -> Self {
        Rig {
            head: [0.; 3],
            body: [0.; 3],
            r_arm: [0.; 3],
            l_arm: [0.; 3],
            r_leg: [0.; 3],
            l_leg: [0.; 3],
            head_off: [0.; 3],
            body_off: [0.; 3],
            arm_off: [0.; 3],
            leg_off: [0.; 3],
            cape: 0.1,
            wings: [0.261_799_4, -0.261_799_4],
            root: [0.; 3],
            root_pivot: [0., 12., 0.],
            lift: 0.,
            center: [0., 16., 0.],
            fit: 1.,
        }
    }
}

impl Rig {
    /// Between `self` (k = 0) and `o` (k = 1).
    fn mix(&self, o: &Rig, k: f32) -> Rig {
        let v = |a: V3, b: V3| add(a, scale(sub(b, a), k));
        let f = |a: f32, b: f32| a + (b - a) * k;
        Rig {
            head: v(self.head, o.head),
            body: v(self.body, o.body),
            r_arm: v(self.r_arm, o.r_arm),
            l_arm: v(self.l_arm, o.l_arm),
            r_leg: v(self.r_leg, o.r_leg),
            l_leg: v(self.l_leg, o.l_leg),
            head_off: v(self.head_off, o.head_off),
            body_off: v(self.body_off, o.body_off),
            arm_off: v(self.arm_off, o.arm_off),
            leg_off: v(self.leg_off, o.leg_off),
            cape: f(self.cape, o.cape),
            wings: [f(self.wings[0], o.wings[0]), f(self.wings[1], o.wings[1])],
            root: v(self.root, o.root),
            root_pivot: v(self.root_pivot, o.root_pivot),
            lift: f(self.lift, o.lift),
            center: v(self.center, o.center),
            fit: f(self.fit, o.fit),
        }
    }
}

/// A pose at `t` seconds; a cycle blends into the next pose.
fn rig(pose: Pose, t: f32) -> Rig {
    if pose != Pose::Cycle {
        return pose_rig(pose, t);
    }
    let now = pose_rig(pose.at(t), t);
    // The last half second of each pose eases into the next.
    let into = (t % Pose::CYCLE_SECS) - (Pose::CYCLE_SECS - 0.5);
    if into <= 0.0 {
        return now;
    }
    let next = pose_rig(pose.at(t + 0.5), t);
    let k = into / 0.5;
    now.mix(&next, k * k * (3.0 - 2.0 * k))
}

/// The game's sneaking stance: body bent forward, head and arms lowered,
/// legs back.
fn sneak(r: &mut Rig) {
    r.body[0] = 0.5;
    r.r_arm[0] += 0.4;
    r.l_arm[0] += 0.4;
    r.head_off = [0., -4.2, 0.];
    r.body_off = [0., -3.2, 0.];
    r.arm_off = [0., -3.2, 0.];
    r.leg_off = [0., -0.2, -4.];
    r.cape = 0.55;
    r.center = [0., 14., 0.];
}

fn pose_rig(pose: Pose, t: f32) -> Rig {
    let tick = t * 20.0;
    let mut r = Rig {
        cape: 0.1 + (t * 1.3).sin() * 0.03,
        ..Rig::default()
    };
    // The game's idle arm sway, which plays under most poses.
    let sway_z = (tick * 0.09).cos() * 0.05 + 0.05;
    let sway_x = (tick * 0.067).sin() * 0.05;
    r.r_arm = [sway_x, 0., sway_z];
    r.l_arm = [-sway_x, 0., -sway_z];
    let walk = |r: &mut Rig, hz: f32, amount: f32, cape: f32| {
        let p = t * TAU * hz;
        r.r_leg[0] = p.cos() * 1.4 * amount;
        r.l_leg[0] = (p + PI).cos() * 1.4 * amount;
        r.r_arm[0] += (p + PI).cos() * amount;
        r.l_arm[0] += p.cos() * amount;
        r.lift = (p * 2.0).cos().abs() * amount * 0.6;
        r.cape = cape + (p * 2.0).sin() * 0.08;
    };
    match pose {
        Pose::Idle | Pose::Cycle => {
            r.head = [(t * 0.37).sin() * 0.06, (t * 0.5).sin() * 0.3, 0.];
        }
        Pose::Walk => walk(&mut r, 0.9, 0.45, 0.35),
        Pose::Run => {
            walk(&mut r, 1.4, 0.85, 0.8);
            r.head[0] = 0.1;
        }
        Pose::Sneak => {
            sneak(&mut r);
            r.head = [0.1, (t * 0.6).sin() * 0.25, 0.];
        }
        Pose::CrouchWalk => {
            walk(&mut r, 0.6, 0.35, 0.0);
            let cape = r.cape;
            sneak(&mut r);
            r.cape = cape + 0.5;
            r.lift = 0.;
        }
        Pose::Jump => {
            // A hop a second: up and down on a parabola, arms out at the top.
            let p = (t % 1.0) / 1.0;
            let h = 4.0 * p * (1.0 - p);
            r.lift = h * 7.0;
            r.r_arm = [-0.3 * h, 0., 0.35 * h + sway_z];
            r.l_arm = [-0.3 * h, 0., -0.35 * h - sway_z];
            r.r_leg[0] = -0.35 * h;
            r.l_leg[0] = 0.25 * h;
            r.cape = 0.15 + (1.0 - h) * 0.5;
            r.center = [0., 19., 0.];
        }
        Pose::Swim => {
            // Face down, crawl strokes, kicking legs, looking ahead.
            let p = t * TAU * 0.5;
            r.root = [1.35, 0., 0.];
            r.lift = 2.;
            r.head = [-1.1, 0., 0.];
            r.r_arm = [-(p.rem_euclid(TAU)), 0., 0.1];
            r.l_arm = [-((p + PI).rem_euclid(TAU)), 0., -0.1];
            r.r_leg[0] = (p * 2.0).sin() * 0.35;
            r.l_leg[0] = -(p * 2.0).sin() * 0.35;
            r.cape = 0.25 + (p * 2.0).sin() * 0.05;
            r.center = [0., 13., 0.];
            r.fit = 0.7;
        }
        Pose::Glide => {
            // The game's fall-flying pose: level, wings spread, arms back.
            r.root = [1.25, 0., (t * 0.9).sin() * 0.12];
            r.head = [-1.0, 0., 0.];
            r.r_arm = [0.15, 0., 0.25];
            r.l_arm = [0.15, 0., -0.25];
            r.r_leg[0] = 0.08;
            r.l_leg[0] = -0.08;
            r.wings = [0.35 + (t * 1.5).sin() * 0.04, -1.45 + (t * 1.5).sin() * 0.05];
            r.cape = 1.2;
            r.lift = 4. + (t * 0.9).sin() * 1.5;
            r.center = [0., 15., 0.];
            r.fit = 0.62;
        }
        Pose::Wave => {
            r.head = [-0.05, 0.15, (t * 3.5).sin() * 0.06];
            r.r_arm = [-0.2, 0., 2.55 + (t * 7.0).sin() * 0.32];
        }
        Pose::Dance => {
            let p = t * TAU * 1.0;
            r.root = [0., p.sin() * 0.25, (p * 2.0).sin() * 0.06];
            r.lift = (p * 2.0).sin().abs() * 1.2;
            r.r_arm = [-0.3, 0., 2.3 + (p * 2.0).sin() * 0.5];
            r.l_arm = [-0.3, 0., -2.3 + (p * 2.0 + PI).sin() * 0.5];
            r.r_leg = [(p * 2.0).sin().max(0.) * -0.6, 0., 0.08];
            r.l_leg = [(p * 2.0 + PI).sin().max(0.) * -0.6, 0., -0.08];
            r.head = [(p * 2.0).sin() * 0.15, 0., (p).sin() * 0.12];
            r.cape = 0.25 + (p * 2.0).sin().abs() * 0.2;
        }
        Pose::LookAround => {
            // Turn to one side, hold, to the other, hold, and up now and then.
            let s = (t * 0.7).sin();
            let held = s.signum() * s.abs().powf(0.4);
            r.head = [-0.25 * (t * 0.45).sin().max(0.) + 0.1 * (t * 1.1).sin(), held * 0.95, 0.];
        }
        Pose::Attack => {
            // A swing every 0.9 s, like the game's attack.
            let p = (t % 0.9) / 0.35;
            if p < 1.0 {
                let s = (p * PI).sin();
                let sq = p.sqrt();
                r.root = [0., -(sq * TAU).sin() * 0.2, 0.];
                r.r_arm = [-0.4 - s * 1.6 + (sq * PI).sin() * -0.3, -(sq * TAU).sin() * 0.25, sway_z - s * 0.4];
            } else {
                r.r_arm[0] -= 0.3;
            }
            r.head = [0.1, 0., 0.];
        }
        Pose::Bow => {
            // Both arms up to aim; the drawing arm pulls back and lets go.
            let p = (t % 2.0) / 2.0;
            let draw = (p / 0.7).min(1.0);
            let head_y = 0.1;
            r.head = [-0.05, head_y, 0.];
            r.r_arm = [-FRAC_PI_2 - 0.05, -0.1 + head_y - 0.4 * draw, 0.];
            r.l_arm = [-FRAC_PI_2 - 0.05, 0.1 + head_y + 0.4, 0.];
            r.cape = 0.12;
        }
        Pose::Sit => {
            r.r_leg = [-1.4137, PI / 10.0, 0.07];
            r.l_leg = [-1.4137, -PI / 10.0, -0.07];
            r.r_arm[0] += -PI / 5.0;
            r.l_arm[0] += -PI / 5.0;
            r.head = [(t * 0.37).sin() * 0.05, (t * 0.5).sin() * 0.25, 0.];
            r.lift = -10.0;
            r.cape = 0.05;
            r.center = [0., 11., 0.];
        }
        Pose::Sleep => {
            // On the back, breathing slowly.
            let b = (t * 1.2).sin() * 0.03;
            r.root = [-FRAC_PI_2, 0., 0.];
            r.root_pivot = [0., 2., 0.];
            r.r_arm = [b, 0., 0.03];
            r.l_arm = [b, 0., -0.03];
            r.head = [0., 0.25, 0.];
            r.cape = 0.;
            r.lift = 2.;
            r.center = [0., 5., -11.];
        }
        Pose::TPose => {
            r.r_arm = [0., 0., FRAC_PI_2];
            r.l_arm = [0., 0., -FRAC_PI_2];
            r.cape = 0.1;
        }
    }
    r
}

/// Whether the texture is the old 64×32 layout.
fn is_legacy(skin: &RgbaImage) -> bool {
    skin.height() * 2 == skin.width()
}

/// Old skins often fill the hat area with a solid colour; the game hides it.
fn hat_is_solid(skin: &RgbaImage) -> bool {
    let s = (skin.width() / 64).max(1);
    (0..16 * s).all(|y| (32 * s..64 * s).all(|x| skin.get_pixel(x, y).0[3] == 255))
}

fn tex_scale(img: &RgbaImage) -> f32 {
    if img.width() >= 64 { (img.width() / 64) as f32 } else { 1.0 }
}

/// The player's faces for one moment.
fn build(scene: &Scene) -> Vec<Quad> {
    let skin = scene.skin;
    let ts = tex_scale(skin);
    let legacy = is_legacy(skin);
    let r = rig(scene.pose, scene.time);
    // The whole player's turn about its pivot, then the lift.
    let root_m = M3::rx(r.root[0]).mul(M3::ry(r.root[1])).mul(M3::rz(r.root[2]));
    let root = |xf: Xf| Xf {
        m: root_m.mul(xf.m),
        t: add(add(root_m.apply(sub(xf.t, r.root_pivot)), r.root_pivot), [0., r.lift, 0.]),
    };
    let mut out = Vec::with_capacity(96);
    let part = |pivot: V3, off: V3, angles: V3| root(Xf::at(add(pivot, off), M3::mc(angles)));
    let boxes = |out: &mut Vec<Quad>, xf: Xf, base: (f32, f32), over: Option<(f32, f32)>, size: V3, from: V3, inflate: f32, mirror: bool| {
        Cuboid { tex: TexId::Skin, ts, uv: base, size, from, inflate: 0., mirror, solid: true }.emit(xf, out);
        if let Some(o) = over {
            Cuboid { tex: TexId::Skin, ts, uv: o, size, from, inflate, mirror: false, solid: false }.emit(xf, out);
        }
    };

    // Head and hat.
    let hat = !(legacy && hat_is_solid(skin));
    boxes(
        &mut out,
        part([0., 24., 0.], r.head_off, r.head),
        (0., 0.),
        hat.then_some((32., 0.)),
        [8., 8., 8.],
        [-4., 0., -4.],
        0.5,
        false,
    );
    // Body and jacket.
    let body = part([0., 24., 0.], r.body_off, r.body);
    boxes(
        &mut out,
        body,
        (16., 16.),
        (!legacy).then_some((16., 32.)),
        [8., 12., 4.],
        [-4., -12., -2.],
        0.25,
        false,
    );
    // Arms: the player's right is -x. Slim arms are 3 wide.
    let aw = if scene.slim { 3. } else { 4. };
    let right_from = [-(aw - 1.), -10., -2.];
    let left_from = [-1., -10., -2.];
    boxes(
        &mut out,
        part([-5., 22., 0.], r.arm_off, r.r_arm),
        (40., 16.),
        (!legacy).then_some((40., 32.)),
        [aw, 12., 4.],
        right_from,
        0.25,
        false,
    );
    let (l_uv, l_over) = if legacy { ((40., 16.), None) } else { ((32., 48.), Some((48., 48.))) };
    boxes(
        &mut out,
        part([5., 22., 0.], r.arm_off, r.l_arm),
        l_uv,
        l_over,
        [aw, 12., 4.],
        left_from,
        0.25,
        legacy,
    );
    // Legs.
    boxes(
        &mut out,
        part([-1.9, 12., 0.], r.leg_off, r.r_leg),
        (0., 16.),
        (!legacy).then_some((0., 32.)),
        [4., 12., 4.],
        [-2., -12., -2.],
        0.25,
        false,
    );
    let (l_uv, l_over) = if legacy { ((0., 16.), None) } else { ((16., 48.), Some((0., 48.))) };
    boxes(
        &mut out,
        part([1.9, 12., 0.], r.leg_off, r.l_leg),
        l_uv,
        l_over,
        [4., 12., 4.],
        [-2., -12., -2.],
        0.25,
        legacy,
    );

    // Cape and elytra hang from the body, so they follow its lean.
    let on_body = |pivot: V3, m: M3| Xf {
        m: body.m.mul(m),
        t: add(body.m.apply(pivot), body.t),
    };
    if let Some(e) = scene.elytra {
        let ts = tex_scale(e);
        let [wx, wz] = r.wings;
        for left in [true, false] {
            let (px, z, from_x) = if left { (5., wz, -10.) } else { (-5., -wz, 0.) };
            Cuboid {
                tex: TexId::Elytra,
                ts,
                uv: (22., 0.),
                size: [10., 20., 2.],
                from: [from_x, -20., -2.],
                inflate: 1.0,
                mirror: !left,
                solid: false,
            }
            .emit(on_body([px, 0., -2.], M3::mc([wx, 0., z])), &mut out);
        }
    } else if let Some(c) = scene.cape {
        // Turned to face away, so its outside (the texture's front) shows
        // from behind.
        Cuboid {
            tex: TexId::Cape,
            ts: tex_scale(c),
            uv: (0., 0.),
            size: [10., 16., 1.],
            from: [-5., -16., 0.],
            inflate: 0.,
            mirror: false,
            solid: false,
        }
        .emit(on_body([0., 0., -2.], M3::rx(r.cape).mul(M3::ry(PI))), &mut out);
    }
    out
}

// ---- backdrops --------------------------------------------------------------------

/// A small deterministic random sequence (xorshift).
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }
}

fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// A sky `w`×`h` pixels that wraps around horizontally: a gradient with
/// blocky Minecraft clouds, plus stars at night. `None` for no backdrop.
pub fn sky(kind: Backdrop, w: u32, h: u32) -> Option<RgbaImage> {
    let (top, bottom, cloud, cloud_shade) = match kind {
        Backdrop::None => return None,
        Backdrop::Day => ([74., 136., 230.], [176., 212., 250.], [250., 252., 255.], [214., 226., 240.]),
        Backdrop::Night => ([6., 9., 24.], [30., 44., 86.], [48., 54., 78.], [34., 38., 58.]),
    };
    let (w, h) = (w.max(1), h.max(1));
    let mut img = RgbaImage::new(w, h);
    for y in 0..h {
        let c = lerp3(top, bottom, y as f32 / h as f32);
        let px = image::Rgba([c[0] as u8, c[1] as u8, c[2] as u8, 255]);
        for x in 0..w {
            img.put_pixel(x, y, px);
        }
    }
    let mut rng = Rng(0x9e37_79b9);
    if kind == Backdrop::Night {
        let stars = (w * h / 900).max(20);
        for _ in 0..stars {
            let (x, y) = (rng.next() % w, rng.next() % (h * 4 / 5));
            let b = 140. + rng.unit() * 115.;
            let size = if rng.unit() > 0.9 { 2 } else { 1 };
            for dy in 0..size {
                for dx in 0..size {
                    let (px, py) = ((x + dx) % w, (y + dy).min(h - 1));
                    img.put_pixel(px, py, image::Rgba([b as u8, b as u8, (b + 10.).min(255.) as u8, 255]));
                }
            }
        }
    }
    // Clouds: blobs of square cells on a coarse grid, lit on top.
    let cell = (h / 40).max(4);
    let (cols, rows) = (w / cell + 1, h / cell + 1);
    let mut grid = vec![false; (cols * rows) as usize];
    let blobs = 7 + w / (cell * 20);
    for _ in 0..blobs {
        let cx = rng.next() % cols;
        let cy = rows / 10 + rng.next() % (rows * 6 / 10).max(1);
        let len = 4 + rng.next() % 9;
        let thick = 1 + rng.next() % 3;
        for dy in 0..thick {
            let start = rng.next() % 3;
            for dx in start..len - (rng.next() % 3).min(len - start - 1) {
                let (gx, gy) = ((cx + dx) % cols, (cy + dy).min(rows - 1));
                grid[(gy * cols + gx) as usize] = true;
            }
        }
    }
    let alpha = if kind == Backdrop::Day { 0.85 } else { 0.75 };
    for gy in 0..rows {
        for gx in 0..cols {
            if !grid[(gy * cols + gx) as usize] {
                continue;
            }
            let below_empty = gy + 1 >= rows || !grid[((gy + 1) * cols + gx) as usize];
            for y in gy * cell..((gy + 1) * cell).min(h) {
                let shade = below_empty && y >= gy * cell + cell * 2 / 3;
                let c = if shade { cloud_shade } else { cloud };
                for x in gx * cell..((gx + 1) * cell).min(w) {
                    let p = img.get_pixel_mut(x, y);
                    for i in 0..3 {
                        p.0[i] = (f32::from(p.0[i]) * (1. - alpha) + c[i] * alpha) as u8;
                    }
                }
            }
        }
    }
    Some(img)
}

// ---- drawing ------------------------------------------------------------------------

/// Byte order of the output.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Rgba,
    Bgra,
}

/// Reusable buffers for drawing frames.
#[derive(Default)]
pub struct Renderer {
    /// Premultiplied colour and depth (1/distance) per supersample.
    color: Vec<[f32; 4]>,
    depth: Vec<f32>,
    /// Supersample size of the buffers.
    size: (usize, usize),
    /// What the last frame drew on (x0, y0, x1, y1): only that is cleared.
    dirty: [usize; 4],
}

struct Target<'a> {
    w: usize,
    h: usize,
    color: &'a mut [[f32; 4]],
    depth: &'a mut [f32],
    dirty: &'a mut [usize; 4],
}

struct Sampler<'a> {
    img: &'a RgbaImage,
}

/// Distance from the camera to the orbit's centre, in model units.
const DIST: f32 = 80.0;
/// The direction light comes from, in camera space.
const LIGHT: V3 = [-0.35, 0.55, 0.76];

impl Renderer {
    /// Draws one frame `w`×`h` pixels, drawn at `ss`×`ss` samples per pixel,
    /// over `backdrop` (scrolled with the camera's turn and `drift`).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        scene: &Scene,
        cam: &Camera,
        w: u32,
        h: u32,
        ss: u32,
        backdrop: Option<&RgbaImage>,
        drift: f32,
        order: Order,
    ) -> Vec<u8> {
        let (w, h, ss) = (w.max(1) as usize, h.max(1) as usize, ss.clamp(1, 4) as usize);
        let (sw, sh) = (w * ss, h * ss);
        if self.size != (sw, sh) {
            self.size = (sw, sh);
            self.color = vec![[0.0; 4]; sw * sh];
            self.depth = vec![0.0; sw * sh];
        } else {
            let [x0, y0, x1, y1] = self.dirty;
            for y in y0..y1.min(sh) {
                let row = y * sw;
                self.color[row + x0..row + x1.min(sw)].fill([0.0; 4]);
                self.depth[row + x0..row + x1.min(sw)].fill(0.0);
            }
        }
        self.dirty = [sw, sh, 0, 0];
        let mut target = Target {
            w: sw,
            h: sh,
            color: &mut self.color,
            depth: &mut self.depth,
            dirty: &mut self.dirty,
        };
        draw_model(scene, cam, &mut target);
        // In output pixels, rounded out.
        let [x0, y0, x1, y1] = self.dirty;
        let area = [x0 / ss, y0 / ss, x1.div_ceil(ss), y1.div_ceil(ss)];
        resolve(&self.color, w, h, ss, area, backdrop, cam.yaw, drift, order)
    }
}

/// Camera space: the orbit's centre at the origin, the camera on +z.
fn draw_model(scene: &Scene, cam: &Camera, t: &mut Target) {
    let quads = build(scene);
    let r = rig(scene.pose, scene.time);
    let center = r.center;
    let rot = M3::rx(cam.pitch).mul(M3::ry(-cam.yaw));
    let (w, h) = (t.w as f32, t.h as f32);
    // Fit 36 units tall (or 26 wide) at zoom 1.
    let f = (0.9 * h / 36.0).min(0.9 * w / 26.0) * DIST * cam.zoom * r.fit;
    let light = norm(LIGHT);
    let textures = |id: TexId| -> Option<Sampler> {
        let img = match id {
            TexId::Skin => Some(scene.skin),
            TexId::Cape => scene.cape,
            TexId::Elytra => scene.elytra,
        }?;
        Some(Sampler { img })
    };

    struct Ready<'q> {
        q: &'q Quad,
        p: [V3; 4],
        shade: f32,
        depth: f32,
    }
    let mut ready: Vec<Ready> = Vec::with_capacity(quads.len());
    for q in &quads {
        let p = q.v.map(|v| rot.apply(sub(v, center)));
        let n = norm(cross(sub(p[3], p[0]), sub(p[1], p[0])));
        let mid = scale(add(add(p[0], p[1]), add(p[2], p[3])), 0.25);
        let to_cam = sub([0., 0., DIST], mid);
        let facing = dot(n, to_cam) > 0.0;
        if q.solid && !facing {
            continue;
        }
        let n = if facing { n } else { scale(n, -1.0) };
        let shade = 0.55 + 0.45 * dot(n, light).max(0.0);
        ready.push(Ready { q, p, shade, depth: mid[2] });
    }
    // Solid faces first (any order: the depth buffer sorts them), then the
    // see-through ones from back to front so their edges blend right.
    ready.sort_by(|a, b| {
        b.q.solid
            .cmp(&a.q.solid)
            .then(if a.q.solid { std::cmp::Ordering::Equal } else { a.depth.total_cmp(&b.depth) })
    });
    for r in &ready {
        let Some(tex) = textures(r.q.tex) else { continue };
        // Project: x right, y down, plus 1/distance for depth and texture.
        let s: [[f32; 3]; 4] = r.p.map(|p| {
            let d = (DIST - p[2]).max(1.0);
            [w * 0.5 + f * p[0] / d, h * 0.5 - f * p[1] / d, 1.0 / d]
        });
        raster(t, s, r.q.uv, r.q, &tex, r.shade);
    }
}

/// Fills one face: a convex quad on screen. Texture coordinates and depth
/// over distance are planar across it, so each is a linear function of
/// the pixel position; every row is cut to the span inside all four edges.
fn raster(t: &mut Target, s: [[f32; 3]; 4], uv: [[f32; 2]; 4], q: &Quad, tex: &Sampler, shade: f32) {
    // Plane gradients from the first three corners.
    let (x0, y0) = (s[0][0], s[0][1]);
    let (e1x, e1y, e2x, e2y) = (s[1][0] - x0, s[1][1] - y0, s[2][0] - x0, s[2][1] - y0);
    let det = e1x * e2y - e2x * e1y;
    if det.abs() < 1e-6 {
        return;
    }
    let plane = |a0: f32, a1: f32, a2: f32| {
        let (d1, d2) = (a1 - a0, a2 - a0);
        let gx = (d1 * e2y - d2 * e1y) / det;
        let gy = (d2 * e1x - d1 * e2x) / det;
        // Value at screen (0, 0), and per step in x and y.
        (a0 - gx * x0 - gy * y0, gx, gy)
    };
    let w = plane(s[0][2], s[1][2], s[2][2]);
    let uw = plane(uv[0][0] * s[0][2], uv[1][0] * s[1][2], uv[2][0] * s[2][2]);
    let vw = plane(uv[0][1] * s[0][2], uv[1][1] * s[1][2], uv[2][1] * s[2][2]);

    let min_y = s.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor().max(0.0) as usize;
    let max_y = s.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil().min(t.h as f32) as usize;
    let min_x = s.iter().map(|p| p[0]).fold(f32::MAX, f32::min).floor().max(0.0);
    let max_x = s.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil().min(t.w as f32);
    if min_y >= max_y || min_x >= max_x {
        return;
    }
    // Edges as e(x, y) = a·x + b·y + c, positive inside.
    let sign = det.signum();
    let edges: [(f32, f32, f32); 4] = std::array::from_fn(|i| {
        let (p, n) = (s[i], s[(i + 1) % 4]);
        let (a, b) = (-(n[1] - p[1]) * sign, (n[0] - p[0]) * sign);
        (a, b, -(a * p[0] + b * p[1]))
    });
    let (mut dx0, mut dx1) = (usize::MAX, 0);
    let img = tex.img.as_raw();
    let tw = tex.img.width() as usize;
    let (tw_f, th_f) = (tex.img.width() as f32, tex.img.height() as f32);
    let cl = [q.clamp[0].max(0.0), q.clamp[1].max(0.0), (q.clamp[2] - 0.001).min(tw_f - 0.001), (q.clamp[3] - 0.001).min(th_f - 0.001)];
    if cl[0] > cl[2] || cl[1] > cl[3] {
        return;
    }
    let k_shade = shade / 255.0;
    for y in min_y..max_y {
        let py = y as f32 + 0.5;
        // The span of pixel centres inside every edge (a small overlap
        // keeps neighbouring faces seamless).
        let (mut lo, mut hi) = (min_x, max_x);
        for &(a, b, c) in &edges {
            let r = b * py + c + 0.01;
            if a.abs() < 1e-9 {
                if r < 0.0 {
                    hi = lo;
                }
            } else if a > 0.0 {
                lo = lo.max((-r / a - 0.5).ceil());
            } else {
                hi = hi.min((-r / a - 0.5).floor() + 1.0);
            }
        }
        if lo >= hi {
            continue;
        }
        let (xs, xe) = (lo as usize, (hi as usize).min(t.w));
        dx0 = dx0.min(xs);
        dx1 = dx1.max(xe);
        let row = y * t.w;
        let base = |p: (f32, f32, f32)| p.0 + p.2 * py;
        let (w_row, u_row, v_row) = (base(w), base(uw), base(vw));
        for x in xs..xe {
            let px = x as f32 + 0.5;
            let iw = w_row + w.1 * px;
            let i = row + x;
            if iw <= t.depth[i] {
                continue;
            }
            let inv = 1.0 / iw;
            let u = ((u_row + uw.1 * px) * inv).clamp(cl[0], cl[2]);
            let v = ((v_row + vw.1 * px) * inv).clamp(cl[1], cl[3]);
            let o = (v as usize * tw + u as usize) * 4;
            let c = &img[o..o + 4];
            let a = if q.solid { 1.0 } else { f32::from(c[3]) * (1.0 / 255.0) };
            if a <= 0.0 {
                continue;
            }
            let k = k_shade * a;
            let src = [f32::from(c[0]) * k, f32::from(c[1]) * k, f32::from(c[2]) * k, a];
            let dst = &mut t.color[i];
            if a >= 1.0 {
                *dst = src;
            } else {
                for ch in 0..4 {
                    dst[ch] = src[ch] + dst[ch] * (1.0 - a);
                }
            }
            t.depth[i] = iw;
        }
    }
    if dx0 < dx1 {
        let d = &mut *t.dirty;
        *d = [d[0].min(dx0), d[1].min(min_y), d[2].max(dx1), d[3].max(max_y)];
    }
}

/// Averages the supersamples and lays the result over the backdrop.
#[allow(clippy::too_many_arguments)]
fn resolve(
    color: &[[f32; 4]],
    w: usize,
    h: usize,
    ss: usize,
    area: [usize; 4],
    backdrop: Option<&RgbaImage>,
    yaw: f32,
    drift: f32,
    order: Order,
) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    let sw = w * ss;
    let n = (ss * ss) as f32;
    // The backdrop turns with the camera (a full turn = its width) and drifts.
    let (bw, bh) = backdrop.map(|b| (b.width() as usize, b.height() as usize)).unwrap_or((1, 1));
    let shift = ((-yaw / TAU + drift) * bw as f32).rem_euclid(bw as f32) as usize;
    let (ri, bi) = if order == Order::Bgra { (2, 0) } else { (0, 2) };
    for y in 0..h {
        let line = &mut out[y * w * 4..(y + 1) * w * 4];
        // This row of the backdrop, from the shift on, wrapping.
        let sky_row = backdrop.map(|b| {
            let by = (y * bh / h).min(bh - 1);
            &b.as_raw()[by * bw * 4..(by + 1) * bw * 4]
        });
        let sky = |x: usize| -> [u8; 3] {
            match sky_row {
                Some(r) => {
                    let o = ((x + shift) % bw) * 4;
                    [r[o], r[o + 1], r[o + 2]]
                }
                None => [0, 0, 0],
            }
        };
        let in_rows = y >= area[1] && y < area[3];
        for x in 0..w {
            let o = x * 4;
            let mut acc = [0.0f32; 4];
            if in_rows && x >= area[0] && x < area[2] {
                for sy in 0..ss {
                    let row = (y * ss + sy) * sw + x * ss;
                    for c in &color[row..row + ss] {
                        for ch in 0..4 {
                            acc[ch] += c[ch];
                        }
                    }
                }
            }
            let a = acc[3] / n;
            if a <= 0.0 {
                // Nothing of the player here: the backdrop as it is.
                if sky_row.is_some() {
                    let p = sky(x);
                    line[o + ri] = p[0];
                    line[o + 1] = p[1];
                    line[o + bi] = p[2];
                    line[o + 3] = 255;
                }
                continue;
            }
            let mut rgb = [acc[0] / n * 255.0, acc[1] / n * 255.0, acc[2] / n * 255.0];
            let alpha = if sky_row.is_some() {
                let p = sky(x);
                for ch in 0..3 {
                    rgb[ch] += f32::from(p[ch]) * (1.0 - a);
                }
                1.0
            } else {
                for v in &mut rgb {
                    *v /= a;
                }
                a
            };
            line[o + ri] = rgb[0].round().clamp(0.0, 255.0) as u8;
            line[o + 1] = rgb[1].round().clamp(0.0, 255.0) as u8;
            line[o + bi] = rgb[2].round().clamp(0.0, 255.0) as u8;
            line[o + 3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// A still picture of the player as an image (for saving).
pub fn picture(scene: &Scene, cam: &Camera, w: u32, h: u32, backdrop: Backdrop) -> RgbaImage {
    let sky = sky(backdrop, w * 2, h);
    let bytes = Renderer::default().render(scene, cam, w, h, 2, sky.as_ref(), 0.0, Order::Rgba);
    RgbaImage::from_raw(w, h, bytes).unwrap_or_else(|| RgbaImage::new(w, h))
}

/// Whether a cape texture has elytra wings drawn (at 22,0, 24×22).
pub fn has_elytra_art(texture: &RgbaImage) -> bool {
    let s = tex_scale(texture) as u32;
    let (w, h) = (texture.width().min(46 * s), texture.height().min(22 * s));
    (0..h).any(|y| (22 * s..w).any(|x| texture.get_pixel(x, y).0[3] > 0))
}

/// The angle between two yaws, for easing the camera back.
pub fn yaw_delta(from: f32, to: f32) -> f32 {
    (to - from + PI).rem_euclid(TAU) - PI
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skinrender::{sample_cape, sample_skin};

    fn scene<'a>(skin: &'a RgbaImage, cape: Option<&'a RgbaImage>) -> Scene<'a> {
        Scene {
            skin,
            slim: false,
            cape,
            elytra: None,
            pose: Pose::Idle,
            time: 0.0,
        }
    }

    fn front() -> Camera {
        Camera {
            yaw: 0.0,
            pitch: 0.0,
            zoom: 1.0,
        }
    }

    fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2], img[i + 3]]
    }

    /// The column of opaque pixels' extent, top and bottom.
    fn extent(img: &[u8], w: u32, h: u32) -> (u32, u32, u32, u32) {
        let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
        for y in 0..h {
            for x in 0..w {
                if px(img, w, x, y)[3] > 128 {
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
        }
        (x0, y0, x1, y1)
    }

    #[test]
    fn front_view_shows_a_player_shape() {
        let skin = sample_skin(false, [200, 40, 40]);
        let (w, h) = (120, 200);
        let img = Renderer::default().render(&scene(&skin, None), &front(), w, h, 1, None, 0.0, Order::Rgba);
        let (x0, y0, x1, y1) = extent(&img, w, h);
        let (pw, ph) = (x1 - x0, y1 - y0);
        // About 16 wide by 32 tall (plus the hat), centred.
        assert!(ph > 120 && ph < 200, "height {ph}");
        let ratio = pw as f32 / ph as f32;
        assert!((0.4..0.6).contains(&ratio), "ratio {ratio}");
        assert!(((x0 + x1) as i32 / 2 - w as i32 / 2).abs() <= 3);
        // The shirt (tinted red) is in the middle; corners are empty.
        let mid = px(&img, w, w / 2, h / 2 - 10);
        assert!(mid[0] > mid[1] && mid[3] == 255, "{mid:?}");
        assert_eq!(px(&img, w, 0, 0)[3], 0);
    }

    #[test]
    fn slim_arms_are_narrower() {
        let wide = sample_skin(false, [200, 40, 40]);
        let slim = sample_skin(true, [200, 40, 40]);
        let (w, h) = (160, 200);
        let mut r = Renderer::default();
        let a = r.render(&scene(&wide, None), &front(), w, h, 1, None, 0.0, Order::Rgba);
        let mut s = scene(&slim, None);
        s.slim = true;
        let b = r.render(&s, &front(), w, h, 1, None, 0.0, Order::Rgba);
        let (a0, _, a1, _) = extent(&a, w, h);
        let (b0, _, b1, _) = extent(&b, w, h);
        assert!(b1 - b0 < a1 - a0, "slim {} vs classic {}", b1 - b0, a1 - a0);
    }

    #[test]
    fn cape_shows_from_behind_only() {
        let skin = sample_skin(false, [200, 40, 40]);
        let cape = sample_cape([20, 200, 20]);
        let (w, h) = (120, 200);
        let mut r = Renderer::default();
        let back = Camera { yaw: PI, ..front() };
        let img = r.render(&scene(&skin, Some(&cape)), &back, w, h, 1, None, 0.0, Order::Rgba);
        let c = px(&img, w, w / 2 + 12, h / 2 - 4);
        assert!(c[1] > 150 && c[0] < 100, "cape green from behind: {c:?}");
        let img = r.render(&scene(&skin, Some(&cape)), &front(), w, h, 1, None, 0.0, Order::Rgba);
        let c = px(&img, w, w / 2, h / 2 - 10);
        assert!(c[0] > c[1], "shirt from the front: {c:?}");
    }

    #[test]
    fn elytra_replaces_the_cape_and_backdrops_fill() {
        let skin = sample_skin(false, [200, 40, 40]);
        let cape = sample_cape([20, 200, 20]);
        let mut s = scene(&skin, Some(&cape));
        s.elytra = Some(&cape);
        let (w, h) = (120, 200);
        let back = Camera { yaw: PI, ..front() };
        let sky = sky(Backdrop::Night, w * 2, h).unwrap();
        let img = Renderer::default().render(&s, &back, w, h, 2, Some(&sky), 0.0, Order::Bgra);
        assert!(img.chunks(4).all(|p| p[3] == 255), "the sky is opaque");
        for pose in Pose::ALL {
            for t in [0.0, 0.7, 2.3] {
                s.pose = pose;
                s.time = t;
                let n = build(&s).len();
                assert_eq!(n, 12 * 6 + 2 * 6, "{pose:?}");
            }
        }
    }

    /// Writes sample renders to `HOPPER_SKIN3D_OUT` for a look by eye.
    #[test]
    fn dump_when_asked() {
        let Some(dir) = std::env::var_os("HOPPER_SKIN3D_OUT") else { return };
        let dir = std::path::PathBuf::from(dir);
        let skin = match std::env::var_os("HOPPER_SKIN3D_SKIN") {
            Some(p) => image::open(p).unwrap().to_rgba8(),
            None => sample_skin(false, [200, 40, 40]),
        };
        let cape = sample_cape([20, 160, 40]);
        let wings = std::env::var_os("HOPPER_SKIN3D_ELYTRA").map(|p| image::open(p).unwrap().to_rgba8());
        for (i, pose) in Pose::ALL.into_iter().enumerate() {
            let mut s = scene(&skin, Some(&cape));
            s.pose = pose;
            s.time = 0.45;
            if pose == Pose::Glide {
                s.elytra = Some(wings.as_ref().unwrap_or(&cape));
            }
            let cam = Camera { yaw: 0.9, ..Camera::default() };
            picture(&s, &cam, 200, 320, Backdrop::Day).save(dir.join(format!("{i:02}-{}.png", pose.label()))).unwrap();
        }
    }

    #[test]
    fn reused_buffers_match_fresh_ones() {
        let skin = sample_skin(false, [200, 40, 40]);
        let mut s = scene(&skin, None);
        let mut r = Renderer::default();
        let _ = r.render(&s, &Camera::default(), 90, 150, 2, None, 0.0, Order::Rgba);
        s.pose = Pose::Run;
        s.time = 0.3;
        let cam = Camera { yaw: 2.0, ..Camera::default() };
        let again = r.render(&s, &cam, 90, 150, 2, None, 0.0, Order::Rgba);
        let fresh = Renderer::default().render(&s, &cam, 90, 150, 2, None, 0.0, Order::Rgba);
        assert!(again == fresh);
    }

    /// Prints how long a frame takes, when `HOPPER_SKIN3D_BENCH` is set.
    #[test]
    fn bench_when_asked() {
        if std::env::var_os("HOPPER_SKIN3D_BENCH").is_none() {
            return;
        }
        let skin = sample_skin(false, [200, 40, 40]);
        let sky = sky(Backdrop::Day, 560, 466).unwrap();
        let mut r = Renderer::default();
        let mut s = scene(&skin, None);
        let t0 = std::time::Instant::now();
        for i in 0..120 {
            s.time = i as f32 / 60.0;
            let _ = r.render(&s, &Camera::default(), 280, 466, 2, Some(&sky), 0.0, Order::Bgra);
        }
        eprintln!("frame: {:?}", t0.elapsed() / 120);
    }

    #[test]
    fn camera_limits() {
        let mut c = Camera::default();
        c.scroll(100.0);
        assert_eq!(c.zoom, Camera::MAX_ZOOM);
        c.scroll(-100.0);
        assert_eq!(c.zoom, Camera::MIN_ZOOM);
        c.drag(0.0, 10_000.0);
        assert_eq!(c.pitch, Camera::MAX_PITCH);
        assert!((yaw_delta(0.1, TAU - 0.1) + 0.2).abs() < 1e-4);
        assert_eq!(Pose::default(), Pose::Walk);
        assert_eq!(Pose::Cycle.at(0.5), Pose::Walk);
        assert_eq!(Pose::Cycle.at(4.5), Pose::Idle);
        assert_eq!(Pose::Run.at(99.0), Pose::Run);
        // Blending into the next pose stays finite all the way round.
        for i in 0..400 {
            let r = rig(Pose::Cycle, i as f32 * 0.17);
            assert!(r.r_arm.iter().chain(&r.root).chain(&r.center).all(|v| v.is_finite()));
        }
        assert!(!has_elytra_art(&sample_cape([1, 2, 3])));
        let mut c = sample_cape([1, 2, 3]);
        c.put_pixel(30, 5, image::Rgba([9, 9, 9, 255]));
        assert!(has_elytra_art(&c));
    }
}
