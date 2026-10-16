//! Remote images (mod and pack icons) cached on disk, so a UI can show them
//! from a local file.

use std::path::{Path, PathBuf};

fn dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Hopper").map(|d| d.cache_dir().join("images"))
}

/// Where `url` is (or will be) cached. Local paths and `file://` URLs map to
/// themselves.
pub fn path_for(url: &str) -> Option<PathBuf> {
    if let Some(p) = url.strip_prefix("file:///").or_else(|| url.strip_prefix("file://")) {
        return Some(PathBuf::from(p));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return (!url.is_empty()).then(|| PathBuf::from(url));
    }
    let ext = url
        .rsplit('/')
        .next()
        .and_then(|n| n.split('?').next())
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()))
        .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"))
        .unwrap_or_else(|| "png".into());
    let hash = sha1_smol::Sha1::from(url).digest().to_string();
    Some(dir()?.join(format!("{hash}.{ext}")))
}

/// An image key as the URL to fetch and the width to show it at: a
/// description's `<img width=156>` is cached as `url#w=156`.
pub fn split_width(key: &str) -> (&str, Option<u32>) {
    match key.rsplit_once("#w=") {
        Some((base, w)) => match w.parse() {
            Ok(w) => (base, Some(w)),
            Err(_) => (key, None),
        },
        None => (key, None),
    }
}

/// `key` as [`split_width`] makes it.
pub fn width_key(url: &str, width: Option<u32>) -> String {
    match width {
        Some(w) => format!("{url}#w={w}"),
        None => url.to_string(),
    }
}

/// A copy of the local image `src` scaled down to `width` pixels wide
/// (PNG, so transparency survives), or `src` itself when it is no wider.
/// Made once per file version and kept in the cache.
pub fn resized(src: &Path, width: u32) -> anyhow::Result<PathBuf> {
    let meta = std::fs::metadata(src)?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let key = format!("{}|{}|{stamp}|w{width}", src.display(), meta.len());
    let hash = sha1_smol::Sha1::from(key).digest().to_string();
    let out = dir()
        .ok_or_else(|| anyhow::anyhow!("no cache folder"))?
        .join("thumbs")
        .join(format!("{hash}.png"));
    if out.is_file() {
        return Ok(out);
    }
    let (w, _) = image::image_dimensions(src)?;
    if w <= width {
        return Ok(src.to_path_buf());
    }
    let img = image::open(src)?.thumbnail(width, u32::MAX);
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    write_atomic(&out, &bytes)?;
    Ok(out)
}

/// The cached file for `url`, when it is already on disk.
pub fn cached(url: &str) -> Option<PathBuf> {
    path_for(url).filter(|p| p.is_file())
}

/// Downloads `url` into the cache (blocking) and returns the file.
pub fn fetch(url: &str) -> anyhow::Result<PathBuf> {
    let path = path_for(url).ok_or_else(|| anyhow::anyhow!("no cache folder"))?;
    if path.is_file() {
        return Ok(path);
    }
    anyhow::ensure!(url.starts_with("http"), "{} is missing", path.display());
    let bytes = crate::cf::agent()
        .get(url)
        .call()?
        .body_mut()
        .with_config()
        .limit(16 << 20)
        .read_to_vec()?;
    write_atomic(&path, &bytes)?;
    Ok(path)
}

/// A copy of the local image `src` no wider or taller than `max` pixels,
/// made once (per file version) and kept in the cache.
pub fn thumbnail(src: &Path, max: u32) -> anyhow::Result<PathBuf> {
    let meta = std::fs::metadata(src)?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let key = format!("{}|{}|{stamp}|{max}", src.display(), meta.len());
    let hash = sha1_smol::Sha1::from(key).digest().to_string();
    let out = dir()
        .ok_or_else(|| anyhow::anyhow!("no cache folder"))?
        .join("thumbs")
        .join(format!("{hash}.jpg"));
    if out.is_file() {
        return Ok(out);
    }
    let img = image::open(src)?.thumbnail(max, max).to_rgb8();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 85).encode_image(&img)?;
    write_atomic(&out, &bytes)?;
    Ok(out)
}

