//! `dsview-cli hwsig`: the DSLogic with real signals from a pattern
//! generator (`tools/patgen-g474`, a NUCLEO-G474RE on the ST-LINK virtual
//! COM port). Where `hwmatrix` uses the device's internal test counter, this
//! goes through the input stage: threshold, filter, triggers on real edges,
//! and the protocol decoders on known data.
//!
//! The generator's pins on DSLogic channels 0..3 are found first (`scan`:
//! every candidate pin toggles at its own rate). The scenarios:
//!
//! - `pins`: the pin of each channel;
//! - `pwm`: TIM2 on PA5 and TIM4 on PB6 at known periods: period, duty,
//!   edge count and the skew between the two;
//! - `stream`: a Gray counter on the mapped pins in every stream channel
//!   mode at its top rate: every step must last the same number of samples
//!   and change exactly one channel (dropped or doubled data shows);
//! - `uart`, `spi`, `i2c`: known bytes through the decoders;
//! - `trigger`: simple, advanced and serial triggers on real edges, the
//!   trigger position;
//! - `vth`: the threshold voltage against the 3.3 V signal;
//! - `filter`: the 1-sample glitch filter;
//! - `files`: odd sample counts, save, reload, save again, export.
//!
//! The device's settings are put back afterwards.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use dsview_control::ControlBackend;
use dsview_control::api::Api;
use dsview_core::{ConfValue, key};
use dsview_session::Session;
use dsview_session::trigger::AdvTrigger;
use serde_json::{Value, json};

const BUFFER: i16 = 0;
const STREAM: i16 = 1;
const SYSCLK: f64 = 170e6;

pub struct Opts<'a> {
    pub port: &'a str,
    pub only: Option<&'a str>,
    pub seconds: f64,
    pub out: Option<&'a str>,
    pub work: PathBuf,
}

/// The pattern generator on its serial port.
struct Patgen {
    f: File,
}

impl Patgen {
    fn open(port: &str) -> Result<Patgen, String> {
        let st = std::process::Command::new("stty")
            .args([
                "-F", port, "115200", "raw", "-echo", "min", "0", "time", "2",
            ])
            .status()
            .map_err(|e| format!("stty: {e}"))?;
        if !st.success() {
            return Err(format!("stty -F {port} failed"));
        }
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(port)
            .map_err(|e| format!("{port}: {e}"))?;
        Ok(Patgen { f })
    }

    fn drain(&mut self) {
        let mut b = [0u8; 256];
        while matches!(self.f.read(&mut b), Ok(n) if n > 0) {}
    }

    /// One command; the reply line ("ok …"), or an error.
    fn cmd(&mut self, c: &str) -> Result<String, String> {
        // any byte stops a running pattern
        let _ = self.f.write_all(b"\n");
        std::thread::sleep(Duration::from_millis(30));
        self.drain();
        self.f
            .write_all(format!("{c}\n").as_bytes())
            .map_err(|e| format!("patgen write: {e}"))?;
        let t = Instant::now();
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while t.elapsed() < Duration::from_secs(15) {
            match self.f.read(&mut b) {
                Ok(1) if b[0] == b'\n' => {
                    let s = String::from_utf8_lossy(&line).trim().to_owned();
                    if s.is_empty() {
                        continue;
                    }
                    return if s.starts_with("ok") {
                        Ok(s)
                    } else {
                        Err(format!("patgen '{c}': {s}"))
                    };
                }
                Ok(1) => line.push(b[0]),
                _ => {}
            }
        }
        Err(format!("patgen '{c}': no reply"))
    }
}

/// A Gray counter value.
fn gray(i: u32) -> u32 {
    i ^ (i >> 1)
}

struct H<'a> {
    api: Api,
    s: &'a Session,
    pg: Patgen,
    o: &'a Opts<'a>,
    failed: u32,
    passed: u32,
    report: Vec<Value>,
    /// The generator pin on channels 0..3 ("" when none was found).
    pins: [String; 4],
    clock: String,
}

type R = Result<String, String>;

