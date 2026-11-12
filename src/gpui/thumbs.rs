//! Pictures (icons, Browse results, banners, gallery shots, the viewer's
//! image, server art) decoded at the size they are shown, and kept to a
//! memory budget.
//!
//! GPUI's `img(path)` decodes the whole file and keeps the decoded copy
//! (and its GPU copy) for as long as the app runs. A 256 px CurseForge
//! icon shown 40 px wide costs 256 KB of heap and as much video memory;
//! a 1,100-mod list that has been scrolled once holds about 280 MB of
//! them. [`source`] gives `img()` an image from this cache instead: decoded
//! no wider than the icon needs (see [`width_for`]) and, past
//! [`BUDGET`], the icons used longest ago are dropped (and their GPU copies
//! with `window.drop_image`). A dropped icon that is drawn again decodes
//! again from the file on a thread.

use gpui_kit::*;
use image::imageops::FilterType;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Decoded icons are kept up to this many bytes (a 128 px icon is 64 KB, so
/// about 1,500 of them).
pub const BUDGET: usize = 96 << 20;

/// Icons used within this long are never dropped: they are on screen, or were
/// a moment ago (a view GPUI reuses without drawing it again isn't asking).
const GRACE: Duration = Duration::from_secs(10);

/// The width to decode a picture shown `px` device pixels wide: `headroom`
/// times that (the GPU's own downscale from twice the size looks as crisp as
/// from the full file), in steps of 32 so a resized window or a zoom change
/// asks for few different sizes.
pub fn width_for_in(px: f32, headroom: f32) -> u32 {
    let want = (px.max(1.0) * headroom).ceil() as u32;
    want.div_ceil(32) * 32
}

/// [`width_for_in`] at the usual 2x.
#[cfg(test)]
pub fn width_for(px: f32) -> u32 {
    width_for_in(px, 2.0)
}

/// What a picture is for: they are counted (and the viewer's released) apart.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Cat {
    /// Mod, pack and instance icons in lists.
    Icons,
    /// Browse's result cards and category icons.
    Browse,
    /// An instance's or server's header art and its backdrop.
    Banner,
    /// The small screenshots under a mod's description.
    Gallery,
    /// The image viewer's picture; let go when it closes.
    Viewer,
    /// A server's picture in lists and its header.
    ServerArt,
}

impl Cat {
    pub const ALL: [Cat; 6] = [Cat::Icons, Cat::Browse, Cat::Banner, Cat::Gallery, Cat::Viewer, Cat::ServerArt];

    pub fn name(self) -> &'static str {
        match self {
            Cat::Icons => "icons",
            Cat::Browse => "browse",
            Cat::Banner => "banners",
            Cat::Gallery => "gallery",
            Cat::Viewer => "viewer",
            Cat::ServerArt => "server_art",
        }
    }

    /// Pictures shown close to their own size need no spare resolution;
    /// small ones are decoded at twice.
    fn headroom(self) -> f32 {
        match self {
            Cat::Viewer => 1.0,
            Cat::Banner | Cat::Gallery => 1.5,
            _ => 2.0,
        }
    }
}

/// How big a picture is shown.
#[derive(Clone, Copy)]
pub enum Want {
    /// Its width, in rems.
    Rems(Rems),
    /// Its width in logical pixels.
    Px(f32),
    /// As wide as the window.
    Window,
}

/// What is kept and how big it is; chooses what goes when it grows too big.
/// (Separate from the images so it can be tested.)
#[derive(Default)]
pub struct Lru {
    items: HashMap<(PathBuf, u32), (usize, Instant)>,
    bytes: usize,
}

impl Lru {
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Notes that `key` (of `size` bytes) is in use at `now`.
    pub fn touch(&mut self, key: &(PathBuf, u32), size: usize, now: Instant) {
        match self.items.get_mut(key) {
            Some(e) => e.1 = now,
            None => {
                self.bytes += size;
                self.items.insert(key.clone(), (size, now));
            }
        }
    }

    /// Forgets `key`.
    pub fn remove(&mut self, key: &(PathBuf, u32)) {
        if let Some((size, _)) = self.items.remove(key) {
            self.bytes -= size;
        }
    }

    /// The keys to drop so that at most `budget` bytes remain: the ones used
    /// longest ago first, none used within `grace` of `now`. They are
    /// forgotten here.
    pub fn evict(&mut self, budget: usize, now: Instant, grace: Duration) -> Vec<(PathBuf, u32)> {
        if self.bytes <= budget {
            return Vec::new();
        }
        let mut old: Vec<_> = self
            .items
            .iter()
            .filter(|(_, (_, at))| now.saturating_duration_since(*at) >= grace)
            .map(|(k, (size, at))| (*at, k.clone(), *size))
            .collect();
        old.sort_by_key(|(at, _, _)| *at);
        let mut gone = Vec::new();
        for (_, key, size) in old {
            if self.bytes <= budget {
                break;
            }
            self.items.remove(&key);
            self.bytes -= size;
            gone.push(key);
        }
        gone
    }
}

