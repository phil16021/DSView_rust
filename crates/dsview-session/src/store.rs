//! Session files: the decoder and session JSON of `.dsl` and `.demo` files
//! (`StoreSession::load_decoders`, `gen_decoders_json`,
//! `MainWindow::load_config_from_json`, `gen_config_json`), saving a
//! capture as `.dsl` (`StoreSession::save_start`/`save_logic`) and
//! exporting it as CSV or VCD (`StoreSession::export_exec`).

use std::io::Write;
use std::path::Path;

use dsview_core::{ConfValue, Mode, channel_type, key};
use dsview_formats::dsl::{DslReader, LogicWriter, WriteChannel};
use dsview_formats::export::{ChannelKind, ExportChannel, ExportPacket, Exporter, Format};
use dsview_formats::srzip::SrzipWriter;
use dsview_formats::zipw::DslZipWriter;
use serde_json::{Map, Value, json};

use crate::decode::{DecoderStack, StackDecoder, opt_json, typed_options};
use crate::session::{DecodeTrace, Session};

/// `SESSION_FORMAT_VERSION`, `DEOCDER_CONFIG_VERSION`
const SESSION_FORMAT_VERSION: i64 = 3;
const DECODER_CONFIG_VERSION: i64 = 2;
/// `QLocale::English`, DSView's language code in saved sessions.
const LANGUAGE: i64 = 31;

/// `sr_config_info.datatype`
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfType {
    Char,
    U64,
    Bool,
    U8,
    List,
    Float,
    RationalVolt,
}

/// `sr_config_info_data` (hwdriver.c): key, type and the name sessions use.
pub fn config_info(k: i32) -> Option<(ConfType, &'static str)> {
    use ConfType::*;
    Some(match k {
        key::CONN => (Char, "Connection"),
        key::SERIALCOMM => (Char, "Serial communication"),
        key::SAMPLERATE => (U64, "Sample rate"),
        key::LIMIT_SAMPLES => (U64, "Sample count"),
        key::ACTUAL_SAMPLES => (U64, "Sample count-actual"),
        key::CLOCK_TYPE => (Bool, "Using External Clock"),
        key::EXT_SAMPLERATE => (U64, "Ext. Clock Frequency [Hz]"),
        key::CLOCK_EDGE => (Bool, "Using Clock Negedge"),
        key::CAPTURE_RATIO => (U64, "Pre-trigger capture ratio"),
        key::PATTERN_MODE => (Char, "Operation Mode"),
        key::RLE => (Bool, "Run Length Encoding"),
        key::WAIT_UPLOAD => (Bool, "Wait Buffer Upload"),
        key::TRIGGER_SLOPE => (U8, "Trigger slope"),
        key::TRIGGER_SOURCE => (U8, "Trigger source"),
        key::TRIGGER_CHANNEL => (U8, "Trigger channel"),
        key::HORIZ_TRIGGERPOS => (U8, "Horizontal trigger position"),
        key::TRIGGER_HOLDOFF => (U64, "Trigger hold off"),
        key::TRIGGER_MARGIN => (U8, "Trigger margin"),
        key::BUFFERSIZE => (U64, "Buffer size"),
        key::TIMEBASE => (U64, "Time base"),
        key::MAX_HEIGHT => (Char, "Max Height"),
        key::MAX_HEIGHT_VALUE => (U8, "Max Height value"),
        key::FILTER => (List, "Filter Targets"),
        key::DATALOG => (Bool, "Datalog"),
        key::OPERATION_MODE => (List, "Operation Mode"),
        key::BUFFER_OPTIONS => (List, "Stop Options"),
        key::CHANNEL_MODE => (List, "Channel Mode"),
        key::THRESHOLD => (List, "Threshold Levels"),
        key::VTH => (Float, "Threshold Level"),
        key::RLE_SUPPORT => (Bool, "Enable RLE Compress"),
        key::BANDWIDTH_LIMIT => (List, "Bandwidth Limit"),
        key::PROBE_COUPLING => (Char, "Coupling"),
        key::PROBE_VDIV => (RationalVolt, "Volts/div"),
        key::PROBE_FACTOR => (U64, "Probe Factor"),
        key::PROBE_MAP_DEFAULT => (Bool, "Map Default"),
        key::PROBE_MAP_UNIT => (Char, "Map Unit"),
        key::PROBE_MAP_MIN => (Float, "Map Min"),
        key::PROBE_MAP_MAX => (Float, "Map Max"),
        _ => return None,
    })
}

/// `QJsonDocument::toJson()`: indented by four spaces (keys sorted).
pub fn qt_json(v: &Value) -> String {
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b"    ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    serde::Serialize::serialize(v, &mut ser).expect("JSON to memory");
    let mut s = String::from_utf8(out).expect("serde_json writes UTF-8");
    s.push('\n');
    s
}

/// `QJsonValue::toInt()`: integral doubles in int range, else 0.
fn to_int(v: Option<&Value>) -> i32 {
    match v.and_then(Value::as_f64) {
        Some(f) if f.fract() == 0.0 && f >= i32::MIN as f64 && f <= i32::MAX as f64 => f as i32,
        _ => 0,
    }
}

// ------------------------------------------------------------ decoders