impl H<'_> {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let p = params.as_object().cloned().unwrap_or_default();
        self.api
            .call(method, &p)
            .map_err(|e| format!("{method}: {e}"))
    }

    fn want(&self, group: &str) -> bool {
        self.o.only.is_none_or(|o| o.split(',').any(|g| g == group))
    }

    fn record(&mut self, what: String, r: R) {
        let ok = r.is_ok();
        let text = r.unwrap_or_else(|e| e);
        println!("{} {what}: {text}", if ok { "ok  " } else { "FAIL" });
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
        self.report
            .push(json!({"test": what, "ok": ok, "result": text}));
    }

    /// Operation mode, channel mode, the first `n` channels on.
    fn setup(&self, op: i16, ch_mode: Option<i16>, n: usize) {
        self.s
            .set_config(key::OPERATION_MODE, ConfValue::I16(op), None);
        if let Some(m) = ch_mode {
            self.s
                .set_config(key::CHANNEL_MODE, ConfValue::I16(m), None);
        }
        let vld = self
            .s
            .config(key::VLD_CH_NUM, None)
            .map_or(16, |v| v.get_i16() as usize);
        for (i, c) in self.s.channels().iter().enumerate() {
            self.s.enable_channel(c.index, i < n.min(vld));
        }
        if !self.s.have_view_data() {
            self.s.commit_settings();
        }
        self.s.reload_signals();
        self.s.device_updated();
        let _ = self.call("set_trigger", json!({"type": "none"}));
        self.s.state().adv_trigger = AdvTrigger::default();
    }

    /// The channel modes of an operation mode: (id, name, channels, top rate).
    fn modes(&self, op: i16) -> Vec<(i16, String, usize, u64)> {
        self.setup(op, None, 16);
        let list = match self.s.config_list(key::CHANNEL_MODE) {
            Some(ConfValue::List(l)) => l,
            _ => Vec::new(),
        };
        let mut v = Vec::new();
        for m in list {
            self.setup(op, Some(m.id as i16), 32);
            let n = self.s.channels().iter().filter(|c| c.enabled).count();
            let top = self.rates().last().copied().unwrap_or(0);
            v.push((m.id as i16, m.name.clone(), n, top));
        }
        v
    }

    /// The buffer mode with at least `n` channels and the highest rate.
    fn fastest_buffer(&self, n: usize) -> Option<(i16, u64)> {
        self.modes(BUFFER)
            .into_iter()
            .filter(|m| m.2 >= n)
            .max_by_key(|m| m.3)
            .map(|m| (m.0, m.3))
    }

    fn rates(&self) -> Vec<u64> {
        self.call("get_config", json!({}))
            .ok()
            .and_then(|c| {
                c["available_samplerates"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_u64).collect())
            })
            .unwrap_or_default()
    }

    /// At least `samples` at `rate`: DSView rounds the count down to its
    /// time steps, so a larger one is asked for until the limit covers it.
    fn sampling(&self, rate: u64, samples: u64) -> Result<(), String> {
        let mut ask = samples;
        for _ in 0..12 {
            self.call("set_sampling", json!({"samplerate": rate, "samples": ask}))?;
            if self.s.sample_limit() >= samples {
                return Ok(());
            }
            ask = ask.saturating_mul(2);
        }
        Err(format!(
            "the device takes no more than {} samples at {rate} Hz",
            self.s.sample_limit()
        ))
    }

    /// Drives generator pins (by channel) to their idle levels before a
    /// capture is armed.
    fn prep(&mut self, levels: &[(usize, u8)]) -> Result<(), String> {
        for &(ch, v) in levels {
            let pin = self.pins[ch].clone();
            self.pg.cmd(&format!("set {pin} {v}"))?;
        }
        Ok(())
    }

    /// A capture; one that does not end in time is stopped.
    fn capture(&self, timeout_s: f64) -> Result<Value, String> {
        let r = self.call("start_capture", json!({ "timeout_s": timeout_s }));
        if r.is_err() && self.s.is_working() {
            self.stop();
        }
        r
    }

    fn stop(&self) {
        let _ = self.call("stop_capture", json!({}));
        let t = Instant::now();
        while self.s.is_working() && t.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Starts a capture, runs the generator command once the device is
    /// armed, waits for the end.
    fn capture_with(&mut self, cmd: &str, timeout_s: f64) -> Result<String, String> {
        self.call("start_capture", json!({"wait": false}))?;
        std::thread::sleep(Duration::from_millis(400));
        let reply = self.pg.cmd(cmd);
        let t = Instant::now();
        while self.s.is_working() {
            if t.elapsed().as_secs_f64() > timeout_s {
                self.stop();
                return Err(format!("capture did not end ({reply:?})"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        reply
    }

    fn sample_count(&self) -> u64 {
        self.s
            .data()
            .logic
            .lock()
            .map_or(0, |l| l.ring_sample_count())
    }

    fn rate(&self) -> f64 {
        self.s.sample_rate() as f64
    }

    /// The level of sample 0 and every edge of channel `ch`.
    fn edges(&self, ch: i32) -> (bool, Vec<u64>) {
        let store = self
            .s
            .data()
            .logic
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !store.has_data(ch) {
            return (false, Vec::new());
        }
        let count = store.ring_sample_count();
        let mut index = 0;
        let mut level = store.get_sample(0, ch);
        let initial = level;
        let mut v = Vec::new();
        while index < count {
            if !store.get_nxt_edge(&mut index, level, count, 1.0, ch) {
                break;
            }
            if index >= count {
                break;
            }
            v.push(index);
            level = !level;
        }
        (initial, v)
    }

    /// The channel a generator pin is on.
    fn ch_of(&self, pin: &str) -> Option<i32> {
        self.pins.iter().position(|p| p == pin).map(|i| i as i32)
    }

    /// Runs the decoder `id` over the capture; the texts of its annotations
    /// in `row` (all rows if empty).
    fn decode(
        &self,
        id: &str,
        channels: Value,
        options: Value,
        row: &str,
    ) -> Result<Vec<String>, String> {
        // remove what an earlier test left
        while self
            .call("list_active_decoders", json!({}))
            .ok()
            .and_then(|v| v["decoders"].as_array().map(|a| !a.is_empty()))
            .unwrap_or(false)
        {
            self.call("remove_decoder", json!({"index": 0}))?;
        }
        self.call(
            "add_decoder",
            json!({"id": id, "channels": channels, "options": options}),
        )?;
        let mut texts = Vec::new();
        let mut offset = 0u64;
        loop {
            let a = self.call(
                "get_annotations",
                json!({"decoder": 0, "row": row, "offset": offset, "limit": 10000}),
            )?;
            let items = a["annotations"].as_array().cloned().unwrap_or_default();
            if items.is_empty() {
                break;
            }
            offset += items.len() as u64;
            texts.extend(
                items
                    .iter()
                    .map(|x| x["text"].as_str().unwrap_or_default().to_owned()),
            );
            if offset >= a["total"].as_u64().unwrap_or(0) {
                break;
            }
        }
        let _ = self.call("remove_decoder", json!({"index": 0}));
        Ok(texts)
    }
}

/// The byte an annotation text ends in ("@3F", "0x3F", "3F", "Data write: 3F").
fn byte_of(t: &str) -> Option<u8> {
    let w = t.split([' ', ':']).rfind(|w| !w.is_empty())?;
    let w = w.trim_start_matches('@').trim_start_matches("0x");
    if w.len() > 2 {
        return None;
    }
    u8::from_str_radix(w, 16).ok()
}

/// "n of m bytes match"; Ok when every expected byte came out in order.
fn compare(got: &[u8], want: &[u8]) -> R {
    let same = got.iter().zip(want).filter(|(a, b)| a == b).count();
    let first_bad = got.iter().zip(want).position(|(a, b)| a != b);
    let extra = got.get(want.len()..).unwrap_or(&[]);
    let t = format!(
        "{} bytes decoded, {same} of {} match{}{}",
        got.len(),
        want.len(),
        if extra.is_empty() {
            String::new()
        } else {
            format!(", extra {:02X?}", &extra[..extra.len().min(8)])
        },
        first_bad.map_or(String::new(), |i| format!(
            ", first difference at {i}: {:02X} for {:02X}",
            got[i], want[i]
        ))
    );
    if got.len() == want.len() && same == want.len() {
        Ok(t)
    } else {
        Err(t)
    }
}

pub fn run(s: &Session, version: &str, o: &Opts) -> i32 {
    let pg = match Patgen::open(o.port) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("hwsig: {e}");
            return 1;
        }
    };
    let mut h = H {
        api: Api::new(s.clone(), version),
        s,
        pg,
        o,
        failed: 0,
        passed: 0,
        report: Vec::new(),
        pins: Default::default(),
        clock: String::new(),
    };
    match h.pg.cmd("id") {
        Ok(id) => {
            println!("     generator: {id}");
            h.clock = id.split(' ').nth(4).unwrap_or("?").to_owned();
        }
        Err(e) => {
            eprintln!("hwsig: {e}");
            return 1;
        }
    }
    let (entries, _) = s.devices();
    let Some(i) = entries
        .iter()
        .position(|e| matches!(e.kind, dsview_session::Kind::Usb { .. }))
    else {
        eprintln!("hwsig: no DSLogic attached");
        return 1;
    };
    if let Err(e) = h.call("select_device", json!({ "index": i })) {
        eprintln!("hwsig: {e}");
        return 1;
    }
    let keep = |k| s.config(k, None);
    let saved = (
        keep(key::OPERATION_MODE),
        keep(key::CHANNEL_MODE),
        keep(key::SAMPLERATE),
        keep(key::LIMIT_SAMPLES),
        keep(key::VTH),
        keep(key::FILTER),
        s.channels()
            .iter()
            .map(|c| (c.index, c.enabled))
            .collect::<Vec<_>>(),
    );
    let _ = std::fs::create_dir_all(&o.work);
    let t0 = Instant::now();

    pins(&mut h);
    if h.want("pwm") {
        pwm(&mut h);
    }
    if h.want("stream") {
        stream(&mut h);
    }
    if h.want("uart") {
        uart(&mut h);
    }
    if h.want("spi") {
        spi(&mut h);
    }
    if h.want("i2c") {
        i2c(&mut h);
    }
    if h.want("trigger") {
        trigger(&mut h);
    }
    if h.want("vth") {
        vth(&mut h);
    }
    if h.want("filter") {
        filter(&mut h);
    }
    if h.want("files") {
        files(&mut h);
    }
    let _ = h.pg.cmd("idle");

    let (op, chm, rate, limit, vth, filt, enabled) = saved;
    for (k, v) in [
        (key::OPERATION_MODE, op),
        (key::CHANNEL_MODE, chm),
        (key::VTH, vth),
        (key::FILTER, filt),
    ] {
        if let Some(v) = v {
            s.set_config(k, v, None);
        }
    }
    for (index, on) in enabled {
        s.enable_channel(index, on);
    }
    if let Some(v) = rate {
        s.set_config(key::SAMPLERATE, v, None);
    }
    if let Some(v) = limit {
        s.set_config(key::LIMIT_SAMPLES, v, None);
    }
    let _ = h.call("set_trigger", json!({"type": "none"}));
    s.state().adv_trigger = AdvTrigger::default();
    s.reload_signals();
    s.device_updated();

    println!(
        "hwsig: {} passed, {} failed, {:.0} s",
        h.passed,
        h.failed,
        t0.elapsed().as_secs_f64()
    );
    if let Some(path) = o.out {
        let doc = json!({"passed": h.passed, "failed": h.failed, "clock": h.clock,
                         "pins": h.pins, "tests": h.report});
        let _ = std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap_or_default());
    }
    i32::from(h.failed > 0)
}

/// Which generator pin is on channels 0..3.
fn pins(h: &mut H) {
    h.setup(BUFFER, None, 4);
    let reply = h.pg.cmd("scan");
    let r = reply.and_then(|reply| {
        // "ok tick_us 4 PA0 PA1 …"
        let names: Vec<String> = reply.split(' ').skip(3).map(str::to_owned).collect();
        let tick_us: f64 = reply
            .split(' ')
            .nth(2)
            .and_then(|t| t.parse().ok())
            .unwrap_or(4.0);
        h.sampling(10_000_000, 2_000_000)?;
        h.capture(30.0)?;
        let rate = h.rate();
        let mut found = Vec::new();
        for ch in 0..4 {
            let (_, e) = h.edges(ch);
            let pin = if e.len() >= 8 {
                let mut d: Vec<u64> = e.windows(2).map(|w| w[1] - w[0]).collect();
                d.sort_unstable();
                let half_us = d[d.len() / 2] as f64 / rate * 1e6;
                let k = (half_us / tick_us).round() as usize;
                k.checked_sub(1)
                    .and_then(|k| names.get(k))
                    .cloned()
                    .unwrap_or_default()
            } else {
                String::new()
            };
            found.push(format!(
                "ch{ch}={}",
                if pin.is_empty() { "-" } else { pin.as_str() }
            ));
            h.pins[ch as usize] = pin;
        }
        let t = found.join(" ");
        if h.pins.iter().all(|p| !p.is_empty()) {
            Ok(t)
        } else {
            Err(format!(
                "{t} (no generator pin on some channel: check ground and wiring)"
            ))
        }
    });
    let _ = h.pg.cmd("idle");
    h.record("pins".into(), r);
    if h.pins.iter().all(|p| !p.is_empty()) {
        let m = format!("map {}", h.pins.join(" "));
        if let Err(e) = h.pg.cmd(&m) {
            h.record("map".into(), Err(e));
        }
    }
}

/// Period, duty and edge count of TIM2 (PA5) and TIM4 (PB6).
fn pwm(h: &mut H) {
    let chans: Vec<(i32, &str)> = [("PA5", 0), ("PB6", 1)]
        .iter()
        .filter_map(|&(p, _)| h.ch_of(p).map(|c| (c, p)))
        .collect();
    if chans.is_empty() {
        h.record(
            "pwm".into(),
            Err("neither PA5 nor PB6 is on a channel".into()),
        );
        return;
    }
    let Some((mode, _)) = h.fastest_buffer(4) else {
        h.record("pwm".into(), Err("no buffer mode with 4 channels".into()));
        return;
    };
    // ppm tolerance: two crystals, or the generator's HSI (±1 %)
    let tol_ppm = if h.clock == "hse24" { 200.0 } else { 15_000.0 };
    let freqs: [u64; 6] = [
        1_000, 100_000, 1_000_000, 10_000_000, 21_250_000, 42_500_000,
    ];
    for f in freqs {
        for duty in [50u32, 25] {
            if duty != 50 && f > 1_000_000 {
                continue;
            }
            h.setup(BUFFER, Some(mode), 4);
            let rates = h.rates();
            // at least 4 samples a period, at most 2 Mi samples for >= 20 periods
            // the shorter phase spans at least 2 samples (a 1-sample pulse
            // vanishes or doubles with the phase)
            let short = u64::from(duty.min(100 - duty));
            let mut rate_list: Vec<u64> = rates
                .iter()
                .copied()
                .filter(|&r| r * short >= 200 * f)
                .collect();
            rate_list.dedup();
            let pick: Vec<u64> = if rate_list.len() > 2 {
                vec![rate_list[0], *rate_list.last().unwrap()]
            } else {
                rate_list
            };
            for rate in pick {
                let samples =
                    ((rate as f64 / f as f64) * 200.0).clamp(65536.0, 4.0 * 1048576.0) as u64;
                let r = (|| -> R {
                    let reply = h.pg.cmd(&format!("pwm {f} {duty}"))?;
                    let cycles: f64 = reply
                        .split(' ')
                        .nth(2)
                        .and_then(|c| c.parse().ok())
                        .ok_or("bad pwm reply")?;
                    h.sampling(rate, samples)?;
                    h.capture(30.0)?;
                    let rate = h.rate();
                    let want_period = cycles / SYSCLK * rate; // samples
                    let mut parts = Vec::new();
                    let mut bad = Vec::new();
                    let mut first_rise = Vec::new();
                    for &(ch, pin) in &chans {
                        let (init, e) = h.edges(ch);
                        // rising edges are those where the level becomes high
                        let rises: Vec<u64> = e
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| (i % 2 == 0) != init)
                            .map(|(_, &x)| x)
                            .collect();
                        let falls: Vec<u64> = e
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| (i % 2 == 0) == init)
                            .map(|(_, &x)| x)
                            .collect();
                        if rises.len() < 3 {
                            bad.push(format!("{pin}: {} edges", e.len()));
                            continue;
                        }
                        first_rise.push(rises[0]);
                        let period =
                            (rises[rises.len() - 1] - rises[0]) as f64 / (rises.len() - 1) as f64;
                        let ppm = (period / want_period - 1.0) * 1e6;
                        // the high time: each rise to the next fall
                        let highs: Vec<f64> = rises
                            .iter()
                            .filter_map(|&r| {
                                falls.iter().find(|&&x| x > r).map(|&x| (x - r) as f64)
                            })
                            .collect();
                        let high = highs.iter().sum::<f64>() / highs.len().max(1) as f64;
                        let duty_got = high / period * 100.0;
                        let jitter = rises
                            .windows(2)
                            .map(|w| ((w[1] - w[0]) as f64 - period).abs())
                            .fold(0.0, f64::max);
                        let count = h.sample_count() as f64;
                        let want_edges = count / want_period * 2.0;
                        parts.push(format!(
                            "{pin} {ppm:+.0} ppm duty {duty_got:.1} % jitter {jitter:.1} edges {}/{want_edges:.0}",
                            e.len()
                        ));
                        if ppm.abs() > tol_ppm {
                            bad.push(format!("{pin} period off by {ppm:.0} ppm"));
                        }
                        // ±1 sample of the high time, plus the generator's rounding
                        let want_duty = f64::from(duty);
                        if (duty_got - want_duty).abs() > 100.0 * 1.5 / period + 1.0 {
                            bad.push(format!("{pin} duty {duty_got:.1} %"));
                        }
                        if jitter > 1.0 + period * tol_ppm * 1e-6 {
                            bad.push(format!("{pin} period jitter {jitter:.1} samples"));
                        }
                        if (e.len() as f64 - want_edges).abs() > 2.0 + want_edges * tol_ppm * 1e-6 {
                            bad.push(format!("{pin} {} edges for {want_edges:.0}", e.len()));
                        }
                    }
                    if first_rise.len() == 2 {
                        let skew = first_rise[1] as f64 - first_rise[0] as f64;
                        // nearest rise: the two may start a period apart
                        let skew =
                            (skew + want_period / 2.0).rem_euclid(want_period) - want_period / 2.0;
                        parts.push(format!("skew {:.1} ns", skew / rate * 1e9));
                    }
                    let t = parts.join(", ");
                    if bad.is_empty() {
                        Ok(t)
                    } else {
                        Err(format!("{t}; {}", bad.join("; ")))
                    }
                })();
                h.record(format!("pwm {f} Hz {duty} % at {rate} Hz"), r);
            }
        }
    }
    let _ = h.pg.cmd("idle");
}

