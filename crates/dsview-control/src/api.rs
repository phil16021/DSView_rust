//! The control methods (`pv/mcp/controlapi.cpp`) on the Rust capture
//! session: the same names, parameters, checks, messages and answers as
//! DSView's Qt GUI gives them.
//!
//! Qt runs every method on its GUI thread and answers the waiting ones
//! (captures, decoding, saving) later from its event loop. Here each
//! connection has its own thread: methods run one at a time under a lock,
//! and the waits happen outside it.

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use dsview_core::{ConfValue, Mode, channel_type, key};
use dsview_decode::{DecoderInfo, OptValue};
use dsview_session::Session;
use dsview_session::decode::{DecoderStack, opt_json};
use dsview_session::session::{State, trig};
use dsview_session::store;
use dsview_store::logic::LogicStore;
use serde_json::{Map, Value, json};

use crate::ControlBackend;

const CAPTURE_START_GRACE: Duration = Duration::from_millis(5000);
const MAX_EDGES: u64 = 100_000;
const MAX_POINTS: u64 = 20_000;
const MAX_ANNOTATIONS: u64 = 5000;
const EXACT: f64 = 9_007_199_254_740_992.0;

type Params = Map<String, Value>;
type Reply = Result<Value, String>;

/// The methods that change state (refused while Qt shows a dialog; there
/// are no dialogs here).
pub const MUTATING: &[&str] = &[
    "select_device",
    "set_mode",
    "set_sampling",
    "set_channels",
    "set_trigger",
    "set_threshold",
    "set_dso",
    "start_capture",
    "add_decoder",
    "remove_decoder",
    "run_decoders",
    "open_file",
    "save_session",
    "export_data",
    "show_region",
    "set_cursors",
];

/// What a GUI adds to the control API: its view and its dialogs. Without a
/// GUI (`dsview-cli serve`) the defaults apply: no dialog is ever open, and
/// the region and cursors are only recorded in the session.
pub trait ViewHooks: Send + Sync {
    /// The open modal dialog's text (`modal_dialog_text()`), if any. Methods
    /// in [`MUTATING`] are refused while one is open.
    fn dialog_open(&self) -> Option<String> {
        None
    }
    /// `View::show_region(start, end, false)`.
    fn show_region(&self, _start: u64, _end: u64) {}
    /// `clear_cursors`, `add_cursor` for each sample, `show_cursors`.
    fn set_cursors(&self, _samples: &[u64]) {}
}

struct NoView;
impl ViewHooks for NoView {}

/// DSView's control API on a [`Session`].
pub struct Api {
    s: Session,
    gui: Mutex<()>,
    version: String,
    view: Box<dyn ViewHooks>,
}

fn mode_name(mode: i32) -> &'static str {
    match mode {
        0 => "logic",
        1 => "dso",
        2 => "analog",
        _ => "unknown",
    }
}

fn channel_type_name(ty: i32) -> &'static str {
    match ty {
        channel_type::LOGIC => "logic",
        channel_type::DSO => "dso",
        channel_type::ANALOG => "analog",
        _ => "other",
    }
}

fn num(v: u64) -> Value {
    crate::json::num(v as f64)
}

/// `opt_u64`: absent or null is `Ok(None)`; anything but a non-negative
/// integral number up to 2^53 is an error.
fn opt_u64(p: &Params, k: &str) -> Result<Option<u64>, ()> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let v = n.as_f64().unwrap_or(-1.0);
            if v < 0.0 || v != v.floor() || v > EXACT {
                Err(())
            } else {
                Ok(Some(v as u64))
            }
        }
        Some(_) => Err(()),
    }
}

fn req_int(p: &Params, k: &str) -> Option<i32> {
    let v = p.get(k)?.as_f64()?;
    (v == v.floor() && (-2147483648.0..=2147483647.0).contains(&v)).then_some(v as i32)
}

/// `QJsonValue::toString()`: "" for non-strings.
fn string<'a>(p: &'a Params, k: &str) -> &'a str {
    p.get(k).and_then(Value::as_str).unwrap_or("")
}

/// `QJsonValue::toBool()`: false for non-booleans.
fn boolean(p: &Params, k: &str) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// `QJsonValue::toDouble(def)`
fn double(p: &Params, k: &str, def: f64) -> f64 {
    p.get(k).and_then(Value::as_f64).unwrap_or(def)
}

fn obj(p: &Params, k: &str) -> Params {
    p.get(k)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn opt_value_json(v: &OptValue) -> Value {
    match v {
        // variant_to_json: int64 values are doubles
        OptValue::Int(i) => crate::json::num(*i as f64),
        other => opt_json(other),
    }
}

/// `check_options`: every option exists and has an allowed value.
fn check_options(d: &DecoderInfo, options: &Params) -> Result<(), String> {
    for (k, v) in options {
        let Some(found) = d.options.iter().find(|o| o.id == *k) else {
            return Err(format!("decoder '{}' has no option '{k}'", d.id));
        };
        if found.values.is_empty() {
            continue;
        }
        let allowed: Vec<Value> = found.values.iter().map(opt_value_json).collect();
        let as_text = |x: &Value| match x {
            Value::String(s) => s.clone(),
            Value::Number(n) => n
                .as_f64()
                .map(|f| dsview_formats::cfmt::fmt_g(f, 6))
                .unwrap_or_default(),
            Value::Bool(b) => b.to_string(),
            _ => String::new(),
        };
        let qt_eq = |a: &Value, b: &Value| match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        };
        if !allowed
            .iter()
            .any(|a| qt_eq(a, v) || as_text(a) == as_text(v))
        {
            return Err(format!(
                "option '{k}' must be one of {}",
                serde_json::to_string(&crate::json::qt(Value::Array(allowed))).unwrap_or_default()
            ));
        }
    }
    Ok(())
}

impl Api {
    pub fn new(s: Session, version: &str) -> Api {
        Api::with_view(s, version, Box::new(NoView))
    }

    /// The API of a GUI showing `s`.
    pub fn with_view(s: Session, version: &str, view: Box<dyn ViewHooks>) -> Api {
        Api {
            s,
            gui: Mutex::new(()),
            version: version.to_owned(),
            view,
        }
    }

    pub fn session(&self) -> &Session {
        &self.s
    }

    fn gui(&self) -> MutexGuard<'_, ()> {
        self.gui.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mode(&self) -> i32 {
        self.s.mode()
    }

    fn have_device(&self) -> bool {
        self.s.state().device.is_some()
    }

    /// `check_idle()`
    fn check_idle(&self) -> Result<(), String> {
        if !self.have_device() {
            return Err("no device is selected".into());
        }
        if self.s.is_working() {
            return Err("a capture is running; call stop_capture or wait for it".into());
        }
        if self.s.state().saving {
            return Err("DSView is saving a file".into());
        }
        Ok(())
    }

    fn logic_store(&self) -> MutexGuard<'_, LogicStore> {
        self.s
            .data()
            .logic
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// `check_logic_data()`
    fn check_logic_data(&self) -> Result<(), String> {
        if self.mode() != Mode::Logic as i32 {
            return Err("this needs logic analyzer mode (set_mode logic)".into());
        }
        if self.logic_store().sample_count() == 0 {
            return Err("no captured logic data; run start_capture first".into());
        }
        Ok(())
    }

    fn snap_rate(&self) -> f64 {
        self.s.state().cur_snap_samplerate as f64
    }

