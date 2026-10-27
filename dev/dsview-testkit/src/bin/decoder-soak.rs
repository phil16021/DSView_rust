//! Decoder watchdog soak: random captures with one random stack per decoder
//! (the generator of the former C/Rust `decode-diff`, tag `c-final`), each
//! decoded by `rust-decode` under a per-stack time limit and an
//! address-space cap. Reports, per decoder, the stacks that never returned
//! (`timeout`), the processes that died (memory), and the stacks that ended
//! in a Python error, each with what replays it.
//!
//! usage: decoder-soak [--runs N] [--seed S] [--jobs J] [--max-samples M]
//!                     [--stack-timeout SECS] [--max-mem MB] [--keep DIR]
//!                     [--allow ID,ID,..]
//!
//! Exits non-zero when a stack timed out or a process died, except for the
//! decoders listed in `--allow`.

use std::collections::BTreeMap;
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

#[repr(C)]
struct Rlimit {
    cur: u64,
    max: u64,
}

unsafe extern "C" {
    fn setrlimit(resource: i32, rlim: *const Rlimit) -> i32;
}

/// Seconds one `rust-decode` process may run before it is killed and resumed
/// after the stacks it finished.
const PROCESS_LIMIT: u64 = 600;

struct Limits {
    stack_timeout: u64,
    max_mem: u64,
}

/// What one process run left: its output, and how it ended.
struct Ran {
    out: String,
    /// Killed after [`PROCESS_LIMIT`].
    killed: bool,
    /// Exited abnormally (not 0, not the watchdog's code): the last stderr line.
    died: Option<String>,
}

