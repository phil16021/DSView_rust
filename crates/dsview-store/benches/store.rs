//! LogicStore on its hot paths: ingest (the capture thread) and the display
//! queries (every frame). The workloads are those of the former C++/Rust
//! comparison (`dev/dsview-oracle/benches/store.rs`, tag `c-final`), timed
//! with `std::time` (median of the runs), so they compare with the numbers
//! recorded in docs/migration/STATUS.md.
//!
//!     cargo bench -p dsview-store [-- ingest|display|edges]

use std::hint::black_box;
use std::time::{Duration, Instant};

use dsview_store::logic::LogicStore;

/// Deterministic LA_CROSS stream: `n` channels, `groups` × 64 samples.
/// Channel c toggles every `period[c]` samples, with random bursts.
fn stream(n: usize, groups: u64, periods: &[u64]) -> Vec<u8> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut out = Vec::with_capacity(groups as usize * n * 8);
    for g in 0..groups {
        for &p in periods.iter().take(n) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let s0 = g * 64;
            let w = if x.is_multiple_of(997) {
                x
            } else if s0 / p == (s0 + 63) / p {
                if (s0 / p) % 2 == 1 { !0 } else { 0 }
            } else {
                (0..64).fold(0u64, |w, i| w | ((((s0 + i) / p) % 2) << i))
            };
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
    out
}

const PACKET: usize = 1 << 20;

/// Median time of `runs` calls of `f`.
fn median(runs: usize, mut f: impl FnMut()) -> Duration {
    let mut t: Vec<Duration> = (0..runs)
        .map(|_| {
            let t0 = Instant::now();
            f();
            t0.elapsed()
        })
        .collect();
    t.sort();
    t[runs / 2]
}

fn ingest() {
    let n = 16;
    let periods: Vec<u64> = (0..n as u64).map(|i| 40 + i * 1_000_003).collect();
    let samples: u64 = 1 << 27;
    let data = stream(n, samples / 64, &periods);
    let chans: Vec<u16> = (0..n as u16).collect();
    let t = median(7, || {
        // a cold start: nothing pooled (the pre-fault starts from scratch)
        dsview_store::logic::leaf_pool::release();
        let mut s = LogicStore::new();
        let mut first = true;
        for p in data.chunks(PACKET) {
            if first {
                s.first_payload(p, samples, &chans, true);
                first = false;
            } else {
                s.append_payload(p);
            }
        }
        s.capture_ended();
        black_box(s.sample_count());
    });
    let mib = data.len() as f64 / (1 << 20) as f64;
    println!(
        "ingest 16 ch × 128M samples, first capture: {:.1} ms, {:.0} MiB/s (gate ≥ 1 GB/s = 954 MiB/s)",
        t.as_secs_f64() * 1e3,
        mib / t.as_secs_f64()
    );
    // a repeated capture with the same settings: `first_payload` keeps the
    // allocations and zeroes them (DSView's repeat and loop captures)
    let mut s = LogicStore::new();
    let t = median(7, || {
        let mut first = true;
        for p in data.chunks(PACKET) {
            if first {
                s.first_payload(p, samples, &chans, true);
                first = false;
            } else {
                s.append_payload(p);
            }
        }
        s.capture_ended();
        black_box(s.sample_count());
    });
    println!(
        "ingest 16 ch × 128M samples, repeated capture: {:.1} ms, {:.0} MiB/s",
        t.as_secs_f64() * 1e3,
        mib / t.as_secs_f64()
    );
}

fn display(which: &str) {
    let samples: u64 = 1 << 28;
    // ch 0: fast clock (every leaf has toggles), ch 1: slow clock (mostly
    // constant leaves)
    let data = stream(2, samples / 64, &[997, 9_000_001]);
    let mut rs = LogicStore::new();
    for (k, p) in data.chunks(PACKET).enumerate() {
        if k == 0 {
            rs.first_payload(p, samples, &[0, 1], true);
        } else {
            rs.append_payload(p);
        }
    }
    rs.capture_ended();
    if which != "edges" {
        let width: u16 = 1920;
        for (name, start, spp) in [
            ("all", 0u64, samples as f64 / width as f64),
            ("mid_zoom", samples / 3, 4096.0),
            ("fine_zoom", samples / 2 + 12345, 2.5),
        ] {
            let end = (start + (spp * width as f64) as u64).min(samples - 1);
            for ch in [0u16, 1] {
                let px = (start as f64 / spp).floor();
                let (mut e, mut t) = (Vec::new(), Vec::new());
                let d = median(51, || {
                    rs.get_display_edges(&mut e, &mut t, start, end, width, 500, px, spp, ch);
                    black_box(e.len());
                });
                println!(
                    "display edges 256M samples, 1920 px, {name}/ch{ch}: {:.1} µs",
                    d.as_secs_f64() * 1e6
                );
            }
        }
    }
    if which != "display" {
        for ch in [0i32, 1] {
            let d = median(21, || {
                let (mut i, mut level, mut n) = (0u64, false, 0u32);
                while i < samples - 1 && n < 20_000 {
                    if !rs.get_nxt_edge(&mut i, level, samples - 1, 1.0, ch) {
                        break;
                    }
                    level = !level;
                    n += 1;
                }
                black_box(n);
            });
            println!(
                "20k next-edge steps, ch{ch}: {:.2} ms",
                d.as_secs_f64() * 1e3
            );
        }
    }
}

fn main() {
    // `cargo bench` passes `--bench`; an optional filter picks a part
    let filter = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .unwrap_or_default();
    if filter.is_empty() || filter == "ingest" {
        ingest();
    }
    if filter.is_empty() || filter == "display" || filter == "edges" {
        display(&filter);
    }
}