enum State {
    Loading,
    Ready(Arc<RenderImage>, Cat),
    Failed,
}

/// The icons, by file and decoded width. One for the whole app (see [`init`]).
pub struct Thumbs {
    shown: HashMap<(PathBuf, u32), State>,
    lru: Lru,
    me: WeakEntity<Thumbs>,
}

struct Holder(Entity<Thumbs>);

impl gpui_kit::Global for Holder {}

/// Makes the app's icon cache.
pub fn init(cx: &mut App) {
    let e = cx.new(|cx| Thumbs { shown: HashMap::new(), lru: Lru::default(), me: cx.entity().downgrade() });
    cx.set_global(Holder(e));
}

/// `path` decoded no wider than `width` (never scaled up), as the BGRA
/// pixels GPUI draws.
pub fn decode(path: &Path, width: u32) -> anyhow::Result<RenderImage> {
    let mut buf = image::ImageReader::open(path)?.with_guessed_format()?.decode()?.into_rgba8();
    if buf.width() > width {
        let h = (buf.height() as u64 * width as u64 / buf.width() as u64).max(1) as u32;
        buf = image::imageops::resize(&buf, width, h, FilterType::CatmullRom);
    }
    for px in buf.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok(RenderImage::new(vec![image::Frame::new(buf)]))
}

fn bytes_of(img: &RenderImage) -> usize {
    let s = img.size(0);
    s.width.0 as usize * s.height.0 as usize * 4
}

impl Thumbs {
    fn load(&mut self, cat: Cat, path: &Path, width: u32, window: &mut Window, cx: &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let key = (path.to_path_buf(), width);
        let now = Instant::now();
        match self.shown.get(&key) {
            Some(State::Ready(img, _)) => {
                let img = img.clone();
                self.lru.touch(&key, bytes_of(&img), now);
                self.trim(now, window, cx);
                Some(Ok(img))
            }
            Some(State::Loading) => None,
            Some(State::Failed) => Some(Err(ImageCacheError::Asset("icon can't be read".into()))),
            None => {
                self.shown.insert(key.clone(), State::Loading);
                let (view, me, path) = (window.current_view(), self.me.clone(), key.0.clone());
                cx.spawn(async move |cx| {
                    let decoded = cx.background_spawn(async move { decode(&path, width) }).await;
                    me.update(cx, |this, cx| {
                        let state = match decoded {
                            Ok(img) => State::Ready(Arc::new(img), cat),
                            Err(e) => {
                                log::debug!("icon {}: {e:#}", key.0.display());
                                State::Failed
                            }
                        };
                        this.shown.insert(key, state);
                        // The view that asked draws it now.
                        App::notify(cx, view);
                    })
                    .ok();
                })
                .detach();
                None
            }
        }
    }

    fn trim(&mut self, now: Instant, window: &mut Window, cx: &mut App) {
        for key in self.lru.evict(BUDGET, now, GRACE) {
            if let Some(State::Ready(img, _)) = self.shown.remove(&key) {
                cx.drop_image(img, Some(window));
            }
        }
    }
}

/// An image source for `img()`: the picture at `path`, of kind `cat`, decoded
/// for a display `want` wide at the window's scale. Plain `path` when there
/// is no cache. Animated files (GIF, WebP) are left to GPUI: they are
/// played, not shown still.
pub fn source_in(cat: Cat, path: &Path, want: Want, cx: &App) -> ImageSource {
    let plain = || ImageSource::Resource(Resource::Path(path.into()));
    let Some(cache) = cx.try_global::<Holder>().map(|g| g.0.clone()) else {
        return plain();
    };
    // GIFs and SVGs aren't decoded here: the first plays, the second is
    // drawn by GPUI itself (the decoder below only reads raster images).
    if hopper::imgcache::maybe_animated(path) || hopper::imgcache::is_svg(path) {
        return plain();
    }
    let path: PathBuf = path.to_path_buf();
    ImageSource::Custom(Arc::new(move |window, cx| {
        let scale = window.scale_factor();
        let px = match want {
            Want::Rems(r) => f32::from(r.to_pixels(window.rem_size())) * scale,
            Want::Px(p) => p * scale,
            Want::Window => f32::from(window.viewport_size().width) * scale,
        };
        let width = width_for_in(px, cat.headroom());
        cache.update(cx, |c, cx| c.load(cat, &path, width, window, cx))
    }))
}