    /// `config_json()`
    fn config_json(&self) -> Value {
        let Some(dev) = self.s.state().device.clone() else {
            return json!({"device": null});
        };
        let mode = self.mode();
        let mut o = Map::new();
        let kind = if dev.is_hardware() {
            "hardware"
        } else if dev.is_demo() {
            "demo"
        } else if dev.is_file() {
            "file"
        } else {
            "other"
        };
        o.insert(
            "device".into(),
            json!({"name": dev.name, "driver": dev.driver, "kind": kind}),
        );
        o.insert("mode".into(), mode_name(mode).into());
        let modes: Vec<Value> = self
            .s
            .modes()
            .into_iter()
            .map(|m| mode_name(m as i32).into())
            .collect();
        o.insert("available_modes".into(), modes.into());
        o.insert("samplerate".into(), num(self.s.sample_rate()));
        if mode != Mode::Dso as i32 {
            o.insert("samples".into(), num(self.s.sample_limit()));
        }
        if let Some(d) = self.s.config_u64(key::HW_DEPTH) {
            o.insert("hardware_depth".into(), num(d));
        }
        let rates: Vec<Value> = self.s.sample_rates().into_iter().map(num).collect();
        o.insert("available_samplerates".into(), rates.into());
        if mode == Mode::Dso as i32
            && let Some(tb) = self.s.config_u64(key::TIMEBASE)
        {
            o.insert("timebase_ns_per_div".into(), num(tb));
        }
        if mode == Mode::Logic as i32
            && let Some(v) = self.s.config_f64(key::VTH)
        {
            o.insert("threshold_volts".into(), crate::json::num(v));
        }
        let channels: Vec<Value> = self
            .s
            .channels()
            .iter()
            .map(|c| {
                let mut x = Map::new();
                x.insert("index".into(), c.index.into());
                x.insert("name".into(), c.name.clone().into());
                x.insert("enabled".into(), c.enabled.into());
                x.insert("type".into(), channel_type_name(c.ty).into());
                if c.ty == channel_type::DSO {
                    x.insert("vdiv_mv".into(), num(c.vdiv));
                    x.insert(
                        "coupling".into(),
                        match c.coupling {
                            1 => "ac",
                            2 => "gnd",
                            _ => "dc",
                        }
                        .into(),
                    );
                }
                Value::Object(x)
            })
            .collect();
        o.insert("channels".into(), channels.into());
        if mode == Mode::Logic as i32 {
            const NAMES: [&str; 6] = ["none", "rising", "high", "falling", "low", "edge"];
            let st = self.s.state();
            let triggers: Vec<Value> = st
                .signals
                .iter()
                .filter(|s| s.ty == channel_type::LOGIC && (1..=5).contains(&s.trig))
                .map(|s| json!({"channel": s.index, "type": NAMES[s.trig as usize]}))
                .collect();
            o.insert("triggers".into(), triggers.into());
            o.insert("trigger_position_percent".into(), st.trig_pos.into());
        }
        Value::Object(o)
    }

    /// `capture_info_json()`
    fn capture_info_json(&self) -> Value {
        let mut o = Map::new();
        if !self.have_device() {
            return Value::Object(o);
        }
        let mode = self.mode();
        let ty = match mode {
            1 => channel_type::DSO,
            2 => channel_type::ANALOG,
            _ => channel_type::LOGIC,
        };
        let data = self.s.data();
        let (count, has): (u64, Box<dyn Fn(i32) -> bool>) = match mode {
            1 => {
                let d = data.dso.lock().unwrap_or_else(|e| e.into_inner());
                let idx: Vec<u16> = d.channel_indexes().to_vec();
                (
                    d.sample_count(),
                    Box::new(move |i| idx.contains(&(i as u16))),
                )
            }
            2 => {
                let d = data.analog.lock().unwrap_or_else(|e| e.into_inner());
                let idx: Vec<u16> = d.channel_indexes().to_vec();
                (
                    d.sample_count(),
                    Box::new(move |i| idx.contains(&(i as u16))),
                )
            }
            _ => {
                let d = self.logic_store();
                let n = if d.sample_count() == 0 {
                    0
                } else {
                    d.ring_sample_count()
                };
                let idx: Vec<u16> = d.channel_indexes().to_vec();
                (n, Box::new(move |i| idx.contains(&(i as u16))))
            }
        };
        let have = count > 0;
        o.insert("mode".into(), mode_name(mode).into());
        o.insert("have_data".into(), have.into());
        if !have {
            return Value::Object(o);
        }
        let st = self.s.state();
        let rate = st.cur_snap_samplerate as f64;
        o.insert("samples".into(), num(count));
        o.insert("samplerate".into(), crate::json::num(rate));
        if rate > 0.0 {
            o.insert("duration_s".into(), crate::json::num(count as f64 / rate));
        }
        o.insert("triggered".into(), st.triged.into());
        o.insert("trigger_sample".into(), num(st.trigger_pos));
        o.insert("decoding".into(), st.decoding.into());
        if st.error != 0 {
            o.insert("error".into(), st.error.into());
        }
        let chans: Vec<Value> = st
            .signals
            .iter()
            .filter(|s| s.ty == ty && has(s.index as i32))
            .map(|s| json!({"index": s.index, "name": s.name}))
            .collect();
        o.insert("channels".into(), chans.into());
        Value::Object(o)
    }

    fn decoder_state_json(&self, index: usize) -> Value {
        let Some(t) = self.s.state().decoders.get(index).cloned() else {
            return Value::Null;
        };
        let stack = &t.stack;
        let mut o = Map::new();
        o.insert("index".into(), index.into());
        o.insert("name".into(), t.name().into());
        let ids: Vec<Value> = stack
            .decoders()
            .iter()
            .map(|d| d.info.id.clone().into())
            .collect();
        o.insert("stack".into(), ids.into());
        o.insert("progress".into(), stack.progress().into());
        o.insert("annotations".into(), num(stack.result_count()));
        let err = stack.error_message();
        if !err.is_empty() {
            o.insert("error".into(), err.into());
        }
        let rows: Vec<Value> = stack
            .rows()
            .iter()
            .map(|r| {
                let mut x = Map::new();
                x.insert("title".into(), r.title().into());
                if let Some(id) = r.id() {
                    x.insert("id".into(), id.into());
                }
                Value::Object(x)
            })
            .collect();
        o.insert("rows".into(), rows.into());
        Value::Object(o)
    }

    /// The decode waiter: once decoding is done, the states of `indexes`.
    fn wait_decoders(&self, indexes: &[usize], timeout: Duration) -> Reply {
        if !self.s.wait(timeout, |st| !st.decoding) {
            return Err("timed out waiting for decoding; it is still running".into());
        }
        let _g = self.gui();
        let count = self.s.state().decoders.len();
        let states: Vec<Value> = indexes
            .iter()
            .filter(|&&i| i < count)
            .map(|&i| self.decoder_state_json(i))
            .collect();
        Ok(json!({ "decoders": states }))
    }

