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

/// `StoreSession::gen_decoders_json`
pub fn decoders_json(s: &Session) -> Value {
    let traces = s.state().decoders.clone();
    let mut out = Vec::new();
    for t in traces {
        let mut obj = Map::new();
        let mut stacked = Vec::new();
        let mut show = Map::new();
        for d in t.stack.decoders() {
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
            show.insert(d.info.id.clone(), d.shown.into());
        }
        obj.insert("version".into(), DECODER_CONFIG_VERSION.into());
        obj.insert("label".into(), t.name().into());
        obj.insert("stacked decoders".into(), stacked.into());
        obj.insert("view_index".into(), (*t.view_index.lock().unwrap()).into());
        for r in t.stack.rows() {
            show.insert(r.title_id(), r.shown.into());
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
        have_decoders = load_config_from_json(s, &doc);
    }
    if !have_decoders
        && s.mode() == Mode::Logic as i32
        && let Some(Value::Array(a)) = read_entry(path, "decoders")
    {
        load_decoders(s, &a);
    }
    // MainWindow replays the file 100 ms later
    s.start_capture(true);
}

/// `MainWindow::load_config_from_json`; true when it loaded decoders.
pub fn load_config_from_json(s: &Session, doc: &Value) -> bool {
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
    }

    let mut have_decoders = false;
    if let Some(Value::Array(a)) = obj.get("decoder")
        && !a.is_empty()
    {
        have_decoders = true;
        load_decoders(s, a);
    }
    load_channel_view_indexes(s, doc);
    have_decoders
}

/// `TriggerDock::get_session()` for the simple trigger.
fn trigger_session(pos: i32) -> Value {
    let xs = vec!["X"; 16].join(" ");
    let mut t = Map::new();
    t.insert("advTriggerMode".into(), false.into());
    t.insert("triggerPos".into(), pos.into());
    t.insert("triggerStages".into(), 0.into());
    t.insert("triggerTab".into(), 0.into());
    for i in 0..16 {
        t.insert(format!("stageTriggerValue0{i}"), xs.clone().into());
        t.insert(format!("stageTriggerValue1{i}"), xs.clone().into());
        t.insert(format!("stageTriggerInv0{i}"), 0.into());
        t.insert(format!("stageTriggerInv1{i}"), 0.into());
        t.insert(format!("stageTriggerLogic{i}"), 1.into());
        t.insert(format!("stageTriggerCount{i}"), 1.into());
        t.insert(format!("stageTriggerContiguous{i}"), false.into());
    }
    for k in ["Start", "Stop", "Clock", "Data"] {
        t.insert(format!("serialTrigger{k}"), xs.clone().into());
    }
    t.insert("serialTriggerChannel".into(), 0.into());
    t.insert("serialTriggerBits".into(), 0.into());
    Value::Object(t)
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
        c.insert("colour".into(), "default".into());
        if sig.ty == channel_type::LOGIC {
            c.insert("strigger".into(), sig.trig.into());
        }
        chans.push(Value::Object(c));
    }
    o.insert("channel".into(), chans.into());
    if mode == Mode::Logic as i32 {
        o.insert("trigger".into(), trigger_session(s.trigger_position()));
    }
    o.insert("decoder".into(), decoders_json(s));
    Value::Object(o)
}

// ------------------------------------------------------------ saving

