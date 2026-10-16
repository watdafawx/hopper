//! Flat previews of Minecraft skin and cape textures: the player seen from
//! the front or back, a head avatar, and a cape. Pixel art is scaled up
//! without smoothing so it stays crisp.

use image::{Rgba, RgbaImage, imageops};

/// A rectangle in skin texture pixels (of a 64 wide texture).
#[derive(Clone, Copy)]
struct Rect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

const fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
    Rect { x, y, w, h }
}

/// One body part's faces: base and overlay, front and back.
struct Part {
    front: Rect,
    back: Rect,
    front_overlay: Option<Rect>,
    back_overlay: Option<Rect>,
}

/// How many texture pixels per skin pixel (2 for a 128 wide HD skin).
fn scale_of(skin: &RgbaImage) -> u32 {
    (skin.width() / 64).max(1)
}

/// A face cut out of the texture, at the texture's scale.
fn cut(skin: &RgbaImage, rect: Rect) -> RgbaImage {
    let s = scale_of(skin);
    let (x, y, w, h) = (rect.x * s, rect.y * s, rect.w * s, rect.h * s);
    if x + w > skin.width() || y + h > skin.height() {
        return RgbaImage::new(w, h);
    }
    imageops::crop_imm(skin, x, y, w, h).to_image()
}

/// Whether the texture is the old 64×32 layout (no overlays but the hat,
/// left limbs mirrored from the right ones).
fn is_legacy(skin: &RgbaImage) -> bool {
    skin.height() * 2 == skin.width()
}

/// Base layers are opaque in game: transparent pixels show as black.
fn opaque(mut img: RgbaImage) -> RgbaImage {
    for p in img.pixels_mut() {
        p.0[3] = 255;
    }
    img
}

/// Draws `layer` over `canvas` at skin pixel (x, y).
fn paste(canvas: &mut RgbaImage, layer: &RgbaImage, x: u32, y: u32, s: u32) {
    imageops::overlay(canvas, layer, i64::from(x * s), i64::from(y * s));
}

/// Old skins often fill the hat area with a solid colour; the game hides
/// it then, and so does this.
fn hat_is_solid(skin: &RgbaImage) -> bool {
    let s = scale_of(skin);
    let hat = imageops::crop_imm(skin, 32 * s, 0, 32 * s, 16 * s).to_image();
    hat.pixels().all(|p| p.0[3] == 255)
}

const HEAD: Part = Part {
    front: r(8, 8, 8, 8),
    back: r(24, 8, 8, 8),
    front_overlay: Some(r(40, 8, 8, 8)),
    back_overlay: Some(r(56, 8, 8, 8)),
};

const BODY: Part = Part {
    front: r(20, 20, 8, 12),
    back: r(32, 20, 8, 12),
    front_overlay: Some(r(20, 36, 8, 12)),
    back_overlay: Some(r(32, 36, 8, 12)),
};

const RIGHT_LEG: Part = Part {
    front: r(4, 20, 4, 12),
    back: r(12, 20, 4, 12),
    front_overlay: Some(r(4, 36, 4, 12)),
    back_overlay: Some(r(12, 36, 4, 12)),
};

const LEFT_LEG: Part = Part {
    front: r(20, 52, 4, 12),
    back: r(28, 52, 4, 12),
    front_overlay: Some(r(4, 52, 4, 12)),
    back_overlay: Some(r(12, 52, 4, 12)),
};

fn right_arm(slim: bool) -> Part {
    let w = if slim { 3 } else { 4 };
    Part {
        front: r(44, 20, w, 12),
        back: r(48 + w, 20, w, 12),
        front_overlay: Some(r(44, 36, w, 12)),
        back_overlay: Some(r(48 + w, 36, w, 12)),
    }
}

fn left_arm(slim: bool) -> Part {
    let w = if slim { 3 } else { 4 };
    Part {
        front: r(36, 52, w, 12),
        back: r(40 + w, 52, w, 12),
        front_overlay: Some(r(52, 52, w, 12)),
        back_overlay: Some(r(56 + w, 52, w, 12)),
    }
}

/// One face of a part with its overlay, as the game draws it.
fn face(skin: &RgbaImage, part: &Part, back: bool, overlay: bool) -> RgbaImage {
    let base = if back { part.back } else { part.front };
    let mut img = opaque(cut(skin, base));
    let over = if back { part.back_overlay } else { part.front_overlay };
    if overlay && let Some(o) = over {
        imageops::overlay(&mut img, &cut(skin, o), 0, 0);
    }
    img
}

