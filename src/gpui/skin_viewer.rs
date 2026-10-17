//! The 3D skin preview on the Accounts page: the player drawn by
//! [`hopper::skin3d`] into one image per frame. Drag to turn, scroll to
//! zoom; a toolbar picks the cape, elytra, backdrop, pause and view reset.
//! It animates only while shown and not paused, and draws nothing new while
//! nothing changes.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hopper::skin3d::{self, Backdrop, Camera, Order, Pose, Renderer, Scene};
use image::RgbaImage;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

/// Samples per pixel along each side (2 = 4× supersampling).
const SUPERSAMPLE: u32 = 2;
/// How long "reset view" takes to ease back.
const EASE_SECS: f32 = 0.4;
/// Frames per second while animating (displays may refresh faster; the
/// whole window redraws each frame, so more would only cost CPU).
const FPS: f32 = 60.0;

pub enum ViewerEvent {
    /// A file was dropped on the preview.
    Dropped(PathBuf),
}

/// A cape the toolbar can put on the preview.
#[derive(Clone, PartialEq)]
pub struct CapeChoice {
    pub label: SharedString,
    pub path: PathBuf,
}

/// What a drawn frame showed, to skip drawing it again.
#[derive(Clone, PartialEq)]
struct FrameKey {
    w: u32,
    h: u32,
    textures: [usize; 3],
    slim: bool,
    pose: Pose,
    time: u32,
    cam: [u32; 3],
    backdrop: Backdrop,
}

#[derive(Default)]
struct Frames {
    renderer: Renderer,
    key: Option<FrameKey>,
    image: Option<Arc<RenderImage>>,
    /// The sky at its size, made once per size and kind.
    sky: Option<((Backdrop, u32, u32), Arc<RgbaImage>)>,
}

pub struct SkinViewer {
    skin: Option<PathBuf>,
    slim: bool,
    capes: Vec<CapeChoice>,
    /// The cape on the preview (None: no cape).
    cape: Option<PathBuf>,
    elytra: bool,
    default_elytra: Option<PathBuf>,
    pose: Pose,
    backdrop: Backdrop,
    paused: bool,
    cam: Camera,
    /// Animation seconds, and when they were last advanced.
    time: f32,
    last: Option<Instant>,
    /// The mouse position of a drag in progress.
    drag: Option<Point<Pixels>>,
    /// Easing back to the default view: when it began, and from where.
    easing: Option<(Instant, Camera)>,
    textures: HashMap<PathBuf, Option<Arc<RgbaImage>>>,
    frames: Rc<RefCell<Frames>>,
    /// The timer for the next animation frame.
    tick: Option<Task<()>>,
}

impl EventEmitter<ViewerEvent> for SkinViewer {}