/// The Gray counter in every stream mode at its top rate.
fn stream(h: &mut H) {
    if h.pins.iter().any(String::is_empty) {
        h.record("stream".into(), Err("pins not found".into()));
        return;
    }
    for (id, name, n, top) in h.modes(STREAM) {
        let bits = n.min(4);
        let r = (|| -> R {
            h.setup(STREAM, Some(id), 32);
            // a step every 100 samples at the top rate
            let step = ((SYSCLK / top as f64) * 100.0).round().max(40.0) as u64;
            let want = (step as f64 / SYSCLK) * top as f64;
            let samples = (top as f64 * h.o.seconds) as u64;
            h.pg.cmd(&format!("count {step} {bits}"))?;
            h.sampling(top, samples)?;
            let t = Instant::now();
            h.capture(h.o.seconds * 3.0 + 30.0)?;
            let dur = t.elapsed().as_secs_f64();
            let len = h.sample_count();
            // every edge of every counter channel, in order
            let mut ev: Vec<(u64, usize)> = Vec::new();
            let mut level = [false; 4];
            for (b, l) in level.iter_mut().enumerate().take(bits) {
                let (init, e) = h.edges(b as i32);
                *l = init;
                ev.extend(e.into_iter().map(|x| (x, b)));
            }
            ev.sort_unstable();
            let value0 = level[..bits]
                .iter()
                .enumerate()
                .fold(0u32, |v, (b, &l)| v | (u32::from(l) << b));
            let mask = (1u32 << bits) - 1;
            // the Gray index of the first value
            let mut idx = (0..=mask).find(|&i| gray(i) == value0).unwrap_or(0);
            let (mut bad_value, mut bad_gap, mut same_time) = (0u64, 0u64, 0u64);
            let mut first_bad: Option<String> = None;
            let mut last = None::<u64>;
            let mut v = value0;
            let mut gaps = 0u64;
            let mut max_dev = 0f64;
            for &(at, b) in &ev {
                v ^= 1 << b;
                idx = (idx + 1) & mask;
                if v != gray(idx) {
                    bad_value += 1;
                    if first_bad.is_none() {
                        first_bad = Some(format!("value {v:#x} at {at}, wanted {:#x}", gray(idx)));
                    }
                    // resynchronise
                    idx = (0..=mask).find(|&i| gray(i) == v).unwrap_or(idx);
                }
                if let Some(p) = last {
                    let g = at - p;
                    if g == 0 {
                        same_time += 1;
                    }
                    gaps += 1;
                    let dev = (g as f64 - want).abs();
                    max_dev = max_dev.max(dev);
                    // one sample, the generator's busy-wait jitter (about
                    // 5 cycles, 30 ns) and the clocks' ppm; a lost USB
                    // block is thousands of samples
                    if dev
                        > 1.0
                            + 40e-9 * top as f64
                            + want * 2e-4 * if h.clock == "hse24" { 1.0 } else { 75.0 }
                    {
                        bad_gap += 1;
                        if first_bad.is_none() {
                            first_bad =
                                Some(format!("step of {g} samples at {at}, wanted {want:.1}"));
                        }
                    }
                }
                last = Some(at);
            }
            let t = format!(
                "{len} samples ({:.1} s in {dur:.1} s), {gaps} steps, {bad_value} wrong values, {bad_gap} wrong steps (largest deviation {max_dev:.0} samples), {same_time} double edges{}",
                len as f64 / top as f64,
                first_bad.map_or(String::new(), |f| format!(", first: {f}"))
            );
            if len >= samples.saturating_sub(1024) && gaps > 0 && bad_value == 0 && bad_gap == 0 {
                Ok(t)
            } else {
                Err(t)
            }
        })();
        h.record(
            format!("stream '{name}' {n} ch {top} Hz, {bits}-bit Gray counter"),
            r,
        );
    }
    let _ = h.pg.cmd("idle");
}