/// The whole player, front or back, at 16×32 skin pixels (times the
/// texture's scale). The player's right arm and leg are on the viewer's
/// left from the front, and on the right from the back.
pub fn body(skin: &RgbaImage, slim: bool, back: bool) -> RgbaImage {
    let s = scale_of(skin);
    let legacy = is_legacy(skin);
    let mut canvas = RgbaImage::new(16 * s, 32 * s);
    let aw = if slim { 3 } else { 4 };

    let head = {
        let base = opaque(cut(skin, if back { HEAD.back } else { HEAD.front }));
        let mut img = base;
        if !(legacy && hat_is_solid(skin)) {
            let hat = if back { HEAD.back_overlay } else { HEAD.front_overlay };
            if let Some(h) = hat {
                imageops::overlay(&mut img, &cut(skin, h), 0, 0);
            }
        }
        img
    };
    let torso = face(skin, &BODY, back, !legacy);
    let r_arm = face(skin, &right_arm(slim), back, !legacy);
    let r_leg = face(skin, &RIGHT_LEG, back, !legacy);
    // Old skins have one arm and one leg; the left ones are mirrors.
    let (l_arm, l_leg) = if legacy {
        (imageops::flip_horizontal(&r_arm), imageops::flip_horizontal(&r_leg))
    } else {
        (
            face(skin, &left_arm(slim), back, true),
            face(skin, &LEFT_LEG, back, true),
        )
    };
    let (left_side_arm, right_side_arm, left_side_leg, right_side_leg) = if back {
        (&l_arm, &r_arm, &l_leg, &r_leg)
    } else {
        (&r_arm, &l_arm, &r_leg, &l_leg)
    };
    paste(&mut canvas, &head, 4, 0, s);
    paste(&mut canvas, &torso, 4, 8, s);
    paste(&mut canvas, left_side_arm, 4 - aw, 8, s);
    paste(&mut canvas, right_side_arm, 12, 8, s);
    paste(&mut canvas, left_side_leg, 4, 20, s);
    paste(&mut canvas, right_side_leg, 8, 20, s);
    canvas
}

/// The face with the hat layer, 8×8 skin pixels.
pub fn head(skin: &RgbaImage) -> RgbaImage {
    let mut img = opaque(cut(skin, HEAD.front));
    if !(is_legacy(skin) && hat_is_solid(skin)) {
        imageops::overlay(&mut img, &cut(skin, r(40, 8, 8, 8)), 0, 0);
    }
    img
}

/// The outside of a cape (what others see from behind), 10×16 cape pixels.
/// Cape textures are 64×32 or a multiple (22×17 for very old ones).
pub fn cape(texture: &RgbaImage) -> RgbaImage {
    let s = if texture.width() >= 64 { texture.width() / 64 } else { 1 };
    let (x, y, w, h) = (s, s, 10 * s, 16 * s);
    if x + w > texture.width() || y + h > texture.height() {
        return RgbaImage::new(w, h);
    }
    imageops::crop_imm(texture, x, y, w, h).to_image()
}

/// Scales pixel art up by a whole factor, keeping hard edges.
pub fn upscale(img: &RgbaImage, factor: u32) -> RgbaImage {
    let f = factor.max(1);
    imageops::resize(img, img.width() * f, img.height() * f, imageops::FilterType::Nearest)
}

/// Scales up so the longer side is at least `target` pixels.
pub fn upscale_to(img: &RgbaImage, target: u32) -> RgbaImage {
    let long = img.width().max(img.height()).max(1);
    upscale(img, target.div_ceil(long))
}

/// A guess at the arm width of a 64×64 skin: slim skins leave the last two
/// columns of the right arm's texture empty.
pub fn looks_slim(skin: &RgbaImage) -> bool {
    if is_legacy(skin) || skin.width() < 64 {
        return false;
    }
    let s = scale_of(skin);
    (20 * s..32 * s).all(|y| {
        (54 * s..56 * s).all(|x| skin.get_pixel(x, y).0[3] == 0)
    })
}

pub fn png_bytes(img: &RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}

