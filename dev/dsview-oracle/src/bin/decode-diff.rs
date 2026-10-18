//! Differential test of the decoder engines: random captures with one random
//! stack per decoder, decoded by `oracle-decode` (C) and `rust-decode`
//! (Rust) with the same random chunk size; the JSON outputs must be equal.
//!
//! usage: decode-diff [--runs N] [--seed S] [--jobs J] [--max-samples M] [--keep DIR]
//!
//! Each run writes a .dsl with 16 signals (clocks, bursts, noise, slow
//! levels) and, for every decoder, a stack: logic decoders get random
//! channel mappings (optional channels sometimes left out) and option
//! values; stacked decoders sit on a random logic decoder whose output they
//! take. PYTHONHASHSEED is fixed for both engines.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use dsview_formats::dsl::{LogicWriter, WriteChannel};
use serde_json::{Value, json};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len() as u64) as usize]
    }
}

const SIGNALS: usize = 16;

/// Bit-packed signal of `n` samples.
fn signal(rng: &mut Rng, n: u64) -> Vec<u8> {
    let words = n.div_ceil(64) as usize;
    let mut bits = vec![0u8; words * 8];
    let set = |bits: &mut [u8], i: u64| bits[(i / 8) as usize] |= 1 << (i % 8);
    match rng.below(6) {
        // clock with jitter
        0 | 1 => {
            let max = rng.below(400) + 2;
            let half = 1 + rng.below(max);
            let mut level = rng.chance(0.5);
            let mut i = 0;
            while i < n {
                let len = (half + rng.below(3)).saturating_sub(1).max(1);
                if level {
                    for k in i..(i + len).min(n) {
                        set(&mut bits, k);
                    }
                }
                level = !level;
                i += len;
            }
        }
        // random bits at a random bit rate (UART-, SPI-data-like)
        2 | 3 => {
            let max = rng.below(300) + 2;
            let bit = 1 + rng.below(max);
            let mut i = 0;
            let idle_high = rng.chance(0.6);
            while i < n {
                let level = if rng.chance(0.05) {
                    idle_high
                } else {
                    rng.chance(0.5)
                };
                if level {
                    for k in i..(i + bit).min(n) {
                        set(&mut bits, k);
                    }
                }
                i += bit;
            }
        }
        // glitchy noise
        4 => {
            for k in 0..n {
                if rng.chance(0.3) {
                    set(&mut bits, k);
                }
            }
        }
        // slow levels (chip selects, enables), sometimes constant
        _ => {
            let mut level = rng.chance(0.5);
            let mut i = 0;
            while i < n {
                let len = 1 + rng.below(n / 3 + 1);
                if level {
                    for k in i..(i + len).min(n) {
                        set(&mut bits, k);
                    }
                }
                level = !level;
                i += len;
            }
        }
    }
    bits
}

/// Signals of a capture with real bus traffic on fixed channels: I2C on 0/1
/// (scl/sda), SPI on 4-7 (clk/mosi/miso/cs, mode 0, MSB first, CS active
/// low), UART 8N1 on 8. Stacks on these decoders are wired to them.
struct Buses {
    on: bool,
    uart_spb: u64,
}

const I2C_SCL: usize = 0;
const I2C_SDA: usize = 1;
const SPI_CLK: usize = 4;
const SPI_MOSI: usize = 5;
const SPI_MISO: usize = 6;
const SPI_CS: usize = 7;
const UART_TX: usize = 8;

fn pack(levels: &[bool], n: u64) -> Vec<u8> {
    let mut bits = vec![0u8; n.div_ceil(64) as usize * 8];
    for (i, &l) in levels.iter().take(n as usize).enumerate() {
        if l {
            bits[i / 8] |= 1 << (i % 8);
        }
    }
    bits
}

/// Two-wire bus levels built sample by sample.
struct Wave {
    a: Vec<bool>,
    b: Vec<bool>,
    la: bool,
    lb: bool,
}

impl Wave {
    fn new(n: u64) -> Wave {
        Wave {
            a: Vec::with_capacity(n as usize),
            b: Vec::with_capacity(n as usize),
            la: true,
            lb: true,
        }
    }
    fn hold(&mut self, k: u64) {
        for _ in 0..k {
            self.a.push(self.la);
            self.b.push(self.lb);
        }
    }
}