fn uart(h: &mut H) {
    let Some(ch) = h.ch_of("PB6") else {
        h.record("uart".into(), Err("PB6 is not on a channel".into()));
        return;
    };
    let Some((mode, _)) = h.fastest_buffer(4) else {
        return;
    };
    let n = 256u32;
    let want: Vec<u8> = (0..n).map(|k| k as u8).collect();
    for (baud, cfg) in [
        (9600u32, "8N1"),
        (115_200, "8N1"),
        (115_200, "8E1"),
        (115_200, "8N2"),
        (1_000_000, "8N1"),
        (2_000_000, "8N1"),
        (4_000_000, "8N1"),
    ] {
        let r = (|| -> R {
            h.setup(BUFFER, Some(mode), 4);
            let rates = h.rates();
            let rate = rates
                .iter()
                .copied()
                .find(|&r| r >= 20 * u64::from(baud))
                .or(rates.last().copied())
                .ok_or("no rates")?;
            let bits = if cfg == "8N1" { 10.0 } else { 11.0 };
            let secs = f64::from(n) * bits / f64::from(baud) + 0.01;
            h.sampling(rate, (secs * rate as f64 * 1.3) as u64 + 65536)?;
            h.call(
                "set_trigger",
                json!({"type": "falling", "channel": ch, "position_percent": 1}),
            )?;
            h.prep(&[(ch as usize, 1)])?;
            let reply = h.capture_with(&format!("uart {baud} {n} {cfg}"), 30.0)?;
            let brr: f64 = reply
                .split(' ')
                .nth(2)
                .and_then(|b| b.parse().ok())
                .unwrap_or(0.0);
            let real_baud = if brr > 0.0 {
                SYSCLK / brr
            } else {
                f64::from(baud)
            };
            let parity = match &cfg[1..2] {
                "E" => "even",
                "O" => "odd",
                _ => "none",
            };
            let stop = if &cfg[2..3] == "2" { 2.0 } else { 1.0 };
            let texts = h.decode(
                "0:uart",
                json!({ "rxtx": ch }),
                json!({"baudrate": real_baud.round() as i64, "parity_type": parity,
                       "num_stop_bits": stop, "format": "hex"}),
                "",
            )?;
            let warnings: Vec<&String> = texts
                .iter()
                .filter(|t| t.contains("error") || t.contains("err"))
                .collect();
            let got: Vec<u8> = texts
                .iter()
                .filter(|t| t.starts_with('@') || (t.len() == 2 && byte_of(t).is_some()))
                .filter_map(|t| byte_of(t))
                .collect();
            let r = compare(&got, &want).map(|t| format!("{t} at {rate} Hz"));
            if !warnings.is_empty() {
                return Err(format!(
                    "{}; {} warnings, first: {}",
                    r.unwrap_or_else(|e| e),
                    warnings.len(),
                    warnings[0]
                ));
            }
            r
        })();
        h.record(format!("uart {baud} {cfg}"), r);
    }
    let _ = h.pg.cmd("idle");
}