/// `StoreSession::load_decoders`: adds a decoder trace per entry (unknown
/// decoders are skipped). Returns how many were added.
pub fn load_decoders(s: &Session, entries: &[Value]) -> usize {
    if s.mode() != Mode::Logic as i32 {
        sr_info!("StoreSession::load_decoders(), is not LOGIC mode.");
        return 0;
    }
    let mut added = 0;
    for entry in entries {
        let empty = Map::new();
        let obj = entry.as_object().unwrap_or(&empty);
        let stacked: Vec<&Map<String, Value>> = obj
            .get("stacked decoders")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .collect();
        let id = obj.get("id").and_then(Value::as_str).unwrap_or("");
        let Some(root) = dsview_decode::decoder_by_id(id) else {
            sr_err!("Protocol not exists! id:{id}");
            continue;
        };
        let subs: Vec<_> = stacked
            .iter()
            .filter_map(|o| o.get("id").and_then(Value::as_str))
            .filter_map(dsview_decode::decoder_by_id)
            .collect();

        let mut decoders = vec![StackDecoder::new(root.clone())];
        decoders.extend(subs.iter().cloned().map(StackDecoder::new));
        let show = obj.get("show").and_then(Value::as_object);
        let channel_objs: Vec<&Map<String, Value>> = obj
            .get("channel")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .collect();
        for (n, d) in decoders.iter_mut().enumerate() {
            let options = if n == 0 {
                for c in d.info.channels.iter().chain(&d.info.opt_channels) {
                    if let Some(o) = channel_objs.iter().find(|o| o.contains_key(&c.id)) {
                        d.probes.push((c.id.clone(), to_int(o.get(&c.id))));
                    }
                }
                obj.get("options").and_then(Value::as_object)
            } else {
                stacked
                    .iter()
                    .find(|o| o.get("id").and_then(Value::as_str) == Some(&d.info.id))
                    .and_then(|o| o.get("options"))
                    .and_then(Value::as_object)
            };
            d.options = typed_options(&d.info, options);
            if let Some(v) = show.and_then(|s| s.get(&d.info.id)) {
                d.shown = v.as_bool().unwrap_or(false);
            }
        }

        let mut stack = DecoderStack::new(decoders);
        if let Some(show) = show {
            let by_id = obj.contains_key("version");
            for i in 0..stack.rows().len() {
                let r = &stack.rows()[i];
                let k = if by_id { r.title_id() } else { r.title() };
                if let Some(v) = show.get(&k) {
                    stack.set_row_shown(i, v.as_bool().unwrap_or(false));
                }
            }
        }
        // the trace is named after the top decoder, or its label
        let mut name = root.name.clone();
        if let Some(last) = subs.last()
            && !last.name.is_empty()
        {
            name = last.name.clone();
        }
        if let Some(l) = obj.get("label").and_then(Value::as_str) {
            name = l.to_owned();
        }
        let trace = DecodeTrace::new(name, stack);
        if obj.contains_key("view_index") {
            *trace.view_index.lock().unwrap() = to_int(obj.get("view_index"));
        }
        s.add_decoder(trace);
        added += 1;
    }
    added
}

/// Decoder options that name a program to run or a file to read
/// (`arm_itm` and `arm_etmv3`: `objdump`, `elffile`; `numbers_and_state`:
/// `mapping`). A file the user opens does not set them by itself: they
/// wait for the user's yes (DEVIATIONS, "Decoder options from files").
pub const GUARDED_OPTIONS: &[&str] = &["objdump", "elffile", "mapping"];

/// A decoder trace from a file, loaded without its guarded options.
#[derive(Clone)]
pub struct HeldDecoder {
    pub trace: std::sync::Arc<DecodeTrace>,
    /// The entry as the file has it.
    pub entry: Value,
    /// `decoder: option = value` per held option, for the question.
    pub settings: Vec<String>,
}

/// The entry without its guarded options (those set to text), and what
/// was taken out.
fn without_guarded(entry: &Value) -> (Value, Vec<String>) {
    let mut out = entry.clone();
    let mut held = Vec::new();
    let mut strip = |o: &mut Value| {
        let id = o.get("id").and_then(Value::as_str).unwrap_or("").to_owned();
        if let Some(opts) = o.get_mut("options").and_then(Value::as_object_mut) {
            for k in GUARDED_OPTIONS {
                if let Some(Value::String(v)) = opts.get(*k)
                    && !v.is_empty()
                {
                    held.push(format!("{id}: {k} = {v}"));
                    opts.remove(*k);
                }
            }
        }
    };
    strip(&mut out);
    if let Some(a) = out
        .get_mut("stacked decoders")
        .and_then(Value::as_array_mut)
    {
        a.iter_mut().for_each(&mut strip);
    }
    (out, held)
}

/// [`load_decoders`] for decoders from a file the user opened: guarded
/// options are left at their defaults and held for
/// [`Session::apply_held_decoder`].
pub fn load_file_decoders(s: &Session, entries: &[Value]) -> usize {
    let mut added = 0;
    for entry in entries {
        let (stripped, settings) = without_guarded(entry);
        if load_decoders(s, std::slice::from_ref(&stripped)) == 0 {
            continue;
        }
        added += 1;
        if !settings.is_empty() {
            let mut st = s.state();
            let trace = st.decoders.last().expect("just added").clone();
            sr_info!(
                "Decoder options held for confirmation: {}",
                settings.join(", ")
            );
            st.held_decoders.push(HeldDecoder {
                trace,
                entry: entry.clone(),
                settings,
            });
        }
    }
    added
}

/// `StoreSession::gen_decoders_json`
pub fn decoders_json(s: &Session) -> Value {
    let traces = s.state().decoders.clone();
    let mut out = Vec::new();
    for t in traces {
        let mut obj = Map::new();
        let mut stacked = Vec::new();
        let mut show = Map::new();
        for (k, d) in t.stack.decoders().iter().enumerate() {
            let mut options = Map::new();
            for o in &d.info.options {
                let v = d.options.get(&o.id).or(o.def.as_ref());
                if let Some(v) = v {
                    options.insert(o.id.clone(), opt_json(v));
                }
            }
            if d.has_channels() {
                let ch: Vec<Value> = d
                    .bound_probes()
                    .into_iter()
                    .map(|(id, i)| json!({ id: i }))
                    .collect();
                obj.insert("id".into(), d.info.id.clone().into());
                obj.insert("channel".into(), ch.into());
                obj.insert("options".into(), options.into());
            } else {
                stacked.push(json!({"id": d.info.id, "options": options}));
            }
            show.insert(d.info.id.clone(), t.stack.decoder_shown(k).into());
        }
        obj.insert("version".into(), DECODER_CONFIG_VERSION.into());
        obj.insert("label".into(), t.name().into());
        obj.insert("stacked decoders".into(), stacked.into());
        obj.insert("view_index".into(), (*t.view_index.lock().unwrap()).into());
        for (i, r) in t.stack.rows().iter().enumerate() {
            show.insert(r.title_id(), t.stack.row_shown(i).into());
        }
        obj.insert("show".into(), show.into());
        out.push(Value::Object(obj));
    }
    Value::Array(out)
}

