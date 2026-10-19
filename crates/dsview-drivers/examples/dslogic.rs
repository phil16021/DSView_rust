//! Drives a DSLogic through the Rust driver alone (no GUI, no C):
//!
//!   dslogic capture [--rate HZ] [--samples N] [--channels N] [--mode stream|buffer|intest]
//!                   [--stop-ms MS] [--dump FILE]
//!   dslogic soak [--cycles N]      start/stop cycles at random rates and stop times
//!
//! `--mode intest` is the FPGA's internal test mode; the capture is checked
//! against its counter pattern. Needs DS_RES_PATH-style resources only when
//! the FPGA is not configured yet (`--res DIR`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use dsview_core::{Body, ConfValue, Packet, key};
use dsview_drivers::dsl::dslogic::{self, LO_OP_BUFFER, LO_OP_INTEST, Probed};
use dsview_drivers::{Device, Step, sys};
use dsview_usb::nusb_backend::NusbBackend;

struct Args {
    rate: u64,
    samples: u64,
    channels: usize,
    mode: String,
    stop_ms: Option<u64>,
    dump: Option<String>,
    cycles: u32,
}

fn parse() -> (String, Args) {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_else(|| "capture".into());
    let mut a = Args {
        rate: 1_000_000,
        samples: 1_000_000,
        channels: 16,
        mode: "stream".into(),
        stop_ms: None,
        dump: None,
        cycles: 100,
    };
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        match k.as_str() {
            "--rate" => a.rate = v.parse().unwrap(),
            "--samples" => a.samples = v.parse().unwrap(),
            "--channels" => a.channels = v.parse().unwrap(),
            "--mode" => a.mode = v,
            "--stop-ms" => a.stop_ms = Some(v.parse().unwrap()),
            "--dump" => a.dump = Some(v),
            "--cycles" => a.cycles = v.parse().unwrap(),
            "--res" => sys::set_res_dir(v.into()),
            other => panic!("unknown option {other}"),
        }
    }
    (cmd, a)
}

fn open() -> Box<dyn Device> {
    let backend = Arc::new(NusbBackend);
    let (list, _) = dslogic::scan(backend.as_ref());
    let c = list.first().expect("no DSLogic found");
    let mut d = match dslogic::probe(backend.clone(), c) {
        Probed::Device(d) => d,
        Probed::FirmwareUploaded => panic!("firmware uploaded; run again once it re-enumerated"),
        Probed::NotReady => panic!("device does not answer"),
    };
    d.open().expect("open");
    d
}

/// Runs one capture; returns the packets and how it ended.
fn capture(d: &mut dyn Device, stop: Option<Duration>) -> (Vec<Packet>, Duration) {
    let mut out = Vec::new();
    d.start(&mut out).expect("start");
    let t0 = Instant::now();
    loop {
        let abort = stop.is_some_and(|s| t0.elapsed() >= s);
        let step = d.poll(abort, &mut out);
        if step == Step::Remove {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "capture did not end within 60 s"
        );
    }
    d.stop(&mut out).unwrap();
    (out, t0.elapsed())
}