fn i2c_bus(rng: &mut Rng, n: u64) -> (Vec<u8>, Vec<u8>) {
    let half = 4 + rng.below(56);
    let addrs = [
        0x50u8, 0x51, 0x57, 0x68, 0x48, 0x1d, 0x53, 0x76, 0x77, 0x40, 0x20, 0x3c, 0x6f, 0x29,
    ];
    let mut w = Wave::new(n); // a = scl, b = sda
    while (w.a.len() as u64) < n {
        w.la = true;
        w.lb = true;
        w.hold(half * (2 + rng.below(40)));
        // START
        w.lb = false;
        w.hold(half);
        w.la = false;
        w.hold(half);
        let addr = if rng.chance(0.8) {
            *rng.pick(&addrs)
        } else {
            rng.below(128) as u8
        };
        let mut bytes = vec![(addr << 1) | rng.below(2) as u8];
        bytes.extend((0..1 + rng.below(8)).map(|_| rng.next() as u8));
        for (i, byte) in bytes.iter().enumerate() {
            for bit in (0..8).rev() {
                w.lb = byte >> bit & 1 == 1;
                w.hold(half / 2 + 1);
                w.la = true;
                w.hold(half);
                w.la = false;
                w.hold(half / 2 + 1);
            }
            // ACK (sometimes NACK on the last byte)
            w.lb = i + 1 == bytes.len() && rng.chance(0.3);
            w.hold(half / 2 + 1);
            w.la = true;
            w.hold(half);
            w.la = false;
            w.hold(half / 2 + 1);
        }
        // STOP (sometimes a repeated START instead)
        w.lb = false;
        w.hold(half);
        w.la = true;
        w.hold(half);
        if rng.chance(0.85) {
            w.lb = true;
        }
        w.hold(half);
    }
    (pack(&w.a, n), pack(&w.b, n))
}

fn spi_bus(rng: &mut Rng, n: u64) -> [Vec<u8>; 4] {
    let half = 2 + rng.below(38);
    let (mut clk, mut mosi, mut miso, mut cs) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let push = |c: bool, o: bool, i: bool, s: bool, k: u64, v: &mut [&mut Vec<bool>; 4]| {
        for _ in 0..k {
            v[0].push(c);
            v[1].push(o);
            v[2].push(i);
            v[3].push(s);
        }
    };
    while (clk.len() as u64) < n {
        let mut v = [&mut clk, &mut mosi, &mut miso, &mut cs];
        push(
            false,
            false,
            false,
            true,
            half * (2 + rng.below(60)),
            &mut v,
        );
        push(false, false, false, false, half, &mut v);
        for _ in 0..1 + rng.below(16) {
            let (o, i) = (rng.next() as u8, rng.next() as u8);
            for bit in (0..8).rev() {
                let (bo, bi) = (o >> bit & 1 == 1, i >> bit & 1 == 1);
                push(false, bo, bi, false, half, &mut v);
                push(true, bo, bi, false, half, &mut v);
            }
        }
        push(false, false, false, false, half, &mut v);
    }
    [pack(&clk, n), pack(&mosi, n), pack(&miso, n), pack(&cs, n)]
}

fn uart_line(rng: &mut Rng, n: u64, spb: u64) -> Vec<u8> {
    let mut l = Vec::with_capacity(n as usize);
    let push = |v: bool, k: u64, l: &mut Vec<bool>| l.extend(std::iter::repeat_n(v, k as usize));
    while (l.len() as u64) < n {
        push(true, spb * rng.below(20), &mut l);
        let byte = if rng.chance(0.5) {
            0x20 + rng.below(95) as u8
        } else {
            rng.next() as u8
        };
        push(false, spb, &mut l);
        for bit in 0..8 {
            push(byte >> bit & 1 == 1, spb, &mut l);
        }
        push(true, spb, &mut l);
    }
    pack(&l, n)
}

fn option_value(rng: &mut Rng, opt: &Value) -> Option<Value> {
    let def = opt.get("def")?;
    let values = opt
        .get("values")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let v = if !values.is_empty() && rng.chance(0.7) {
        rng.pick(&values).clone()
    } else {
        def.clone()
    };
    // saved form: strings as strings, numbers as numbers
    if let Some(s) = v.get("s") {
        Some(s.clone())
    } else if let Some(x) = v.get("x").and_then(Value::as_i64) {
        // sometimes perturb integers without a value list
        let x = if values.is_empty() && rng.chance(0.3) {
            (x + rng.below(5) as i64 - 2).max(0)
        } else {
            x
        };
        Some(json!(x))
    } else {
        v.get("d").cloned()
    }
}

