//! Animated GIFs and WebPs played at their own pace.
//!
//! GPUI's `img()` animates a GIF by redrawing its view on every display
//! frame (60–144 a second) while it is on screen. A [`Clip`] is the frames
//! as separate still images instead: a page shows the one for "now" and
//! redraws only when the frame changes (see [`Clip::wait_now`]), usually
//! 10–20 times a second.
//!
//! A small clip is decoded once and kept whole. A big one (a long 800px
//! GIF is hundreds of MB decoded) streams: a thread decodes a frame or two
//! ahead, looping, and frames that have been shown are handed back with
//! [`Clip::take_shown`] so the UI can free their GPU copies.

use gpui_kit::RenderImage;
use image::AnimationDecoder;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Clips whose frames fit in this many bytes are decoded once and kept.
const KEEP_BUDGET: usize = 48 << 20;
/// Frames are scaled down to this width at most (the description column is
/// narrower).
const MAX_WIDTH: u32 = 720;
/// Streamed clips (big ones) are scaled down further: frames are decoded
/// again each loop, and a smaller frame is cheaper at every step.
const STREAM_MAX_WIDTH: u32 = 560;
/// A streamed clip keeps its frames compressed after the first loop, up to
/// this many bytes; one that doesn't fit decodes the file every loop.
const STREAM_CACHE: usize = 40 << 20;
/// How often a stream that fell behind its decoder looks again.
const LATE_RETRY: Duration = Duration::from_millis(15);

pub struct Clip(Inner);

enum Inner {
    /// Every frame, decoded once.
    Kept {
        frames: Vec<Arc<RenderImage>>,
        /// When each frame ends, in ms from the start of the loop.
        ends: Vec<u64>,
        /// When playback started (when it was decoded).
        start: Instant,
    },
    Streamed(Mutex<Stream>),
}

struct Stream {
    /// Frames from the decoder thread, with how long each shows (ms).
    rx: Receiver<(Arc<RenderImage>, u64)>,
    current: Arc<RenderImage>,
    /// When `current` gives way to the next frame.
    due: Instant,
    /// Frames shown and replaced, waiting for [`Clip::take_shown`].
    shown: Vec<Arc<RenderImage>>,
}

impl Clip {
    fn kept(frames: Vec<Arc<RenderImage>>, ends: Vec<u64>) -> Self {
        Clip(Inner::Kept { frames, ends, start: Instant::now() })
    }

    /// The frame to show now.
    pub fn frame_now(&self) -> Arc<RenderImage> {
        match &self.0 {
            Inner::Kept { frames, ends, start } => {
                frames[index_at(ends, now_ms(*start))].clone()
            }
            Inner::Streamed(s) => {
                let mut s = s.lock().unwrap_or_else(|e| e.into_inner());
                s.advance();
                s.current.clone()
            }
        }
    }

    /// How long until the frame shown now changes.
    pub fn wait_now(&self) -> Duration {
        match &self.0 {
            Inner::Kept { ends, start, .. } => wait_at(ends, now_ms(*start)),
            Inner::Streamed(s) => {
                let s = s.lock().unwrap_or_else(|e| e.into_inner());
                s.due.saturating_duration_since(Instant::now()).max(LATE_RETRY)
            }
        }
    }

    /// Frames no longer shown, whose GPU copies can go. Kept clips reuse
    /// theirs, so only a streamed one has any.
    pub fn take_shown(&self) -> Vec<Arc<RenderImage>> {
        match &self.0 {
            Inner::Kept { .. } => Vec::new(),
            Inner::Streamed(s) => {
                std::mem::take(&mut s.lock().unwrap_or_else(|e| e.into_inner()).shown)
            }
        }
    }

    /// Every frame it holds, to free when the clip goes.
    pub fn frames(&self) -> Vec<Arc<RenderImage>> {
        match &self.0 {
            Inner::Kept { frames, .. } => frames.clone(),
            Inner::Streamed(s) => {
                let s = s.lock().unwrap_or_else(|e| e.into_inner());
                s.shown.iter().cloned().chain([s.current.clone()]).collect()
            }
        }
    }
}