/// `StoreSession::save_start` + `save_logic`/`save_dso` for the whole
/// capture. Errors carry DSView's messages.
pub fn save_dsl(s: &Session, path: &Path, version: &str) -> Result<(), String> {
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
    let writer = LogicWriter {
        driver,
        samplerate: rate,
        total_samples: store.sample_count(),
        trigger_time_ms: time_ms,
        trigger_pos: trig_pos,
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
    let blocks = store.get_block_num();
    let mut fill = Vec::new();
    for c in &saved {
        for b in 0..blocks {
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

/// `Formatting::DateTimeToString(TIME_STR_FORMAT_ALL)` in local time.
fn date_time_string(ms: i64) -> String {
    let t = (ms / 1000) as libc::time_t;
    // SAFETY: localtime_r writes the struct it is given
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
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

/// `StoreSession::export_exec` for logic and DSO data (whole capture,
/// changes only, like DSView's default).
pub fn export(s: &Session, path: &Path, format: Format) -> Result<(), String> {
    let mode = s.mode();
    let kind = match mode {
        1 => ChannelKind::Dso,
        2 => ChannelKind::Analog,
        _ => ChannelKind::Logic,
    };
    if kind == ChannelKind::Analog {
        return Err("analog export is not supported yet".into());
    }
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
    let mut ex = Exporter::new(format, kind, channels, 0).map_err(|e| e.to_string())?;
    let (rate, time_ms, sigs) = {
        let st = s.state();
        (
            st.cur_snap_samplerate,
            st.session_time_ms,
            st.signals.clone(),
        )
    };
    ex.time_string = date_time_string(time_ms);
    ex.date_string = ex.time_string.clone();
    let file = std::fs::File::create(path)
        .map_err(|_| format!("Failed to open file for writing: {}", path.display()))?;
    let mut out = std::io::BufWriter::new(file);
    let mut emit = |t: Option<String>| -> Result<(), String> {
        if let Some(t) = t {
            out.write_all(t.as_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let ref_min = s.config(key::REF_MIN, None).map(|v| v.get_u32());
    let ref_max = s.config(key::REF_MAX, None).map(|v| v.get_u32());
    if kind == ChannelKind::Logic {
        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
        emit(
            ex.receive(&ExportPacket::Meta {
                samplerate: Some(rate),
                limit_samples: Some(store.sample_count()),
                ref_min,
                ref_max,
            })
            .map_err(|e| e.to_string())?,
        )?;
        let sig_idx: Vec<i32> = sigs
            .iter()
            .filter(|x| x.ty == channel_type::LOGIC && store.has_data(x.index as i32))
            .map(|x| x.index as i32)
            .collect();
        let unitsize = sig_idx.len().div_ceil(8);
        const USIZE: u64 = 8192;
        for blk in 0..store.get_block_num() {
            let bufs: Vec<(*const u8, bool)> = sig_idx
                .iter()
                .map(|&i| store.get_block_buf(blk, i))
                .collect();
            let samples = store.get_block_size(blk) * 8;
            let mut xbuf = vec![0u8; USIZE as usize * unitsize];
            let mut i = 0;
            while i < samples {
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
                emit(
                    ex.receive(&ExportPacket::Logic {
                        data: &xbuf[..size * unitsize],
                        unitsize,
                        export_original: false,
                    })
                    .map_err(|e| e.to_string())?,
                )?;
                i += USIZE;
            }
        }
    } else {
        let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        let n = store.sample_count();
        emit(
            ex.receive(&ExportPacket::Meta {
                samplerate: Some(rate),
                limit_samples: Some(n),
                ref_min,
                ref_max,
            })
            .map_err(|e| e.to_string())?,
        )?;
        let chans: Vec<&[u8]> = sigs
            .iter()
            .filter(|x| x.ty == channel_type::DSO && store.has_data(x.index as i32))
            .filter_map(|x| store.get_samples(0, x.index as i32))
            .collect();
        let ch_num = chans.len();
        const USIZE: u64 = 8192;
        let mut i = 0;
        let mut buf = vec![0u8; USIZE as usize * ch_num.max(1)];
        while i < n {
            let size = USIZE.min(n - i) as usize;
            for (c, data) in chans.iter().enumerate() {
                for j in 0..size {
                    buf[j * ch_num + c] = data.get(i as usize + j).copied().unwrap_or(0);
                }
            }
            emit(
                ex.receive(&ExportPacket::Dso {
                    data: &buf[..size * ch_num],
                    num_samples: size,
                })
                .map_err(|e| e.to_string())?,
            )?;
            i += USIZE;
        }
    }
    emit(ex.receive(&ExportPacket::End).map_err(|e| e.to_string())?)?;
    out.flush().map_err(|e| e.to_string())
}