fn options(rng: &mut Rng, dec: &Value) -> Value {
    let mut m = serde_json::Map::new();
    for o in dec
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if rng.chance(0.8)
            && let (Some(id), Some(v)) = (o.get("id").and_then(Value::as_str), option_value(rng, o))
        {
            m.insert(id.to_owned(), v);
        }
    }
    Value::Object(m)
}

fn is_logic(d: &Value) -> bool {
    d.get("inputs")
        .and_then(Value::as_array)
        .is_some_and(|i| i.iter().any(|x| x == "logic"))
}

fn bottom_entry(rng: &mut Rng, dec: &Value, buses: &Buses, samplerate: u64) -> Value {
    let id = dec["id"].as_str().unwrap_or("");
    if buses.on && rng.chance(0.85) {
        let wired = match id {
            "0:i2c" | "1:i2c" => Some((json!([{ "scl": I2C_SCL }, { "sda": I2C_SDA }]), json!({}))),
            "0:spi" | "1:spi" => Some((
                json!([{ "clk": SPI_CLK }, { "mosi": SPI_MOSI }, { "miso": SPI_MISO }, { "cs": SPI_CS }]),
                json!({ "cpol": 0, "cpha": 0, "bitorder": "msb-first", "wordsize": 8, "cs_polarity": "active-low" }),
            )),
            "0:uart" | "1:uart" => Some((
                json!([{ "rxtx": UART_TX }]),
                json!({ "baudrate": samplerate / buses.uart_spb, "num_data_bits": 8, "parity_type": "none",
                        "num_stop_bits": 1.0, "bit_order": "lsb-first", "invert": "no" }),
            )),
            _ => None,
        };
        if let Some((channels, mut opts)) = wired {
            // the other options still vary
            if let (Value::Object(o), Value::Object(r)) = (&mut opts, options(rng, dec)) {
                for (k, v) in r {
                    o.entry(k).or_insert(v);
                }
            }
            return json!({ "id": dec["id"], "channel": channels, "options": opts, "stacked decoders": [] });
        }
    }
    let mut sigs: Vec<usize> = (0..SIGNALS).collect();
    // shuffle
    for i in (1..sigs.len()).rev() {
        sigs.swap(i, rng.below(i as u64 + 1) as usize);
    }
    let mut next = sigs.into_iter();
    let mut channels = Vec::new();
    for c in dec
        .get("channels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(id), Some(s)) = (c.get("id").and_then(Value::as_str), next.next()) {
            channels.push(json!({ id: s }));
        }
    }
    for c in dec
        .get("opt_channels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if rng.chance(0.5)
            && let (Some(id), Some(s)) = (c.get("id").and_then(Value::as_str), next.next())
        {
            channels.push(json!({ id: s }));
        }
    }
    json!({
        "id": dec["id"],
        "channel": channels,
        "options": options(rng, dec),
        "stacked decoders": [],
    })
}