    /// The capture waiter: the capture must start within the grace period
    /// and end within `timeout`; then (after automatic decoding) its info.
    fn wait_capture(&self, before: (u64, u64), timeout: Duration) -> Reply {
        let started = |st: &State| st.started_seq > before.0 || st.working || st.running;
        if !self.s.wait(CAPTURE_START_GRACE.min(timeout), started) {
            return Err("the capture did not start (check the DSView window for a message)".into());
        }
        let ended = |st: &State| st.ended_seq > before.1 && !st.working && !st.running;
        if !self.s.wait(timeout, ended) {
            return Err(
                "timed out waiting for the capture to finish; it may still be running \
                        (use capture_status or stop_capture)"
                    .into(),
            );
        }
        if !self.s.is_decoding() {
            let _g = self.gui();
            return Ok(self.capture_info_json());
        }
        let count = self.s.state().decoders.len();
        let all: Vec<usize> = (0..count).collect();
        let r = self.wait_decoders(&all, Duration::from_secs(120));
        let _g = self.gui();
        let mut info = self.capture_info_json();
        if let Value::Object(o) = &mut info {
            match r {
                Ok(v) => {
                    o.insert("decoders".into(), v["decoders"].clone());
                }
                Err(e) => {
                    o.insert("decode_warning".into(), e.into());
                }
            }
        }
        Ok(info)
    }

    fn seqs(&self) -> (u64, u64) {
        let st = self.s.state();
        (st.started_seq, st.ended_seq)
    }

    // ------------------------------------------------------------ device + capture

    fn list_devices(&self) -> Reply {
        let (list, active) = self.s.devices();
        let arr: Vec<Value> = list
            .iter()
            .enumerate()
            .map(|(i, e)| json!({"index": i, "name": e.name, "active": i as i32 == active}))
            .collect();
        Ok(json!({ "devices": arr }))
    }

    fn select_device(&self, p: &Params) -> Reply {
        let Some(index) = req_int(p, "index") else {
            return Err("parameter 'index' (from list_devices) is required".into());
        };
        if self.s.is_working() || self.s.state().saving {
            return Err("DSView is capturing or saving".into());
        }
        let (list, active) = self.s.devices();
        let count = list.len() as i32;
        if index < 0 || index >= count {
            return Err(format!("device index out of range (0..{})", count - 1));
        }
        if index != active && !self.s.set_device(list[index as usize].handle) {
            return Err("could not open the device".into());
        }
        Ok(self.config_json())
    }

    fn set_mode(&self, p: &Params) -> Reply {
        self.check_idle()?;
        let name = string(p, "mode");
        let mode = match name {
            "logic" => 0,
            "dso" => 1,
            "analog" => 2,
            _ => return Err("mode must be 'logic', 'dso' or 'analog'".into()),
        };
        if !self.s.modes().iter().any(|m| *m as i32 == mode) {
            return Err(format!("this device does not support mode {name}"));
        }
        if self.mode() != mode {
            self.s.switch_work_mode(mode);
        }
        Ok(self.config_json())
    }

    fn set_sampling(&self, p: &Params) -> Reply {
        self.check_idle()?;
        let (Ok(rate), Ok(samples)) = (opt_u64(p, "samplerate"), opt_u64(p, "samples")) else {
            return Err("samplerate and samples must be non-negative integers".into());
        };
        let (rate, samples) = (rate.unwrap_or(0), samples.unwrap_or(0));
        if rate != 0 {
            let allowed = self.s.sample_rates();
            if !allowed.is_empty() && !allowed.contains(&rate) {
                return Err(
                    "unsupported samplerate; see available_samplerates in get_config".into(),
                );
            }
            self.s
                .set_config(key::SAMPLERATE, ConfValue::U64(rate), None);
        }
        if samples != 0 {
            if self.mode() == Mode::Dso as i32 {
                return Err("in dso mode use set_dso timebase_ns instead of samples".into());
            }
            self.s
                .set_config(key::LIMIT_SAMPLES, ConfValue::U64(samples), None);
        }
        self.s.update_sample_rate_list();
        self.s.commit_settings();
        // DSV_MSG_DEVICE_DURATION_UPDATED: the trigger dock's range
        self.s.update_trigger_range();
        let mut o = Map::new();
        o.insert("samplerate".into(), num(self.s.sample_rate()));
        if self.mode() != Mode::Dso as i32 {
            o.insert("samples".into(), num(self.s.sample_limit()));
        }
        o.insert(
            "note".into(),
            "sample counts snap to the depth steps DSView offers".into(),
        );
        Ok(Value::Object(o))
    }

    fn set_channels(&self, p: &Params) -> Reply {
        self.check_idle()?;
        let Some(list) = p.get("channels").and_then(Value::as_array) else {
            return Err("parameter 'channels' must be an array of {index, enabled?, name?}".into());
        };
        let mode = self.mode();
        let chans = self.s.channels();
        for v in list {
            let c = v.as_object().cloned().unwrap_or_default();
            let Some(index) = req_int(&c, "index") else {
                return Err("each channel needs an integer 'index'".into());
            };
            let Some(pos) = chans.iter().rposition(|x| x.index as i32 == index) else {
                return Err(format!("channel {index} does not exist"));
            };
            if c.contains_key("enabled") {
                let en = boolean(&c, "enabled");
                if mode == Mode::Dso as i32 {
                    self.s
                        .set_config(key::PROBE_EN, ConfValue::Bool(en), Some(pos));
                } else {
                    self.s.enable_channel(index as u16, en);
                }
            }
            if c.contains_key("name") {
                // QString::left(31): UTF-16 units
                let units: Vec<u16> = string(&c, "name").encode_utf16().take(31).collect();
                let name = String::from_utf16_lossy(&units);
                self.s.set_channel_name(index as u16, &name);
            }
        }
        // DSV_MSG_DEVICE_OPTIONS_UPDATED: the signals follow the channels
        self.s.reload_signals();
        self.s.device_updated();
        Ok(self.config_json())
    }

    fn set_trigger(&self, p: &Params) -> Reply {
        self.check_idle()?;
        if self.mode() != Mode::Logic as i32 {
            return Err("set_trigger supports logic mode; DSO triggers are set in the GUI".into());
        }
        let ty = p.get("type").and_then(Value::as_str).unwrap_or("none");
        let t = match ty {
            "none" => trig::NONE,
            "rising" => trig::RISING,
            "high" => trig::HIGH,
            "falling" => trig::FALLING,
            "low" => trig::LOW,
            "edge" => trig::EDGE,
            _ => return Err("type must be none, rising, falling, high, low or edge".into()),
        };
        let mut channel = -1;
        if t != trig::NONE {
            match req_int(p, "channel") {
                Some(c) => channel = c,
                None => return Err("parameter 'channel' is required for a trigger".into()),
            }
        }
        if !self.s.set_simple_trigger(channel, t) {
            return Err(format!("logic channel {channel} is not shown"));
        }
        if p.contains_key("position_percent") {
            match req_int(p, "position_percent") {
                Some(pos) if (0..=100).contains(&pos) => self.s.set_trigger_position(pos),
                _ => return Err("position_percent must be 0..100".into()),
            }
        }
        Ok(self.config_json())
    }

    fn set_threshold(&self, p: &Params) -> Reply {
        self.check_idle()?;
        let Some(v) = p.get("volts").and_then(Value::as_f64) else {
            return Err("parameter 'volts' (number) is required".into());
        };
        if self.s.config_f64(key::VTH).is_none() {
            return Err("this device has no adjustable threshold voltage".into());
        }
        if !(0.0..=5.0).contains(&v) {
            return Err("volts must be between 0 and 5".into());
        }
        self.s.set_config(key::VTH, ConfValue::F64(v), None);
        self.s.reload_signals();
        Ok(self.config_json())
    }

