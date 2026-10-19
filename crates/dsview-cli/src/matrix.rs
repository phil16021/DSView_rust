//! `dsview-cli hwmatrix`: the DSLogic hardware matrix (PLAN §3 item 6),
//! through the session and the control API as the GUI uses them:
//!
//! - the internal test counter with 1, 4, 8 and 16 channels;
//! - buffer and stream captures in every channel mode at every sample rate
//!   the mode offers, checked for their sample count;
//! - simple, advanced and serial triggers on the test counter at 0, 50 and
//!   100 %, checked where the counter allows it;
//! - a stop while the trigger waits, and one in the middle of a stream;
//! - start/stop cycles at random rates and stop times.
//!
//! The device's settings are put back afterwards.

use std::time::{Duration, Instant};

use dsview_control::ControlBackend;
use dsview_control::api::Api;
use dsview_core::{ConfValue, key};
use dsview_session::Session;
use dsview_session::trigger::AdvTrigger;
use serde_json::{Value, json};

const BUFFER: i16 = 0;
const STREAM: i16 = 1;
const INTEST: i16 = 2;

struct M<'a> {
    api: Api,
    s: &'a Session,
    failed: u32,
    passed: u32,
    report: Vec<Value>,
}

impl M<'_> {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let p = params.as_object().cloned().unwrap_or_default();
        self.api
            .call(method, &p)
            .map_err(|e| format!("{method}: {e}"))
    }

    fn record(&mut self, what: String, r: Result<String, String>) {
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

    /// The device options dialog's way: operation mode, channel mode, the
    /// first `n` channels on, then the session reloads.
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

    /// A capture; one that does not end in time is stopped.
    fn capture(&self, timeout_s: f64) -> Result<Value, String> {
        let r = self.call("start_capture", json!({ "timeout_s": timeout_s }));
        if r.is_err() && self.s.is_working() {
            let _ = self.call("stop_capture", json!({}));
            let t = Instant::now();
            while self.s.is_working() && t.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        r
    }

    /// The counter value of sample `i` over the enabled channels.
    fn counter_bits(&self, i: u64, ch: u16) -> bool {
        let store = self
            .s
            .data()
            .logic
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        store.get_sample(i, i32::from(ch))
    }

    /// Breaks in the counter over the whole capture.
    fn counter_breaks(&self) -> (usize, u64, u64) {
        let store = self
            .s
            .data()
            .logic
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let chans: Vec<i32> = store
            .channel_indexes()
            .iter()
            .map(|&i| i32::from(i))
            .filter(|&i| store.has_data(i))
            .collect();
        let n = chans.len().min(32);
        let len = store.ring_sample_count();
        let mask = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
        let value = |i: u64| {
            chans[..n].iter().enumerate().fold(0u32, |v, (k, &ch)| {
                v | (u32::from(store.get_sample(i, ch)) << k)
            })
        };
        let mut bad = 0;
        let mut prev = value(0);
        for i in 1..len {
            let v = value(i);
            if v != prev.wrapping_add(1) & mask {
                bad += 1;
            }
            prev = v;
        }
        (n, len, bad)
    }

    /// The nearest sample to `at` where channel `ch` rises, as an offset.
    fn rise_near(&self, at: u64, ch: u16, span: u64) -> Option<i64> {
        let lo = at.saturating_sub(span).max(1);
        (lo..at + span)
            .filter(|&i| !self.counter_bits(i - 1, ch) && self.counter_bits(i, ch))
            .map(|i| i as i64 - at as i64)
            .min_by_key(|d| d.abs())
    }
}

/// One `X01RFC` value string with `set` (channel, character), bit 15 first.
fn value(set: &[(usize, char)]) -> String {
    let mut v = ['X'; 16];
    for &(ch, c) in set {
        v[15 - ch] = c;
    }
    v.iter().map(char::to_string).collect::<Vec<_>>().join(" ")
}

pub fn run(s: &Session, version: &str, cycles: u32, quick: bool, out: Option<&str>) -> i32 {
    let mut m = M {
        api: Api::new(s.clone(), version),
        s,
        failed: 0,
        passed: 0,
        report: Vec::new(),
    };
    // the first DSLogic
    let (entries, _) = s.devices();
    let Some(i) = entries
        .iter()
        .position(|e| matches!(e.kind, dsview_session::Kind::Usb { .. }))
    else {
        eprintln!("hwmatrix: no DSLogic attached");
        return 1;
    };
    if let Err(e) = m.call("select_device", json!({ "index": i })) {
        eprintln!("hwmatrix: {e}");
        return 1;
    }
    if !s
        .state()
        .device
        .as_ref()
        .is_some_and(|d| d.driver.starts_with("DSLogic"))
    {
        eprintln!("hwmatrix: the device is not a DSLogic");
        return 1;
    }
    let keep = |k| s.config(k, None);
    let saved = (
        keep(key::OPERATION_MODE),
        keep(key::CHANNEL_MODE),
        keep(key::SAMPLERATE),
        keep(key::LIMIT_SAMPLES),
        s.channels()
            .iter()
            .map(|c| (c.index, c.enabled))
            .collect::<Vec<_>>(),
    );
    let t0 = Instant::now();

    // 1. the internal test counter
    for n in [1usize, 4, 8, 16] {
        m.setup(INTEST, None, n);
        let r = m
            .call("set_sampling", json!({ "samples": 4u64 << 20 }))
            .and_then(|_| m.capture(30.0))
            .and_then(|_| {
                let (ch, len, bad) = m.counter_breaks();
                let t = format!("{ch} channels, {len} samples, {bad} breaks");
                if len >= 4 << 20 && bad == 0 && ch == n {
                    Ok(t)
                } else {
                    Err(t)
                }
            });
        m.record(format!("counter {n} ch"), r);
    }

    // 2. buffer and stream in every channel mode at every rate
    for op in [BUFFER, STREAM] {
        m.setup(op, None, 16);
        let modes = match s.config_list(key::CHANNEL_MODE) {
            Some(ConfValue::List(l)) => l,
            _ => Vec::new(),
        };
        for mode in modes {
            m.setup(op, Some(mode.id as i16), 32);
            let n = s.channels().iter().filter(|c| c.enabled).count();
            let mut rates = m.rates();
            if quick && rates.len() > 3 {
                rates = vec![rates[0], rates[rates.len() / 2], rates[rates.len() - 1]];
            }
            for rate in rates {
                // about 100 ms, at least 4096 samples
                let want = (rate / 10).clamp(4096, 2 << 20);
                let r = m
                    .call("set_sampling", json!({"samplerate": rate, "samples": want}))
                    .and_then(|_| m.capture(60.0))
                    .and_then(|info| {
                        // the limit the device took (DSView rounds it)
                        let want = s.sample_limit();
                        let got = s.data().logic.lock().map_or(0, |l| l.ring_sample_count());
                        let t = format!("{got} of {want} samples, {}", info["duration_s"]);
                        if got + 1024 >= want && got > 0 {
                            Ok(t)
                        } else {
                            Err(t)
                        }
                    });
                let name = if op == BUFFER { "buffer" } else { "stream" };
                m.record(format!("{name} '{}' {n} ch {rate} Hz", mode.name), r);
            }
        }
    }

    // 3. triggers on the counter (bit 15 rises every 65536 samples)
    let samples = 1u64 << 20;
    for pos in [0, 50, 100] {
        m.setup(INTEST, None, 16);
        let r = m
            .call("set_sampling", json!({ "samples": samples }))
            .and_then(|_| {
                m.call(
                    "set_trigger",
                    json!({"type": "rising", "channel": 15, "position_percent": pos}),
                )
            })
            .and_then(|_| m.capture(30.0))
            .and_then(|_| {
                let info = m.call("capture_info", json!({}))?;
                let at = info["trigger_sample"].as_u64().unwrap_or(0);
                let off = m.rise_near(at, 15, 16);
                let t = format!(
                    "triggered {}, at {at}, ch15 rises at {off:?}",
                    info["triggered"]
                );
                if info["triggered"] == true && off.is_some() {
                    Ok(t)
                } else {
                    Err(t)
                }
            });
        let _ = m.call("set_trigger", json!({"type": "none"}));
        m.record(format!("simple trigger ch15 rising at {pos} %"), r);

        // two stages: ch15 rises, then ch14 rises (counter 0x8000, then 0xC000)
        m.setup(INTEST, None, 16);
        {
            let mut st = s.state();
            let mut t = AdvTrigger {
                advanced: true,
                stages: 1,
                ..AdvTrigger::default()
            };
            t.stage[0].value0 = value(&[(15, 'R')]);
            t.stage[1].value0 = value(&[(14, 'R')]);
            st.adv_trigger = t;
        }
        s.set_trigger_position(pos);
        let r = m.capture(30.0).and_then(|_| {
            let info = m.call("capture_info", json!({}))?;
            let at = info["trigger_sample"].as_u64().unwrap_or(0);
            let off = m.rise_near(at, 14, 16);
            let high15 = m.counter_bits(at, 15);
            let t = format!(
                "triggered {}, at {at}, ch14 rises at {off:?}, ch15 high {high15}",
                info["triggered"]
            );
            if info["triggered"] == true && off.is_some() && high15 {
                Ok(t)
            } else {
                Err(t)
            }
        });
        m.record(format!("advanced trigger 2 stages at {pos} %"), r);

        // serial: start ch15 rising, stop ch15 falling, clock ch0 rising,
        // data ch1 (0101… on the counter), 4 bits: the trigger follows the
        // start by the 4 clock edges. (With an all-X start condition the
        // FPGA never triggers.)
        let variants: [(&str, String, String); 1] = [(
            "start ch15 rising, stop ch15 falling",
            value(&[(15, 'R')]),
            value(&[(15, 'F')]),
        )];
        let mut tried = Vec::new();
        let mut r = Err(String::new());
        for (name, start, stop) in variants {
            {
                let mut st = s.state();
                let mut t = AdvTrigger {
                    advanced: true,
                    tab: 1,
                    ..AdvTrigger::default()
                };
                t.serial.start = start;
                t.serial.stop = stop;
                t.serial.clock = value(&[(0, 'R')]);
                t.serial.channel = 1;
                t.serial.data = value(&[(3, '0'), (2, '1'), (1, '0'), (0, '1')]);
                t.serial.bits = 3;
                st.adv_trigger = t;
            }
            let one = m
                .capture(5.0)
                .map_err(|e| format!("{name}: {e}"))
                .and_then(|_| {
                    let info = m.call("capture_info", json!({}))?;
                    if info["triggered"] == true {
                        Ok(format!("{name}: triggered at {}", info["trigger_sample"]))
                    } else {
                        Err(format!("{name}: not triggered"))
                    }
                });
            match one {
                Ok(t) => {
                    tried.push(t);
                    r = Ok(tried.join("; "));
                }
                Err(e) => tried.push(e),
            }
        }
        if r.is_err() {
            r = Err(tried.join("; "));
        }
        s.state().adv_trigger = AdvTrigger::default();
        m.record(format!("serial trigger at {pos} %"), r);
    }

    // 4. stops: the trigger waits (an unconnected input stays low), and a
    // stream is stopped in the middle
    m.setup(BUFFER, None, 16);
    let stop_after = |m: &M, ms: u64| -> Result<String, String> {
        m.call("start_capture", json!({"wait": false}))?;
        std::thread::sleep(Duration::from_millis(ms));
        let t = Instant::now();
        m.call("stop_capture", json!({}))?;
        while s.is_working() {
            if t.elapsed() > Duration::from_secs(5) {
                return Err("the capture did not end within 5 s of the stop".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(format!(
            "ended {} ms after the stop",
            t.elapsed().as_millis()
        ))
    };
    let r = m
        .call(
            "set_sampling",
            json!({"samplerate": 1_000_000, "samples": 1u64 << 20}),
        )
        .and_then(|_| m.call("set_trigger", json!({"type": "rising", "channel": 0})))
        .and_then(|_| stop_after(&m, 1000));
    let _ = m.call("set_trigger", json!({"type": "none"}));
    m.record("stop while the trigger waits".into(), r);
    m.setup(STREAM, None, 16);
    let r = m
        .call(
            "set_sampling",
            json!({"samplerate": 10_000_000, "samples": 100_000_000u64}),
        )
        .and_then(|_| stop_after(&m, 500));
    m.record("stop in the middle of a stream".into(), r);

    // 5. start/stop cycles
    let mut x = 0x9E37_79B9u64;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let rates = [1_000_000u64, 5_000_000, 10_000_000, 20_000_000];
    let mut broken = Vec::new();
    let tc = Instant::now();
    for i in 0..cycles {
        let op = if next() % 2 == 0 { BUFFER } else { STREAM };
        m.setup(op, None, 16);
        let rate = rates[(next() % 4) as usize];
        let r = m
            .call(
                "set_sampling",
                json!({"samplerate": rate, "samples": rate / 2}),
            )
            .and_then(|_| match next() % 3 {
                0 => m.capture(30.0).map(|_| String::new()),
                k => stop_after(&m, next() % (200 * k)),
            });
        if let Err(e) = r {
            broken.push(format!("cycle {i}: {e}"));
        }
        if i % 100 == 99 {
            println!("     {} cycles, {:.0} s", i + 1, tc.elapsed().as_secs_f64());
        }
    }
    let r = if broken.is_empty() {
        Ok(format!(
            "{cycles} cycles in {:.0} s",
            tc.elapsed().as_secs_f64()
        ))
    } else {
        Err(broken.join("; "))
    };
    m.record(format!("{cycles} start/stop cycles"), r);

    // the settings as they were
    let (op, chm, rate, limit, enabled) = saved;
    if let Some(v) = op {
        s.set_config(key::OPERATION_MODE, v, None);
    }
    if let Some(v) = chm {
        s.set_config(key::CHANNEL_MODE, v, None);
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
    s.reload_signals();
    s.device_updated();

    println!(
        "hwmatrix: {} passed, {} failed, {:.0} s",
        m.passed,
        m.failed,
        t0.elapsed().as_secs_f64()
    );
    if let Some(path) = out {
        let doc = json!({"passed": m.passed, "failed": m.failed, "tests": m.report});
        let _ = std::fs::write(path, serde_json::to_string_pretty(&doc).unwrap_or_default());
    }
    i32::from(m.failed > 0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn value_strings_put_bit_15_first() {
        assert_eq!(
            super::value(&[(15, 'R'), (0, '1')]),
            "R X X X X X X X X X X X X X X 1"
        );
    }
}