/// One stack per decoder (stacked decoders on a random provider).
fn stacks(rng: &mut Rng, decoders: &[Value], buses: &Buses, samplerate: u64) -> Vec<Value> {
    let mut out = Vec::new();
    for d in decoders {
        if is_logic(d) {
            out.push(bottom_entry(rng, d, buses, samplerate));
            continue;
        }
        let inputs: Vec<&Value> = d
            .get("inputs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        let providers: Vec<&Value> = decoders
            .iter()
            .filter(|p| {
                is_logic(p)
                    && p.get("outputs")
                        .and_then(Value::as_array)
                        .is_some_and(|o| o.iter().any(|x| inputs.contains(&x)))
            })
            .collect();
        if providers.is_empty() {
            continue;
        }
        // prefer the bus decoders, which then see real traffic
        let busy: Vec<&Value> = providers
            .iter()
            .copied()
            .filter(|p| {
                matches!(
                    p["id"].as_str(),
                    Some("0:i2c" | "1:i2c" | "0:spi" | "1:spi" | "1:uart")
                )
            })
            .collect();
        let p = if !busy.is_empty() && rng.chance(0.8) {
            *rng.pick(&busy)
        } else {
            *rng.pick(&providers)
        };
        let mut entry = bottom_entry(rng, p, buses, samplerate);
        entry["stacked decoders"] = json!([{ "id": d["id"], "options": options(rng, d) }]);
        out.push(entry);
    }
    out
}

/// Seconds an engine process may run. A process killed after finishing some
/// stacks is resumed after them (with 1-sample chunks a whole capture can
/// take longer); killed without finishing any, the per-stack watchdog
/// missed a hang. `DECODE_DIFF_TIME_LIMIT` overrides it.
fn time_limit() -> u64 {
    std::env::var("DECODE_DIFF_TIME_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(600)
}
/// Seconds one stack may take before the runner reports a hang.
const STACK_TIMEOUT: u64 = 10;
/// Address-space limit per engine process: a decoder looping forever while
/// growing a list must not take the machine down.
const MEMORY_LIMIT: u64 = 3 << 30;

#[repr(C)]
struct Rlimit {
    cur: u64,
    max: u64,
}

unsafe extern "C" {
    fn setrlimit(resource: i32, rlim: *const Rlimit) -> i32;
}

/// An engine's output; `killed` when it hit [`time_limit`] (the output is then
/// cut off anywhere).
struct Ran {
    out: String,
    killed: bool,
}

/// Index after the last stack `out` finished, or `first` when none.
fn stacks_done(out: &str, first: usize) -> usize {
    out.lines()
        .filter(|l| l.contains("\"t\":\"stack_end\""))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["index"].as_u64())
        .map(|i| i as usize + 1)
        .max()
        .unwrap_or(first)
}

/// The lines of `out` before stack `k` starts.
fn before_stack(out: &str, k: usize) -> String {
    let begin = format!("\"index\":{k},\"t\":\"stack\"");
    let mut s = String::new();
    for l in out.lines() {
        if l.contains(&begin) {
            break;
        }
        s.push_str(l);
        s.push('\n');
    }
    s
}

fn run_engine(bin: &Path, capture: &Path, max_chunk: u64, first: usize) -> Result<Ran> {
    run_engine_with(bin, capture, max_chunk, first, usize::MAX, STACK_TIMEOUT)
}

/// `count` stacks from `first`, each limited to `stack_timeout` seconds.
fn run_engine_with(
    bin: &Path,
    capture: &Path,
    max_chunk: u64,
    first: usize,
    count: usize,
    stack_timeout: u64,
) -> Result<Ran> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(bin);
    cmd.arg(capture)
        .args(["--max-chunk", &max_chunk.to_string()])
        .args([
            "--first",
            &first.to_string(),
            "--stack-timeout",
            &stack_timeout.to_string(),
            "--count",
            &count.min(1 << 30).to_string(),
        ])
        .env("PYTHONHASHSEED", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // SAFETY: only async-signal-safe setrlimit between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            const RLIMIT_AS: i32 = 9;
            let l = Rlimit {
                cur: MEMORY_LIMIT,
                max: MEMORY_LIMIT,
            };
            setrlimit(RLIMIT_AS, &l);
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("running {}", bin.display()))?;
    let mut stdout = child.stdout.take().context("stdout")?;
    let mut stderr = child.stderr.take().context("stderr")?;
    let reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        v
    });
    let err_reader = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr.read_to_end(&mut v);
        v
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(time_limit());
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break Some(st);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let out = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    let Some(status) = status else {
        return Ok(Ran {
            out: String::from_utf8_lossy(&out).into_owned(),
            killed: true,
        });
    };
    if !status.success() && status.code() != Some(dsview_oracle::watchdog::TIMEOUT_EXIT) {
        bail!(
            "{} exited with {}: {}",
            bin.display(),
            status,
            String::from_utf8_lossy(&err).lines().last().unwrap_or("")
        );
    }
    Ok(Ran {
        out: String::from_utf8_lossy(&out).into_owned(),
        killed: false,
    })
}

/// Masks object addresses ("at 0x7f...") in error texts.
fn normalize(line: &str) -> String {
    let mut s = String::with_capacity(line.len());
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'0' && i + 2 < b.len() && b[i + 1] == b'x' && b[i + 2].is_ascii_hexdigit() {
            let mut j = i + 2;
            while j < b.len() && b[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j - i - 2 >= 6 {
                s.push_str("0xADDR");
                i = j;
                continue;
            }
        }
        s.push(b[i] as char);
        i += 1;
    }
    s
}

struct Outcome {
    lines: usize,
    anns: usize,
    diff: Option<String>,
    /// Per decoder id: (stacks run, stacks where it annotated, stacks with an error).
    coverage: std::collections::BTreeMap<String, (usize, usize, usize)>,
    /// Decoders that looped forever in both engines.
    hung: Vec<String>,
    /// Index of the first differing line.
    diff_line: Option<usize>,
    /// The C and Rust lines at the index `one_run` was asked to probe.
    probe: (String, String),
}

