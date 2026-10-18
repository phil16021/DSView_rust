//! Driver differential test (Phase 3): runs seeded scenarios through
//! libsigrok4DSL's public ds_* API, once with the C demo driver and once with
//! the Rust one behind rs_driver.c (DSVIEW_IMPL_DRV_DEMO=rust), both on the
//! same virtual clock and random sequence (src/drv.rs), and compares every
//! answer: config get/list values with their GVariant types, the channel
//! list, mode list, status block, and every datafeed packet.
//!
//! usage: drv-diff [--runs N] [--seed S] [--jobs J] [--keep DIR]
//!        drv-diff run --impl c|rust <scenario.json>   (one side, JSONL on stdout)
//!        drv-diff gen SEED                              (print scenario SEED)
//!
//! Each side runs in its own process (libsigrok keeps global state).

use std::ffi::{CString, c_void};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use dsview_core::key;
use dsview_oracle::drv::{self, OracleDrvPacket, take_string};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("run") => return run_side(&args[1..]),
        Some("gen") => {
            let seed = args.get(1).context("gen SEED")?.parse()?;
            let dir = std::env::temp_dir().join("drv-diff-files");
            std::fs::create_dir_all(&dir)?;
            let files = capture_files(&dir)?;
            println!("{}", serde_json::to_string_pretty(&generate(seed, &files))?);
            return Ok(());
        }
        _ => {}
    }
    let mut runs = 50u64;
    let mut seed = 1u64;
    let mut jobs = 4usize;
    let mut keep: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--runs" => runs = it.next().context("--runs N")?.parse()?,
            "--seed" => seed = it.next().context("--seed S")?.parse()?,
            "--jobs" => jobs = it.next().context("--jobs J")?.parse()?,
            "--keep" => keep = Some(PathBuf::from(it.next().context("--keep DIR")?)),
            "-h" | "--help" => {
                println!("usage: drv-diff [--runs N] [--seed S] [--jobs J] [--keep DIR]");
                return Ok(());
            }
            other => bail!("unexpected argument {other}"),
        }
    }
    let dir = match &keep {
        Some(d) => {
            std::fs::create_dir_all(d)?;
            d.clone()
        }
        None => std::env::temp_dir().join(format!("drv-diff-{}", std::process::id())),
    };
    std::fs::create_dir_all(&dir)?;

    let files = capture_files(&dir)?;
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let stats = Mutex::new((0usize, 0usize));
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed) as u64;
                    if i >= runs {
                        break;
                    }
                    let run_seed = seed + i;
                    match one(run_seed, &dir, &files) {
                        Ok((lines, packets)) => {
                            let mut st = stats.lock().unwrap();
                            st.0 += lines;
                            st.1 += packets;
                        }
                        Err(e) => {
                            eprintln!("seed {run_seed}: {e:#}");
                            failures.lock().unwrap().push(run_seed);
                        }
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    let (lines, packets) = stats.into_inner().unwrap();
    println!(
        "{} runs, {} identical ({} answers, {} packets compared), {} differed{}",
        runs,
        runs as usize - failures.len(),
        lines,
        packets,
        failures.len(),
        if failures.is_empty() {
            String::new()
        } else {
            format!(": seeds {failures:?}")
        }
    );
    let crashes = C_CRASHES.lock().unwrap();
    if !crashes.is_empty() {
        println!(
            "{} runs where the C driver crashed or read undefined memory (Rust matched up to there and finished):",
            crashes.len()
        );
        for (seed, ctx) in crashes.iter() {
            println!("  seed {seed}: after {ctx}");
        }
    }
    if keep.is_none() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// Generates, runs and compares scenario `seed`; returns (lines, packets).
fn one(seed: u64, dir: &Path, files: &[PathBuf]) -> Result<(usize, usize)> {
    let scenario = generate(seed, files);
    let path = dir.join(format!("scenario-{seed}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&scenario)?)?;
    let exe = std::env::current_exe()?;
    let spawn = |imp: &str| {
        Command::new(&exe)
            .args(["run", "--impl", imp])
            .arg(&path)
            .output()
    };
    let (c, r) = std::thread::scope(|s| {
        let c = s.spawn(|| spawn("c"));
        let r = s.spawn(|| spawn("rust"));
        (c.join().unwrap(), r.join().unwrap())
    });
    let (c, r) = (c?, r?);
    // The C driver crashes on some inputs (DEVIATIONS.md, F18): then Rust must
    // match everything C printed before the crash and finish cleanly.
    let c_crashed = !c.status.success() && c.status.code().is_none() && r.status.success();
    for (name, out) in [("c", &c), ("rust", &r)] {
        if !out.status.success() && !(name == "c" && c_crashed) {
            bail!(
                "{name} side failed ({}): {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
        }
    }
    let c_text = String::from_utf8_lossy(&c.stdout);
    let r_text = String::from_utf8_lossy(&r.stdout);
    let (cl, rl): (Vec<&str>, Vec<&str>) = (c_text.lines().collect(), r_text.lines().collect());
    let packets = cl.iter().filter(|l| l.contains("\"pkt\":")).count();
    let mut context = "";
    let compared = if c_crashed {
        cl.len()
    } else {
        cl.len().max(rl.len())
    };
    for i in 0..compared {
        let (a, b) = (cl.get(i).copied(), rl.get(i).copied());
        if let Some(a) = a
            && a.starts_with("{\"op\"")
        {
            context = a;
        }
        if a != b && analog_overread(&cl[..i], a.unwrap_or("")) {
            // C reads past its pattern buffer here (F18): undefined data
            C_CRASHES
                .lock()
                .unwrap()
                .push((seed, format!("{context} (analog over-read, F18)")));
            return Ok((i, packets));
        }
        if a != b {
            std::fs::write(dir.join(format!("c-{seed}.jsonl")), c_text.as_bytes())?;
            std::fs::write(dir.join(format!("rust-{seed}.jsonl")), r_text.as_bytes())?;
            bail!(
                "line {}: after {context}\n  c:    {}\n  rust: {}",
                i + 1,
                a.unwrap_or("<end>"),
                b.unwrap_or("<end>")
            );
        }
    }
    let _ = std::fs::remove_file(&path);
    if c_crashed {
        C_CRASHES.lock().unwrap().push((seed, context.to_owned()));
    }
    Ok((cl.len(), packets))
}

/// True when `line` is a demo analog packet the C driver builds from beyond
/// its buffer: the packet (samplerate / 100 bytes) is longer than the random
/// buffer (1 MiB) or the expanded pattern (103/2048 * 2 bytes per sample,
/// whole 206-byte cycles). `before` holds the capture's context lines.
fn analog_overread(before: &[&str], line: &str) -> bool {
    if !line.contains("\"pkt\":10006") {
        return false;
    }
    let ctx = |k: i32| {
        before
            .iter()
            .rev()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .find(|v| v["ctx"].as_i64() == Some(i64::from(k)))
            .and_then(|v| v["v"].as_str().map(str::to_owned))
            .unwrap_or_default()
    };
    let num = |s: String| s.rsplit(' ').next().and_then(|n| n.parse::<u64>().ok());
    if ctx(key::DEVICE_MODE) != "0 int16 2" {
        return false;
    }
    let (Some(rate), Some(limit)) = (num(ctx(key::SAMPLERATE)), num(ctx(key::LIMIT_SAMPLES)))
    else {
        return false;
    };
    let buf = if ctx(key::PATTERN_MODE).contains("'random'") {
        // DSO_BUF_LEN of random samples
        1 << 20
    } else {
        (103.0 / 2048.0 * limit as f64 * 2.0) as u64 / 206 * 206
    };
    let mut packet = rate * 2 / 200;
    if packet % 2 != 0 {
        packet += 1;
    }
    packet + 1 >= buf
}

/// Seeds where the C driver crashed, with the last op before the crash.
static C_CRASHES: Mutex<Vec<(u64, String)>> = Mutex::new(Vec::new());

// --------------------------------------------------------------- scenarios

struct Rand(u64);

impl Rand {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

const PATTERNS: &[&str] = &[
    "random", "protocol", "sine", "square", "triangle", "sawtooth", "nosuch",
];
const RATES: &[u64] = &[
    10,
    100,
    1_000,
    50_000,
    200_000,
    1_000_000,
    5_000_000,
    10_000_000,
    50_000_000,
    100_000_000,
    125_000_000,
    250_000_000,
    1_000_000_000,
    2_000_000_000,
    7,
];
const TIMEBASES: &[u64] = &[
    10,
    500,
    2_000,
    20_000,
    100_000,
    1_000_000,
    10_000_000,
    100_000_000,
    400_000_000,
];
const VDIVS: &[u64] = &[10, 20, 50, 100, 200, 500, 1000, 2000];

/// A random config change in the GUI's style (sometimes with a wrong type).
fn random_set(r: &mut Rand) -> Value {
    // channel keys always name a channel: C dereferences a NULL one (the GUI
    // never passes NULL; the Rust driver's answer is a unit test)
    let ch = r.below(2) as i64;
    let (k, ch, v): (i32, i64, String) = match r.below(19) {
        0 => (key::DEVICE_MODE, -1, format!("int16 {}", r.below(3))),
        1 | 2 => (key::PATTERN_MODE, -1, format!("'{}'", r.pick(PATTERNS))),
        3 => (key::SAMPLERATE, -1, format!("uint64 {}", r.pick(RATES))),
        4 => (
            key::LIMIT_SAMPLES,
            -1,
            format!(
                "uint64 {}",
                r.pick(&[1u64, 1000, 4096, 100_000, 1_000_000, 3_333_333])
            ),
        ),
        5 => (key::CHANNEL_MODE, -1, format!("int16 {}", 15 + r.below(6))),
        6 => (key::TIMEBASE, -1, format!("uint64 {}", r.pick(TIMEBASES))),
        7 => (key::PROBE_VDIV, ch, format!("uint64 {}", r.pick(VDIVS))),
        8 => (key::PROBE_OFFSET, ch, format!("uint16 {}", r.below(300))),
        9 => (key::PROBE_COUPLING, ch, format!("byte {}", r.below(3))),
        10 => (key::INSTANT, -1, format!("{}", r.chance(50))),
        11 => (key::MAX_HEIGHT, -1, format!("'{}X'", 1 + r.below(6))),
        12 => (key::PROBE_MAP_DEFAULT, ch, format!("{}", r.chance(50))),
        13 => (
            key::PROBE_MAP_UNIT,
            ch,
            format!("'{}'", r.pick(&["V", "A", "g"])),
        ),
        14 => (
            key::PROBE_MAP_MIN,
            ch,
            format!("{:.3}", r.below(2000) as f64 / 7.0 - 100.0),
        ),
        15 => (key::LOOP_MODE, -1, format!("{}", r.chance(30))),
        16 => (key::HORIZ_TRIGGERPOS, -1, format!("byte {}", r.below(101))),
        17 => (key::PROBE_EN, ch, format!("{}", r.chance(60))),
        // wrong GVariant types: GLib's getters yield zero values
        _ => (
            *r.pick(&[
                key::SAMPLERATE,
                key::TIMEBASE,
                key::LIMIT_SAMPLES,
                key::INSTANT,
            ]),
            -1,
            "int32 5".to_string(),
        ),
    };
    json!({"op": "set", "key": k, "ch": ch, "value": v})
}

fn generate(seed: u64, files: &[PathBuf]) -> Value {
    let mut r = Rand(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    for _ in 0..4 {
        r.next();
    }
    if r.chance(30) {
        return generate_file(&mut r, seed, files);
    }
    let device = if r.chance(80) {
        "Demo Logic"
    } else {
        "Demo Oscilloscope"
    };
    let step_us = *r.pick(&[1i64, 7, 50, 400]);
    let mut ops = vec![
        json!({"op": "activate", "name": device}),
        json!({"op": "dump"}),
    ];
    // a target mode and pattern first, so every combination gets captured
    let mode = r.below(3);
    if mode != 0 || device != "Demo Logic" {
        ops.push(json!({"op": "set", "key": key::DEVICE_MODE, "ch": -1, "value": format!("int16 {mode}")}));
    }
    let pattern = match mode {
        0 => *r.pick(&["random", "random", "protocol"]),
        _ => *r.pick(&["random", "sine", "square", "triangle", "sawtooth"]),
    };
    ops.push(
        json!({"op": "set", "key": key::PATTERN_MODE, "ch": -1, "value": format!("'{pattern}'")}),
    );
    if mode == 0 && pattern == "random" {
        if r.chance(40) {
            ops.push(json!({"op": "set", "key": key::CHANNEL_MODE, "ch": -1, "value": format!("int16 {}", 16 + r.below(4))}));
        }
        ops.push(json!({"op": "set", "key": key::SAMPLERATE, "ch": -1, "value": format!("uint64 {}", r.pick(RATES))}));
        ops.push(json!({"op": "set", "key": key::LIMIT_SAMPLES, "ch": -1, "value": format!("uint64 {}", r.pick(&[1000u64, 50_000, 1_000_000, 16_000_000]))}));
    }
    if mode == 1 {
        ops.push(json!({"op": "set", "key": key::INSTANT, "ch": -1, "value": format!("{}", r.chance(40))}));
        ops.push(json!({"op": "set", "key": key::TIMEBASE, "ch": -1, "value": format!("uint64 {}", r.pick(TIMEBASES))}));
    }
    if mode == 2 {
        ops.push(json!({"op": "set", "key": key::SAMPLERATE, "ch": -1, "value": format!("uint64 {}", r.pick(RATES))}));
    }
    for _ in 0..3 + r.below(8) {
        match r.below(10) {
            0..=4 => ops.push(random_set(&mut r)),
            5 => ops.push(json!({"op": "enable", "ch": r.below(17) as i64, "on": r.chance(60)})),
            6 => ops.push(json!({"op": "dump"})),
            7 => {
                let other = if device == "Demo Logic" {
                    "Demo Oscilloscope"
                } else {
                    "Demo Logic"
                };
                ops.push(json!({"op": "activate", "name": other}));
                ops.push(json!({"op": "activate", "name": device}));
            }
            _ => ops.push(json!({"op": "capture", "stop_after": 1 + r.below(40)})),
        }
        if r.chance(40) {
            ops.push(json!({"op": "capture", "stop_after": 1 + r.below(25)}));
        }
    }
    ops.push(json!({"op": "capture", "stop_after": 1 + r.below(30)}));
    ops.push(json!({"op": "dump"}));
    json!({"seed": seed, "rng": r.next(), "step_us": step_us, "ops": ops})
}

/// A file-driver scenario: open a capture, query, change, replay it.
fn generate_file(r: &mut Rand, seed: u64, files: &[PathBuf]) -> Value {
    let path = r.pick(files).to_string_lossy().into_owned();
    let mut ops = vec![
        json!({"op": "open_file", "path": path}),
        json!({"op": "dump"}),
    ];
    for _ in 0..2 + r.below(6) {
        match r.below(8) {
            0..=2 => {
                let ch = r.below(2) as i64;
                let (k, ch, v): (i32, i64, String) = match r.below(9) {
                    0 => (key::PROBE_EN, ch, format!("{}", r.chance(50))),
                    1 => (key::PROBE_VDIV, ch, format!("uint64 {}", r.pick(VDIVS))),
                    2 => (key::PROBE_HW_OFFSET, ch, format!("uint16 {}", r.below(256))),
                    3 => (key::PROBE_MAP_UNIT, ch, format!("'{}'", r.pick(&["V", "A"]))),
                    4 => (key::MAX_HEIGHT, -1, format!("'{}X'", 1 + r.below(6))),
                    5 => (key::STATUS_MAX, ch, format!("byte {}", r.below(256))),
                    6 => (key::SAMPLERATE, -1, format!("uint64 {}", r.pick(RATES))),
                    7 => (key::PATTERN_MODE, -1, "'sine'".to_string()),
                    _ => (key::LIMIT_SAMPLES, -1, "int32 5".to_string()),
                };
                ops.push(json!({"op": "set", "key": k, "ch": ch, "value": v}));
            }
            3 => ops.push(json!({"op": "enable", "ch": r.below(17) as i64, "on": r.chance(50)})),
            4 => ops.push(json!({"op": "dump"})),
            5 => {
                ops.push(json!({"op": "activate", "name": "Demo Logic"}));
                ops.push(json!({"op": "open_file", "path": path}));
            }
            _ => ops.push(json!({"op": "capture", "stop_after": if r.chance(70) { 0 } else { 1 + r.below(5) }})),
        }
    }
    ops.push(json!({"op": "capture", "stop_after": 0}));
    ops.push(json!({"op": "dump"}));
    json!({"seed": seed, "rng": r.next(), "step_us": 1, "ops": ops})
}

/// The demo captures plus synthesized ones: logic v1/v2/v3 with several and
/// odd-sized blocks, a DSO capture with a trigger position, a capture with a
/// missing block.
fn capture_files(dir: &Path) -> Result<Vec<PathBuf>> {
    use std::io::Write as _;
    let root = dsview_oracle::repo_root();
    let mut files: Vec<PathBuf> = [
        "logic/protocol",
        "dso/sine",
        "dso/square",
        "analog/sine",
        "analog/sawtooth",
    ]
    .iter()
    .map(|f| root.join(format!("DSView/demo/{f}.demo")))
    .collect();

    let write = |name: &str, entries: &[(String, Vec<u8>)]| -> Result<PathBuf> {
        let path = dir.join(name);
        let mut z = zip::ZipWriter::new(std::fs::File::create(&path)?);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (n, d) in entries {
            z.start_file(n.as_str(), opts)?;
            z.write_all(d)?;
        }
        z.finish()?;
        Ok(path)
    };
    let pattern = |seed: u64, n: usize| -> Vec<u8> {
        let mut x = seed | 1;
        (0..n)
            .map(|i| {
                if i % 64 == 0 {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                }
                // runs of equal bytes, like real captures
                if (x >> (i % 64)) & 1 == 1 {
                    0xff
                } else {
                    (x >> 8) as u8
                }
            })
            .collect()
    };

    // logic, three blocks of 4 KiB per channel, probes 0 1 3 4 7, 97000 samples
    for version in [2u32, 3] {
        let probes = [(0, "A"), (1, "B"), (3, "C"), (4, "D"), (7, "E")];
        let mut header = String::new();
        header += &format!(
            "[version]\nversion = {version}\n[header]\ndriver = DSLogic\ndevice mode = 0\n"
        );
        header += "capturefile = data\ntotal samples = 97000\ntotal probes = 5\ntotal blocks = 3\n";
        header += "samplerate = 50 MHz\ntrigger time = 1700000000000\ntrigger pos = 1234\n";
        for (i, (_, name)) in probes.iter().enumerate() {
            header += &format!("probe{} = {name}\n", probes[i].0);
        }
        let mut entries = vec![("header".to_string(), header.into_bytes())];
        for (dir_i, (idx, _)) in probes.iter().enumerate() {
            for b in 0..3 {
                let len = if b == 2 { 4096 - 512 } else { 4096 };
                entries.push((
                    format!("L-{idx}/{b}"),
                    pattern((dir_i * 10 + b) as u64 + 7, len),
                ));
            }
        }
        files.push(write(&format!("logic-v{version}.dsl"), &entries)?);
        if version == 3 {
            entries.retain(|(n, _)| n != "L-1/1");
            files.push(write("logic-broken.dsl", &entries)?);
        }
    }

    // logic version 1: one `data` entry of 16-bit sample units
    let header = "[header]\ndriver = DSLogic\ndevice mode = 0\ncapturefile = data\n\
        total samples = 3000\ntotal probes = 4\nsamplerate = 1 MHz\nunitsize = 2\n\
        probe1 = a\nprobe2 = b\nprobe3 = c\nprobe4 = d\n";
    files.push(write(
        "logic-v1.dsl",
        &[
            ("header".into(), header.as_bytes().to_vec()),
            ("data".into(), pattern(99, 6016)),
        ],
    )?);

    // DSO with a trigger position
    let mut z = zip::ZipArchive::new(std::fs::File::open(root.join("DSView/demo/dso/sine.demo"))?)?;
    let mut entries = Vec::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let name = f.name()?.into_owned();
        let mut d = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut d)?;
        if name == "header" {
            d = String::from_utf8_lossy(&d)
                .replace("trigger pos = 0", "trigger pos = 2500")
                .into_bytes();
        }
        entries.push((name, d));
    }
    files.push(write("dso-trig.dsl", &entries)?);
    Ok(files)
}

// ------------------------------------------------------------- one side

/// Lines go out as they are made, so a side that crashes leaves its prefix.
fn emit(line: String) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{line}");
}

extern "C" fn on_packet(_user: *mut c_void, p: *const OracleDrvPacket) {
    let p = unsafe { &*p };
    let data: &[u8] = if p.data.is_null() || p.len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(p.data.cast(), p.len as usize) }
    };
    let sha = Sha256::digest(data);
    let hex: String = sha[..8].iter().map(|b| format!("{b:02x}")).collect();
    emit(
        json!({
            "pkt": p.ty, "st": p.status, "len": p.len, "ns": p.num_samples,
            "fmt": p.format, "idx": p.index, "ord": p.order, "us": p.unitsize,
            "derr": p.data_error, "epat": p.error_pattern, "mq": p.mq, "unit": p.unit,
            "mqf": p.mqflags, "stog": p.samplerate_tog, "tf": p.trig_flag, "tch": p.trig_ch,
            "ub": p.unit_bits, "up": p.unit_pitch, "np": p.nprobes, "sha": hex,
            "fv": p.feed_version, "trig": p.trig_real_pos,
        })
        .to_string(),
    );
}

fn channel_count() -> i64 {
    let state = unsafe { take_string(drv::oracle_drv_state()) };
    state.lines().filter(|l| l.starts_with(" ch ")).count() as i64
}

fn dump() {
    let state = unsafe { take_string(drv::oracle_drv_state()) };
    for l in state.lines() {
        emit(json!({"state": l}).to_string());
    }
    let n = channel_count();
    for &(name, k) in key::ALL {
        for ch in -1..n {
            let v = unsafe { take_string(drv::oracle_drv_get(k, ch as i32)) };
            if !v.starts_with("6 ") {
                // SR_ERR_NA answers are the same for every key; keep the rest
                emit(json!({"get": name, "ch": ch, "v": v}).to_string());
            }
        }
        let v = unsafe { take_string(drv::oracle_drv_list(k)) };
        emit(json!({"list": name, "v": v}).to_string());
    }
}

fn run_side(args: &[String]) -> Result<()> {
    let mut imp = "c".to_string();
    let mut path = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--impl" => imp = it.next().context("--impl c|rust")?.clone(),
            p => path = Some(PathBuf::from(p)),
        }
    }
    let scenario: Value = serde_json::from_slice(&std::fs::read(path.context("scenario path")?)?)?;
    // read by sr_driver_list() on its first call, inside ds_lib_init
    unsafe {
        std::env::set_var("DSVIEW_IMPL_DRV_DEMO", &imp);
        std::env::set_var("DSVIEW_IMPL_DRV_FILE", &imp);
    }
    drv::install(
        scenario["rng"].as_u64().unwrap_or(1),
        scenario["step_us"].as_i64().unwrap_or(1),
    );
    let user_dir = CString::new(
        dsview_oracle::repo_root()
            .join("DSView")
            .to_string_lossy()
            .as_bytes(),
    )?;
    let log_level = std::env::var("DRV_DIFF_LOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let rc = unsafe { drv::oracle_drv_init(user_dir.as_ptr(), log_level) };
    if rc != 0 {
        bail!("ds_lib_init: {rc}");
    }

    for op in scenario["ops"].as_array().context("ops")? {
        emit(json!({"op": op}).to_string());
        match op["op"].as_str().unwrap_or("") {
            "activate" => {
                let name = CString::new(op["name"].as_str().unwrap_or(""))?;
                let rc = unsafe { drv::oracle_drv_activate(name.as_ptr()) };
                emit(json!({"rc": rc}).to_string());
            }
            "open_file" => {
                let path = CString::new(op["path"].as_str().unwrap_or(""))?;
                let rc = unsafe { drv::oracle_drv_open_file(path.as_ptr()) };
                emit(json!({"rc": rc}).to_string());
            }
            "dump" => dump(),
            "set" => {
                let v = CString::new(op["value"].as_str().unwrap_or(""))?;
                let rc = unsafe {
                    drv::oracle_drv_set(
                        op["key"].as_i64().unwrap_or(0) as i32,
                        op["ch"].as_i64().unwrap_or(-1) as i32,
                        v.as_ptr(),
                    )
                };
                emit(json!({"rc": rc}).to_string());
            }
            "enable" => {
                let rc = unsafe {
                    drv::oracle_drv_enable(
                        op["ch"].as_i64().unwrap_or(0) as i32,
                        op["on"].as_bool().unwrap_or(true) as i32,
                    )
                };
                emit(json!({"rc": rc}).to_string());
            }
            "capture" => {
                for k in [
                    key::DEVICE_MODE,
                    key::PATTERN_MODE,
                    key::INSTANT,
                    key::SAMPLERATE,
                    key::LIMIT_SAMPLES,
                ] {
                    let v = unsafe { take_string(drv::oracle_drv_get(k, -1)) };
                    emit(json!({"ctx": k, "v": v}).to_string());
                }
                let rc = unsafe {
                    drv::oracle_drv_capture(
                        op["stop_after"].as_i64().unwrap_or(1) as i32,
                        on_packet,
                        std::ptr::null_mut(),
                    )
                };
                emit(json!({"rc": rc}).to_string());
                let state = unsafe { take_string(drv::oracle_drv_state()) };
                for l in state.lines() {
                    emit(json!({"state": l}).to_string());
                }
            }
            other => bail!("unknown op {other}"),
        }
    }
    unsafe { drv::oracle_drv_exit() };
    Ok(())
}
