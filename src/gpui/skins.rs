//! Skin and cape textures drawn as crisp images (heads, the player front
//! or back, capes), made once per texture file and kept for the session.

use gpui_kit::*;
use hopper::skinrender;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What to draw from a texture.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum View {
    /// The face with the hat layer.
    Head,
    /// The whole player; from behind it wears `cape` (a cape texture file).
    Body {
        slim: bool,
        back: bool,
        cape: Option<PathBuf>,
    },
    /// A cape's outside.
    Cape,
}

thread_local! {
    static MADE: RefCell<HashMap<(PathBuf, View), Option<Arc<Image>>>> = RefCell::new(HashMap::new());
}

/// The drawn image for the texture at `path`, or None when it can't be read.
pub fn image(path: &Path, view: View) -> Option<Arc<Image>> {
    let key = (path.to_path_buf(), view);
    if let Some(hit) = MADE.with(|m| m.borrow().get(&key).cloned()) {
        return hit;
    }
    let made = draw(&key.0, &key.1);
    if let Err(e) = &made {
        log::debug!("skin {}: {e:#}", key.0.display());
    }
    let img = made
        .ok()
        .map(|png| Arc::new(Image::from_bytes(ImageFormat::Png, png)));
    MADE.with(|m| m.borrow_mut().insert(key, img.clone()));
    img
}

fn draw(path: &Path, view: &View) -> anyhow::Result<Vec<u8>> {
    let tex = image::open(path)?.to_rgba8();
    let out = match view {
        View::Head => skinrender::upscale_to(&skinrender::head(&tex), 64),
        View::Cape => skinrender::upscale_to(&skinrender::cape(&tex), 256),
        View::Body { slim, back, cape } => {
            let mut body = skinrender::body(&tex, *slim, *back);
            if *back && let Some(c) = cape {
                let cape = skinrender::cape(&image::open(c)?.to_rgba8());
                // At the body's scale, between the shoulders.
                let s = body.width() / 16;
                let cape = image::imageops::resize(
                    &cape,
                    10 * s,
                    16 * s,
                    image::imageops::FilterType::Nearest,
                );
                image::imageops::overlay(&mut body, &cape, i64::from(3 * s), i64::from(8 * s));
            }
            skinrender::upscale_to(&body, 512)
        }
    };
    Ok(skinrender::png_bytes(&out))
}