fn run_decode(
    bin: &Path,
    capture: &Path,
    max_chunk: u64,
    first: usize,
    lim: &Limits,
) -> Result<Ran> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(bin);
    cmd.arg(capture)
        .args(["--max-chunk", &max_chunk.to_string()])
        .args(["--first", &first.to_string()])
        .args(["--stack-timeout", &lim.stack_timeout.to_string()])
        .env("PYTHONHASHSEED", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mem = lim.max_mem << 20;
    // SAFETY: only async-signal-safe setrlimit between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            const RLIMIT_AS: i32 = 9;
            setrlimit(RLIMIT_AS, &Rlimit { cur: mem, max: mem });
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(PROCESS_LIMIT);
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
    let out = String::from_utf8_lossy(&reader.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).into_owned();
    let died = status
        .filter(|s| !s.success() && s.code() != Some(dsview_testkit::watchdog::TIMEOUT_EXIT))
        .map(|s| {
            format!(
                "{s}: {}",
                err.lines().last().unwrap_or("(no stderr)").trim()
            )
        });
    Ok(Ran {
        out,
        killed: status.is_none(),
        died,
    })
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

/// The top decoder of each stack in `out` that ended in an error, with the
/// first error text.
fn errors(out: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut top = String::new();
    let mut reported = false;
    for l in out.lines() {
        let Ok(v) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        match v["t"].as_str() {
            Some("stack") => {
                top = v["decoders"]
                    .as_array()
                    .and_then(|d| d.last())
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                reported = false;
            }
            Some("error") if !reported => {
                found.push((top.clone(), v["msg"].as_str().unwrap_or("").to_owned()));
                reported = true;
            }
            _ => {}
        }
    }
    found
}

/// One problem: the decoder, what happened, and what replays it.
#[derive(Clone, Debug)]
struct Finding {
    decoder: String,
    kind: &'static str,
    detail: String,
    seed: u64,
    chunk: u64,
    stack: Value,
}

struct Outcome {
    stacks: usize,
    findings: Vec<Finding>,
    errors: Vec<(String, String)>,
}

fn one_run(
    seed: u64,
    decoders: &[Value],
    bin: &Path,
    max_samples: u64,
    dir: &Path,
    lim: &Limits,
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
    let top = |e: &Value| -> String {
        e["stacked decoders"][0]["id"]
            .as_str()
            .or(e["id"].as_str())
            .unwrap_or("?")
            .to_owned()
    };
    let max_chunk = *rng.pick(&[0u64, 0, 1, 7, 64, 333, 1000, 4096, 16384, 50_000]);

    let path = dir.join(format!("decoder-soak-{seed}.dsl"));
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
        decoders_json: Value::Array(stacks.clone()).to_string(),
        session_json: "{}".into(),
    };
    let refs: Vec<&[u8]> = signals.iter().map(Vec::as_slice).collect();
    writer.write(std::fs::File::create(&path)?, &refs)?;

    let mut findings = Vec::new();
    let mut errs = Vec::new();
    let mut first = 0usize;
    let finding = |k: usize, kind: &'static str, detail: String| Finding {
        decoder: top(&stacks[k]),
        kind,
        detail,
        seed,
        chunk: max_chunk,
        stack: stacks[k].clone(),
    };
    while first < stacks.len() {
        let ran = run_decode(bin, &path, max_chunk, first, lim)?;
        errs.extend(errors(&ran.out));
        let done = stacks_done(&ran.out, first);
        let timeout = ran
            .out
            .lines()
            .last()
            .and_then(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["t"] == "timeout")
            .and_then(|v| v["index"].as_u64());
        if let Some(k) = timeout {
            let k = k as usize;
            findings.push(finding(k, "timeout", format!("> {} s", lim.stack_timeout)));
            first = k + 1;
        } else if ran.killed || ran.died.is_some() {
            // the stack in progress when the process ended
            let k = done.min(stacks.len() - 1);
            let (kind, detail) = match ran.died {
                Some(d) => ("died", d),
                None => ("process limit", format!("> {PROCESS_LIMIT} s")),
            };
            findings.push(finding(k, kind, detail));
            first = k + 1;
        } else {
            break;
        }
    }
    if findings.is_empty() {
        let _ = std::fs::remove_file(&path);
    }
    Ok(Outcome {
        stacks: stacks.len(),
        findings,
        errors: errs,
    })
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut runs, mut seed, mut jobs, mut max_samples) = (10u64, 1u64, 4usize, 60_000u64);
    let mut lim = Limits {
        stack_timeout: 10,
        max_mem: 3 << 10,
    };
    let mut keep = std::env::temp_dir();
    let mut allow: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        let mut val = || args.next().context("missing value");
        match a.as_str() {
            "--runs" => runs = val()?.parse()?,
            "--seed" => seed = val()?.parse()?,
            "--jobs" => jobs = val()?.parse()?,
            "--max-samples" => max_samples = val()?.parse()?,
            "--stack-timeout" => lim.stack_timeout = val()?.parse()?,
            "--max-mem" => lim.max_mem = val()?.parse()?,
            "--keep" => keep = PathBuf::from(val()?),
            "--allow" => allow = val()?.split(',').map(str::to_owned).collect(),
            "-h" | "--help" => {
                println!(
                    "usage: decoder-soak [--runs N] [--seed S] [--jobs J] [--max-samples M] \
                     [--stack-timeout SECS] [--max-mem MB] [--keep DIR] [--allow ID,..]"
                );
                return Ok(());
            }
            other => bail!("unexpected argument {other}"),
        }
    }
    std::fs::create_dir_all(&keep)?;
    let exe = std::env::current_exe()?;
    let bin = exe.parent().context("bin dir")?.join("rust-decode");
    let list = Command::new(&bin).arg("--list").output()?;
    let decoders: Vec<Value> = String::from_utf8(list.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    println!(
        "{} decoders, {runs} runs from seed {seed}, {jobs} jobs, {} s per stack, {} MB",
        decoders.len(),
        lim.stack_timeout,
        lim.max_mem
    );

    let next = AtomicUsize::new(0);
    let findings: Mutex<Vec<Finding>> = Mutex::new(Vec::new());
    let errors: Mutex<BTreeMap<String, (usize, String)>> = Mutex::new(BTreeMap::new());
    let stacks = AtomicUsize::new(0);
    let failed_runs = Mutex::new(Vec::new());
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
                    match one_run(sd, &decoders, &bin, max_samples, &keep, &lim) {
                        Ok(o) => {
                            stacks.fetch_add(o.stacks, Ordering::Relaxed);
                            for f in &o.findings {
                                println!(
                                    "{:<13} {} (seed {sd}, chunk {}): {}",
                                    f.kind.to_uppercase(),
                                    f.decoder,
                                    f.chunk,
                                    f.detail
                                );
                            }
                            if o.findings.is_empty() {
                                println!("ok   seed {sd}: {} stacks", o.stacks);
                            }
                            findings.lock().unwrap().extend(o.findings);
                            let mut e = errors.lock().unwrap();
                            for (d, msg) in o.errors {
                                let x = e.entry(d).or_insert((0, msg));
                                x.0 += 1;
                            }
                        }
                        Err(e) => {
                            println!("ERR  seed {sd}: {e:#}");
                            failed_runs.lock().unwrap().push(sd);
                        }
                    }
                }
            });
        }
    });

    let findings = findings.into_inner().unwrap();
    let mut by: BTreeMap<(String, &str), Vec<&Finding>> = BTreeMap::new();
    for f in &findings {
        by.entry((f.decoder.clone(), f.kind)).or_default().push(f);
    }
    println!(
        "\n{runs} runs, {} stacks; {} findings",
        stacks.load(Ordering::Relaxed),
        findings.len()
    );
    for ((d, kind), fs) in &by {
        let f = fs[0];
        println!(
            "  {d}: {} × {kind}; replay: rust-decode {}/decoder-soak-{}.dsl --max-chunk {} --stack '{}'",
            fs.len(),
            keep.display(),
            f.seed,
            f.chunk,
            json!(f.stack)
        );
    }
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        println!("stacks ending in a Python error (decoder: count, first message):");
        for (d, (n, msg)) in &errors {
            let msg: String = msg.chars().take(160).collect();
            println!("  {d}: {n}, {msg}");
        }
    }
    let blocking: Vec<&Finding> = findings
        .iter()
        .filter(|f| !allow.contains(&f.decoder))
        .collect();
    let failed = failed_runs.into_inner().unwrap();
    if !failed.is_empty() {
        bail!("{} runs could not be run: {failed:?}", failed.len());
    }
    if !blocking.is_empty() {
        bail!("{} stacks hung or died", blocking.len());
    }
    Ok(())
}