fn logic_bytes(out: &[Packet]) -> Vec<u8> {
    out.iter()
        .filter_map(|p| match &p.body {
            Body::Logic { data, .. } => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
}

/// Splits LA_CROSS data into per-channel sample bits.
fn channel_bits(data: &[u8], n: usize) -> Vec<Vec<bool>> {
    let mut chans = vec![Vec::new(); n];
    for (i, w) in data.as_chunks::<8>().0.iter().enumerate() {
        let word = u64::from_le_bytes(*w);
        for b in 0..64 {
            chans[i % n].push(word >> b & 1 == 1);
        }
    }
    chans
}

/// Internal test mode: the samples form a binary counter across the
/// channels (channel k = counter bit k). Returns the number of samples
/// that break the count.
fn verify_counter(data: &[u8], n: usize) -> (usize, usize) {
    let chans = channel_bits(data, n);
    let len = chans.iter().map(Vec::len).min().unwrap_or(0);
    let value = |s: usize| (0..n).fold(0u32, |v, k| v | (u32::from(chans[k][s]) << k));
    let mask = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
    let mut bad = 0;
    for s in 1..len {
        if value(s) != value(s - 1).wrapping_add(1) & mask {
            bad += 1;
        }
    }
    (len, bad)
}

fn main() {
    let (cmd, a) = parse();
    if std::env::var("DSLOGIC_LOG").is_ok() {
        sys::set_log_sink(Box::new(|l, m| eprintln!("drv[{l}]: {m}")));
    }
    let mut d = open();
    match cmd.as_str() {
        "capture" => {
            let op = match a.mode.as_str() {
                "buffer" => LO_OP_BUFFER,
                "intest" => LO_OP_INTEST,
                _ => dslogic::LO_OP_STREAM,
            };
            d.config_set(key::OPERATION_MODE, &ConfValue::I16(op), None)
                .unwrap();
            if a.mode != "intest" {
                let n = d.channels().len();
                for i in 0..n {
                    d.config_set(key::PROBE_EN, &ConfValue::Bool(i < a.channels), Some(i))
                        .unwrap();
                }
                d.config_set(key::SAMPLERATE, &ConfValue::U64(a.rate), None)
                    .unwrap();
                d.config_set(key::LIMIT_SAMPLES, &ConfValue::U64(a.samples), None)
                    .unwrap();
            }
            let rate = d
                .config_get(key::SAMPLERATE, None)
                .unwrap()
                .unwrap()
                .get_u64();
            let limit = d
                .config_get(key::LIMIT_SAMPLES, None)
                .unwrap()
                .unwrap()
                .get_u64();
            let en = d.channels().iter().filter(|c| c.enabled).count();
            println!("mode {}: {en} channels, {rate} Hz, {limit} samples", a.mode);
            let (out, took) = capture(d.as_mut(), a.stop_ms.map(Duration::from_millis));
            let data = logic_bytes(&out);
            let kinds: Vec<String> = out
                .iter()
                .map(|p| format!("{}", p.type_id()))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            println!(
                "{} packets ({}), {} bytes = {} samples, in {:.3} s, last {:?}",
                out.len(),
                kinds.join(","),
                data.len(),
                data.len() * 8 / en.max(1),
                took.as_secs_f64(),
                out.last().map(|p| &p.body)
            );
            if a.mode == "intest" {
                let (n, bad) = verify_counter(&data, en);
                println!("counter check: {n} samples, {bad} breaks");
            }
            if let Some(f) = a.dump {
                std::fs::write(f, &data).unwrap();
            }
        }
        "soak" => {
            let rates = [1_000_000u64, 5_000_000, 10_000_000, 20_000_000];
            let mut x = 0x9E37_79B9u64;
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            let t0 = Instant::now();
            for i in 0..a.cycles {
                let rate = rates[(next() % 4) as usize];
                d.config_set(key::SAMPLERATE, &ConfValue::U64(rate), None)
                    .unwrap();
                d.config_set(key::LIMIT_SAMPLES, &ConfValue::U64(rate), None)
                    .unwrap();
                let stop = match next() % 4 {
                    0 => None,
                    k => Some(Duration::from_millis(next() % (300 * k))),
                };
                let (out, took) = capture(d.as_mut(), stop);
                let ends = out.iter().filter(|p| p.body == Body::End).count();
                assert_eq!(ends, 1, "cycle {i}: {ends} END packets");
                if i % 50 == 0 || took > Duration::from_secs(5) {
                    println!(
                        "cycle {i}: {rate} Hz, stop {stop:?}, {} bytes, {:.3} s",
                        logic_bytes(&out).len(),
                        took.as_secs_f64()
                    );
                }
            }
            println!(
                "{} cycles in {:.1} s, all ended",
                a.cycles,
                t0.elapsed().as_secs_f64()
            );
        }
        other => panic!("unknown command {other}"),
    }
    d.close().unwrap();
}
