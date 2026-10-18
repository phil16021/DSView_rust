//! `dsview --soak MINUTES`: a loop capture on the demo's random pattern with
//! the window open (the view following the data), the resident memory
//! sampled every 5 s after a 5-minute warm-up and judged by its trend, as
//! `dsview-cli soak` does.

use std::time::{Duration, Instant};

pub struct Soak {
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
        Soak {
            until: now + total,
            start: now,
            warmup: Duration::from_secs(300).min(total / 2),
            next: Duration::from_secs(5),
            samples: Vec::new(),
        }
    }

    /// Called every frame; true when the soak is over.
    pub fn frame(&mut self) -> bool {
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

    /// Prints the result; true when the trend stays under 1 %.
    pub fn report(&self, captures: u64) -> bool {
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