    fn set_dso(&self, p: &Params) -> Reply {
        self.check_idle()?;
        if self.mode() != Mode::Dso as i32 {
            return Err("set_dso needs oscilloscope mode (set_mode dso)".into());
        }
        let (Ok(tb), Ok(vdiv)) = (opt_u64(p, "timebase_ns"), opt_u64(p, "vdiv_mv")) else {
            return Err("timebase_ns and vdiv_mv must be non-negative integers".into());
        };
        let (tb, vdiv) = (tb.unwrap_or(0), vdiv.unwrap_or(0));
        if tb != 0 {
            let lo = self.s.config_u64(key::MIN_TIMEBASE).unwrap_or(0);
            let hi = self.s.config_u64(key::MAX_TIMEBASE).unwrap_or(0);
            if (lo != 0 && tb < lo) || (hi != 0 && tb > hi) {
                return Err(format!("timebase_ns must be within {lo}..{hi}"));
            }
            self.s.set_config(key::TIMEBASE, ConfValue::U64(tb), None);
            self.s.update_sample_rate_list();
            self.s.commit_settings();
        }
        if vdiv != 0 || p.contains_key("coupling") {
            let Some(channel) = req_int(p, "channel") else {
                return Err("parameter 'channel' is required for vdiv_mv/coupling".into());
            };
            let chans = self.s.channels();
            let pos = chans
                .iter()
                .position(|c| c.ty == channel_type::DSO && c.index as i32 == channel);
            let Some(pos) = pos.filter(|&i| chans[i].enabled) else {
                return Err(format!("dso channel {channel} is not enabled"));
            };
            if vdiv != 0 {
                self.step_vdiv(pos, vdiv);
            }
            if p.contains_key("coupling") {
                let cp = match string(p, "coupling") {
                    "dc" => 0u8,
                    "ac" => 1,
                    "gnd" => 2,
                    _ => return Err("coupling must be dc, ac or gnd".into()),
                };
                self.s
                    .set_config(key::PROBE_COUPLING, ConfValue::Byte(cp), Some(pos));
            }
        }
        Ok(self.config_json())
    }

    /// Steps the V/div dial toward `want` like the GUI knob (at most 64
    /// steps, never past it).
    fn step_vdiv(&self, ch: usize, want: u64) {
        let list: Vec<u64> = match self.s.config_list(key::PROBE_VDIV) {
            Some(ConfValue::U64s(v)) => v,
            Some(ConfValue::Dict(d)) => d
                .into_iter()
                .find_map(|(_, v)| match v {
                    ConfValue::U64s(v) => Some(v),
                    _ => None,
                })
                .unwrap_or_default(),
            _ => return,
        };
        for _ in 0..64 {
            let cur = self.s.channels().get(ch).map_or(0, |c| c.vdiv);
            if cur == want {
                break;
            }
            let i = list.iter().position(|&v| v == cur);
            let next = match (i, cur < want) {
                (Some(i), true) if i + 1 < list.len() => list[i + 1],
                (Some(i), false) if i > 0 => list[i - 1],
                _ => break,
            };
            if (cur < want && next > want) || (cur > want && next < want) {
                break;
            }
            if !self
                .s
                .set_config(key::PROBE_VDIV, ConfValue::U64(next), Some(ch))
            {
                break;
            }
        }
    }

    fn start_capture(&self, p: &Params) -> Reply {
        let (instant, wait, timeout, before) = {
            let _g = self.gui();
            self.check_idle()?;
            if !self.s.have_enabled_channel() {
                return Err("no channel is enabled; use set_channels".into());
            }
            let mode = self.mode();
            let instant = if p.contains_key("instant") {
                boolean(p, "instant")
            } else {
                mode == Mode::Dso as i32
            };
            let wait = !p.contains_key("wait") || boolean(p, "wait");
            let timeout = if p.contains_key("timeout_s") {
                double(p, "timeout_s", 60.0)
            } else {
                60.0
            }
            .clamp(1.0, 3600.0);
            let before = self.seqs();
            // the analog toolbar has no single-shot button
            if !(instant && mode == Mode::Analog as i32) && !self.s.start_capture(instant) {
                // the GUI's message box shows it as well
                let refused = self.s.state().start_error.clone();
                if let Some(e) = refused {
                    return Err(e);
                }
            }
            (instant, wait, timeout, before)
        };
        let _ = instant;
        if !wait {
            return Ok(json!({"started": true}));
        }
        self.wait_capture(before, Duration::from_secs_f64(timeout))
    }

    fn stop_capture(&self) -> Reply {
        if self.s.is_working() {
            self.s.stop_capture();
        }
        Ok(json!({"working": self.s.is_working()}))
    }

    fn capture_status(&self) -> Reply {
        let mut o = Map::new();
        let working = self.s.is_working();
        o.insert("working".into(), working.into());
        if working && let Some((t, prog)) = self.s.capture_status() {
            o.insert("triggered".into(), t.into());
            o.insert("progress_percent".into(), prog.into());
        }
        o.insert("decoding".into(), self.s.is_decoding().into());
        o.insert("saving".into(), self.s.state().saving.into());
        o.insert("capture".into(), self.capture_info_json());
        Ok(Value::Object(o))
    }

    // ------------------------------------------------------------ readout

    /// `walk_edges`: the level at `start` and the edges up to `end`.
    fn walk_edges(
        store: &LogicStore,
        ch: i32,
        start: u64,
        end: u64,
        limit: u64,
    ) -> (bool, Vec<u64>, bool) {
        let count = store.ring_sample_count();
        let mut index = start;
        let mut sample = store.get_sample(start, ch);
        let initial = sample;
        let mut edges = Vec::new();
        let mut truncated = false;
        while index <= end {
            if !store.get_nxt_edge(&mut index, sample, count, 1.0, ch) {
                break;
            }
            if index > end {
                break;
            }
            if edges.len() as u64 >= limit {
                truncated = true;
                break;
            }
            edges.push(index);
            sample = !sample;
        }
        (initial, edges, truncated)
    }

    fn get_edges(&self, p: &Params) -> Reply {
        self.check_logic_data()?;
        let store = self.logic_store();
        let channel = req_int(p, "channel").filter(|&c| store.has_data(c));
        let Some(channel) = channel else {
            return Err("parameter 'channel' must be a captured logic channel index".into());
        };
        let count = store.ring_sample_count();
        let (Ok(start), Ok(end), Ok(limit)) =
            (opt_u64(p, "start"), opt_u64(p, "end"), opt_u64(p, "limit"))
        else {
            return Err("start, end and limit must be non-negative integers".into());
        };
        let start = start.unwrap_or(0);
        let end = end
            .unwrap_or(count.wrapping_sub(1))
            .min(count.wrapping_sub(1));
        if start > end {
            return Err("start must not be greater than end".into());
        }
        let limit = limit.unwrap_or(1000).clamp(1, MAX_EDGES);
        let (initial, edges, truncated) = Self::walk_edges(&store, channel, start, end, limit);
        drop(store);
        let rate = self.snap_rate();
        let mut level = initial;
        let arr: Vec<Value> = edges
            .iter()
            .map(|&e| {
                level = !level;
                json!({
                    "sample": num(e),
                    "time_ns": crate::json::num(if rate > 0.0 { e as f64 * 1e9 / rate } else { 0.0 }),
                    "edge": if level { "rising" } else { "falling" },
                })
            })
            .collect();
        let mut o = Map::new();
        o.insert("channel".into(), channel.into());
        o.insert("samplerate".into(), crate::json::num(rate));
        o.insert("start".into(), num(start));
        o.insert("end".into(), num(end));
        o.insert("initial_level".into(), i32::from(initial).into());
        o.insert("edges".into(), arr.into());
        o.insert("truncated".into(), truncated.into());
        if truncated && let Some(l) = edges.last() {
            o.insert("next_start".into(), num(l + 1));
        }
        Ok(Value::Object(o))
    }