/// Reads a JSON entry of a `.dsl`/`.demo` file (`get_*_json_from_data_file`).
fn read_entry(path: &Path, name: &str) -> Option<Value> {
    let mut r = DslReader::open(path).ok()?;
    let bytes = r.meta_entry(name).ok().flatten()?;
    // qstrnlen: the text ends at the first NUL
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    serde_json::from_slice(&bytes[..end]).ok()
}

/// `load_demo_decoder_config`: the decoders of a demo pattern and the
/// channels' view positions.
pub fn load_demo_decoders(s: &Session, file: &Path) {
    if let Some(Value::Array(a)) = read_entry(file, "decoders") {
        load_decoders(s, &a);
    }
    if let Some(doc) = read_entry(file, "session") {
        load_channel_view_indexes(s, &doc);
    }
}

/// `load_channel_view_indexs`: positions by signal order (C reads past the
/// end when the file lists fewer channels; extra signals keep theirs here).
fn load_channel_view_indexes(s: &Session, doc: &Value) {
    if s.mode() != Mode::Logic as i32 {
        return;
    }
    let idx: Vec<i32> = doc
        .get("channel")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|o| o.get("view_index").is_some())
        .map(|o| to_int(o.get("view_index")))
        .collect();
    let mut st = s.state();
    for (sig, v) in st.signals.iter_mut().zip(idx) {
        sig.view_index = v;
    }
}

/// The capture file device just became active (MainWindow's `is_file()`
/// branch): its session settings and decoders, then the replay.
pub fn load_file_session(s: &Session) {
    let path = match s.state().device.as_ref().map(|d| d.kind.clone()) {
        Some(crate::Kind::File(p)) => p,
        _ => return,
    };
    let path = Path::new(&path);
    let mut have_decoders = false;
    if let Some(doc) = read_entry(path, "session") {
        have_decoders = load_config(s, &doc, true);
    }
    if !have_decoders
        && s.mode() == Mode::Logic as i32
        && let Some(Value::Array(a)) = read_entry(path, "decoders")
    {
        load_file_decoders(s, &a);
    }
    // MainWindow replays the file 100 ms later
    s.start_capture(true);
}

/// `load_probe_analog_settings`: only what the session stores (a logic
/// session must not zero a DSO channel's settings).
fn load_probe_analog_settings(ch: &mut dsview_core::Channel, o: &Map<String, Value>) {
    let num = |k: &str| o.get(k).and_then(Value::as_f64);
    if let Some(v) = num("vdiv").filter(|v| *v > 0.0) {
        ch.vdiv = v as u64;
    }
    if let Some(v) = num("coupling") {
        ch.coupling = v as u8;
    }
    if let Some(v) = num("vfactor").filter(|v| *v > 0.0) {
        ch.vfactor = v as u64;
    }
    if let Some(v) = num("trigValue") {
        ch.trig_value = v as u8;
    }
    if let Some(v) = o.get("mapUnit") {
        ch.map_unit = Some(v.as_str().unwrap_or("").to_owned());
    }
    if let Some(v) = num("mapMin") {
        ch.map_min = v;
    }
    if let Some(v) = num("mapMax") {
        ch.map_max = v;
    }
}

/// `MainWindow::load_config_from_json`; true when it loaded decoders.
pub fn load_config_from_json(s: &Session, doc: &Value) -> bool {
    load_config(s, doc, false)
}

/// [`load_config_from_json`]; `from_user_file` holds the guarded decoder
/// options ([`load_file_decoders`]).
pub fn load_config(s: &Session, doc: &Value, from_user_file: bool) -> bool {
    let Some(obj) = doc.as_object() else {
        return false;
    };
    if !obj.contains_key("Version") {
        return false;
    }
    let format_ver = to_int(obj.get("Version"));
    if format_ver < 2 {
        sr_err!("Profile version is error!");
        return false;
    }
    let mode = s.mode();
    let hardware = s.state().device.as_ref().is_some_and(|d| d.is_hardware());
    if hardware {
        let driver = s.state().device.as_ref().map_or("", |d| d.driver);
        if obj.get("Device").and_then(Value::as_str) != Some(driver)
            || to_int(obj.get("DeviceMode")) != mode
        {
            sr_err!("Profile is not compatible with current device or mode!");
            return false;
        }
    }

    // device settings
    if let Some(ConfValue::I32s(keys)) = s.config_list(key::DEVICE_SESSIONS) {
        for k in keys {
            let Some((ty, name)) = config_info(k) else {
                continue;
            };
            let Some(v) = obj.get(name) else { continue };
            let as_str = v.as_str().unwrap_or("");
            let value = match ty {
                ConfType::Bool => Some(ConfValue::Bool(to_int(Some(v)) != 0)),
                ConfType::U64 => Some(ConfValue::U64(as_str.trim().parse().unwrap_or(0))),
                ConfType::U8 => Some(ConfValue::Byte(if as_str.is_empty() {
                    to_int(Some(v)) as u8
                } else {
                    as_str.trim().parse::<u32>().unwrap_or(0) as u8
                })),
                ConfType::Float => Some(ConfValue::F64(if as_str.is_empty() {
                    v.as_f64().unwrap_or(0.0)
                } else {
                    as_str.trim().parse().unwrap_or(0.0)
                })),
                ConfType::Char => Some(ConfValue::Str(as_str.to_owned())),
                // format 2 stored option names; their codes are not mapped here
                ConfType::List => Some(ConfValue::I16(if format_ver > 2 {
                    to_int(Some(v)) as i16
                } else {
                    0
                })),
                ConfType::RationalVolt => None,
            };
            if let Some(value) = value
                && !s.set_config(k, value, None)
            {
                sr_err!("Set device config option failed, id:{k}");
            }
        }
    }

    // channels
    let saved: Vec<&Map<String, Value>> = obj
        .get("channel")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .collect();
    let channels = s.channels();
    for c in &channels {
        let found = if mode == Mode::Dso as i32 {
            saved.iter().find(|o| {
                o.get("name").and_then(Value::as_str) == Some(&c.name)
                    && o.get("type").and_then(Value::as_f64) == Some(c.ty as f64)
            })
        } else {
            saved
                .iter()
                .find(|o| to_int(o.get("index")) == c.index as i32 && to_int(o.get("type")) == c.ty)
        };
        match found {
            Some(o) => {
                let en = o.get("enabled").and_then(Value::as_bool).unwrap_or(false);
                s.update_channel(c.index, c.ty, |ch| load_probe_analog_settings(ch, o));
                s.enable_channel(c.index, en);
                if mode != Mode::Dso as i32 {
                    let mut name = o
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_owned();
                    if name.is_empty() {
                        name = c.index.to_string();
                    }
                    s.set_channel_name(c.index, &name);
                }
            }
            None if mode != Mode::Dso as i32 => {
                s.enable_channel(c.index, false);
            }
            None => {}
        }
    }
    s.reload_signals();

    // signals: names and simple triggers
    if mode != Mode::Dso as i32 {
        let mut st = s.state();
        for sig in st.signals.iter_mut() {
            if let Some(o) = saved.iter().find(|o| {
                to_int(o.get("index")) == sig.index as i32 && to_int(o.get("type")) == sig.ty
            }) {
                let mut name = o
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_owned();
                if name.is_empty() {
                    name = sig.index.to_string();
                }
                sig.name = name;
                sig.colour = o
                    .get("colour")
                    .and_then(Value::as_str)
                    .and_then(parse_colour);
                if sig.ty == channel_type::LOGIC {
                    let t = o.get("strigger").and_then(Value::as_f64).unwrap_or(0.0) as i32;
                    sig.trig = if (1..=5).contains(&t) { t } else { 0 };
                }
            }
        }
    }

    s.update_sample_rate_list();
    s.device_updated();
    if let Some(t) = obj.get("trigger").and_then(Value::as_object)
        && t.contains_key("triggerPos")
    {
        s.set_trigger_position(t["triggerPos"].as_f64().unwrap_or(0.0) as i32);
        let ch_num = s
            .config(key::TOTAL_CH_NUM, None)
            .map_or(16, |v| v.get_i16() as usize);
        s.state().adv_trigger.load_json(t, ch_num);
    }

    let mut have_decoders = false;
    if let Some(Value::Array(a)) = obj.get("decoder")
        && !a.is_empty()
    {
        have_decoders = true;
        if from_user_file {
            load_file_decoders(s, a);
        } else {
            load_decoders(s, a);
        }
    }
    load_channel_view_indexes(s, doc);
    have_decoders
}