/// A simple made-up skin (64×64), for previews and tests: a different flat
/// colour per part so a renderer's mistakes show.
pub fn sample_skin(slim: bool, tint: [u8; 3]) -> RgbaImage {
    let mut skin = RgbaImage::new(64, 64);
    let fill = |img: &mut RgbaImage, rect: Rect, c: [u8; 4]| {
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                img.put_pixel(x, y, Rgba(c));
            }
        }
    };
    let [tr, tg, tb] = tint;
    let skin_tone = [198, 150, 112, 255];
    let hair = [72, 46, 28, 255];
    let shirt = [tr, tg, tb, 255];
    let pants = [52, 58, 120, 255];
    let shoes = [60, 60, 64, 255];
    // Head: all six faces skin, hair on top and back, a face on the front.
    fill(&mut skin, r(0, 0, 32, 16), skin_tone);
    fill(&mut skin, r(8, 0, 8, 8), hair);
    fill(&mut skin, r(24, 8, 8, 8), hair);
    fill(&mut skin, r(8, 8, 8, 2), hair);
    fill(&mut skin, r(9, 12, 2, 1), [255, 255, 255, 255]);
    fill(&mut skin, r(10, 12, 1, 1), [40, 60, 160, 255]);
    fill(&mut skin, r(13, 12, 2, 1), [255, 255, 255, 255]);
    fill(&mut skin, r(13, 12, 1, 1), [40, 60, 160, 255]);
    fill(&mut skin, r(11, 14, 2, 1), [140, 80, 70, 255]);
    // Body and arms (shirt with skin hands), legs.
    fill(&mut skin, r(16, 16, 24, 16), shirt);
    fill(&mut skin, r(40, 16, 16, 16), shirt);
    fill(&mut skin, r(32, 48, 16, 16), shirt);
    fill(&mut skin, r(44, 29, 8, 3), skin_tone);
    fill(&mut skin, r(36, 61, 8, 3), skin_tone);
    fill(&mut skin, r(0, 16, 16, 16), pants);
    fill(&mut skin, r(16, 48, 16, 16), pants);
    fill(&mut skin, r(4, 29, 8, 3), shoes);
    fill(&mut skin, r(20, 61, 8, 3), shoes);
    // A belt on the jacket layer.
    fill(&mut skin, r(20, 43, 8, 1), [30, 30, 30, 255]);
    if slim {
        // Slim arms leave the texture's last columns empty.
        fill(&mut skin, r(54, 20, 2, 12), [0, 0, 0, 0]);
        fill(&mut skin, r(46, 52, 2, 12), [0, 0, 0, 0]);
        fill(&mut skin, r(50, 16, 2, 4), [0, 0, 0, 0]);
    }
    skin
}