    fn read_logic(&self, p: &Params) -> Reply {
        self.check_logic_data()?;
        let store = self.logic_store();
        let count = store.ring_sample_count();
        let (Ok(start), Ok(end), Ok(limit)) = (
            opt_u64(p, "start"),
            opt_u64(p, "end"),
            opt_u64(p, "max_transitions"),
        ) else {
            return Err("start, end and max_transitions must be non-negative integers".into());
        };
        let start = start.unwrap_or(0);
        let end = end
            .unwrap_or(count.wrapping_sub(1))
            .min(count.wrapping_sub(1));
        if start > end {
            return Err("start must not be greater than end".into());
        }
        let limit = limit.unwrap_or(2000).clamp(1, MAX_EDGES);
        let channels: Vec<i32> = match p.get("channels").and_then(Value::as_array) {
            Some(a) => a
                .iter()
                .map(|v| match v.as_f64() {
                    Some(f) if f == f.floor() && f.abs() < 2147483648.0 => f as i32,
                    _ => -1,
                })
                .collect(),
            None => self
                .s
                .state()
                .signals
                .iter()
                .filter(|s| s.ty == channel_type::LOGIC && store.has_data(s.index as i32))
                .map(|s| s.index as i32)
                .collect(),
        };
        let mut out = Map::new();
        for ch in channels {
            if ch < 0 || !store.has_data(ch) {
                return Err(format!("channel {ch} has no captured data"));
            }
            let (initial, edges, truncated) = Self::walk_edges(&store, ch, start, end, limit);
            let t: Vec<Value> = edges.into_iter().map(num).collect();
            out.insert(
                ch.to_string(),
                json!({"initial_level": i32::from(initial), "transitions": t, "truncated": truncated}),
            );
        }
        drop(store);
        Ok(json!({
            "samplerate": crate::json::num(self.snap_rate()),
            "start": num(start),
            "end": num(end),
            "channels": out,
            "format": "each channel starts at initial_level at 'start' and toggles at every sample in transitions",
        }))
    }

    /// The enabled DSO channel `channel` (its position in the channel list).
    fn dso_channel(&self, channel: i32) -> Option<(usize, dsview_core::Channel)> {
        let chans = self.s.channels();
        chans
            .iter()
            .position(|c| c.ty == channel_type::DSO && c.index as i32 == channel && c.enabled)
            .map(|i| (i, chans[i].clone()))
    }

    /// Millivolts per sample code (the on-screen trace's calibration).
    fn mv_per_code(&self, pos: usize, c: &dsview_core::Channel) -> f64 {
        mv_per_code(&self.s, pos, c)
    }

    fn read_waveform(&self, p: &Params) -> Reply {
        if self.mode() != Mode::Dso as i32 {
            return Err("read_waveform supports oscilloscope (dso) mode".into());
        }
        let Some(channel) = req_int(p, "channel") else {
            return Err("parameter 'channel' is required".into());
        };
        let Some((pos, c)) = self.dso_channel(channel) else {
            return Err(format!("dso channel {channel} is not enabled"));
        };
        let mv_per_code = self.mv_per_code(pos, &c);
        let store = self.s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        if store.sample_count() == 0 || !store.has_data(channel) {
            return Err("no captured waveform; run start_capture first".into());
        }
        let n = store.sample_count();
        let Some(buf) = store.get_samples(0, channel) else {
            return Err("waveform is not available yet".into());
        };
        let (Ok(start), Ok(end), Ok(max_points)) = (
            opt_u64(p, "start"),
            opt_u64(p, "end"),
            opt_u64(p, "max_points"),
        ) else {
            return Err("start, end and max_points must be non-negative integers".into());
        };
        let start = start.unwrap_or(0);
        let end = end.unwrap_or(n - 1).min(n - 1);
        if start > end {
            return Err("start must not be greater than end".into());
        }
        let max_points = max_points.unwrap_or(1000).clamp(1, MAX_POINTS);
        let span = end - start + 1;
        let step = span.div_ceil(max_points);
        let hw_offset = c.hw_offset as f64;
        let (mut mins, mut maxs) = (Vec::new(), Vec::new());
        let mut i = start;
        while i <= end {
            let (mut lo, mut hi) = (255u8, 0u8);
            let mut j = i;
            while j < i + step && j <= end {
                let v = buf.get(j as usize).copied().unwrap_or(0);
                lo = lo.min(v);
                hi = hi.max(v);
                j += 1;
            }
            // a higher code is a lower voltage
            mins.push(crate::json::num(
                ((hw_offset - hi as f64) * mv_per_code * 1000.0).round() / 1000.0,
            ));
            maxs.push(crate::json::num(
                ((hw_offset - lo as f64) * mv_per_code * 1000.0).round() / 1000.0,
            ));
            i += step;
        }
        drop(store);
        let mut o = Map::new();
        o.insert("channel".into(), channel.into());
        o.insert("samplerate".into(), crate::json::num(self.snap_rate()));
        o.insert("start".into(), num(start));
        o.insert("end".into(), num(end));
        o.insert("samples_per_point".into(), num(step));
        if step == 1 {
            o.insert("mv".into(), maxs.into());
        } else {
            o.insert("min_mv".into(), mins.into());
            o.insert("max_mv".into(), maxs.into());
        }
        Ok(Value::Object(o))
    }

    // ------------------------------------------------------------ decoders

    fn list_decoders(&self, p: &Params) -> Reply {
        let filter = string(p, "filter").to_lowercase();
        let arr: Vec<Value> = dsview_decode::decoders()
            .iter()
            .filter(|d| {
                if filter.is_empty() {
                    return true;
                }
                let mut hay = format!("{} {} {}", d.id, d.name, d.longname).to_lowercase();
                for t in &d.tags {
                    hay.push(' ');
                    hay.push_str(&t.to_lowercase());
                }
                hay.contains(&filter)
            })
            .map(|d| {
                json!({
                    "id": d.id, "name": d.name, "longname": d.longname,
                    "inputs": d.inputs, "outputs": d.outputs, "tags": d.tags,
                })
            })
            .collect();
        Ok(json!({
            "decoders": arr,
            "note": "decoders with input 'logic' attach to channels; others stack on a decoder whose outputs match their inputs",
        }))
    }

    fn describe_decoder(&self, p: &Params) -> Reply {
        let Some(d) = dsview_decode::decoder_by_id(string(p, "id")) else {
            return Err("unknown decoder id; see list_decoders".into());
        };
        let chans = |l: &[dsview_decode::decoder::ChannelInfo]| -> Vec<Value> {
            l.iter()
                .map(|c| json!({"id": c.id, "name": c.name, "desc": c.desc}))
                .collect()
        };
        let options: Vec<Value> = d
            .options
            .iter()
            .map(|o| {
                let mut x = Map::new();
                x.insert("id".into(), o.id.clone().into());
                x.insert("desc".into(), o.desc.clone().unwrap_or_default().into());
                x.insert(
                    "default".into(),
                    o.def.as_ref().map_or(Value::Null, opt_value_json),
                );
                if !o.values.is_empty() {
                    let v: Vec<Value> = o.values.iter().map(opt_value_json).collect();
                    x.insert("values".into(), v.into());
                }
                Value::Object(x)
            })
            .collect();
        let rows: Vec<Value> = d
            .annotation_rows
            .iter()
            .map(|r| json!({"id": r.id, "desc": r.desc}))
            .collect();
        Ok(json!({
            "id": d.id, "name": d.name, "longname": d.longname, "desc": d.desc,
            "inputs": d.inputs, "outputs": d.outputs,
            "channels": chans(&d.channels),
            "optional_channels": chans(&d.opt_channels),
            "options": options,
            "annotation_rows": rows,
        }))
    }

