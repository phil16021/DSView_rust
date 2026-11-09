//! Frame timing for `DSVIEW_FRAME_LOG` and the soak's frame gate (F42/F43):
//! the gaps between frames, the time spent in them, and which saw new data.

use std::time::Instant;

#[derive(Default)]
pub struct FrameStats {
    last: Option<Instant>,
    /// Gaps between two frames, in ms.
    gaps: Vec<f32>,
    pub frames: u32,
    /// Frames that saw new capture data.
    pub data: u32,
    cpu: f64,
    max_cpu: f64,
}

impl FrameStats {
    /// One frame that began at `t0` and took `cpu_ms`.
    pub fn frame(&mut self, t0: Instant, cpu_ms: f64, data: bool) {
        if let Some(l) = self.last {
            self.gaps.push(((t0 - l).as_secs_f64() * 1000.0) as f32);
        }
        self.last = Some(t0);
        self.frames += 1;
        self.data += u32::from(data);
        self.cpu += cpu_ms;
        self.max_cpu = self.max_cpu.max(cpu_ms);
    }

    /// Starts a new window; the next gap is measured from the last frame.
    pub fn reset(&mut self) {
        *self = FrameStats {
            last: self.last,
            ..FrameStats::default()
        };
    }

    pub fn max_gap(&self) -> f64 {
        self.gaps.iter().copied().fold(0.0, f32::max).into()
    }

    /// The gap that `p` (0-100) percent of the gaps do not exceed.
    pub fn gap_percentile(&self, p: f64) -> f64 {
        if self.gaps.is_empty() {
            return 0.0;
        }
        let mut g = self.gaps.clone();
        g.sort_by(f32::total_cmp);
        let i = ((p / 100.0) * (g.len() - 1) as f64).round() as usize;
        g[i.min(g.len() - 1)].into()
    }

    pub fn mean_cpu(&self) -> f64 {
        self.cpu / f64::from(self.frames.max(1))
    }

    pub fn max_cpu(&self) -> f64 {
        self.max_cpu
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn gaps_percentiles_and_reset() {
        let mut s = FrameStats::default();
        let t = Instant::now();
        let mut at = Duration::ZERO;
        // 99 gaps of 33 ms and one of 250 ms
        for i in 0..101 {
            s.frame(t + at, 1.0, i % 2 == 0);
            at += Duration::from_millis(if i == 50 { 250 } else { 33 });
        }
        assert_eq!(s.frames, 101);
        assert_eq!(s.data, 51);
        assert!((s.max_gap() - 250.0).abs() < 0.01);
        assert!((s.gap_percentile(50.0) - 33.0).abs() < 0.01);
        assert!((s.gap_percentile(99.0) - 33.0).abs() < 0.01);
        assert!((s.gap_percentile(100.0) - 250.0).abs() < 0.01);
        s.reset();
        assert_eq!((s.frames, s.max_gap()), (0, 0.0));
        // the first gap after a reset counts from the frame before it
        let last = at - Duration::from_millis(33);
        s.frame(t + last + Duration::from_millis(40), 1.0, false);
        assert!((s.max_gap() - 40.0).abs() < 0.01);
    }
}