/// A soft, heavily blurred backdrop made from the image `src`, made once
/// (per file version) and kept in the cache. The blur runs on a small copy
/// (cheap), then is scaled up smoothly so the UI can stretch it over a wide
/// header without blocky edges.
pub fn blurred(src: &Path) -> anyhow::Result<PathBuf> {
    let meta = std::fs::metadata(src)?;
    let stamp = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let key = format!("blur2|{}|{}|{stamp}", src.display(), meta.len());
    let hash = sha1_smol::Sha1::from(key).digest().to_string();
    let out = dir()
        .ok_or_else(|| anyhow::anyhow!("no cache folder"))?
        .join("blur")
        .join(format!("{hash}.jpg"));
    if out.is_file() {
        return Ok(out);
    }
    let img = blur_image(&image::open(src)?);
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 88).encode_image(&img)?;
    write_atomic(&out, &bytes)?;
    Ok(out)
}

/// The blur itself: down to 48px, a wide gaussian, a little extra
/// saturation (blurring greys colors out), then up to 256px.
fn blur_image(img: &image::DynamicImage) -> image::RgbImage {
    use image::imageops::FilterType;
    // Flatten transparency onto black, so see-through icons don't blur
    // into a white haze.
    let small = img.resize_exact(48, 48, FilterType::Triangle).to_rgba8();
    let mut flat = image::RgbImage::new(48, 48);
    for (x, y, p) in small.enumerate_pixels() {
        let a = p[3] as f32 / 255.0;
        flat.put_pixel(
            x,
            y,
            image::Rgb([
                (p[0] as f32 * a) as u8,
                (p[1] as f32 * a) as u8,
                (p[2] as f32 * a) as u8,
            ]),
        );
    }
    let mut soft = image::imageops::blur(&flat, 5.0);
    for p in soft.pixels_mut() {
        let [r, g, b] = p.0.map(|c| c as f32);
        let grey = 0.299 * r + 0.587 * g + 0.114 * b;
        let sat = |c: f32| (grey + (c - grey) * 1.35).clamp(0.0, 255.0) as u8;
        p.0 = [sat(r), sat(g), sat(b)];
    }
    image::imageops::resize(&soft, 256, 256, FilterType::Triangle)
}

/// Whether `path` may hold several frames (GIF, WebP).
pub fn maybe_animated(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("gif") || e.eq_ignore_ascii_case("webp"))
}

/// The first frame of a cached GIF or WebP as a PNG next to it, made once.
/// An animated icon makes a UI redraw every frame for as long as it shows;
/// a still one costs nothing. Other images come back as they are.
pub fn still(path: &Path) -> anyhow::Result<PathBuf> {
    if !maybe_animated(path) {
        return Ok(path.to_path_buf());
    }
    let out = path.with_extension("still.png");
    if out.is_file() {
        return Ok(out);
    }
    let img = image::open(path)?;
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    write_atomic(&out, &bytes)?;
    Ok(out)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_paths_map_to_themselves() {
        assert_eq!(path_for(r"C:\x\icon.png"), Some(PathBuf::from(r"C:\x\icon.png")));
        assert_eq!(path_for("file:///C:/x/i.png"), Some(PathBuf::from("C:/x/i.png")));
        assert_eq!(path_for(""), None);
    }

    #[test]
    fn gifs_become_a_still_png() {
        let dir = std::env::temp_dir().join(format!("hopper-still-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gif = dir.join("icon.gif");
        let frame = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        image::DynamicImage::ImageRgba8(frame).save(&gif).unwrap();
        let png = still(&gif).unwrap();
        assert_eq!(png, dir.join("icon.still.png"));
        assert_eq!(image::open(&png).unwrap().width(), 4);
        let jpg = dir.join("x.jpg");
        assert_eq!(still(&jpg).unwrap(), jpg);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn blur_keeps_the_average_color_and_softens_edges() {
        // Left half red, right half blue.
        let src = image::RgbaImage::from_fn(64, 64, |x, _| {
            if x < 32 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let out = blur_image(&image::DynamicImage::ImageRgba8(src));
        assert_eq!(out.dimensions(), (256, 256));
        let left = out.get_pixel(8, 128);
        let mid = out.get_pixel(128, 128);
        assert!(left[0] > 200 && left[2] < 60, "{left:?}");
        // The seam is a blend, not a hard edge.
        assert!(mid[0] > 40 && mid[2] > 40, "{mid:?}");
    }

    #[test]
    fn remote_urls_keep_their_extension() {
        let p = path_for("https://media.forgecdn.net/avatars/1/2/thumb.webp?x=1").unwrap();
        assert_eq!(p.extension().unwrap(), "webp");
        let q = path_for("https://cdn.modrinth.com/data/abc/icon").unwrap();
        assert_eq!(q.extension().unwrap(), "png");
    }
}