    fn add_decoder(&self, p: &Params) -> Reply {
        let (index, run) = {
            let _g = self.gui();
            self.check_idle()?;
            if self.mode() != Mode::Logic as i32 {
                return Err("protocol decoders need logic analyzer mode".into());
            }
            let Some(root) = dsview_decode::decoder_by_id(string(p, "id")) else {
                return Err("unknown decoder id; see list_decoders".into());
            };
            if !root.inputs.iter().any(|i| i == "logic") {
                return Err(format!(
                    "'{}' does not take logic input; stack it on another decoder via 'stack'",
                    root.id
                ));
            }
            let map = obj(p, "channels");
            let logic: Vec<i32> = self.s.channels().iter().map(|c| c.index as i32).collect();
            let mut channel_arr = Vec::new();
            for (k, v) in &map {
                let known = root
                    .channels
                    .iter()
                    .chain(&root.opt_channels)
                    .any(|c| c.id == *k);
                if !known {
                    return Err(format!("decoder '{}' has no channel '{k}'", root.id));
                }
                let index = match v.as_f64() {
                    Some(f) if f == f.floor() && f.abs() < 2147483648.0 => f as i32,
                    _ => -1,
                };
                if !logic.contains(&index) {
                    return Err(format!("logic channel {index} does not exist"));
                }
                channel_arr.push(json!({ k: index }));
            }
            for c in &root.channels {
                if !map.contains_key(&c.id) {
                    return Err(format!("required channel '{}' is not mapped", c.id));
                }
            }
            let options = obj(p, "options");
            check_options(&root, &options)?;
            let mut stacked = Vec::new();
            let mut prev_outputs = root.outputs.clone();
            for v in p
                .get("stack")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let so = v.as_object().cloned().unwrap_or_default();
                let sid = string(&so, "id");
                let Some(d) = dsview_decode::decoder_by_id(sid) else {
                    return Err(format!("unknown stacked decoder id: {sid}"));
                };
                if !d.inputs.iter().any(|i| prev_outputs.contains(i)) {
                    return Err(format!(
                        "'{}' cannot stack on the previous decoder (inputs don't match its outputs)",
                        d.id
                    ));
                }
                let sopts = obj(&so, "options");
                check_options(&d, &sopts)?;
                stacked.push(json!({"id": d.id, "options": sopts}));
                prev_outputs = d.outputs.clone();
            }
            let mut dec = Map::new();
            dec.insert("id".into(), root.id.clone().into());
            dec.insert("channel".into(), channel_arr.into());
            dec.insert("options".into(), options.into());
            if !stacked.is_empty() {
                dec.insert("stacked decoders".into(), stacked.into());
            }
            if store::load_decoders(&self.s, &[Value::Object(dec)]) == 0 {
                return Err("DSView could not add the decoder".into());
            }
            let index = self.s.state().decoders.len() - 1;
            let run = !p.contains_key("run") || boolean(p, "run");
            let started = run && self.s.have_view_data() && self.s.start_decode(index);
            if !started {
                return Ok(json!({"decoders": [self.decoder_state_json(index)]}));
            }
            (index, run)
        };
        let _ = run;
        self.wait_decoders(&[index], Duration::from_secs(120))
    }

    fn list_active_decoders(&self) -> Reply {
        let count = self.s.state().decoders.len();
        let arr: Vec<Value> = (0..count).map(|i| self.decoder_state_json(i)).collect();
        Ok(json!({"decoders": arr, "decoding": self.s.is_decoding()}))
    }

    fn remove_decoder(&self, p: &Params) -> Reply {
        let count = self.s.state().decoders.len() as i32;
        let index = req_int(p, "index").filter(|&i| i >= 0 && i < count);
        let Some(index) = index else {
            return Err(
                "parameter 'index' must be a decoder index from list_active_decoders".into(),
            );
        };
        if self.s.is_working() {
            return Err("a capture is running".into());
        }
        if !self.s.remove_decoder(index as usize) {
            return Err("could not remove the decoder".into());
        }
        Ok(json!({"removed": index}))
    }

    fn run_decoders(&self, p: &Params) -> Reply {
        let (indexes, timeout) = {
            let _g = self.gui();
            self.check_idle()?;
            if !self.s.have_view_data() {
                return Err("no captured data to decode".into());
            }
            let count = self.s.state().decoders.len() as i32;
            let indexes: Vec<usize> = if p.contains_key("index") {
                match req_int(p, "index").filter(|&i| i >= 0 && i < count) {
                    Some(i) => vec![i as usize],
                    None => {
                        return Err("parameter 'index' must be a decoder index from \
                                    list_active_decoders"
                            .into());
                    }
                }
            } else {
                (0..count as usize).collect()
            };
            if indexes.is_empty() {
                return Err("no decoders are configured; use add_decoder".into());
            }
            for &i in &indexes {
                self.s.start_decode(i);
            }
            let timeout = if p.contains_key("timeout_s") {
                double(p, "timeout_s", 120.0)
            } else {
                120.0
            };
            (indexes, timeout.clamp(1.0, 3600.0))
        };
        self.wait_decoders(&indexes, Duration::from_secs_f64(timeout))
    }

    fn get_annotations(&self, p: &Params) -> Reply {
        let traces = self.s.state().decoders.clone();
        let count = traces.len() as i32;
        let index = req_int(p, "decoder").filter(|&i| i >= 0 && i < count);
        let Some(index) = index else {
            return Err(
                "parameter 'decoder' must be a decoder index from list_active_decoders".into(),
            );
        };
        if self.s.is_decoding() {
            return Err("decoding is still running; call run_decoders (it waits) and retry".into());
        }
        let (Ok(start), Ok(end), Ok(offset), Ok(limit)) = (
            opt_u64(p, "start"),
            opt_u64(p, "end"),
            opt_u64(p, "offset"),
            opt_u64(p, "limit"),
        ) else {
            return Err("start, end, offset and limit must be non-negative integers".into());
        };
        let start = start.unwrap_or(0);
        let end = end.unwrap_or(u64::MAX >> 12);
        let offset = offset.unwrap_or(0);
        let limit = limit.unwrap_or(500).clamp(1, MAX_ANNOTATIONS);
        let row_filter = string(p, "row").to_lowercase();
        let stack: &DecoderStack = &traces[index as usize].stack;
        let rate = stack.sample_rate() as f64;
        let mut items = Vec::new();
        for (ri, row) in stack.rows().iter().enumerate() {
            let title = row.title();
            let id = row.id().unwrap_or("");
            if !row_filter.is_empty()
                && !title.to_lowercase().contains(&row_filter)
                && id.to_lowercase() != row_filter
            {
                continue;
            }
            for a in stack.annotations(ri, start, end) {
                items.push((a.start, a.end, title.clone(), a.text_in(stack.format())));
            }
        }
        items.sort_by_key(|x| x.0);
        let arr: Vec<Value> = items
            .iter()
            .skip(offset.min(usize::MAX as u64) as usize)
            .take(limit as usize)
            .map(|(s, e, row, text)| {
                let mut o = Map::new();
                o.insert("start_sample".into(), num(*s));
                o.insert("end_sample".into(), num(*e));
                if rate > 0.0 {
                    o.insert("start_ns".into(), crate::json::num(*s as f64 * 1e9 / rate));
                }
                o.insert("row".into(), row.clone().into());
                o.insert("text".into(), text.clone().into());
                Value::Object(o)
            })
            .collect();
        let shown = arr.len() as u64;
        let mut o = Map::new();
        o.insert("decoder".into(), index.into());
        o.insert("total".into(), num(items.len() as u64));
        o.insert("offset".into(), num(offset));
        o.insert("annotations".into(), arr.into());
        if offset + shown < items.len() as u64 {
            o.insert("next_offset".into(), num(offset + shown));
        }
        Ok(Value::Object(o))
    }

    // ------------------------------------------------------------ files, measurements, view

    fn open_file(&self, p: &Params) -> Reply {
        let before = {
            let _g = self.gui();
            if self.s.is_working() || self.s.state().saving {
                return Err("DSView is capturing or saving".into());
            }
            let path = string(p, "path");
            let fi = std::path::Path::new(path);
            let readable = std::fs::File::open(fi).is_ok() && fi.is_file();
            if path.is_empty() || !readable {
                return Err(format!("file not found or not readable: {path}"));
            }
            let abs = std::fs::canonicalize(fi)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_owned());
            let before = self.seqs();
            if self.s.set_file(&abs).is_err() {
                // "Failed to load": DSView falls back to the default device
                self.s.set_default_device();
            }
            before
        };
        self.wait_capture(before, Duration::from_secs(120))
    }

    fn store_job(&self, path: &str, export: Option<dsview_formats::export::Format>) -> Reply {
        self.s.set_saving(true);
        let r = match export {
            None => store::save_dsl(&self.s, std::path::Path::new(path), &self.version),
            Some(f) => store::export(&self.s, std::path::Path::new(path), f),
        };
        self.s.set_saving(false);
        r.map(|()| json!({"path": path}))
    }

    fn save_session(&self, p: &Params) -> Reply {
        self.check_idle()?;
        let path = string(p, "path");
        if !path.ends_with(".dsl") {
            return Err("path must end with .dsl".into());
        }
        if !self.s.have_view_data() {
            return Err("no captured data to save".into());
        }
        self.store_job(path, None)
    }

    fn export_data(&self, p: &Params) -> Reply {
        self.check_idle()?;
        let path = string(p, "path");
        let suffix = std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let format = p
            .get("format")
            .and_then(Value::as_str)
            .map_or(suffix, str::to_owned)
            .to_lowercase();
        let f = match format.as_str() {
            "csv" => dsview_formats::export::Format::Csv,
            "vcd" => dsview_formats::export::Format::Vcd,
            "srzip" => dsview_formats::export::Format::Srzip,
            _ => return Err("format must be csv, vcd or srzip".into()),
        };
        if format != "csv" && self.mode() != Mode::Logic as i32 {
            return Err(format!("{format} export needs logic data; use csv"));
        }
        if !path.ends_with(&format!(".{format}")) {
            return Err(format!("path must end with .{format}"));
        }
        if !self.s.have_view_data() {
            return Err("no captured data to export".into());
        }
        self.store_job(path, Some(f))
    }

    fn measure_logic(&self, p: &Params) -> Reply {
        self.check_logic_data()?;
        let store = self.logic_store();
        let channel = req_int(p, "channel").filter(|&c| store.has_data(c));
        let Some(channel) = channel else {
            return Err("parameter 'channel' must be a captured logic channel index".into());
        };
        let count = store.ring_sample_count();
        let (Ok(start), Ok(end)) = (opt_u64(p, "start"), opt_u64(p, "end")) else {
            return Err("start and end must be non-negative integers".into());
        };
        let start = start.unwrap_or(0);
        let end = end
            .unwrap_or(count.wrapping_sub(1))
            .min(count.wrapping_sub(1));
        if start > end {
            return Err("start must not be greater than end".into());
        }
        let (initial, edges, truncated) = Self::walk_edges(&store, channel, start, end, 10_000_000);
        drop(store);
        let rate = self.snap_rate();
        let ns = if rate > 0.0 { 1e9 / rate } else { 0.0 };
        let n = edges.len();
        let high_after = |j: usize| initial ^ ((j + 1) & 1 == 1);
        let (mut rising, mut falling) = (0u64, 0u64);
        let (mut first_rise, mut last_rise) = (None, None);
        let (mut min_high, mut max_high, mut min_low, mut max_low) = (u64::MAX, 0, u64::MAX, 0);
        for j in 0..n {
            if high_after(j) {
                rising += 1;
                first_rise.get_or_insert(j);
                last_rise = Some(j);
            } else {
                falling += 1;
            }
            if j + 1 < n {
                let w = edges[j + 1] - edges[j];
                if high_after(j) {
                    min_high = min_high.min(w);
                    max_high = max_high.max(w);
                } else {
                    min_low = min_low.min(w);
                    max_low = max_low.max(w);
                }
            }
        }
        let periods = rising.saturating_sub(1);
        let (mut period_total, mut high_total) = (0u64, 0u64);
        if periods > 0
            && let (Some(f), Some(l)) = (first_rise, last_rise)
        {
            period_total = edges[l] - edges[f];
            for j in f..l {
                if high_after(j) {
                    high_total += edges[j + 1] - edges[j];
                }
            }
        }
        let mut o = Map::new();
        o.insert("channel".into(), channel.into());
        o.insert("start".into(), num(start));
        o.insert("end".into(), num(end));
        o.insert("initial_level".into(), i32::from(initial).into());
        o.insert("rising_edges".into(), num(rising));
        o.insert("falling_edges".into(), num(falling));
        let f = crate::json::num;
        if periods > 0 {
            let period = period_total as f64 / periods as f64;
            o.insert("period_ns".into(), f(period * ns));
            o.insert("frequency_hz".into(), f(rate / period));
            if period_total > 0 {
                o.insert(
                    "duty_percent".into(),
                    f(100.0 * high_total as f64 / period_total as f64),
                );
            }
        }
        if max_high > 0 {
            o.insert("min_high_ns".into(), f(min_high as f64 * ns));
            o.insert("max_high_ns".into(), f(max_high as f64 * ns));
        }
        if max_low > 0 {
            o.insert("min_low_ns".into(), f(min_low as f64 * ns));
            o.insert("max_low_ns".into(), f(max_low as f64 * ns));
        }
        if truncated {
            o.insert("truncated".into(), true.into());
        }
        Ok(Value::Object(o))
    }

    fn measure_dso(&self, p: &Params) -> Reply {
        if self.mode() != Mode::Dso as i32 {
            return Err("measure_dso needs oscilloscope mode".into());
        }
        let Some(channel) = req_int(p, "channel") else {
            return Err("parameter 'channel' is required".into());
        };
        let Some(m) = dso_measurements(&self.s, channel) else {
            return Err(format!("dso channel {channel} is not enabled"));
        };
        Ok(json!({
            "channel": channel,
            "measurements": m,
            "note": "values are formatted as shown in DSView; '--' means not available",
        }))
    }

    fn show_region(&self, p: &Params) -> Reply {
        let (Ok(start), Ok(end)) = (opt_u64(p, "start"), opt_u64(p, "end")) else {
            return Err("start and end are sample indexes with end > start".into());
        };
        let (start, end) = (start.unwrap_or(0), end.unwrap_or(0));
        if end <= start {
            return Err("start and end are sample indexes with end > start".into());
        }
        if !self.s.have_view_data() {
            return Err("no captured data".into());
        }
        self.s.state().region = Some((start, end));
        self.view.show_region(start, end);
        Ok(json!({"shown": true}))
    }

    fn set_cursors(&self, p: &Params) -> Reply {
        let Some(samples) = p.get("samples").and_then(Value::as_array) else {
            return Err(
                "parameter 'samples' must be an array of sample indexes (empty clears)".into(),
            );
        };
        if samples.len() > 16 {
            return Err("at most 16 cursors".into());
        }
        let mut cursors = Vec::new();
        self.s.state().cursors.clear();
        for v in samples {
            match v.as_f64() {
                Some(f) if f >= 0.0 => cursors.push(f as u64),
                _ => return Err("cursor positions must be non-negative sample indexes".into()),
            }
            self.s.state().cursors = cursors.clone();
        }
        self.view.set_cursors(&cursors);
        Ok(json!({"cursors": samples.len()}))
    }

    fn version(&self) -> Reply {
        let mut impls = Map::new();
        for sub in [
            "FORMATS",
            "STORE",
            "DECODE",
            "DRV_DEMO",
            "DRV_FILE",
            "DRV_DSLOGIC",
            "DRV_DSCOPE",
            "ACQ",
        ] {
            impls.insert(sub.to_lowercase(), "rust".into());
        }
        Ok(json!({
            "application": "DSView",
            "version": self.version,
            "api": 1,
            "implementations": impls,
        }))
    }
}

