//! `dsview-cli soak`: long captures with the resident memory watched. The
//! growth is measured from the first sample after the warm-up (5 minutes,
//! or half the run) to the last.

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

/// Runs the soak; true when it passed (captures ran, memory growth < 1 %).
pub fn run(s: &Session, mode: &str, minutes: f64, device: &str) -> bool {
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
    let mut baseline = None;
    let mut last = 0;
    let mut next = Duration::from_secs(60).min(total);
    println!("minute rss_kib captures decoders_done");
    while t0.elapsed() < total {
        std::thread::sleep(Duration::from_millis(200));
        if !s.is_working() {
            eprintln!("soak: the capture stopped after {:?}", t0.elapsed());
            return false;
        }
        if t0.elapsed() >= next {
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
            if t0.elapsed() >= warmup {
                baseline.get_or_insert(rss);
            }
            last = rss;
            next += Duration::from_secs(60);
        }
    }
    s.stop_capture();
    let captures = s.state().captures;
    let Some(base) = baseline else {
        eprintln!("soak: too short to measure");
        return false;
    };
    let growth = (last as f64 - base as f64) / base as f64 * 100.0;
    println!(
        "soak {mode} on {device}: {captures} capture(s), rss {base} -> {last} KiB ({growth:+.2} %)"
    );
    growth < 1.0
}