/// Lets go of every picture of `cat` (and its GPU copy): the viewer closed.
pub fn release(cat: Cat, cx: &mut App) {
    let Some(cache) = cx.try_global::<Holder>().map(|g| g.0.clone()) else { return };
    let gone: Vec<Arc<RenderImage>> = cache.update(cx, |c, _| {
        let keys: Vec<_> = c
            .shown
            .iter()
            .filter(|(_, s)| matches!(s, State::Ready(_, k) if *k == cat))
            .map(|(k, _)| k.clone())
            .collect();
        keys.into_iter()
            .filter_map(|k| {
                c.lru.remove(&k);
                match c.shown.remove(&k) {
                    Some(State::Ready(img, _)) => Some(img),
                    _ => None,
                }
            })
            .collect()
    });
    for img in gone {
        cx.drop_image(img, None);
    }
}

/// How many pictures of each [`Cat`] are held, and their decoded bytes.
pub fn stats(cx: &App) -> Vec<(Cat, usize, usize)> {
    let mut out: Vec<(Cat, usize, usize)> = Cat::ALL.iter().map(|c| (*c, 0, 0)).collect();
    if let Some(h) = cx.try_global::<Holder>() {
        for s in h.0.read(cx).shown.values() {
            if let State::Ready(img, cat) = s {
                let row = out.iter_mut().find(|r| r.0 == *cat).expect("every Cat is listed");
                row.1 += 1;
                row.2 += bytes_of(img);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's own `test` macro would shadow #[test].
    use super::{GRACE, Lru, bytes_of, decode, width_for, width_for_in};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn key(n: u32) -> (PathBuf, u32) {
        (PathBuf::from(format!("{n}.png")), 128)
    }

    #[test]
    fn width_steps_and_headroom() {
        assert_eq!(width_for(40.0), 96);
        assert_eq!(width_for(50.0), 128);
        assert_eq!(width_for(75.0), 160);
        assert_eq!(width_for(0.0), 32);
    }

    #[test]
    fn headroom_by_kind() {
        // A viewer picture is decoded at the size it is shown, an icon at twice.
        assert_eq!(width_for_in(1000.0, 1.0), 1024);
        assert_eq!(width_for_in(1000.0, 2.0), 2016);
        assert_eq!(width_for_in(200.0, 1.5), 320);
        assert_eq!(width_for_in(0.0, 1.0), 32);
    }

    #[test]
    fn removed_keys_free_their_bytes() {
        let mut l = Lru::default();
        let t = Instant::now();
        l.touch(&key(1), 100, t);
        l.touch(&key(2), 50, t);
        l.remove(&key(1));
        l.remove(&key(9));
        assert_eq!((l.len(), l.bytes()), (1, 50));
    }

    #[test]
    fn nothing_goes_under_the_budget() {
        let mut l = Lru::default();
        let t = Instant::now();
        for n in 0..10 {
            l.touch(&key(n), 100, t);
        }
        assert!(l.evict(1000, t + Duration::from_secs(60), GRACE).is_empty());
        assert_eq!((l.len(), l.bytes()), (10, 1000));
    }

    #[test]
    fn oldest_go_first_down_to_the_budget() {
        let mut l = Lru::default();
        let t = Instant::now();
        for n in 0..10 {
            l.touch(&key(n), 100, t + Duration::from_secs(n as u64));
        }
        // Icon 0 is touched again: it is now the newest.
        l.touch(&key(0), 100, t + Duration::from_secs(20));
        let gone = l.evict(600, t + Duration::from_secs(60), GRACE);
        assert_eq!(gone, vec![key(1), key(2), key(3), key(4)]);
        assert_eq!((l.len(), l.bytes()), (6, 600));
    }

    #[test]
    fn icons_used_a_moment_ago_stay_even_over_budget() {
        let mut l = Lru::default();
        let t = Instant::now();
        for n in 0..10 {
            l.touch(&key(n), 100, t);
        }
        // All used just now (on screen): none may go, however far over.
        assert!(l.evict(100, t + Duration::from_secs(1), GRACE).is_empty());
        assert_eq!(l.len(), 10);
        // Later they may.
        assert_eq!(l.evict(100, t + Duration::from_secs(30), GRACE).len(), 9);
    }

    #[test]
    fn decodes_no_wider_than_asked_and_never_scales_up() {
        let dir = std::env::temp_dir().join(format!("hopper-thumbs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        image::RgbaImage::from_pixel(256, 256, image::Rgba([10, 20, 30, 255])).save(&p).unwrap();
        let small = decode(&p, 96).unwrap();
        assert_eq!(bytes_of(&small), 96 * 96 * 4);
        let whole = decode(&p, 512).unwrap();
        assert_eq!(bytes_of(&whole), 256 * 256 * 4);
        std::fs::remove_dir_all(&dir).ok();
    }
}