/// A difference only counts when it shows again: CPython's type cache can
/// make even the C engine's output change between identical runs
/// (DEVIATIONS F17). Returns the outcome and, for a difference that went
/// away, the report with the engine(s) whose output changed and whether
/// Rust was one of them (Rust must be deterministic: that still fails).
fn checked_run(
    seed: u64,
    decoders: &[Value],
    bins: &(PathBuf, PathBuf),
    max_samples: u64,
    dir: &Path,
) -> Result<(Outcome, Option<(String, bool)>)> {
    let first = one_run(seed, decoders, bins, max_samples, dir, false, None)?;
    let (Some(d), Some(at)) = (first.diff.clone(), first.diff_line) else {
        return Ok((first, None));
    };
    for _ in 0..2 {
        let again = one_run(seed, decoders, bins, max_samples, dir, true, Some(at))?;
        if again.diff.is_none() {
            // which engine printed something else at the line that differed
            let c_changed = normalize(&again.probe.0) != normalize(&first.probe.0);
            let rust_changed = normalize(&again.probe.1) != normalize(&first.probe.1);
            let changed: Vec<&str> = [(c_changed, "C"), (rust_changed, "Rust")]
                .iter()
                .filter(|c| c.0)
                .map(|c| c.1)
                .collect();
            let report = format!("{d}\n  gone on a rerun; output changed in: {changed:?}");
            return Ok((again, Some((report, rust_changed))));
        }
    }
    Ok((first, None))
}

/// Coverage of one engine output: which decoder (the top of each stack)
/// produced annotations or errors.
fn coverage(out: &[&str]) -> std::collections::BTreeMap<String, (usize, usize, usize)> {
    let mut cov: std::collections::BTreeMap<String, (usize, usize, usize)> = Default::default();
    let mut top = String::new();
    let (mut annotated, mut errored) = (false, false);
    let flush =
        |top: &str,
         a: bool,
         e: bool,
         cov: &mut std::collections::BTreeMap<String, (usize, usize, usize)>| {
            if !top.is_empty() {
                let c = cov.entry(top.to_owned()).or_default();
                c.0 += 1;
                c.1 += usize::from(a);
                c.2 += usize::from(e);
            }
        };
    for l in out {
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        match v["t"].as_str() {
            Some("stack") => {
                flush(&top, annotated, errored, &mut cov);
                top = v["decoders"]
                    .as_array()
                    .and_then(|d| d.last())
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                annotated = false;
                errored = false;
            }
            Some("ann") => {
                if v["inst"]
                    .as_str()
                    .is_some_and(|i| i.rsplit_once('-').is_some_and(|(d, _)| d == top))
                {
                    annotated = true;
                }
            }
            Some("error") => errored = true,
            _ => {}
        }
    }
    flush(&top, annotated, errored, &mut cov);
    cov
}