fn spi(h: &mut H) {
    let Some((mode4, _)) = h.fastest_buffer(4) else {
        return;
    };
    let n = 256u32;
    let want: Vec<u8> = (0..n).map(|k| k as u8).collect();
    // bit-banged on the four channels: SCK ch0, CS ch1, MOSI ch2, MISO ch3 (inverted)
    let want_inv: Vec<u8> = want.iter().map(|b| !b).collect();
    for (half, mode) in [(850u32, 0u32), (85, 0), (85, 1), (85, 2), (85, 3), (40, 0)] {
        let r = (|| -> R {
            h.setup(BUFFER, Some(mode4), 4);
            let rate = *h.rates().last().ok_or("no rates")?;
            let secs = f64::from(n) * 8.0 * 2.0 * f64::from(half) / SYSCLK * 1.5 + 0.001;
            let samples = (secs * rate as f64) as u64 + 65536;
            h.sampling(rate, samples)?;
            h.call(
                "set_trigger",
                json!({"type": "falling", "channel": 1, "position_percent": 1}),
            )?;
            h.prep(&[(0, ((mode >> 1) & 1) as u8), (1, 1), (2, 0), (3, 1)])?;
            h.capture_with(&format!("bspi {half} {mode} {n}"), 30.0)?;
            let info = h.call("capture_info", json!({}))?;
            let cs_edges = h.edges(1);
            let diag = format!(
                "{} of {samples} samples (limit {}), trigger at {}, CS level {} edges {:?}",
                h.sample_count(),
                h.s.sample_limit(),
                info["trigger_sample"],
                u8::from(cs_edges.0),
                &cs_edges.1[..cs_edges.1.len().min(4)]
            );
            let opts = json!({"cpol": (mode >> 1) & 1, "cpha": mode & 1});
            let chans = json!({"clk": 0, "cs": 1, "mosi": 2, "miso": 3});
            let mosi: Vec<u8> = h
                .decode("0:spi", chans.clone(), opts.clone(), "mosi data")?
                .iter()
                .filter_map(|t| byte_of(t))
                .collect();
            let miso: Vec<u8> = h
                .decode("0:spi", chans, opts, "miso data")?
                .iter()
                .filter_map(|t| byte_of(t))
                .collect();
            let a = compare(&mosi, &want);
            let b = compare(&miso, &want_inv);
            let sck = SYSCLK / (2.0 * f64::from(half));
            let t = format!(
                "SCK {:.2} MHz at {rate} Hz: MOSI {}; MISO {}; {diag}",
                sck / 1e6,
                a.clone().unwrap_or_else(|e| e),
                b.clone().unwrap_or_else(|e| e)
            );
            if a.is_ok() && b.is_ok() {
                Ok(t)
            } else {
                Err(t)
            }
        })();
        h.record(format!("spi bit-banged mode {mode} half {half}"), r);
    }
    // SPI1 (SCK PA5, MOSI PA7; its CS PA4 need not be wired): the
    // capture triggers on the first SCK edge (the generator waits 2 ms
    // after it switches SCK to its idle level)
    if let (Some(sck), Some(mosi)) = (h.ch_of("PA5"), h.ch_of("PA7")) {
        let cs = h.ch_of("PA4");
        for (div, mode) in [(5u32, 0u32), (3, 0), (3, 1), (3, 2), (3, 3), (2, 0), (1, 0)] {
            let r = (|| -> R {
                h.setup(BUFFER, Some(mode4), 4);
                let rate = *h.rates().last().ok_or("no rates")?;
                h.sampling(rate, 1 << 20)?;
                let (ch, edge) = match cs {
                    Some(c) => (c, "falling"),
                    None if mode & 2 == 0 => (sck, "rising"),
                    None => (sck, "falling"),
                };
                h.call(
                    "set_trigger",
                    json!({"type": edge, "channel": ch, "position_percent": 1}),
                )?;
                h.prep(&[(sck as usize, ((mode >> 1) & 1) as u8)])?;
                h.capture_with(&format!("spi {div} {mode} {n}"), 30.0)?;
                let mut chans = json!({"clk": sck, "mosi": mosi});
                if let Some(c) = cs {
                    chans["cs"] = c.into();
                }
                let got: Vec<u8> = h
                    .decode(
                        "0:spi",
                        chans,
                        json!({"cpol": (mode >> 1) & 1, "cpha": mode & 1}),
                        "mosi data",
                    )?
                    .iter()
                    .filter_map(|t| byte_of(t))
                    .collect();
                let sck_hz = SYSCLK / f64::from(2u32 << div);
                compare(&got, &want)
                    .map(|t| format!("SCK {:.2} MHz at {rate} Hz: {t}", sck_hz / 1e6))
            })();
            h.record(format!("spi SPI1 mode {mode} div {div}"), r);
        }
    } else {
        println!("     (SPI1 skipped: PA5/PA7 are not both on channels)");
    }
    let _ = h.pg.cmd("idle");
}