impl Stream {
    /// Moves on to every frame whose time has come that the decoder has
    /// ready. One that is late is skipped to, not played catch-up.
    fn advance(&mut self) {
        let now = Instant::now();
        while now >= self.due {
            match self.rx.try_recv() {
                Ok((img, ms)) => {
                    self.shown.push(std::mem::replace(&mut self.current, img));
                    let next = self.due + Duration::from_millis(ms);
                    self.due = if next < now { now + Duration::from_millis(ms) } else { next };
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
    }
}

fn now_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn index_at(ends: &[u64], ms: u64) -> usize {
    let total = ends.last().copied().unwrap_or(1).max(1);
    ends.partition_point(|&e| e <= ms % total).min(ends.len().saturating_sub(1))
}

fn wait_at(ends: &[u64], ms: u64) -> Duration {
    let total = ends.last().copied().unwrap_or(1).max(1);
    let end = ends[index_at(ends, ms)];
    Duration::from_millis(end.saturating_sub(ms % total).max(1))
}

/// A frame delay as browsers treat it: under 20 ms means 100 ms.
fn delay_ms(d: image::Delay) -> u64 {
    let (n, den) = d.numer_denom_ms();
    let ms = (n as u64) / (den.max(1) as u64);
    if ms < 20 { 100 } else { ms }
}

/// The frames of the GIF or animated WebP at `path`, decoded and ready to
/// upload: scaled to at most `max` wide, BGRA as GPUI wants.
fn frames(
    path: &Path,
    max: u32,
) -> anyhow::Result<Option<impl Iterator<Item = anyhow::Result<(Arc<RenderImage>, u64)>>>> {
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
    Ok(Some(frames.map(move |frame| {
        let frame = frame?;
        let ms = delay_ms(frame.delay());
        let mut buf = frame.into_buffer();
        if buf.width() > max {
            let h = (buf.height() as u64 * max as u64 / buf.width() as u64).max(1) as u32;
            // Box averaging: several times faster than a filtered resize,
            // and every frame of a stream goes through it.
            buf = image::imageops::thumbnail(&buf, max, h);
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        Ok((Arc::new(RenderImage::new(vec![image::Frame::new(buf)])), ms))
    })))
}

/// The GIF or WebP at `path` to play, no wider than `width` (or
/// [`MAX_WIDTH`]). `None` when it has a single frame.
pub fn decode(path: &Path, width: Option<u32>) -> anyhow::Result<Option<Clip>> {
    decode_within(path, width, KEEP_BUDGET)
}

fn decode_within(path: &Path, width: Option<u32>, budget: usize) -> anyhow::Result<Option<Clip>> {
    let max = width.unwrap_or(MAX_WIDTH).clamp(1, MAX_WIDTH);
    let Some(all) = frames(path, max)? else { return Ok(None) };
    let (mut out, mut ends, mut bytes, mut at) = (Vec::new(), Vec::new(), 0usize, 0u64);
    for f in all {
        // A GIF cut off by the download cap ends where the cut is.
        let (img, ms) = match f {
            Ok(f) => f,
            Err(_) if !out.is_empty() => break,
            Err(e) => return Err(e),
        };
        bytes += img.as_bytes(0).map_or(0, <[u8]>::len);
        if bytes > budget {
            return stream(path.to_path_buf(), max.min(STREAM_MAX_WIDTH));
        }
        at += ms;
        ends.push(at);
        out.push(img);
    }
    Ok((out.len() >= 2).then(|| Clip::kept(out, ends)))
}

/// A clip whose frames a thread decodes as they're needed, looping.
fn stream(path: PathBuf, max: u32) -> anyhow::Result<Option<Clip>> {
    // The frame showing plus one ready: little memory, no stutter.
    let (tx, rx) = sync_channel(1);
    std::thread::Builder::new()
        .name("gif".into())
        .spawn(move || decode_loop(&path, max, &tx))?;
    let Ok((current, ms)) = rx.recv() else {
        anyhow::bail!("no frames");
    };
    let due = Instant::now() + Duration::from_millis(ms);
    Ok(Some(Clip(Inner::Streamed(Mutex::new(Stream { rx, current, due, shown: Vec::new() })))))
}

/// A decoded frame kept LZ4-compressed: BGRA pixels, size, and how long
/// it shows (ms). The pixels are stored XORed with the frame before, which
/// is mostly zeros (most of a GIF stays put from frame to frame) and packs
/// several times smaller, so long clips fit the cache too.
struct Packed {
    bytes: Vec<u8>,
    w: u32,
    h: u32,
    ms: u64,
}

impl Packed {
    /// Packs `img` against `prev`, the pixels of the frame packed before it
    /// (empty for the first), which become `img`'s.
    fn pack(img: &RenderImage, ms: u64, prev: &mut Vec<u8>) -> Option<Self> {
        let size = img.size(0);
        let raw = img.as_bytes(0)?;
        if prev.len() != raw.len() {
            *prev = vec![0; raw.len()];
        }
        xor(prev, raw);
        let bytes = lz4_flex::compress_prepend_size(prev);
        prev.copy_from_slice(raw);
        Some(Packed { bytes, w: size.width.0 as u32, h: size.height.0 as u32, ms })
    }

    /// Unpacks against `prev`, the pixels of the frame unpacked before it
    /// (empty for the first), which become this frame's.
    fn unpack(&self, prev: &mut Vec<u8>) -> Option<(Arc<RenderImage>, u64)> {
        let mut raw = lz4_flex::decompress_size_prepended(&self.bytes).ok()?;
        if prev.len() == raw.len() {
            xor(&mut raw, prev);
        }
        prev.clone_from(&raw);
        let buf = image::RgbaImage::from_raw(self.w, self.h, raw)?;
        Some((Arc::new(RenderImage::new(vec![image::Frame::new(buf)])), self.ms))
    }
}

fn xor(into: &mut [u8], with: &[u8]) {
    into.iter_mut().zip(with).for_each(|(a, b)| *a ^= b);
}

/// Sends the frames of `path` round and round until the clip is dropped
/// (the send fails). A frame that won't decode ends the loop there (a GIF
/// cut off by the download cap); one before any frame ends playback.
///
/// The first loop decodes the file and keeps each frame compressed (up to
/// [`STREAM_CACHE`]); later loops unpack those, which costs a fraction of
/// decoding a big GIF again.
fn decode_loop(path: &Path, max: u32, tx: &SyncSender<(Arc<RenderImage>, u64)>) {
    let mut packed: Option<Vec<Packed>> = Some(Vec::new());
    loop {
        if let Some(frames) = packed.as_ref().filter(|p| !p.is_empty()) {
            let mut prev = Vec::new();
            for f in frames {
                let Some(frame) = f.unpack(&mut prev) else { return };
                if tx.send(frame).is_err() {
                    return;
                }
            }
            continue;
        }
        let Ok(Some(all)) = frames(path, max) else { return };
        let mut sent = 0;
        let mut kept = 0usize;
        let mut prev = Vec::new();
        for f in all {
            let Ok(frame) = f.inspect_err(|e| log::debug!("gif {}: {e:#}", path.display())) else {
                break;
            };
            if let Some(p) = packed.as_mut() {
                match Packed::pack(&frame.0, frame.1, &mut prev) {
                    Some(pk) if kept + pk.bytes.len() <= STREAM_CACHE => {
                        kept += pk.bytes.len();
                        p.push(pk);
                    }
                    _ => packed = None,
                }
            }
            if tx.send(frame).is_err() {
                return;
            }
            sent += 1;
        }
        if sent == 0 {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gif(dir: &Path, delays: &[u32], w: u32) -> std::path::PathBuf {
        use image::codecs::gif::{GifEncoder, Repeat};
        let path = dir.join(format!("a{w}-{}.gif", delays.len()));
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

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hopper-anim-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn frames_follow_their_delays() {
        let dir = dir("kept");
        // 10 ms counts as 100 ms, as in browsers.
        let clip = decode(&gif(&dir, &[50, 200, 10], 4), None).unwrap().unwrap();
        let Inner::Kept { ends, frames, .. } = &clip.0 else { panic!("small clips are kept") };
        assert_eq!(ends, &vec![50, 250, 350]);
        assert_eq!(index_at(ends, 0), 0);
        assert_eq!(index_at(ends, 49), 0);
        assert_eq!(index_at(ends, 50), 1);
        assert_eq!(index_at(ends, 300), 2);
        assert_eq!(index_at(ends, 350), 0, "loops");
        assert_eq!(wait_at(ends, 60), Duration::from_millis(190));
        assert_eq!(frames.len(), 3);
        // Wide frames shrink to the width asked for.
        let wide = decode(&gif(&dir, &[100, 100], 40), Some(10)).unwrap().unwrap();
        assert_eq!(wide.frame_now().size(0).width.0, 10);
        // One frame: nothing to play.
        assert!(decode(&gif(&dir, &[100], 4), None).unwrap().is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cut_off_gifs_play_what_they_have() {
        let dir = dir("cut");
        let full = gif(&dir, &[100, 100, 100, 100], 40);
        let bytes = std::fs::read(&full).unwrap();
        let cut = dir.join("cut.gif");
        std::fs::write(&cut, &bytes[..bytes.len() * 3 / 4]).unwrap();
        let clip = decode(&cut, None).unwrap().unwrap();
        let Inner::Kept { frames, .. } = &clip.0 else { panic!("small clips are kept") };
        assert!((2..4).contains(&frames.len()), "{} frames", frames.len());
        // Streamed, it loops over the same part.
        let s = decode_within(&cut, None, 1).unwrap().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        s.frame_now();
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn packed_frames_unpack_to_the_same_pixels() {
        let frame = |seed: u8| {
            let buf = image::RgbaImage::from_fn(5, 3, |x, y| {
                image::Rgba([seed, x as u8 * seed, y as u8, if x == 2 { seed } else { 255 }])
            });
            RenderImage::new(vec![image::Frame::new(buf)])
        };
        let frames = [frame(1), frame(7), frame(7), frame(200)];
        let mut prev = Vec::new();
        let packed: Vec<Packed> = frames.iter().map(|f| Packed::pack(f, 50, &mut prev).unwrap()).collect();
        // Twice round, as a looping clip unpacks them.
        for _ in 0..2 {
            let mut prev = Vec::new();
            for (f, p) in frames.iter().zip(&packed) {
                let (img, ms) = p.unpack(&mut prev).unwrap();
                assert_eq!(img.as_bytes(0), f.as_bytes(0));
                assert_eq!(ms, 50);
            }
        }
    }

    #[test]
    fn big_clips_stream_and_hand_back_shown_frames() {
        let dir = dir("stream");
        // Three 4x2 frames are 96 bytes: over a 40 byte budget.
        let clip = decode_within(&gif(&dir, &[20, 20, 20], 4), None, 40).unwrap().unwrap();
        assert!(matches!(clip.0, Inner::Streamed(_)));
        let first = clip.frame_now();
        assert!(clip.take_shown().is_empty());
        // Frames come round and round; each one replaced is handed back once.
        let mut seen = 0;
        for _ in 0..40 {
            std::thread::sleep(Duration::from_millis(25));
            if !Arc::ptr_eq(&clip.frame_now(), &first) {
                seen += 1;
            }
        }
        assert!(seen > 0, "it moves on");
        let shown = clip.take_shown();
        assert!(shown.len() >= 4, "it loops: {} shown", shown.len());
        assert!(clip.take_shown().is_empty());
        std::fs::remove_dir_all(dir).ok();
    }
}
