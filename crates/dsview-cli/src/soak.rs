//! `dsview-cli soak`: long captures with the resident memory watched. After
//! a warm-up (5 minutes, or half the run) the resident set is sampled every
//! 5 s; the growth is the least-squares trend of those samples over the run.

use std::time::{Duration, Instant};

use dsview_core::{ConfValue, key};
use dsview_session::session::CollectMode;
use dsview_session::{Kind, Session};

/// Resident set size in KiB (`/proc/self/statm`).
fn rss_kib() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    // SAFETY: sysconf has no preconditions
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as u64;
    pages * page / 1024
}

fn select(s: &Session, device: &str) -> bool {
    let (list, _) = s.devices();
    let want = |k: &Kind| match device {
        "demo" => *k == Kind::Demo,
        _ => matches!(k, Kind::Usb { .. }),
    };
    // the demo logic analyzer (the demo oscilloscope comes first)
    let pick = list
        .iter()
        .rev()
        .find(|e| want(&e.kind) && (device != "demo" || e.name.contains("Logic")));
    match pick {
        Some(e) => s.set_device(e.handle),
        None => false,
    }
}

/// Runs the soak; true when it passed (the capture ran throughout, the
/// memory trend stays under 1 %). `samples_file` receives "seconds rss_kib" lines.
pub fn run(s: &Session, mode: &str, minutes: f64, device: &str, samples_file: Option<&str>) -> bool {
    if !select(s, device) {
        eprintln!("soak: no {device} device");
        return false;
    }
    let collect = match mode {
        "loop" => CollectMode::Loop,
        "repeat" => CollectMode::Repeat,
        _ => {
            eprintln!("soak: --mode must be loop or repeat");
            return false;
        }
    };
    if device == "demo" {
        s.set_demo_pattern(if collect == CollectMode::Loop {
            "random"
        } else {
            "protocol"
        });
    } else if collect == CollectMode::Loop {
        // a hardware loop needs stream mode; 1 MHz keeps USB 2 far from overflow
        s.set_config(key::OPERATION_MODE, ConfValue::I16(1), None);
        s.reload_signals();
        s.set_config(key::SAMPLERATE, ConfValue::U64(1_000_000), None);
        s.set_config(key::LIMIT_SAMPLES, ConfValue::U64(10_000_000), None);
        s.update_sample_rate_list();
        s.commit_settings();
    }
    s.set_collect_mode(collect, 0.0);
    if !s.start_capture(false) {
        eprintln!("soak: the capture did not start");
        return false;
    }
    let t0 = Instant::now();
    let total = Duration::from_secs_f64(minutes * 60.0);
    // the ring buffer and the decoders' Python state fill up first
    let warmup = Duration::from_secs(300).min(total / 2);
    let mut samples = Vec::new();
    let mut next_sample = Duration::from_secs(5);
    let mut next_print = Duration::from_secs(60).min(total);
    println!("minute rss_kib captures decoders_done");
    while t0.elapsed() < total {
        std::thread::sleep(Duration::from_millis(200));
        if !s.is_working() {
            eprintln!("soak: the capture stopped after {:?}", t0.elapsed());
            return false;
        }
        if t0.elapsed() >= next_sample {
            if t0.elapsed() >= warmup {
                samples.push(rss_kib());
            }
            next_sample += Duration::from_secs(5);
        }
        if t0.elapsed() >= next_print {
            let rss = rss_kib();
            let st = s.state();
            let done = st
                .decoders
                .iter()
                .filter(|d| d.stack.progress() == 100)
                .count();
            println!(
                "{:6.1} {rss} {} {done}",
                t0.elapsed().as_secs_f64() / 60.0,
                st.captures
            );
            drop(st);
            next_print += Duration::from_secs(60);
        }
    }
    s.stop_capture();
    let captures = s.state().captures;
    if samples.len() < 8 {
        eprintln!("soak: too short to measure");
        return false;
    }
    // Single samples swing: repeat captures allocate and free per round, and
    // a loop capture's ring holds one row of leaves more or less depending on
    // where it stands. The trend is the least-squares slope over all samples,
    // as growth over the measured span relative to the mean.
    let n = samples.len() as f64;
    let xs: Vec<f64> = (0..samples.len()).map(|i| i as f64 * 5.0).collect();
    let ys: Vec<f64> = samples.iter().map(|&v| v as f64).collect();
    let (mx, my) = (xs.iter().sum::<f64>() / n, ys.iter().sum::<f64>() / n);
    let cov: f64 = xs.iter().zip(&ys).map(|(x, y)| (x - mx) * (y - my)).sum();
    let var: f64 = xs.iter().map(|x| (x - mx) * (x - mx)).sum();
    let slope = cov / var;
    let span = xs[xs.len() - 1] - xs[0];
    let growth = slope * span / my * 100.0;
    let (lo, hi) = (
        samples.iter().min().copied().unwrap_or(0),
        samples.iter().max().copied().unwrap_or(0),
    );
    println!(
        "soak {mode} on {device}: {captures} capture(s), rss mean {:.0} KiB, range {lo}..{hi} KiB, \
         trend {growth:+.2} % over {:.0} min ({} samples)",
        my,
        span / 60.0,
        samples.len()
    );
    if let Some(path) = samples_file {
        let text: String = xs
            .iter()
            .zip(&samples)
            .map(|(x, v)| format!("{} {v}\n", x + warmup.as_secs_f64()))
            .collect();
        let _ = std::fs::write(path, text);
    }
    growth < 1.0
}