fn i2c(h: &mut H) {
    let Some((mode4, _)) = h.fastest_buffer(4) else {
        return;
    };
    let n = 64u32;
    // write 0x50: 0..n-1, repeated start, read 0x50: A5 5A
    let mut want: Vec<u8> = vec![0x50];
    want.extend((0..n).map(|k| k as u8));
    want.extend([0x50, 0xA5, 0x5A]);
    for half in [4250u32, 850, 425, 170] {
        let r = (|| -> R {
            h.setup(BUFFER, Some(mode4), 4);
            let rates = h.rates();
            let khz = SYSCLK / (2.0 * f64::from(half)) / 1e3;
            let rate = rates
                .iter()
                .copied()
                .find(|&r| r as f64 >= khz * 1e3 * 40.0)
                .or(rates.last().copied())
                .ok_or("no rates")?;
            let secs = f64::from(n + 4) * 9.0 * 2.0 * f64::from(half) / SYSCLK * 1.3 + 0.001;
            h.sampling(rate, (secs * rate as f64) as u64 + 65536)?;
            h.call(
                "set_trigger",
                json!({"type": "falling", "channel": 1, "position_percent": 1}),
            )?;
            h.prep(&[(0, 1), (1, 1)])?;
            h.capture_with(&format!("i2c {half} {n}"), 30.0)?;
            let texts = h.decode(
                "0:i2c",
                json!({"scl": 0, "sda": 1}),
                json!({"address_format": "shifted"}),
                "address/data",
            )?;
            let got: Vec<u8> = texts
                .iter()
                .filter(|t| t.contains("Address") || t.contains("Data"))
                .filter_map(|t| byte_of(t))
                .collect();
            let acks = texts.iter().filter(|t| t.as_str() == "ACK").count();
            let nacks = texts.iter().filter(|t| t.as_str() == "NACK").count();
            let rs = texts
                .iter()
                .filter(|t| t.to_lowercase().contains("repeat"))
                .count();
            let mut r = compare(&got, &want).map(|t| format!("{khz:.0} kHz at {rate} Hz: {t}"));
            let want_acks = n as usize + 3;
            if acks != want_acks || nacks != 1 || rs != 1 {
                r = Err(format!(
                    "{}; {acks} ACK (want {want_acks}), {nacks} NACK, {rs} repeated starts; first: {:?}",
                    r.unwrap_or_else(|e| e),
                    &texts[..texts.len().min(6)]
                ));
            }
            r
        })();
        h.record(format!("i2c half {half}"), r);
    }
    let _ = h.pg.cmd("idle");
}