/// `QColor(string)` for "#rrggbb"; none for "default" or anything else.
fn parse_colour(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// `MainWindow::gen_config_json`
pub fn session_json(s: &Session, version: &str) -> Value {
    let mode = s.mode();
    let mut o = Map::new();
    let (driver, hardware) = s
        .state()
        .device
        .as_ref()
        .map_or(("", false), |d| (d.driver, d.is_hardware()));
    o.insert("Version".into(), SESSION_FORMAT_VERSION.into());
    o.insert("Device".into(), driver.into());
    o.insert("DeviceMode".into(), mode.into());
    o.insert("Language".into(), LANGUAGE.into());
    o.insert("Title".into(), format!("DSView v{version}").into());
    if hardware && mode == Mode::Logic as i32 {
        // COLLECT_SINGLE
        o.insert("CollectMode".into(), 0.into());
    }
    if let Some(ConfValue::I32s(keys)) = s.config_list(key::DEVICE_SESSIONS) {
        for k in keys {
            let Some((ty, name)) = config_info(k) else {
                continue;
            };
            let Some(v) = s.config(k, None) else { continue };
            let j: Value = match ty {
                ConfType::Bool => v.get_bool().into(),
                ConfType::U64 => v.get_u64().to_string().into(),
                ConfType::U8 => v.get_byte().into(),
                ConfType::Float => dsview_formats::cfmt::fmt_g(v.get_f64(), 6).into(),
                ConfType::Char => v.get_str().unwrap_or("").into(),
                ConfType::List => v.get_i16().into(),
                ConfType::RationalVolt => continue,
            };
            o.insert(name.into(), j);
        }
    }
    let channels = s.channels();
    let sigs = s.state().signals.clone();
    let mut chans = Vec::new();
    for sig in &sigs {
        let enabled = channels
            .iter()
            .any(|c| c.index == sig.index && c.ty == sig.ty && c.enabled);
        let mut c = Map::new();
        c.insert("index".into(), sig.index.into());
        c.insert("view_index".into(), sig.view_index.into());
        c.insert("type".into(), sig.ty.into());
        c.insert("enabled".into(), enabled.into());
        c.insert("name".into(), sig.name.clone().into());
        c.insert(
            "colour".into(),
            sig.colour
                .map_or("default".to_owned(), |[r, g, b]| {
                    format!("#{r:02x}{g:02x}{b:02x}")
                })
                .into(),
        );
        if sig.ty == channel_type::LOGIC {
            c.insert("strigger".into(), sig.trig.into());
        }
        chans.push(Value::Object(c));
    }
    o.insert("channel".into(), chans.into());
    if mode == Mode::Logic as i32 {
        let ch_num = s
            .config(key::TOTAL_CH_NUM, None)
            .map_or(16, |v| v.get_i16() as usize);
        let pos = s.trigger_position();
        let t = s.state().adv_trigger.session_json(pos, ch_num);
        o.insert("trigger".into(), t);
    }
    o.insert("decoder".into(), decoders_json(s));
    Value::Object(o)
}

// ------------------------------------------------------------ saving

/// A save's or export's progress (`StoreSession::get_progress`) and its
/// Cancel, shared with the progress window.
#[derive(Debug, Default)]
pub struct Progress {
    pub done: std::sync::atomic::AtomicU64,
    pub total: std::sync::atomic::AtomicU64,
    pub cancel: std::sync::atomic::AtomicBool,
}

impl Progress {
    /// `on_progress_updated`: the percent written, 100 once done.
    pub fn percent(&self) -> i32 {
        use std::sync::atomic::Ordering::Relaxed;
        let (d, t) = (self.done.load(Relaxed), self.total.load(Relaxed));
        if d < t {
            (d as f64 / t as f64 * 100.0) as i32
        } else {
            100
        }
    }

    fn set_total(&self, n: u64) {
        self.total.store(n, std::sync::atomic::Ordering::Relaxed);
    }

    fn add(&self, n: u64) {
        self.done.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn canceled(&self) -> bool {
        self.cancel.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// `StoreSession::save_start` + `save_logic`/`save_dso` for the whole
/// capture. Errors carry DSView's messages.
pub fn save_dsl(s: &Session, path: &Path, version: &str) -> Result<(), String> {
    save_dsl_range(s, path, version, None)
}

/// `StoreSession::SetDataRange`: the samples between two cursors, the start
/// rounded down and the end up to whole 64-sample units (0 for either end:
/// the capture's); an end past the data means the capture's end.
fn logic_range(
    store: &dsview_store::logic::LogicStore,
    range: Option<(u64, u64)>,
) -> Result<(u64, u64), String> {
    let ring = store.ring_sample_count();
    let (mut start, mut end) = range.unwrap_or((0, 0));
    if start > ring {
        return Err("ERROR:the start curosr is invalid!".into());
    }
    if end > ring {
        end = 0;
    }
    start -= start % 64;
    if end > 0 && !end.is_multiple_of(64) {
        end += 64 - end % 64;
        if end > ring {
            end = 0;
        }
    }
    Ok((start, if end == 0 { ring } else { end }))
}

/// [`save_dsl`] of the samples `range` covers (logic captures; other modes
/// save everything, as C does).
pub fn save_dsl_range(
    s: &Session,
    path: &Path,
    version: &str,
    range: Option<(u64, u64)>,
) -> Result<(), String> {
    save_dsl_range_with(s, path, version, range, &Progress::default())
}

/// [`save_dsl_range`] counting the bytes written into `prog`; a Cancel
/// discards the file (`m_zipDoc.Discard`).
pub fn save_dsl_range_with(
    s: &Session,
    path: &Path,
    version: &str,
    range: Option<(u64, u64)>,
    prog: &Progress,
) -> Result<(), String> {
    let r = save_dsl_inner(s, path, version, range, prog);
    if prog.canceled() {
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    r
}

fn save_dsl_inner(
    s: &Session,
    path: &Path,
    version: &str,
    range: Option<(u64, u64)>,
    prog: &Progress,
) -> Result<(), String> {
    let mode = s.mode();
    if !s.have_view_data() {
        return Err("No data to save.".into());
    }
    let session = qt_json(&session_json(s, version));
    let decoders = qt_json(&decoders_json(s));
    if mode != Mode::Logic as i32 {
        return save_dso(s, path, &session, &decoders);
    }
    let st = s.state();
    let (rate, trig_pos, time_ms) = (st.cur_snap_samplerate, st.trigger_pos, st.session_time_ms);
    let sigs: Vec<u16> = st
        .signals
        .iter()
        .filter(|x| x.ty == channel_type::LOGIC)
        .map(|x| x.index)
        .collect();
    let driver = st.device.as_ref().map_or("", |d| d.driver).to_owned();
    drop(st);
    let channels = s.channels();
    let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
    let saved: Vec<&dsview_core::Channel> = channels
        .iter()
        .filter(|c| c.enabled && sigs.contains(&c.index) && store.has_data(c.index as i32))
        .collect();
    let ranged = range.is_some_and(|(a, b)| a > 0 || b > 0);
    let (start, end) = logic_range(&store, range)?;
    let writer = LogicWriter {
        driver,
        samplerate: rate,
        // a range is saved as a capture of its own (DEVIATIONS.md)
        total_samples: if ranged {
            end - start
        } else {
            store.sample_count()
        },
        trigger_time_ms: time_ms,
        trigger_pos: if ranged {
            trig_pos.saturating_sub(start)
        } else {
            trig_pos
        },
        channels: saved
            .iter()
            .map(|c| WriteChannel {
                index: c.index,
                name: c.name.clone(),
            })
            .collect(),
        decoders_json: decoders.clone(),
        session_json: session.clone(),
    };
    let mut z = DslZipWriter::create(path).map_err(|e| e.to_string())?;
    let map = |e: dsview_formats::Error| e.to_string();
    z.add("header", writer.header_text().as_bytes())
        .map_err(map)?;
    z.add("decoders", decoders.as_bytes()).map_err(map)?;
    z.add("session", session.as_bytes()).map_err(map)?;
    if ranged {
        // the range's bytes per channel, cut into whole blocks again
        const BLOCK_BYTES: usize = (dsview_store::logic::LEAF_BLOCK_SAMPLES / 8) as usize;
        prog.set_total((end - start) / 8 * saved.len() as u64);
        for c in &saved {
            if prog.canceled() {
                break;
            }
            let mut pending: Vec<u8> = Vec::with_capacity(BLOCK_BYTES);
            let mut n = 0;
            let mut cursor = start;
            while cursor < end && !prog.canceled() {
                let (b, off) = store.get_block_index_with_sample(cursor);
                let size = store.get_block_size(b) * 8;
                if size <= off {
                    break;
                }
                let take = (size - off).min(end - cursor);
                let (buf, level) = store.get_block_buf(b, c.index as i32);
                // whole 64-sample words: the capture's end may cut one (F11)
                let (from, to) = ((off / 8) as usize, ((off + take).div_ceil(64) * 8) as usize);
                prog.add((to - from) as u64);
                if buf.is_null() {
                    pending.extend(std::iter::repeat_n(if level { 0xff } else { 0 }, to - from));
                } else {
                    // SAFETY: inside the block's bytes; the store is locked
                    pending.extend_from_slice(unsafe {
                        std::slice::from_raw_parts(buf.add(from), to - from)
                    });
                }
                while pending.len() >= BLOCK_BYTES {
                    let rest = pending.split_off(BLOCK_BYTES);
                    z.add(&format!("L-{}/{n}", c.index), &pending)
                        .map_err(map)?;
                    pending = rest;
                    n += 1;
                }
                cursor += take;
            }
            if !pending.is_empty() {
                z.add(&format!("L-{}/{n}", c.index), &pending)
                    .map_err(map)?;
            }
        }
        drop(store);
        return z.finish().map_err(map);
    }
    let blocks = store.get_block_num();
    let mut fill = Vec::new();
    prog.set_total(store.ring_sample_count() / 8 * saved.len() as u64);
    for c in &saved {
        for b in 0..blocks {
            if prog.canceled() {
                return Ok(());
            }
            let (buf, level) = store.get_block_buf(b, c.index as i32);
            let size = store.get_block_size(b) as usize;
            let data: &[u8] = if buf.is_null() {
                fill.clear();
                fill.resize(size, if level { 0xff } else { 0 });
                &fill
            } else {
                // SAFETY: a stored block holds `size` bytes; the store is locked
                unsafe { std::slice::from_raw_parts(buf, size) }
            };
            z.add(&format!("L-{}/{b}", c.index), data).map_err(map)?;
            prog.add(size as u64);
        }
    }
    drop(store);
    z.finish().map_err(map)
}

fn save_dso(s: &Session, path: &Path, session: &str, decoders: &str) -> Result<(), String> {
    let st = s.state();
    let (rate, trig_pos, time_ms) = (st.cur_snap_samplerate, st.trigger_pos, st.session_time_ms);
    let driver = st.device.as_ref().map_or("", |d| d.driver).to_owned();
    drop(st);
    let channels = s.channels();
    let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
    let n = store.sample_count();
    let with_data: Vec<&dsview_core::Channel> = channels
        .iter()
        .filter(|c| c.ty == channel_type::DSO && store.has_data(c.index as i32))
        .collect();
    let mut h = String::from("[version]\nversion = 3\n[header]\n");
    h += &format!("driver = {driver}\ndevice mode = {}\n", Mode::Dso as i32);
    h += "capturefile = data\n";
    h += &format!("total samples = {n}\n");
    h += &format!("total probes = {}\ntotal blocks = 1\n", store.channel_num());
    h += &format!(
        "samplerate = {}\n",
        dsview_formats::samplerate::format(rate)
    );
    h += &format!("trigger time = {time_ms}\ntrigger pos = {trig_pos}\n");
    for (i, c) in with_data.iter().enumerate() {
        h += &format!("probe{i} = {}\n", c.name);
        h += &format!(" enable{i} = {}\n", i32::from(c.enabled));
        h += &format!(" coupling{i} = {}\n", c.coupling);
        h += &format!(" vDiv{i} = {}\n", c.vdiv);
        h += &format!(" vFactor{i} = {}\n", c.vfactor);
        h += &format!(" vOffset{i} = {}\n", c.hw_offset);
        h += &format!(" vTrig{i} = {}\n", c.trig_value);
    }
    let map = |e: dsview_formats::Error| e.to_string();
    let mut z = DslZipWriter::create(path).map_err(|e| e.to_string())?;
    z.add("header", h.as_bytes()).map_err(map)?;
    z.add("decoders", decoders.as_bytes()).map_err(map)?;
    z.add("session", session.as_bytes()).map_err(map)?;
    for c in &with_data {
        let data = store.get_samples(0, c.index as i32).unwrap_or(&[]);
        let len = (n as usize).min(data.len());
        z.add(&format!("O-{}/0", c.index), &data[..len])
            .map_err(map)?;
    }
    drop(store);
    z.finish().map_err(map)
}

/// Where exported packets go: a text output module's file, or srzip's
/// archive.
// one per export: its size does not matter
#[allow(clippy::large_enum_variant)]
enum Sink {
    Text(Exporter, std::io::BufWriter<std::fs::File>),
    Srzip(SrzipWriter),
}

impl Sink {
    fn send(&mut self, p: &ExportPacket<'_>) -> Result<(), String> {
        match self {
            Sink::Text(ex, out) => {
                if let Some(t) = ex.receive(p).map_err(|e| e.to_string())? {
                    out.write_all(t.as_bytes()).map_err(|e| e.to_string())?;
                }
                Ok(())
            }
            Sink::Srzip(w) => w.receive(p).map_err(|e| e.to_string()),
        }
    }

    fn finish(self) -> Result<(), String> {
        match self {
            Sink::Text(_, mut out) => out.flush().map_err(|e| e.to_string()),
            Sink::Srzip(w) => w.finish().map(|_| ()).map_err(|e| e.to_string()),
        }
    }
}

/// The local time now, for zip entries: year, month, day, hour, minute,
/// second.
fn local_now() -> [u16; 6] {
    let tm = local_tm(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64),
    );
    [
        (tm.tm_year + 1900) as u16,
        (tm.tm_mon + 1) as u16,
        tm.tm_mday as u16,
        tm.tm_hour as u16,
        tm.tm_min as u16,
        tm.tm_sec as u16,
    ]
}

fn local_tm(secs: i64) -> libc::tm {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r writes the struct it is given
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

/// `Formatting::DateTimeToString(TIME_STR_FORMAT_ALL)` in local time.
fn date_time_string(ms: i64) -> String {
    let tm = local_tm(ms / 1000);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

fn export_kind(ty: i32) -> ChannelKind {
    match ty {
        channel_type::LOGIC => ChannelKind::Logic,
        channel_type::DSO => ChannelKind::Dso,
        channel_type::ANALOG => ChannelKind::Analog,
        _ => ChannelKind::Other,
    }
}

/// `StoreSession::export_exec` for logic, DSO and analog data (whole
/// capture; logic rows only where a value changes, DSView's default).
pub fn export(s: &Session, path: &Path, format: Format) -> Result<(), String> {
    export_range(s, path, format, None, false)
}

/// [`export`] of the logic samples between two cursors (`SetDataRange`), as
/// original data (every sample) or compressed (CSV: changes only).
pub fn export_range(
    s: &Session,
    path: &Path,
    format: Format,
    range: Option<(u64, u64)>,
    original: bool,
) -> Result<(), String> {
    export_range_with(s, path, format, range, original, &Progress::default())
}

/// [`export_range`] counting the samples written into `prog`; a Cancel
/// stops it and keeps what was written, as Qt does.
pub fn export_range_with(
    s: &Session,
    path: &Path,
    format: Format,
    range: Option<(u64, u64)>,
    original: bool,
    prog: &Progress,
) -> Result<(), String> {
    let mode = s.mode();
    let kind = match mode {
        1 => ChannelKind::Dso,
        2 => ChannelKind::Analog,
        _ => ChannelKind::Logic,
    };
    if !s.have_view_data() {
        return Err("No data to export.".into());
    }
    let channels: Vec<ExportChannel> = s
        .channels()
        .into_iter()
        .map(|c| ExportChannel {
            index: c.index,
            kind: export_kind(c.ty),
            enabled: c.enabled,
            name: c.name,
            vdiv: c.vdiv,
            vfactor: c.vfactor,
            hw_offset: c.hw_offset,
            map_max: c.map_max,
            map_min: c.map_min,
            map_unit: c.map_unit.unwrap_or_default(),
        })
        .collect();
    // logic: the range's raw start is the output's first sample index
    let (start_index, end_index) = if kind == ChannelKind::Logic {
        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
        let ring = store.ring_sample_count();
        let (a, mut b) = range.unwrap_or((0, 0));
        if a > ring {
            return Err("ERROR:the start curosr is invalid!".into());
        }
        if b > ring {
            b = 0;
        }
        (a, b)
    } else {
        (0, 0)
    };
    let (rate, time_ms, sigs) = {
        let st = s.state();
        (
            st.cur_snap_samplerate,
            st.session_time_ms,
            st.signals.clone(),
        )
    };
    let mut sink = if format == Format::Srzip {
        if kind != ChannelKind::Logic {
            return Err("srzip exports logic data only.".into());
        }
        Sink::Srzip(SrzipWriter::new(path, channels, local_now()))
    } else {
        let mut ex =
            Exporter::new(format, kind, channels, start_index).map_err(|e| e.to_string())?;
        ex.time_string = date_time_string(time_ms);
        ex.date_string = ex.time_string.clone();
        let file = std::fs::File::create(path)
            .map_err(|_| format!("Failed to open file for writing: {}", path.display()))?;
        Sink::Text(ex, std::io::BufWriter::new(file))
    };
    let ref_min = s.config(key::REF_MIN, None).map(|v| v.get_u32());
    let ref_max = s.config(key::REF_MAX, None).map(|v| v.get_u32());
    if kind == ChannelKind::Logic {
        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
        sink.send(&ExportPacket::Meta {
            samplerate: Some(rate),
            limit_samples: Some(store.sample_count()),
            ref_min,
            ref_max,
        })?;
        let sig_idx: Vec<i32> = sigs
            .iter()
            .filter(|x| x.ty == channel_type::LOGIC && store.has_data(x.index as i32))
            .map(|x| x.index as i32)
            .collect();
        let unitsize = sig_idx.len().div_ceil(8);
        const USIZE: u64 = 8192;
        let (start_block, start_offset) = if start_index > 0 {
            store.get_block_index_with_sample(start_index)
        } else {
            (0, 0)
        };
        let (end_block, end_offset) = if end_index > 0 {
            store.get_block_index_with_sample(end_index)
        } else {
            (0, 0)
        };
        let ring = store.ring_sample_count();
        prog.set_total(match (start_index, end_index) {
            (a, b) if b > 0 => b.saturating_sub(a),
            (a, _) => ring.saturating_sub(a),
        });
        for blk in 0..store.get_block_num() {
            if prog.canceled() {
                break;
            }
            if blk < start_block {
                continue;
            }
            if blk > end_block && end_block > 0 {
                break;
            }
            let mut block_size = store.get_block_size(blk);
            if blk == end_block && end_offset / 8 < block_size && end_offset > 0 {
                block_size = end_offset / 8;
            }
            let skip = if blk == start_block && start_offset > 0 {
                block_size -= start_offset / 8;
                (start_offset / 8) as usize
            } else {
                0
            };
            let bufs: Vec<(*const u8, bool)> = sig_idx
                .iter()
                .map(|&i| {
                    let (p, level) = store.get_block_buf(blk, i);
                    // SAFETY: the block holds at least `skip` bytes
                    (
                        if p.is_null() {
                            p
                        } else {
                            unsafe { p.add(skip) }
                        },
                        level,
                    )
                })
                .collect();
            let samples = block_size * 8;
            let mut xbuf = vec![0u8; USIZE as usize * unitsize];
            let mut i = 0;
            while i < samples && !prog.canceled() {
                let size = USIZE.min(samples - i) as usize;
                xbuf[..size * unitsize].fill(0);
                for j in 0..size {
                    for (k, &(p, level)) in bufs.iter().enumerate() {
                        let bit = if p.is_null() {
                            level
                        } else {
                            let pos = i as usize + j;
                            // SAFETY: inside the block's `samples / 8` bytes
                            unsafe { *p.add(pos / 8) & (1 << (j % 8)) != 0 }
                        };
                        if bit {
                            xbuf[j * unitsize + k / 8] |= 1 << (k % 8);
                        }
                    }
                }
                sink.send(&ExportPacket::Logic {
                    data: &xbuf[..size * unitsize],
                    unitsize,
                    export_original: original,
                })?;
                prog.add(size as u64);
                i += USIZE;
            }
        }
    } else if kind == ChannelKind::Analog {
        let store = s.data().analog.lock().unwrap_or_else(|e| e.into_inner());
        let n = store.sample_count();
        sink.send(&ExportPacket::Meta {
            samplerate: Some(rate),
            limit_samples: Some(n),
            ref_min,
            ref_max,
        })?;
        // the ring from its oldest sample: [ring_start..n), then [0..ring_start)
        let ring_start = if n < store.total_sample_count() {
            0
        } else {
            store.ring_sample_count()
        };
        // C steps by channel count only: one byte per sample and channel
        let ch = store.channel_num() as usize;
        let data = store.data();
        const USIZE: u64 = 8192;
        prog.set_total(n);
        for (from, count) in [(ring_start, n - ring_start), (0, ring_start)] {
            if count == 0 {
                break;
            }
            let mut i = 0;
            while i < count && !prog.canceled() {
                let size = USIZE.min(count - i) as usize;
                let at = ((from + i) as usize).saturating_mul(ch);
                let chunk = data.get(at..).unwrap_or(&[]);
                let chunk = &chunk[..(size * ch).min(chunk.len())];
                sink.send(&ExportPacket::Analog {
                    data: chunk,
                    num_samples: size,
                })?;
                prog.add(size as u64);
                i += USIZE;
            }
        }
    } else {
        let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        let n = store.sample_count();
        sink.send(&ExportPacket::Meta {
            samplerate: Some(rate),
            limit_samples: Some(n),
            ref_min,
            ref_max,
        })?;
        let chans: Vec<&[u8]> = sigs
            .iter()
            .filter(|x| x.ty == channel_type::DSO && store.has_data(x.index as i32))
            .filter_map(|x| store.get_samples(0, x.index as i32))
            .collect();
        let ch_num = chans.len();
        const USIZE: u64 = 8192;
        let mut i = 0;
        let mut buf = vec![0u8; USIZE as usize * ch_num.max(1)];
        prog.set_total(n);
        while i < n && !prog.canceled() {
            let size = USIZE.min(n - i) as usize;
            for (c, data) in chans.iter().enumerate() {
                for j in 0..size {
                    buf[j * ch_num + c] = data.get(i as usize + j).copied().unwrap_or(0);
                }
            }
            sink.send(&ExportPacket::Dso {
                data: &buf[..size * ch_num],
                num_samples: size,
            })?;
            prog.add(size as u64);
            i += USIZE;
        }
    }
    sink.send(&ExportPacket::End)?;
    sink.finish()
}

/// `ProtocolExp::save_proc`: rows `rows` (indexes into the stack's rows) of
/// decoder trace `t`. Each line holds the annotations starting at the
/// smallest start sample left in any column: `Id, Time[ns], <row titles>`.
/// A `.txt` path gets a table whose columns line up; any other path CSV
/// (cells quoted where they need it). Qt's TXT was the CSV text (F27).
pub fn export_protocol(
    t: &crate::session::DecodeTrace,
    rows: &[usize],
    sample_count: u64,
    path: &Path,
) -> Result<(), String> {
    export_protocol_with(t, rows, sample_count, path, &mut |_| true)
}

/// `export_protocol`, reporting the percent of annotations written after
/// each line to `progress`; it stops (keeping what was written, as Qt's
/// Cancel does) when `progress` returns false.
pub fn export_protocol_with(
    t: &crate::session::DecodeTrace,
    rows: &[usize],
    sample_count: u64,
    path: &Path,
    progress: &mut dyn FnMut(u32) -> bool,
) -> Result<(), String> {
    // EXPORT_DEC_ROW_COUNT_MAX
    let rows: Vec<usize> = rows.iter().copied().take(20).collect();
    if rows.is_empty() {
        return Err("There is no decoded row to export.".into());
    }
    let txt = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("txt"));
    let st = &t.stack;
    let titles: Vec<String> = rows.iter().map(|&r| st.rows()[r].title()).collect();
    let anns: Vec<Vec<crate::decode::Ann>> = rows
        .iter()
        .map(|&r| st.row_sorted(r, sample_count))
        .collect();
    let total: usize = anns.iter().map(Vec::len).sum();
    let ns_per_sample = 1e9 / (st.sample_rate() as f64).max(1.0);
    let file = std::fs::File::create(path)
        .map_err(|_| format!("Failed to open file for writing: {}", path.display()))?;
    let mut out = std::io::BufWriter::new(file);
    let w = |out: &mut std::io::BufWriter<std::fs::File>, s: String| {
        out.write_all(s.as_bytes()).map_err(|e| e.to_string())
    };
    let mut header = vec!["Id".to_owned(), "Time[ns]".to_owned()];
    header.extend(titles);
    // a TXT table needs every line for its column widths
    let mut table: Vec<Vec<String>> = Vec::new();
    if txt {
        table.push(header);
    } else {
        w(&mut out, csv_line(&header))?;
    }
    let mut written = 0usize;
    for (n, line) in crate::decode::align_rows(&anns).iter().enumerate() {
        let sample = line
            .iter()
            .zip(&anns)
            .find_map(|(i, a)| i.map(|i| a[i].start))
            .unwrap_or(0);
        let mut cells = Vec::with_capacity(rows.len() + 2);
        cells.push((n + 1).to_string());
        cells.push(dsview_formats::cfmt::fmt_f(
            sample as f64 * ns_per_sample,
            2,
        ));
        for (i, a) in line.iter().zip(&anns) {
            cells.push(i.map_or(String::new(), |i| a[i].text_in(st.format())));
        }
        written += line.iter().flatten().count();
        if txt {
            table.push(cells);
        } else {
            w(&mut out, csv_line(&cells))?;
        }
        if !progress((written * 100 / total.max(1)) as u32) {
            break;
        }
    }
    if txt {
        let n = table[0].len();
        let widths: Vec<usize> = (0..n)
            .map(|c| {
                table
                    .iter()
                    .map(|r| r[c].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for r in &table {
            let mut l = String::new();
            for (c, cell) in r.iter().enumerate() {
                if c > 0 {
                    l.push_str("  ");
                }
                l.push_str(cell);
                l.extend(std::iter::repeat_n(' ', widths[c] - cell.chars().count()));
            }
            let l = l.trim_end();
            w(&mut out, format!("{l}\n"))?;
        }
    }
    out.flush().map_err(|e| e.to_string())
}

/// One CSV line; a cell with a comma, a quote or a line break is quoted.
fn csv_line(cells: &[String]) -> String {
    let mut l = String::new();
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            l.push(',');
        }
        if c.contains([',', '"', '\n', '\r']) {
            l.push('"');
            l.push_str(&c.replace('"', "\"\""));
            l.push('"');
        } else {
            l.push_str(c);
        }
    }
    l.push('\n');
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_with_separators_are_quoted() {
        let cells = ["1", "a,b", "say \"hi\"", "two\nlines", "plain"].map(String::from);
        assert_eq!(
            csv_line(&cells),
            "1,\"a,b\",\"say \"\"hi\"\"\",\"two\nlines\",plain\n"
        );
    }
}