impl SkinViewer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // The last frame stays in the GPU atlas until dropped.
        cx.on_release(|this: &mut Self, cx| {
            if let Some(img) = this.frames.borrow_mut().image.take() {
                cx.drop_image(img, None);
            }
        })
        .detach();
        Self {
            skin: None,
            slim: false,
            capes: Vec::new(),
            cape: None,
            elytra: false,
            default_elytra: None,
            pose: Pose::default(),
            backdrop: Backdrop::Day,
            paused: false,
            cam: Camera::default(),
            time: 0.0,
            last: None,
            drag: None,
            easing: None,
            textures: HashMap::new(),
            frames: Rc::default(),
            tick: None,
        }
    }

    /// The skin to show (a PNG file) and its arms.
    pub fn set_skin(&mut self, path: Option<PathBuf>, slim: bool) {
        self.skin = path;
        self.slim = slim;
    }

    /// The capes the toolbar offers; the preview wears `active` whenever the
    /// list changes.
    pub fn set_capes(&mut self, capes: Vec<CapeChoice>, active: Option<PathBuf>) {
        if capes != self.capes {
            self.capes = capes;
            self.cape = active;
        }
    }

    /// The elytra texture shown when no cape is picked.
    pub fn set_default_elytra(&mut self, path: Option<PathBuf>) {
        self.default_elytra = path;
    }

    pub fn pose(&self) -> Pose {
        self.pose
    }

    pub fn set_pose(&mut self, pose: Pose, cx: &mut Context<Self>) {
        self.pose = pose;
        cx.notify();
    }

    fn texture(&mut self, path: &Path) -> Option<Arc<RgbaImage>> {
        self.textures
            .entry(path.to_path_buf())
            .or_insert_with(|| match image::open(path) {
                Ok(i) => Some(Arc::new(i.to_rgba8())),
                Err(e) => {
                    log::debug!("skin preview {}: {e:#}", path.display());
                    None
                }
            })
            .clone()
    }

    fn drag_to(&mut self, at: Point<Pixels>, held: bool, cx: &mut Context<Self>) {
        let Some(from) = self.drag else { return };
        if !held {
            self.drag = None;
            cx.notify();
            return;
        }
        self.cam.drag(f32::from(at.x - from.x), f32::from(at.y - from.y));
        self.drag = Some(at);
        cx.notify();
    }

    fn reset_view(&mut self, cx: &mut Context<Self>) {
        self.easing = Some((Instant::now(), self.cam));
        cx.notify();
    }

    /// Moves the camera along a reset in progress.
    fn ease(&mut self, now: Instant) -> bool {
        let Some((start, from)) = self.easing else { return false };
        let k = ((now - start).as_secs_f32() / EASE_SECS).min(1.0);
        let e = 1.0 - (1.0 - k).powi(3);
        let to = Camera::default();
        self.cam = Camera {
            yaw: from.yaw + skin3d::yaw_delta(from.yaw, to.yaw) * e,
            pitch: from.pitch + (to.pitch - from.pitch) * e,
            zoom: from.zoom + (to.zoom - from.zoom) * e,
        };
        if k >= 1.0 {
            self.easing = None;
        }
        true
    }

    /// The textures for a frame: skin, cape, elytra.
    fn scene_textures(&mut self) -> Option<(Arc<RgbaImage>, Option<Arc<RgbaImage>>, Option<Arc<RgbaImage>>)> {
        let skin = self.skin.clone().and_then(|p| self.texture(&p))?;
        let cape = self.cape.clone().and_then(|p| self.texture(&p));
        let elytra = if self.elytra {
            // The game draws the elytra with the cape's texture when there is
            // one (old capes have no wings drawn: use the plain elytra then).
            cape.clone()
                .filter(|c| skin3d::has_elytra_art(c))
                .or_else(|| self.default_elytra.clone().and_then(|p| self.texture(&p)))
        } else {
            None
        };
        Some((skin, cape, elytra))
    }

    fn save_png(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((skin, cape, elytra)) = self.scene_textures() else { return };
        let (slim, pose, time, cam, backdrop) = (self.slim, self.pose, self.time, self.cam, self.backdrop);
        let dir = directories::UserDirs::new()
            .and_then(|u| u.picture_dir().or(u.download_dir()).map(Path::to_path_buf))
            .unwrap_or_default();
        let rx = cx.prompt_for_new_path(&dir, Some("skin.png"));
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(dest))) = rx.await else { return };
            let shown = dest.display().to_string();
            let r = cx
                .background_spawn(async move {
                    let scene = Scene {
                        skin: &skin,
                        slim,
                        cape: cape.as_deref(),
                        elytra: elytra.as_deref(),
                        pose,
                        time,
                    };
                    skin3d::picture(&scene, &cam, 640, 1024, backdrop).save(&dest)
                })
                .await;
            cx.update(|window, cx| match r {
                Ok(()) => window.push_notification(Notification::success(format!("Saved {shown}.")), cx),
                Err(e) => window.push_notification(Notification::error(format!("Couldn't save the picture: {e}")), cx),
            })
            .ok();
        })
        .detach();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let view = cx.entity().downgrade();
        let capes = self.capes.clone();
        let cape = self.cape.clone();
        let cape_menu = {
            let view = view.clone();
            Button::new("viewer-capes")
                .xsmall()
                .ghost()
                .icon(IconName::Shirt)
                .label("Capes")
                .tooltip("Cape on the preview")
                .disabled(capes.is_empty())
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                    let pick = |p: Option<PathBuf>| {
                        let view = view.clone();
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            view.update(cx, |v, cx| {
                                v.cape = p.clone();
                                cx.notify();
                            })
                            .ok();
                        }
                    };
                    menu = menu.item(PopupMenuItem::new("No cape").checked(cape.is_none()).on_click(pick(None)));
                    for c in &capes {
                        menu = menu.item(
                            PopupMenuItem::new(c.label.clone())
                                .checked(cape.as_ref() == Some(&c.path))
                                .on_click(pick(Some(c.path.clone()))),
                        );
                    }
                    menu
                })
        };
        let backdrop = self.backdrop;
        let backdrop_icon = |b: Backdrop| match b {
            Backdrop::None => IconName::Ban,
            Backdrop::Day => IconName::Sun,
            Backdrop::Night => IconName::Moon,
        };
        let backdrop_menu = {
            let view = view.clone();
            Button::new("viewer-backdrop")
                .xsmall()
                .ghost()
                .icon(backdrop_icon(backdrop))
                .tooltip("Background")
                .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                    for b in Backdrop::ALL {
                        let view = view.clone();
                        menu = menu.item(
                            PopupMenuItem::new(b.label())
                                .icon(Icon::new(backdrop_icon(b)))
                                .checked(b == backdrop)
                                .on_click(move |_, _, cx| {
                                    view.update(cx, |v, cx| {
                                        v.backdrop = b;
                                        cx.notify();
                                    })
                                    .ok();
                                }),
                        );
                    }
                    menu
                })
        };
        h_flex()
            .id("viewer-toolbar")
            .gap_0p5()
            .p_1()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background.opacity(0.88))
            .shadow_md()
            // Clicks here shouldn't start turning the player.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(cape_menu)
            .child(
                Button::new("viewer-elytra")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Feather)
                    .selected(self.elytra)
                    .tooltip(if self.elytra { "Hide the elytra" } else { "Show an elytra" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.elytra = !this.elytra;
                        cx.notify();
                    })),
            )
            .child(backdrop_menu)
            .child(
                Button::new("viewer-pause")
                    .xsmall()
                    .ghost()
                    .icon(if self.paused { IconName::Play } else { IconName::Pause })
                    .tooltip(if self.paused { "Play the animation" } else { "Pause the animation" })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.paused = !this.paused;
                        this.last = None;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("viewer-reset")
                    .xsmall()
                    .ghost()
                    .icon(IconName::Shrink)
                    .tooltip("Reset the view")
                    .on_click(cx.listener(|this, _, _, cx| this.reset_view(cx))),
            )
            .child(
                Button::new("viewer-save")
                    .xsmall()
                    .ghost()
                    .icon(IconName::ImageDown)
                    .tooltip("Save as PNG")
                    .on_click(cx.listener(|this, _, window, cx| this.save_png(window, cx))),
            )
    }
}