/// Simple, advanced and serial triggers on real edges.
fn trigger(h: &mut H) {
    let Some((mode4, _)) = h.fastest_buffer(4) else {
        return;
    };
    let Some(pwm_ch) = h.ch_of("PA5") else {
        h.record("trigger".into(), Err("PA5 is not on a channel".into()));
        return;
    };
    // 1. the trigger position: a burst on PA5 that starts 400 ms after the
    // capture is armed, so the pre-trigger part is full (the hardware keeps
    // at most 90 %)
    for pos in [0, 10, 50, 90, 100] {
        let r = (|| -> R {
            h.setup(BUFFER, Some(mode4), 4);
            h.sampling(10_000_000, 1 << 20)?;
            h.call(
                "set_trigger",
                json!({"type": "rising", "channel": pwm_ch, "position_percent": pos}),
            )?;
            h.prep(&[(pwm_ch as usize, 0)])?;
            h.capture_with("burst 0 10 8500", 30.0)?;
            let info = h.call("capture_info", json!({}))?;
            let at = info["trigger_sample"].as_u64().unwrap_or(0);
            let len = h.sample_count();
            let (init, e) = h.edges(pwm_ch);
            let first_rise = e
                .iter()
                .enumerate()
                .find(|(i, _)| (i % 2 == 0) != init)
                .map(|(_, &x)| x);
            let want_at = len as f64 * f64::from(pos.min(90)) / 100.0;
            let t = format!(
                "triggered {}, at {at} of {len} (wanted about {want_at:.0}), first rise at {first_rise:?}, {} edges",
                info["triggered"],
                e.len()
            );
            let ok = info["triggered"] == true
                && first_rise.is_some_and(|f| f.abs_diff(at) <= 2)
                && (at as f64 - want_at).abs() <= 0.01 * len as f64 + 1024.0;
            if ok { Ok(t) } else { Err(t) }
        })();
        h.record(format!("trigger position {pos} %, burst after 400 ms"), r);
    }
    // the same with a running 1 kHz square wave: the first rise comes before
    // the pre-trigger part is full, and the hardware fires on it anyway (no
    // check of the position, only that the trigger sample is a rise)
    for pos in [10, 50] {
        let r = (|| -> R {
            h.setup(BUFFER, Some(mode4), 4);
            h.sampling(10_000_000, 1 << 20)?;
            h.call(
                "set_trigger",
                json!({"type": "rising", "channel": pwm_ch, "position_percent": pos}),
            )?;
            h.pg.cmd("pwm 1000 50")?;
            h.capture(30.0)?;
            let info = h.call("capture_info", json!({}))?;
            let at = info["trigger_sample"].as_u64().unwrap_or(0);
            let (init, e) = h.edges(pwm_ch);
            let near = e
                .iter()
                .enumerate()
                .filter(|(i, _)| (i % 2 == 0) != init)
                .map(|(_, &x)| x as i64 - at as i64)
                .min_by_key(|d| d.abs());
            let t = format!(
                "triggered {}, at {at} of {}, nearest rise {near:?}",
                info["triggered"],
                h.sample_count()
            );
            if info["triggered"] == true && near.is_some_and(|d| d.abs() <= 2) {
                Ok(t)
            } else {
                Err(t)
            }
        })();
        h.record(format!("trigger on a running 1 kHz wave at {pos} %"), r);
    }

    // 2. the falling CS of a bit-banged SPI burst
    let r = (|| -> R {
        h.setup(BUFFER, Some(mode4), 4);
        h.sampling(*h.rates().last().ok_or("no rates")?, 1 << 20)?;
        h.call(
            "set_trigger",
            json!({"type": "falling", "channel": 1, "position_percent": 10}),
        )?;
        h.prep(&[(0, 0), (1, 1), (2, 0), (3, 1)])?;
        h.capture_with("bspi 170 0 32", 30.0)?;
        let info = h.call("capture_info", json!({}))?;
        let at = info["trigger_sample"].as_u64().unwrap_or(0);
        let (init, e) = h.edges(1);
        let fall = e
            .iter()
            .enumerate()
            .find(|(i, _)| (i % 2 == 0) == init)
            .map(|(_, &x)| x);
        let t = format!("trigger at {at}, CS falls at {fall:?}");
        if info["triggered"] == true && fall.is_some_and(|f| f.abs_diff(at) <= 2) {
            Ok(t)
        } else {
            Err(t)
        }
    })();
    h.record("trigger falling CS".into(), r);

    // 3. advanced, two stages: CS falls, then SCK rises
    let r = (|| -> R {
        h.setup(BUFFER, Some(mode4), 4);
        h.sampling(*h.rates().last().ok_or("no rates")?, 1 << 20)?;
        {
            let mut st = h.s.state();
            let mut t = AdvTrigger {
                advanced: true,
                stages: 1,
                ..AdvTrigger::default()
            };
            t.stage[0].value0 = crate::matrix::value(&[(1, 'F')]);
            t.stage[1].value0 = crate::matrix::value(&[(0, 'R')]);
            st.adv_trigger = t;
        }
        h.s.set_trigger_position(10);
        h.prep(&[(0, 0), (1, 1), (2, 0), (3, 1)])?;
        h.capture_with("bspi 170 0 32", 30.0)?;
        let info = h.call("capture_info", json!({}))?;
        let at = info["trigger_sample"].as_u64().unwrap_or(0);
        let (init, e) = h.edges(0);
        let first_rise = e
            .iter()
            .enumerate()
            .find(|(i, _)| (i % 2 == 0) != init)
            .map(|(_, &x)| x);
        let t = format!("trigger at {at}, first SCK rise at {first_rise:?}");
        if info["triggered"] == true && first_rise.is_some_and(|f| f.abs_diff(at) <= 2) {
            Ok(t)
        } else {
            Err(t)
        }
    })();
    h.s.state().adv_trigger = AdvTrigger::default();
    h.record("trigger advanced CS falls, then SCK rises".into(), r);

    // 4. serial: start CS falling, stop CS rising, clock SCK rising, data
    // MOSI; the 8 bits of byte 0x5A of the burst 0..255
    let r = (|| -> R {
        h.setup(BUFFER, Some(mode4), 4);
        let rate = *h.rates().last().ok_or("no rates")?;
        h.sampling(rate, 1 << 22)?;
        {
            let mut st = h.s.state();
            let mut t = AdvTrigger {
                advanced: true,
                tab: 1,
                ..AdvTrigger::default()
            };
            t.serial.start = crate::matrix::value(&[(1, 'F')]);
            t.serial.stop = crate::matrix::value(&[(1, 'R')]);
            t.serial.clock = crate::matrix::value(&[(0, 'R')]);
            t.serial.channel = 2;
            // bit 15 first: the last 8 bits shifted in, MSB first on the wire
            let bits: Vec<(usize, char)> = (0..8)
                .map(|b| (b, if (0x5Au8 >> b) & 1 == 1 { '1' } else { '0' }))
                .collect();
            t.serial.data = crate::matrix::value(&bits);
            t.serial.bits = 7;
            st.adv_trigger = t;
        }
        h.s.set_trigger_position(10);
        h.prep(&[(0, 0), (1, 1), (2, 0), (3, 1)])?;
        h.capture_with("bspi 170 0 256", 30.0)?;
        let info = h.call("capture_info", json!({}))?;
        let at = info["trigger_sample"].as_u64().unwrap_or(0);
        // which SCK rise of the burst the trigger is at: byte k, bit b
        let (init, e) = h.edges(0);
        let rises: Vec<u64> = e
            .iter()
            .enumerate()
            .filter(|(i, _)| (i % 2 == 0) != init)
            .map(|(_, &x)| x)
            .collect();
        let k = rises.iter().position(|&r| r + 2 >= at);
        let t = format!(
            "triggered {}, at {at}: SCK rise {k:?} (byte {:?}, bit {:?}); want the end of byte 0x5A = rise {}",
            info["triggered"],
            k.map(|k| k / 8),
            k.map(|k| k % 8),
            0x5A * 8 + 7
        );
        if info["triggered"] == true && k.is_some_and(|k| k / 8 == 0x5A) {
            Ok(t)
        } else {
            Err(t)
        }
    })();
    h.s.state().adv_trigger = AdvTrigger::default();
    h.record("trigger serial on SPI byte 0x5A".into(), r);
    let _ = h.pg.cmd("idle");
}

/// The threshold voltage against the 3.3 V square wave.
fn vth(h: &mut H) {
    let Some(ch) = h.ch_of("PA5") else {
        return;
    };
    let Some((mode4, _)) = h.fastest_buffer(4) else {
        return;
    };
    let mut seen = Vec::new();
    let mut parts = Vec::new();
    for mv in [
        300u32, 600, 1000, 1500, 2000, 2500, 3000, 3300, 3600, 4000, 5000,
    ] {
        h.setup(BUFFER, Some(mode4), 4);
        let v = f64::from(mv) / 1000.0;
        let r = (|| -> Result<usize, String> {
            h.call("set_threshold", json!({ "volts": v }))?;
            h.pg.cmd("pwm 100000 50")?;
            h.sampling(10_000_000, 1 << 20)?;
            h.capture(30.0)?;
            Ok(h.edges(ch).1.len())
        })();
        match r {
            Ok(n) => {
                parts.push(format!("{v:.1} V: {n}"));
                seen.push((mv, n));
            }
            Err(e) => parts.push(format!("{v:.1} V: {e}")),
        }
    }
    let _ = h.call("set_threshold", json!({ "volts": 1.0 }));
    // 100 kHz at 10 MHz: an edge every 50 samples
    let want = h.sample_count() as f64 / 50.0;
    let full = |n: usize| (n as f64 - want).abs() <= 4.0;
    let low_ok = seen
        .iter()
        .filter(|(mv, _)| (600..=2500).contains(mv))
        .all(|&(_, n)| full(n));
    let high_off = seen
        .iter()
        .filter(|(mv, _)| *mv >= 4000)
        .all(|&(_, n)| n == 0);
    let monotonic = seen.windows(2).all(|w| w[1].1 <= w[0].1 + 4);
    let t = parts.join(", ");
    h.record(
        "vth sweep on PA5 (100 kHz, 3.3 V)".into(),
        if low_ok && high_off && monotonic && seen.len() == 11 {
            Ok(t)
        } else {
            Err(t)
        },
    );
    let _ = h.pg.cmd("idle");
}