impl ControlBackend for Api {
    fn call(&self, method: &str, p: &Map<String, Value>) -> Result<Value, String> {
        if MUTATING.contains(&method)
            && let Some(text) = self.view.dialog_open()
        {
            return Err(format!(
                "DSView is showing a dialog ({text}); ask the user to close it in the DSView \
                 window, then retry"
            ));
        }
        match method {
            // methods that wait take the GUI lock themselves
            "start_capture" => self.start_capture(p),
            "add_decoder" => self.add_decoder(p),
            "run_decoders" => self.run_decoders(p),
            "open_file" => self.open_file(p),
            _ => {
                let _g = self.gui();
                match method {
                    "list_devices" => self.list_devices(),
                    "select_device" => self.select_device(p),
                    "get_config" => Ok(self.config_json()),
                    "set_mode" => self.set_mode(p),
                    "set_sampling" => self.set_sampling(p),
                    "set_channels" => self.set_channels(p),
                    "set_trigger" => self.set_trigger(p),
                    "set_threshold" => self.set_threshold(p),
                    "set_dso" => self.set_dso(p),
                    "stop_capture" => self.stop_capture(),
                    "capture_status" => self.capture_status(),
                    "capture_info" => Ok(self.capture_info_json()),
                    "get_edges" => self.get_edges(p),
                    "read_logic" => self.read_logic(p),
                    "read_waveform" => self.read_waveform(p),
                    "list_decoders" => self.list_decoders(p),
                    "describe_decoder" => self.describe_decoder(p),
                    "list_active_decoders" => self.list_active_decoders(),
                    "remove_decoder" => self.remove_decoder(p),
                    "get_annotations" => self.get_annotations(p),
                    "save_session" => self.save_session(p),
                    "export_data" => self.export_data(p),
                    "measure_logic" => self.measure_logic(p),
                    "measure_dso" => self.measure_dso(p),
                    "show_region" => self.show_region(p),
                    "set_cursors" => self.set_cursors(p),
                    "version" => self.version(),
                    m => Err(format!("unknown method: {m}")),
                }
            }
        }
    }
}