impl Render for SkinViewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let now = Instant::now();
        if !self.paused {
            // A long gap (the page was hidden) doesn't jump ahead.
            let dt = self.last.map_or(0.0, |l| (now - l).as_secs_f32().min(0.1));
            self.time += dt;
            self.last = Some(now);
        }
        let easing = self.ease(now);
        let textures = self.scene_textures();
        let animating = textures.is_some() && (!self.paused && self.pose.animated() || easing);
        if (animating && !cx.reduce_motion() || easing) && self.tick.is_none() {
            // A little early: the redraw then waits for the display's next
            // refresh, which lands it close to the frame rate.
            let next = std::time::Duration::from_secs_f32(1.0 / FPS - 0.004);
            self.tick = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(next).await;
                this.update(cx, |this, cx| {
                    this.tick = None;
                    cx.notify();
                })
                .ok();
            }));
        }

        let (slim, pose, time, cam, backdrop) = (self.slim, self.pose, self.time, self.cam, self.backdrop);
        let dragging = self.drag.is_some();
        let frames = self.frames.clone();
        let view = cx.entity().downgrade();
        let picture = textures.map(|(skin, cape, elytra)| {
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    let scale = window.scale_factor();
                    let w = (f32::from(bounds.size.width) * scale).round() as u32;
                    let h = (f32::from(bounds.size.height) * scale).round() as u32;
                    if w == 0 || h == 0 {
                        return;
                    }
                    let ptr = |t: Option<&Arc<RgbaImage>>| t.map_or(0, |t| Arc::as_ptr(t) as usize);
                    let key = FrameKey {
                        w,
                        h,
                        textures: [ptr(Some(&skin)), ptr(cape.as_ref()), ptr(elytra.as_ref())],
                        slim,
                        pose,
                        time: time.to_bits(),
                        cam: [cam.yaw.to_bits(), cam.pitch.to_bits(), cam.zoom.to_bits()],
                        backdrop,
                    };
                    let mut f = frames.borrow_mut();
                    if f.key.as_ref() != Some(&key) || f.image.is_none() {
                        // The sky wraps around once per turn: twice the view wide.
                        let sky_key = (backdrop, w * 2, h);
                        if f.sky.as_ref().map(|s| s.0) != Some(sky_key) {
                            f.sky = skin3d::sky(backdrop, w * 2, h).map(|s| (sky_key, Arc::new(s)));
                        }
                        let sky = f.sky.as_ref().map(|s| s.1.clone());
                        let scene = Scene {
                            skin: &skin,
                            slim,
                            cape: cape.as_deref(),
                            elytra: elytra.as_deref(),
                            pose,
                            time,
                        };
                        let bytes = f.renderer.render(
                            &scene,
                            &cam,
                            w,
                            h,
                            SUPERSAMPLE,
                            sky.as_deref(),
                            time * 0.004,
                            Order::Bgra,
                        );
                        let Some(buf) = RgbaImage::from_raw(w, h, bytes) else { return };
                        let img = RenderImage::new(vec![image::Frame::new(buf)]);
                        if let Some(old) = f.image.replace(Arc::new(img)) {
                            window.drop_image(old).ok();
                        }
                        f.key = Some(key);
                    }
                    if let Some(img) = f.image.clone() {
                        window
                            .paint_image(bounds, bounds, Corners::default(), img, 0, false)
                            .ok();
                    }
                    if dragging {
                        // Follow the mouse outside the preview too.
                        let v = view.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                let held = e.pressed_button == Some(MouseButton::Left);
                                v.update(cx, |this, cx| this.drag_to(e.position, held, cx)).ok();
                            }
                        });
                        let v = view.clone();
                        window.on_mouse_event(move |e: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && e.button == MouseButton::Left {
                                v.update(cx, |this, cx| this.drag_to(e.position, false, cx)).ok();
                            }
                        });
                    }
                },
            )
            .size_full()
        });
        let ring = theme.ring;
        div()
            .id("skin-viewer")
            .relative()
            .size_full()
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.5))
            .when(!dragging, |d| d.cursor_grab())
            .when(dragging, |d| d.cursor_grabbing())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, _, cx| {
                    this.drag = Some(e.position);
                    this.easing = None;
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    this.drag_to(e.position, e.pressed_button == Some(MouseButton::Left), cx);
                }
            }))
            .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, window, cx| {
                let dy = f32::from(e.delta.pixel_delta(window.line_height()).y);
                this.cam.scroll(dy / 60.0);
                this.easing = None;
                cx.stop_propagation();
                cx.notify();
            }))
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.border_2().border_color(ring))
            .on_drop(cx.listener(|_, paths: &ExternalPaths, _, cx| {
                cx.stop_propagation();
                if let Some(p) = paths.paths().first() {
                    cx.emit(ViewerEvent::Dropped(p.clone()));
                }
            }))
            .map(|d| match picture {
                Some(c) => d.child(c),
                None => d.child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Spinner::new()),
                ),
            })
            .child(
                h_flex()
                    .absolute()
                    .top_2()
                    .left_0()
                    .right_0()
                    .justify_center()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                        h_flex()
                            .gap_1p5()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(theme.background.opacity(0.6))
                            .child(Icon::new(IconName::Move).xsmall())
                            .child("Drag to turn")
                            .child(div().w_px().h_3().bg(theme.border))
                            .child(Icon::new(IconName::ChevronsUpDown).xsmall())
                            .child("Scroll to zoom"),
                    ),
            )
            .child(
                h_flex()
                    .absolute()
                    .bottom_2()
                    .left_0()
                    .right_0()
                    .justify_center()
                    .child(self.toolbar(cx)),
            )
    }
}
