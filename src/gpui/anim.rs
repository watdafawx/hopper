//! Animated GIFs and WebPs played at their own pace.
//!
//! GPUI's `img()` animates a GIF by redrawing its view on every display
//! frame (60–144 a second) while it is on screen. A [`Clip`] is the frames
//! as separate still images instead: a page shows the one for "now" and
//! redraws only when the frame changes (see [`Clip::wait`]), usually 10–20
//! times a second.

use gpui_kit::RenderImage;
use image::AnimationDecoder;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Decoded frames above this many bytes stay a still image.
const BUDGET: usize = 96 << 20;
/// Frames are scaled down to this width at most (the description column is
/// narrower).
const MAX_WIDTH: u32 = 720;

pub struct Clip {
    frames: Vec<Arc<RenderImage>>,
    /// When each frame ends, in ms from the start of the loop.
    ends: Vec<u64>,
    /// When playback started (when it was decoded).
    start: Instant,
}

impl Clip {
    fn total(&self) -> u64 {
        self.ends.last().copied().unwrap_or(1).max(1)
    }

    fn index_at(&self, ms: u64) -> usize {
        let t = ms % self.total();
        self.ends.partition_point(|&e| e <= t).min(self.frames.len() - 1)
    }

    /// The frame to show `ms` into playback.
    pub fn frame(&self, ms: u64) -> Arc<RenderImage> {
        self.frames[self.index_at(ms)].clone()
    }

    /// How long after `ms` the frame changes.
    pub fn wait(&self, ms: u64) -> Duration {
        let t = ms % self.total();
        let end = self.ends[self.index_at(ms)];
        Duration::from_millis(end.saturating_sub(t).max(1))
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// The frame to show now.
    pub fn frame_now(&self) -> Arc<RenderImage> {
        self.frame(self.now())
    }

    /// How long until the frame shown now changes.
    pub fn wait_now(&self) -> Duration {
        self.wait(self.now())
    }

    pub fn frames(&self) -> &[Arc<RenderImage>] {
        &self.frames
    }
}

/// A frame delay as browsers treat it: under 20 ms means 100 ms.
fn delay_ms(d: image::Delay) -> u64 {
    let (n, den) = d.numer_denom_ms();
    let ms = (n as u64) / (den.max(1) as u64);
    if ms < 20 { 100 } else { ms }
}

/// The frames of the GIF or WebP at `path`, no wider than `width` (or
/// [`MAX_WIDTH`]). `None` when it has a single frame or is too big to hold.
pub fn decode(path: &Path, width: Option<u32>) -> anyhow::Result<Option<Clip>> {
    use image::codecs::{gif::GifDecoder, webp::WebPDecoder};
    let reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let frames = match image::ImageFormat::from_path(path)? {
        image::ImageFormat::Gif => GifDecoder::new(reader)?.into_frames(),
        image::ImageFormat::WebP => {
            let mut d = WebPDecoder::new(reader)?;
            if !d.has_animation() {
                return Ok(None);
            }
            d.set_background_color(image::Rgba([0, 0, 0, 0]))?;
            d.into_frames()
        }
        _ => return Ok(None),
    };
    let max = width.unwrap_or(MAX_WIDTH).clamp(1, MAX_WIDTH);
    let (mut out, mut ends, mut bytes, mut at) = (Vec::new(), Vec::new(), 0usize, 0u64);
    for frame in frames {
        let frame = frame?;
        let delay = frame.delay();
        let mut buf = frame.into_buffer();
        if buf.width() > max {
            let h = (buf.height() as u64 * max as u64 / buf.width() as u64).max(1) as u32;
            buf = image::imageops::resize(&buf, max, h, image::imageops::FilterType::Triangle);
        }
        bytes += buf.len();
        if bytes > BUDGET {
            return Ok(None);
        }
        // GPUI wants BGRA.
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        at += delay_ms(delay);
        ends.push(at);
        out.push(Arc::new(RenderImage::new(vec![image::Frame::new(buf)])));
    }
    if out.len() < 2 {
        return Ok(None);
    }
    Ok(Some(Clip { frames: out, ends, start: Instant::now() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(dir: &Path, delays: &[u32], w: u32) -> std::path::PathBuf {
        use image::codecs::gif::{GifEncoder, Repeat};
        let path = dir.join(format!("a{w}.gif"));
        let mut enc = GifEncoder::new(std::fs::File::create(&path).unwrap());
        enc.set_repeat(Repeat::Infinite).unwrap();
        for (i, &d) in delays.iter().enumerate() {
            let px = image::RgbaImage::from_pixel(w, 2, image::Rgba([i as u8 * 80, 0, 0, 255]));
            let delay = image::Delay::from_numer_denom_ms(d, 1);
            enc.encode_frame(image::Frame::from_parts(px, 0, 0, delay)).unwrap();
        }
        drop(enc);
        path
    }

    #[test]
    fn frames_follow_their_delays() {
        let dir = std::env::temp_dir().join(format!("hopper-anim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 10 ms counts as 100 ms, as in browsers.
        let clip = decode(&gif(&dir, &[50, 200, 10], 4), None).unwrap().unwrap();
        assert_eq!(clip.ends, vec![50, 250, 350]);
        assert_eq!(clip.index_at(0), 0);
        assert_eq!(clip.index_at(49), 0);
        assert_eq!(clip.index_at(50), 1);
        assert_eq!(clip.index_at(300), 2);
        assert_eq!(clip.index_at(350), 0, "loops");
        assert_eq!(clip.wait(60), Duration::from_millis(190));
        // Wide frames shrink to the width asked for.
        let wide = decode(&gif(&dir, &[100, 100], 40), Some(10)).unwrap().unwrap();
        assert_eq!(wide.frames[0].size(0).width.0, 10);
        // One frame: nothing to play.
        assert!(decode(&gif(&dir, &[100], 4), None).unwrap().is_none());
        std::fs::remove_dir_all(dir).ok();
    }
}