/// The 1-sample glitch filter: short pulses go, long ones stay.
fn filter(h: &mut H) {
    let Some((mode4, _)) = h.fastest_buffer(4) else {
        return;
    };
    let filters: Vec<i16> = match h.s.config_list(key::FILTER) {
        Some(ConfValue::List(l)) => l.iter().map(|m| m.id as i16).collect(),
        _ => vec![0, 1],
    };
    if filters.len() < 2 {
        h.record("filter".into(), Err(format!("filter options: {filters:?}")));
        return;
    }
    // every 20 us: pulses of 0 (two back-to-back writes, ~6-12 ns) and of
    // 340 cycles (2 us); sampled at 100 MHz
    for (width, name) in [(0u32, "short"), (340, "2 us")] {
        let mut counts = Vec::new();
        for &f in &filters[..2] {
            let r = (|| -> Result<(usize, u64), String> {
                h.setup(BUFFER, Some(mode4), 4);
                h.s.set_config(key::FILTER, ConfValue::I16(f), None);
                h.sampling(100_000_000, 1 << 21)?;
                h.pg.cmd(&format!("glitch 0 {width} 20"))?;
                h.capture(30.0)?;
                let (init, e) = h.edges(0);
                // pulse widths: rise to fall
                let widths: Vec<u64> = e
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| c[1] - c[0])
                    .collect();
                let _ = init;
                Ok((widths.len(), widths.iter().copied().min().unwrap_or(0)))
            })();
            counts.push((f, r));
        }
        h.s.set_config(key::FILTER, ConfValue::I16(filters[0]), None);
        // a pulse every 20 us at 100 MHz
        let want = h.sample_count() / 2000;
        let t = counts
            .iter()
            .map(|(f, r)| match r {
                Ok((n, w)) => format!("filter {f}: {n} pulses, narrowest {w} samples"),
                Err(e) => format!("filter {f}: {e}"),
            })
            .collect::<Vec<_>>()
            .join("; ");
        let ok = match (&counts[0].1, &counts[1].1) {
            (Ok((off, _)), Ok((on, _))) => {
                if width == 0 {
                    // without the filter some show as 1-sample pulses; with it none
                    *on == 0 && *off > 0
                } else {
                    off.abs_diff(want as usize) <= 2 && on.abs_diff(want as usize) <= 2
                }
            }
            _ => false,
        };
        h.record(
            format!("filter, {name} pulses"),
            if ok { Ok(t) } else { Err(t) },
        );
    }
    let _ = h.pg.cmd("idle");
}

/// Odd sample counts through save, reload, save and export.
fn files(h: &mut H) {
    let Some((mode4, _)) = h.fastest_buffer(4) else {
        return;
    };
    // a whole buffer capture, and streams stopped at odd times (their
    // length is no multiple of 64 or 1024, F11)
    for (name, stop_ms) in [
        ("buffer", 0u64),
        ("stream stopped", 137),
        ("stream stopped", 1021),
    ] {
        let r = (|| -> R {
            let samples = if stop_ms == 0 {
                h.setup(BUFFER, Some(mode4), 4);
                h.sampling(10_000_000, 100_003)?;
                h.s.sample_limit()
            } else {
                let modes = h.modes(STREAM);
                let m = modes.iter().find(|m| m.2 >= 4).ok_or("no stream mode")?;
                h.setup(STREAM, Some(m.0), 4);
                h.sampling(10_000_000, 100_000_000)?;
                0
            };
            h.pg.cmd("count 170 4")?;
            if stop_ms == 0 {
                h.capture(30.0)?;
            } else {
                h.call("start_capture", json!({"wait": false}))?;
                std::thread::sleep(Duration::from_millis(stop_ms));
                h.stop();
            }
            let got = h.sample_count();
            let edges: Vec<Vec<u64>> = (0..4).map(|c| h.edges(c).1).collect();
            let mut changes: Vec<u64> = edges.iter().flatten().copied().collect();
            changes.sort_unstable();
            changes.dedup();
            let dir = h.o.work.clone();
            let tag = format!("{}-{stop_ms}", name.replace(' ', "-"));
            let a = dir.join(format!("files-{tag}-a.dsl"));
            let b = dir.join(format!("files-{tag}-b.dsl"));
            h.call("save_session", json!({"path": a}))?;
            h.call("open_file", json!({"path": a}))?;
            let reloaded = h.sample_count();
            let same = (0..4).all(|c| h.edges(c).1 == edges[c as usize]);
            h.call("save_session", json!({"path": b}))?;
            let mut exports = Vec::new();
            let mut csv_ok = true;
            for ext in ["csv", "vcd", "srzip"] {
                let p = dir.join(format!("files-{tag}.{ext}"));
                let e = h.call("export_data", json!({"path": p}));
                exports.push(format!("{ext} {}", if e.is_ok() { "ok" } else { "failed" }));
                if ext == "csv" && e.is_ok() {
                    // one row for the start and one per change
                    let rows = std::fs::read_to_string(&p)
                        .map(|t| {
                            t.lines()
                                .filter(|l| !l.starts_with(';'))
                                .count()
                                .saturating_sub(1)
                        })
                        .unwrap_or(0);
                    csv_ok = rows == changes.len() + 1;
                    exports.push(format!("csv rows {rows} for {} changes", changes.len()));
                }
            }
            let diff =
                crate::diff::dsl(&[a.display().to_string(), b.display().to_string()]).is_ok();
            // back to the device for the next test
            let (entries, _) = h.s.devices();
            if let Some(i) = entries
                .iter()
                .position(|e| matches!(e.kind, dsview_session::Kind::Usb { .. }))
            {
                h.call("select_device", json!({ "index": i }))?;
            }
            let t = format!(
                "captured {got}{}, reloaded {reloaded}, edges {}, round trip {}, {}",
                if samples > 0 {
                    format!(" of {samples}")
                } else {
                    String::new()
                },
                if same { "equal" } else { "DIFFER" },
                if diff { "identical" } else { "DIFFERS" },
                exports.join(", ")
            );
            if (samples == 0 || got == samples)
                && got > 0
                && reloaded == got
                && same
                && diff
                && csv_ok
                && !t.contains("failed")
            {
                Ok(t)
            } else {
                Err(t)
            }
        })();
        h.record(format!("files, {name} {stop_ms} ms"), r);
    }
    let _ = h.pg.cmd("idle");
}