fn one_run(
    seed: u64,
    decoders: &[Value],
    bins: &(PathBuf, PathBuf),
    max_samples: u64,
    dir: &Path,
    keep: bool,
    probe: Option<usize>,
) -> Result<Outcome> {
    let mut rng = Rng(seed);
    let total = 1000 + rng.below(max_samples.saturating_sub(1000));
    let samplerate = *rng.pick(&[
        1_000_000u64,
        2_000_000,
        4_000_000,
        10_000_000,
        16_000_000,
        25_000_000,
        50_000_000,
        100_000_000,
    ]);
    let mut signals: Vec<Vec<u8>> = (0..SIGNALS).map(|_| signal(&mut rng, total)).collect();
    let buses = Buses {
        on: rng.chance(0.8),
        uart_spb: 4 + rng.below(100),
    };
    if buses.on {
        let (scl, sda) = i2c_bus(&mut rng, total);
        signals[I2C_SCL] = scl;
        signals[I2C_SDA] = sda;
        let [clk, mosi, miso, cs] = spi_bus(&mut rng, total);
        signals[SPI_CLK] = clk;
        signals[SPI_MOSI] = mosi;
        signals[SPI_MISO] = miso;
        signals[SPI_CS] = cs;
        signals[UART_TX] = uart_line(&mut rng, total, buses.uart_spb);
    }
    let stacks = stacks(&mut rng, decoders, &buses, samplerate);
    let stacks_ids: Vec<String> = stacks
        .iter()
        .map(|e| {
            e["stacked decoders"][0]["id"]
                .as_str()
                .or(e["id"].as_str())
                .unwrap_or("?")
                .to_owned()
        })
        .collect();
    let max_chunk = *rng.pick(&[0u64, 0, 1, 7, 64, 333, 1000, 4096, 16384, 50_000]);

    let path = dir.join(format!("decode-diff-{seed}.dsl"));
    let writer = LogicWriter {
        driver: "virtual-demo".into(),
        samplerate,
        total_samples: total,
        trigger_time_ms: 0,
        trigger_pos: 0,
        channels: (0..SIGNALS as u16)
            .map(|i| WriteChannel {
                index: i,
                name: i.to_string(),
            })
            .collect(),
        decoders_json: Value::Array(stacks).to_string(),
        session_json: "{}".into(),
    };
    let refs: Vec<&[u8]> = signals.iter().map(Vec::as_slice).collect();
    writer.write(std::fs::File::create(&path)?, &refs)?;

    // run both engines; when a decoder loops forever in both, note it and
    // resume after that stack
    let mut c = String::new();
    let mut r = String::new();
    let mut hung = Vec::new();
    let mut first = 0usize;
    loop {
        let (ca, ra) = (
            run_engine(&bins.0, &path, max_chunk, first)?,
            run_engine(&bins.1, &path, max_chunk, first)?,
        );
        if ca.killed || ra.killed {
            // resume after the stacks every killed engine finished
            let k = [&ca, &ra]
                .iter()
                .filter(|x| x.killed)
                .map(|x| stacks_done(&x.out, first))
                .min()
                .unwrap_or(first);
            if k > first {
                c.push_str(&before_stack(&ca.out, k));
                r.push_str(&before_stack(&ra.out, k));
                first = k;
                continue;
            }
            let who = match (ca.killed, ra.killed) {
                (true, true) => "C and Rust",
                (true, false) => "C",
                _ => "Rust",
            };
            return Ok(Outcome {
                lines: 0,
                anns: 0,
                diff: Some(format!(
                    "seed {seed}: {who} hit the {} s limit inside stack {first}",
                    time_limit()
                )),
                coverage: Default::default(),
                hung: Vec::new(),
                diff_line: None,
                probe: Default::default(),
            });
        }
        let (cs, rs) = (ca.out, ra.out);
        let timeout = |o: &str| {
            o.lines()
                .last()
                .and_then(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|v| v["t"] == "timeout")
                .and_then(|v| v["index"].as_u64())
        };
        let (ct, rt) = (timeout(&cs), timeout(&rs));
        // only one engine ran out of time on stack k (it is merely slower):
        // run that stack alone in both, with a longer limit
        let slow = match (ct, rt) {
            (Some(k), None) | (None, Some(k)) => Some(k as usize),
            (Some(a), Some(b)) if a != b => Some(a.min(b) as usize),
            _ => None,
        };
        if let Some(k) = slow {
            let begin = format!("\"index\":{k},\"t\":\"stack\"");
            let cut = |o: &str| -> String {
                let mut out = String::new();
                for l in o.lines() {
                    if l.contains(&begin) || l.starts_with("{\"t\":\"timeout\"") {
                        break;
                    }
                    out.push_str(l);
                    out.push('\n');
                }
                out
            };
            c.push_str(&cut(&cs));
            r.push_str(&cut(&rs));
            let one = |bin: &Path| run_engine_with(bin, &path, max_chunk, k, 1, STACK_TIMEOUT * 5);
            let (c1, r1) = (one(&bins.0)?.out, one(&bins.1)?.out);
            let (c1t, r1t) = (timeout(&c1), timeout(&r1));
            if c1t.is_some() && r1t.is_some() {
                // both stopped mid-way: compare what both produced
                let (cl, rl): (Vec<&str>, Vec<&str>) = (c1.lines().collect(), r1.lines().collect());
                let n = (cl.len() - 1).min(rl.len() - 1);
                for l in &cl[..n] {
                    c.push_str(l);
                    c.push('\n');
                }
                for l in &rl[..n] {
                    r.push_str(l);
                    r.push('\n');
                }
                hung.push(stacks_ids[k].clone());
            } else {
                c.push_str(&c1);
                r.push_str(&r1);
                if c1t.is_some() != r1t.is_some() {
                    break;
                }
            }
            first = k + 1;
            continue;
        }
        if let (Some(a), Some(b)) = (ct, rt)
            && a == b
        {
            // both hung on the same stack: compare the common prefix
            let (cl, rl): (Vec<&str>, Vec<&str>) = (cs.lines().collect(), rs.lines().collect());
            let n = (cl.len() - 1).min(rl.len() - 1);
            for l in &cl[..n] {
                c.push_str(l);
                c.push('\n');
            }
            for l in &rl[..n] {
                r.push_str(l);
                r.push('\n');
            }
            hung.push(stacks_ids[a as usize].clone());
            first = a as usize + 1;
            continue;
        }
        c.push_str(&cs);
        r.push_str(&rs);
        match (ct, rt) {
            (Some(a), Some(b)) if a == b => {
                hung.push(stacks_ids[a as usize].clone());
                first = a as usize + 1;
            }
            _ => break,
        }
    }
    let (cl, rl): (Vec<&str>, Vec<&str>) = (c.lines().collect(), r.lines().collect());
    let anns = cl
        .iter()
        .filter(|l| l.starts_with("{\"t\":\"ann\""))
        .count();
    let mut diff = None;
    let mut diff_line = None;
    let mut stack = String::new();
    for i in 0..cl.len().max(rl.len()) {
        let (a, b) = (
            cl.get(i).copied().unwrap_or("<missing>"),
            rl.get(i).copied().unwrap_or("<missing>"),
        );
        if a.contains("\"t\":\"stack\"") {
            stack = a.to_owned();
        }
        if normalize(a) != normalize(b) {
            diff = Some(format!(
                "seed {seed} (total {total}, rate {samplerate}, chunk {max_chunk}) line {}\n  in   {stack}\n  C    {a}\n  Rust {b}",
                i + 1
            ));
            diff_line = Some(i);
            break;
        }
    }
    let line = |o: &[&str], i: usize| o.get(i).copied().unwrap_or("<missing>").to_owned();
    let at = probe.or(diff_line).unwrap_or(usize::MAX);
    if diff.is_none() && !keep && std::env::var_os("DECODE_DIFF_KEEP_ALL").is_none() {
        let _ = std::fs::remove_file(&path);
    }
    Ok(Outcome {
        lines: cl.len(),
        anns,
        diff,
        coverage: coverage(&cl),
        hung,
        diff_line,
        probe: (line(&cl, at), line(&rl, at)),
    })
}

