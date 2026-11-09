//! `dsview --soak MINUTES [--soak-device NAME]`: a loop capture (the demo's
//! random pattern, or a device) with the window open (the view following the
//! data), the resident memory sampled every 5 s after a 5-minute warm-up and
//! judged by its trend, as `dsview-cli soak` does. The frames are judged too
//! (F42/F43): from the first data on, 99 % of the gaps between two frames
//! must stay within `DSVIEW_SOAK_MAX_P99_MS` (100 ms) and every gap within
//! `DSVIEW_SOAK_MAX_GAP_MS` (300 ms). A capture restarted over the control
//! socket (to add decoders) may stop for up to 30 s.

use std::time::{Duration, Instant};

use dsview_session::fair::lock_stats::{self, Stat};

use crate::framestats::FrameStats;

/// A frame-gap limit in ms from the environment.
fn limit_ms(var: &str, default: f64) -> f64 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// How long the capture may stay stopped (a restart over the control socket).
const STOP_GRACE: Duration = Duration::from_secs(30);

pub struct Soak {
    /// Frames from the first one with data on.
    frames: FrameStats,
    data_seen: bool,
    stopped_since: Option<Instant>,
    pub until: Instant,
    start: Instant,
    warmup: Duration,
    next: Duration,
    samples: Vec<u64>,
}

/// Resident set size in KiB (`/proc/self/statm`).
fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    pages * 4096 / 1024
}

impl Soak {
    pub fn new(minutes: f64) -> Soak {
        let total = Duration::from_secs_f64(minutes * 60.0);
        let now = Instant::now();
        lock_stats::enable();
        Soak {
            frames: FrameStats::default(),
            data_seen: false,
            stopped_since: None,
            until: now + total,
            start: now,
            warmup: Duration::from_secs(300).min(total / 2),
            next: Duration::from_secs(5),
            samples: Vec::new(),
        }
    }

    /// Called every frame (begun at `t0`, `data`: it saw new capture data);
    /// true when the soak is over.
    pub fn frame(&mut self, t0: Instant, data: bool) -> bool {
        self.data_seen |= data;
        if self.data_seen {
            self.frames
                .frame(t0, t0.elapsed().as_secs_f64() * 1000.0, data);
        }
        let t = self.start.elapsed();
        if t >= self.next {
            if t >= self.warmup {
                self.samples.push(rss_kib());
            }
            if self.next.as_secs().is_multiple_of(60) {
                println!("{:6.1} min rss {} KiB", t.as_secs_f64() / 60.0, rss_kib());
            }
            self.next += Duration::from_secs(5);
        }
        Instant::now() >= self.until
    }

    /// Prints the result; true when the trend stays under 1 % and no frame
    /// gap exceeds the limit.
    pub fn report(&self, captures: u64) -> bool {
        let frames_ok = self.report_frames();
        self.report_rss(captures) && frames_ok
    }

    /// True once the capture has been stopped longer than [`STOP_GRACE`].
    pub fn capture_ended(&mut self, working: bool) -> bool {
        if working {
            self.stopped_since = None;
            return false;
        }
        self.stopped_since
            .get_or_insert_with(Instant::now)
            .elapsed()
            > STOP_GRACE
    }

    /// The frame gate: the gaps from the first data on.
    fn report_frames(&self) -> bool {
        let f = &self.frames;
        let (p99_limit, limit) = (
            limit_ms("DSVIEW_SOAK_MAX_P99_MS", 100.0),
            limit_ms("DSVIEW_SOAK_MAX_GAP_MS", 300.0),
        );
        let (dc, dw, dm) = lock_stats::take(Stat::Device);
        let (sc, sw, sm) = lock_stats::take(Stat::Store);
        let ok = self.data_seen && f.gap_percentile(99.0) <= p99_limit && f.max_gap() <= limit;
        println!(
            "soak frames: {} ({} with new data), gap p50 {:.0} ms p99 {:.0} ms (limit {p99_limit:.0}) max {:.0} ms (limit {limit:.0}), \
             frame time mean {:.1} ms max {:.1} ms; device locks {dc} waited {:.0} ms longest {:.1} ms; \
             store locks {sc} waited {:.0} ms longest {:.1} ms{}",
            f.frames,
            f.data,
            f.gap_percentile(50.0),
            f.gap_percentile(99.0),
            f.max_gap(),
            f.mean_cpu(),
            f.max_cpu(),
            dw as f64 / 1e6,
            dm as f64 / 1e6,
            sw as f64 / 1e6,
            sm as f64 / 1e6,
            if self.data_seen {
                ""
            } else {
                " (no data seen)"
            }
        );
        ok
    }

    /// The resident set's trend.
    fn report_rss(&self, captures: u64) -> bool {
        let n = self.samples.len() as f64;
        if n < 8.0 {
            println!("soak: too short to measure");
            return false;
        }
        let xs: Vec<f64> = (0..self.samples.len()).map(|i| i as f64 * 5.0).collect();
        let ys: Vec<f64> = self.samples.iter().map(|&v| v as f64).collect();
        let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
        let cov: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
        let var: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
        let span = xs[xs.len() - 1];
        let growth = cov / var * span / my * 100.0;
        let lo = self.samples.iter().min().copied().unwrap_or(0);
        let hi = self.samples.iter().max().copied().unwrap_or(0);
        println!(
            "soak loop with the GUI: {captures} capture(s), rss mean {my:.0} KiB, range {lo}..{hi} KiB, \
             trend {growth:+.2} % over {:.0} min ({} samples)",
            span / 60.0,
            self.samples.len()
        );
        growth < 1.0
    }
}
