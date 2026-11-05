//! Remote images (mod and pack icons, galleries) cached on disk, so a UI can
//! show them from a local file. Picture URLs on CurseForge's and Modrinth's
//! CDNs never change what they serve, so a cached file is never asked
//! again; the cache is kept to [`MAX_BYTES`] instead (see [`prune`]).

use std::path::{Path, PathBuf};

fn dir() -> Option<PathBuf> {
    crate::paths::dirs().map(|d| d.cache_dir().join("images"))
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
    let (w, _) = image::ImageReader::open(src)?.with_guessed_format()?.into_dimensions()?;
    if w <= width {
        return Ok(src.to_path_buf());
    }
    let img = open(src)?.thumbnail(width, u32::MAX);
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    write_atomic(&out, &bytes)?;
    Ok(out)
}

/// The cached file for `url`, when it is already on disk.
pub fn cached(url: &str) -> Option<PathBuf> {
    path_for(url).filter(|p| p.is_file())
}

/// The most of an animated image that is downloaded; the rest is cut off.
const ANIMATED_MAX: u64 = 64 << 20;

/// Downloads `url` into the cache (blocking) and returns the file.
pub fn fetch(url: &str) -> anyhow::Result<PathBuf> {
    let path = path_for(url).ok_or_else(|| anyhow::anyhow!("no cache folder"))?;
    if path.is_file() {
        return Ok(path);
    }
    anyhow::ensure!(url.starts_with("http"), "{} is missing", path.display());
    // Big GIFs take longer than API calls are given.
    let animated = maybe_animated(&path);
    let agent = if animated { crate::cf::download_agent() } else { crate::cf::agent() };
    let resp = agent.get(url).call()?;
    let bytes = if animated {
        // Description GIFs run to 100+ MB. A cut-off one still decodes:
        // its first stretch plays, rather than nothing showing.
        use std::io::Read;
        let mut bytes = Vec::new();
        resp.into_body().into_reader().take(ANIMATED_MAX).read_to_end(&mut bytes)?;
        bytes
    } else {
        resp.into_body().with_config().limit(16 << 20).read_to_vec()?
    };
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
    let img = open(src)?.thumbnail(max, max).to_rgb8();
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
    let img = blur_image(&open(src)?);
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
    let img = open(path)?;
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)?;
    write_atomic(&out, &bytes)?;
    Ok(out)
}

/// Decodes the image at `path` by what its bytes are, not its name: some
/// CDNs serve PNGs under a `.webp` name, which the WebP decoder rejects.
fn open(path: &Path) -> anyhow::Result<image::DynamicImage> {
    Ok(image::ImageReader::open(path)?.with_guessed_format()?.decode()?)
}

/// How big the cache may get before [`prune`] clears the pictures used
/// longest ago.
pub const MAX_BYTES: u64 = 1 << 30;

/// Marks the cached file `path` as just used, so [`prune`] keeps it. Its
/// access time is set (not its modified time: scaled copies are keyed by
/// that), on a thread, once per file per run.
pub fn touch(path: &Path) {
    use std::sync::mpsc::{Sender, channel};
    use std::sync::{Mutex, OnceLock};
    static TX: OnceLock<Mutex<Sender<PathBuf>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<PathBuf>();
        std::thread::spawn(move || {
            let mut done = std::collections::HashSet::new();
            while let Ok(p) = rx.recv() {
                if done.insert(p.clone()) {
                    let now = std::fs::FileTimes::new().set_accessed(std::time::SystemTime::now());
                    let _ = std::fs::File::options().write(true).open(&p).and_then(|f| f.set_times(now));
                }
            }
        });
        Mutex::new(tx)
    });
    if let Ok(tx) = tx.lock() {
        let _ = tx.send(path.to_path_buf());
    }
}

/// Keeps the image cache under `max` bytes: when it's over, the files used
/// longest ago go until it's at four fifths. Half-written downloads left by
/// a run that stopped go too.
pub fn prune(max: u64) {
    if let Some(d) = dir() {
        let (n, bytes) = prune_dir(&d, max);
        if n > 0 {
            log::info!("image cache: cleared {n} old pictures ({} MB)", bytes >> 20);
        }
    }
}

/// [`prune`] on folder `dir` (and the folders in it): how many files went
/// and how many bytes they held.
pub fn prune_dir(dir: &Path, max: u64) -> (usize, u64) {
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    let day_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
    let (mut gone, mut freed) = (0, 0);
    while let Some(d) = stack.pop() {
        let Ok(list) = std::fs::read_dir(&d) else { continue };
        for e in list.flatten() {
            let Ok(meta) = e.metadata() else { continue };
            let path = e.path();
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let used = meta.accessed().or_else(|_| meta.modified()).unwrap_or(std::time::UNIX_EPOCH);
            let stale_part = path.extension().is_some_and(|x| x == "part") && used < day_ago;
            if stale_part {
                if std::fs::remove_file(&path).is_ok() {
                    gone += 1;
                    freed += meta.len();
                }
                continue;
            }
            files.push((path, meta.len(), used));
        }
    }
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= max {
        return (gone, freed);
    }
    files.sort_by_key(|f| f.2);
    let goal = max / 5 * 4;
    for (path, len, _) in files {
        if total <= goal {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= len;
            gone += 1;
            freed += len;
        }
    }
    (gone, freed)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // Its own name: two threads can fetch the same picture at once (a
    // description showing a gallery image), and must not write one file.
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.part", std::process::id()));
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
    fn a_png_named_webp_still_decodes() {
        let dir = std::env::temp_dir().join(format!("hopper-misnamed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("real.png");
        let frame = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 255, 0, 255]));
        image::DynamicImage::ImageRgba8(frame).save(&png).unwrap();
        let webp = dir.join("icon.webp");
        std::fs::rename(&png, &webp).unwrap();
        let still = still(&webp).unwrap();
        assert_eq!(image::open(&still).unwrap().width(), 8);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn prune_clears_the_pictures_used_longest_ago() {
        let dir = std::env::temp_dir().join(format!("hopper-prune-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("thumbs")).unwrap();
        let now = std::time::SystemTime::now();
        // Four 100-byte files, used 4, 3, 2 and 1 hours ago (one in a sub-folder).
        let names = ["a.png", "b.png", "thumbs/c.png", "d.png"];
        for (i, n) in names.iter().enumerate() {
            let p = dir.join(n);
            std::fs::write(&p, [0u8; 100]).unwrap();
            let used = now - std::time::Duration::from_secs((4 - i as u64) * 3600);
            let t = std::fs::FileTimes::new().set_accessed(used).set_modified(used);
            std::fs::File::options().write(true).open(&p).unwrap().set_times(t).unwrap();
        }
        // Under the limit: nothing goes.
        assert_eq!(prune_dir(&dir, 1000), (0, 0));
        // Over it: down to four fifths of 300, so the two oldest go.
        assert_eq!(prune_dir(&dir, 300), (2, 200));
        assert!(!dir.join("a.png").exists() && !dir.join("b.png").exists());
        assert!(dir.join("thumbs/c.png").exists() && dir.join("d.png").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn remote_urls_keep_their_extension() {
        let p = path_for("https://media.forgecdn.net/avatars/1/2/thumb.webp?x=1").unwrap();
        assert_eq!(p.extension().unwrap(), "webp");
        let q = path_for("https://cdn.modrinth.com/data/abc/icon").unwrap();
        assert_eq!(q.extension().unwrap(), "png");
    }
}