/// Millivolts per sample code of DSO channel `c` at list position `pos`
/// (the on-screen trace's calibration), with the V/div of the capture as
/// `DsoSignal::get_voltage` (a V/div changed afterwards only stretches the
/// trace).
pub fn mv_per_code(s: &Session, pos: usize, c: &dsview_core::Channel) -> f64 {
    let vdiv = s
        .state()
        .cap_vdiv
        .iter()
        .find(|(i, _)| *i == c.index)
        .map_or(c.vdiv, |&(_, v)| v);
    let ref_min = s
        .config(key::REF_MIN, None)
        .map_or(1.0, |v| v.get_u32() as f64);
    let ref_max = s
        .config(key::REF_MAX, None)
        .map_or(((1u32 << c.bits.clamp(1, 16)) - 1) as f64, |v| {
            v.get_u32() as f64
        });
    let factor = s
        .config(key::PROBE_FACTOR, Some(pos))
        .map_or(1, |v| v.get_u64()) as f64;
    let range = (ref_max - ref_min).max(1.0);
    vdiv as f64 * factor * dsview_session::session::DSO_VDIVS as f64 / range
}

/// Runs `f` on the samples, hw offset, mV per code, rate and status block
/// of enabled DSO channel `channel` (None if it is not one).
fn with_dso_channel<T>(
    s: &Session,
    channel: i32,
    f: impl FnOnce(Option<&[u8]>, f64, f64, f64, Option<&dsview_dsp::measure::HwStatus>) -> T,
) -> Option<T> {
    let chans = s.channels();
    let pos = chans
        .iter()
        .position(|c| c.ty == channel_type::DSO && c.index as i32 == channel && c.enabled)?;
    let c = &chans[pos];
    let mv_per_code = mv_per_code(s, pos, c);
    let rate = s.state().cur_snap_samplerate as f64;
    let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
    let data = store
        .get_samples(0, channel)
        .map(|d| &d[..(store.sample_count() as usize).min(d.len())]);
    let hw = s
        .state()
        .dso_status
        .map(|status| dsview_dsp::measure::HwStatus {
            status,
            index: chans
                .iter()
                .filter(|x| x.ty == channel_type::DSO)
                .position(|x| x.index as i32 == channel)
                .unwrap_or(0),
            total_channels: chans.len() as u64,
            enabled_channels: chans
                .iter()
                .filter(|x| x.ty == channel_type::DSO && x.enabled)
                .count() as u64,
            is_file: store.is_file(),
        });
    Some(f(data, c.hw_offset as f64, mv_per_code, rate, hw.as_ref()))
}

/// The twenty measurements of enabled DSO channel `channel` as DSView
/// shows them (`DsoSignal::get_measure`), keyed by name; None if the
/// channel is not an enabled DSO channel.
pub fn dso_measurements(s: &Session, channel: i32) -> Option<Value> {
    with_dso_channel(s, channel, |data, hw_offset, mv, rate, hw| {
        dsview_dsp::measure::measure(data, hw_offset, mv, rate, hw)
    })
}

/// The raw measurement of enabled DSO channel `channel` (codes, ns), as
/// `DsoSignal` keeps it for auto-set.
pub fn dso_measured(s: &Session, channel: i32) -> Option<dsview_dsp::measure::M> {
    with_dso_channel(s, channel, |data, hw_offset, _, rate, hw| {
        dsview_dsp::measure::measure_raw(data, hw_offset, rate, hw)
    })
}
