//! The numbers a run is judged by: average FPS, 1% low, frame-time
//! percentiles, and the sums across several runs.

use serde::{Deserialize, Serialize};

/// The `p`th percentile (0–100) of `v`, nearest rank. Sorts `v`.
pub fn percentile(v: &mut [f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let rank = ((p / 100.0) * v.len() as f64).ceil() as usize;
    Some(v[rank.clamp(1, v.len()) - 1])
}

/// The middle value. Sorts `v`.
pub fn median(v: &mut [f64]) -> Option<f64> {
    percentile(v, 50.0)
}

pub fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Frame times longer than this are a gap (minimized, a breakpoint, the
/// PC asleep), not a frame.
pub const GAP_MS: f64 = 5_000.0;

/// How smooth a run was, from its frame times.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameStats {
    pub frames: u64,
    pub avg_fps: f64,
    /// FPS of the slowest 1% of frames (1000 / their average frame time).
    pub low1_fps: f64,
    /// FPS of the slowest 0.1% of frames.
    pub low01_fps: f64,
    pub avg_ms: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl FrameStats {
    /// From frame times in ms. `None` with no frames.
    pub fn from_frame_times(times: &[f64]) -> Option<Self> {
        let mut t: Vec<f64> = times
            .iter()
            .copied()
            .filter(|x| *x > 0.0 && *x <= GAP_MS)
            .collect();
        if t.is_empty() {
            return None;
        }
        let total: f64 = t.iter().sum();
        let n = t.len();
        t.sort_by(|a, b| b.total_cmp(a));
        let worst = |share: f64| {
            let k = ((n as f64 * share).ceil() as usize).max(1);
            1000.0 / (t[..k].iter().sum::<f64>() / k as f64)
        };
        let low1_fps = worst(0.01);
        let low01_fps = worst(0.001);
        let max_ms = t[0];
        Some(Self {
            frames: n as u64,
            avg_fps: n as f64 * 1000.0 / total,
            low1_fps,
            low01_fps,
            avg_ms: total / n as f64,
            p50_ms: percentile(&mut t, 50.0).unwrap_or(0.0),
            p99_ms: percentile(&mut t, 99.0).unwrap_or(0.0),
            max_ms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut v = vec![5.0, 1.0, 4.0, 2.0, 3.0];
        assert_eq!(percentile(&mut v, 50.0), Some(3.0));
        assert_eq!(percentile(&mut v, 99.0), Some(5.0));
        assert_eq!(percentile(&mut v, 0.0), Some(1.0));
        assert_eq!(percentile(&mut [], 50.0), None);
        let mut hundred: Vec<f64> = (1..=100).map(f64::from).collect();
        assert_eq!(percentile(&mut hundred, 99.0), Some(99.0));
        assert_eq!(median(&mut hundred), Some(50.0));
        assert_eq!(mean(&[1.0, 2.0, 6.0]), Some(3.0));
        assert_eq!(mean(&[]), None);
    }

    #[test]
    fn frame_stats() {
        // 990 frames at 10 ms, 10 at 50 ms: the 1% low is the 10 slow ones.
        let mut t = vec![10.0; 990];
        t.extend([50.0; 10]);
        let s = FrameStats::from_frame_times(&t).unwrap();
        assert_eq!(s.frames, 1000);
        assert!((s.avg_ms - 10.4).abs() < 1e-9);
        assert!((s.avg_fps - 1000.0 / 10.4).abs() < 1e-9);
        assert!((s.low1_fps - 20.0).abs() < 1e-9);
        assert!((s.low01_fps - 20.0).abs() < 1e-9);
        assert_eq!(s.p50_ms, 10.0);
        assert_eq!(s.p99_ms, 10.0);
        assert_eq!(s.max_ms, 50.0);
    }

    #[test]
    fn frame_stats_skip_gaps() {
        // A minute minimized isn't a frame.
        let s = FrameStats::from_frame_times(&[16.0, 16.0, 60_000.0, 0.0]).unwrap();
        assert_eq!(s.frames, 2);
        assert_eq!(s.max_ms, 16.0);
        assert!(FrameStats::from_frame_times(&[]).is_none());
        assert!(FrameStats::from_frame_times(&[9_000.0]).is_none());
    }
}