/// `--bench R`: protocol.demo's signals repeated R times, decoded with its
/// saved stacks by both engines; prints the times.
fn bench(repeat: u64, bins: &(PathBuf, PathBuf), dir: &Path) -> Result<()> {
    let demo = dsview_oracle::repo_root().join("DSView/demo/logic/protocol.demo");
    let cap = dsview_oracle::capture::load(&demo)?;
    let total = cap.total_samples * repeat;
    anyhow::ensure!(
        cap.total_samples % 64 == 0,
        "demo length must be a multiple of 64"
    );
    let mut channels = Vec::new();
    let mut bits: Vec<Vec<u8>> = Vec::new();
    for (i, s) in cap.signals.iter().enumerate() {
        if let Some(s) = s {
            let one = &s[..(cap.total_samples / 8) as usize];
            channels.push(WriteChannel {
                index: i as u16,
                name: i.to_string(),
            });
            bits.push(one.repeat(repeat as usize));
        }
    }
    let path = dir.join("decode-bench.dsl");
    let writer = LogicWriter {
        driver: "virtual-demo".into(),
        samplerate: cap.samplerate,
        total_samples: total,
        trigger_time_ms: 0,
        trigger_pos: 0,
        channels,
        decoders_json: Value::Array(cap.decoders.clone()).to_string(),
        session_json: "{}".into(),
    };
    let refs: Vec<&[u8]> = bits.iter().map(Vec::as_slice).collect();
    writer.write(std::fs::File::create(&path)?, &refs)?;
    println!(
        "{} stacks over {total} samples ({repeat} x protocol.demo)",
        cap.decoders.len()
    );
    let mut outs = Vec::new();
    for (name, bin) in [("C   ", &bins.0), ("Rust", &bins.1)] {
        let t = std::time::Instant::now();
        let ran = run_engine(bin, &path, 0, 0)?;
        anyhow::ensure!(!ran.killed, "{name} timed out");
        let out = ran.out;
        let dt = t.elapsed().as_secs_f64();
        let anns = out
            .lines()
            .filter(|l| l.starts_with("{\"t\":\"ann\""))
            .count();
        println!(
            "{name} {dt:.2} s, {anns} annotations, {:.2} Msamples/s",
            total as f64 / dt / 1e6
        );
        outs.push(out);
    }
    anyhow::ensure!(outs[0] == outs[1], "outputs differ");
    println!("outputs identical");
    if std::env::var_os("DECODE_DIFF_KEEP_ALL").is_none() {
        let _ = std::fs::remove_file(&path);
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut runs, mut seed, mut jobs, mut max_samples) = (10u64, 1u64, 4usize, 60_000u64);
    let mut keep = std::env::temp_dir();
    let mut bench_repeat = 0u64;
    while let Some(a) = args.next() {
        let mut val = || args.next().context("missing value");
        match a.as_str() {
            "--runs" => runs = val()?.parse()?,
            "--seed" => seed = val()?.parse()?,
            "--jobs" => jobs = val()?.parse()?,
            "--max-samples" => max_samples = val()?.parse()?,
            "--keep" => keep = PathBuf::from(val()?),
            "--bench" => bench_repeat = val()?.parse()?,
            other => bail!("unexpected argument {other}"),
        }
    }
    let exe = std::env::current_exe()?;
    let bin_dir = exe.parent().context("bin dir")?;
    let bins = (bin_dir.join("oracle-decode"), bin_dir.join("rust-decode"));
    if bench_repeat > 0 {
        return bench(bench_repeat, &bins, &keep);
    }
    let list = Command::new(&bins.1).arg("--list").output()?;
    let decoders: Vec<Value> = String::from_utf8(list.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    println!(
        "{} decoders, {runs} runs from seed {seed}, {jobs} jobs",
        decoders.len()
    );

    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let flakes = Mutex::new(Vec::new());
    let totals = Mutex::new((0usize, 0usize));
    let hangs = Mutex::new(Vec::new());
    let cov: Mutex<std::collections::BTreeMap<String, (usize, usize, usize)>> =
        Mutex::new(Default::default());
    let decoders = Arc::new(decoders);
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let k = next.fetch_add(1, Ordering::Relaxed) as u64;
                    if k >= runs {
                        break;
                    }
                    let sd = seed + k;
                    match checked_run(sd, &decoders, &bins, max_samples, &keep) {
                        Ok((o, flaky)) => {
                            if let Some((f, rust_changed)) = flaky {
                                println!("FLAKY {f}");
                                flakes.lock().unwrap().push(sd);
                                if rust_changed {
                                    failures.lock().unwrap().push(sd);
                                }
                            }
                            let mut t = totals.lock().unwrap();
                            t.0 += o.lines;
                            t.1 += o.anns;
                            let mut c = cov.lock().unwrap();
                            for (k, (a, b, e)) in o.coverage {
                                let x = c.entry(k).or_default();
                                x.0 += a;
                                x.1 += b;
                                x.2 += e;
                            }
                            if !o.hung.is_empty() {
                                hangs
                                    .lock()
                                    .unwrap()
                                    .extend(o.hung.iter().map(|d| (sd, d.clone())));
                            }
                            if let Some(d) = o.diff {
                                println!("DIFF {d}");
                                failures.lock().unwrap().push(sd);
                            } else {
                                println!(
                                    "ok   seed {sd}: {} lines, {} annotations",
                                    o.lines, o.anns
                                );
                            }
                        }
                        Err(e) => {
                            println!("ERR  seed {sd}: {e:#}");
                            failures.lock().unwrap().push(sd);
                        }
                    }
                }
            });
        }
    });
    let c = cov.lock().unwrap();
    let silent: Vec<&String> = c.iter().filter(|(_, v)| v.1 == 0).map(|(k, _)| k).collect();
    let erroring: Vec<String> = c
        .iter()
        .filter(|(_, v)| v.2 > 0)
        .map(|(k, v)| format!("{k} {}/{}", v.2, v.0))
        .collect();
    println!(
        "coverage: {} of {} decoders annotated in at least one run; never: {:?}",
        c.len() - silent.len(),
        c.len(),
        silent
    );
    println!("stacks ending in an error (identical in C and Rust): {erroring:?}");
    let mut by_decoder: std::collections::BTreeMap<String, usize> = Default::default();
    for (_, d) in hangs.lock().unwrap().iter() {
        *by_decoder.entry(d.clone()).or_default() += 1;
    }
    println!("decoders that looped forever in both engines (stack timeouts): {by_decoder:?}");
    let t = totals.lock().unwrap();
    let f = failures.lock().unwrap();
    println!(
        "{} runs, {} output lines, {} annotations; {} failing: {:?}",
        runs,
        t.0,
        t.1,
        f.len(),
        f
    );
    let fl = flakes.lock().unwrap();
    if !fl.is_empty() {
        println!(
            "{} runs differed once but not on a rerun (see FLAKY): {:?}",
            fl.len(),
            fl
        );
    }
    if !f.is_empty() {
        bail!("{} runs differ", f.len());
    }
    Ok(())
}