/// A made-up cape texture (64×32) in one colour with an edge.
pub fn sample_cape(color: [u8; 3]) -> RgbaImage {
    let mut img = RgbaImage::new(64, 32);
    let [cr, cg, cb] = color;
    for y in 0..17 {
        for x in 0..22 {
            let edge = x == 1 || x == 10 || y == 1 || y == 16 || x == 12 || x == 21;
            let c = if edge {
                [cr / 2, cg / 2, cb / 2, 255]
            } else {
                [cr, cg, cb, 255]
            };
            img.put_pixel(x, y, Rgba(c));
        }
    }
    // A mark in the middle of the outside.
    for y in 6..10 {
        for x in 5..7 {
            img.put_pixel(x, y, Rgba([255, 255, 255, 255]));
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(img: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
        img.get_pixel(x, y).0
    }

    /// A skin where every part's front and back has its own colour.
    fn marked() -> RgbaImage {
        let mut skin = RgbaImage::new(64, 64);
        let mut mark = |rect: Rect, c: [u8; 4]| {
            for y in rect.y..rect.y + rect.h {
                for x in rect.x..rect.x + rect.w {
                    skin.put_pixel(x, y, Rgba(c));
                }
            }
        };
        mark(HEAD.front, [1, 0, 0, 255]);
        mark(HEAD.back, [2, 0, 0, 255]);
        mark(BODY.front, [3, 0, 0, 255]);
        mark(BODY.back, [4, 0, 0, 255]);
        mark(right_arm(false).front, [5, 0, 0, 255]);
        mark(right_arm(false).back, [6, 0, 0, 255]);
        mark(left_arm(false).front, [7, 0, 0, 255]);
        mark(left_arm(false).back, [8, 0, 0, 255]);
        mark(RIGHT_LEG.front, [9, 0, 0, 255]);
        mark(RIGHT_LEG.back, [10, 0, 0, 255]);
        mark(LEFT_LEG.front, [11, 0, 0, 255]);
        mark(LEFT_LEG.back, [12, 0, 0, 255]);
        // A hat pixel in the corner of the face.
        skin.put_pixel(40, 8, Rgba([99, 0, 0, 255]));
        skin
    }

    #[test]
    fn front_view_places_parts() {
        let v = body(&marked(), false, false);
        assert_eq!((v.width(), v.height()), (16, 32));
        assert_eq!(px(&v, 4, 0), [99, 0, 0, 255], "hat over the face");
        assert_eq!(px(&v, 5, 0), [1, 0, 0, 255]);
        assert_eq!(px(&v, 8, 10), [3, 0, 0, 255]);
        assert_eq!(px(&v, 0, 10), [5, 0, 0, 255], "right arm on the viewer's left");
        assert_eq!(px(&v, 15, 10), [7, 0, 0, 255]);
        assert_eq!(px(&v, 4, 25), [9, 0, 0, 255]);
        assert_eq!(px(&v, 11, 25), [11, 0, 0, 255]);
    }

    #[test]
    fn back_view_swaps_sides() {
        let v = body(&marked(), false, true);
        assert_eq!(px(&v, 6, 4), [2, 0, 0, 255]);
        assert_eq!(px(&v, 8, 10), [4, 0, 0, 255]);
        assert_eq!(px(&v, 0, 10), [8, 0, 0, 255], "left arm on the viewer's left");
        assert_eq!(px(&v, 15, 10), [6, 0, 0, 255]);
        assert_eq!(px(&v, 4, 25), [12, 0, 0, 255]);
        assert_eq!(px(&v, 11, 25), [10, 0, 0, 255]);
    }

    #[test]
    fn slim_arms_are_three_wide() {
        let v = body(&sample_skin(true, [200, 40, 40]), true, false);
        assert_eq!(px(&v, 0, 10)[3], 0, "outer column stays empty");
        assert_eq!(px(&v, 1, 10)[3], 255);
        assert_eq!(px(&v, 14, 10)[3], 255);
        assert_eq!(px(&v, 15, 10)[3], 0);
        assert!(looks_slim(&sample_skin(true, [1, 2, 3])));
        assert!(!looks_slim(&sample_skin(false, [1, 2, 3])));
    }

    #[test]
    fn legacy_skins_mirror_and_hide_a_solid_hat() {
        let mut skin = RgbaImage::new(64, 32);
        for p in skin.pixels_mut() {
            *p = Rgba([0, 0, 0, 255]);
        }
        // Right arm front: a mark on its outer (x = 44) column.
        for y in 20..32 {
            skin.put_pixel(44, y, Rgba([50, 0, 0, 255]));
        }
        skin.put_pixel(9, 9, Rgba([77, 0, 0, 255]));
        let v = body(&skin, false, false);
        assert_eq!(px(&v, 0, 12), [50, 0, 0, 255]);
        assert_eq!(px(&v, 15, 12), [50, 0, 0, 255], "left arm mirrors the right");
        assert_eq!(px(&v, 5, 1), [77, 0, 0, 255], "solid black hat is hidden");
    }

    #[test]
    fn heads_capes_and_scaling() {
        let h = head(&marked());
        assert_eq!((h.width(), h.height()), (8, 8));
        assert_eq!(px(&h, 0, 0), [99, 0, 0, 255]);
        let c = cape(&sample_cape([10, 20, 30]));
        assert_eq!((c.width(), c.height()), (10, 16));
        let big = upscale(&h, 8);
        assert_eq!((big.width(), big.height()), (64, 64));
        assert_eq!(px(&big, 7, 7), [99, 0, 0, 255]);
        assert_eq!(px(&big, 8, 8), [1, 0, 0, 255], "hard edges");
        assert_eq!(upscale_to(&c, 100).height(), 112);
        // HD textures keep their detail.
        let hd = upscale(&marked(), 2);
        assert_eq!(body(&hd, false, false).width(), 32);
    }

    #[test]
    fn transparent_base_pixels_render_black() {
        let skin = RgbaImage::new(64, 64);
        let v = body(&skin, false, false);
        assert_eq!(px(&v, 8, 10), [0, 0, 0, 255]);
    }
}
