//! The Rust decoder engine (`dsview-decode`) driven exactly like
//! `csrc/oracle_decode.c` drives the C one: DecoderStack's chunking over
//! LogicSnapshot leaves, StoreSession's option typing, and the same JSON
//! lines byte for byte, so `rust-decode` and `oracle-decode` outputs can be
//! diffed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use dsview_decode::{OptValue, Output, OutputData, Session};
use dsview_formats::cfmt::fmt_g;
use dsview_formats::header::{strtod, strtoll};
use serde_json::Value;

/// LogicSnapshot leaf size (pv/data/logicsnapshot.h).
const LEAF_SAMPLES: u64 = 1 << 24;
/// DecoderStack::MaxChunkSize.
const MAX_CHUNK_SIZE: u64 = 16384;

/// One decoder of a stack, in DSView's saved form.
pub struct StackDecoder {
    pub id: String,
    /// Option values as saved: JSON strings or numbers.
    pub options: Vec<(String, Value)>,
    /// Decoder channel id -> signal index (bottom decoder only).
    pub channels: Vec<(String, i32)>,
}

/// Parses one saved decoder stack (`{"id":..., "channel":[...], "options":{...},
/// "stacked decoders":[...]}`).
pub fn parse_stack(entry: &Value) -> anyhow::Result<Vec<StackDecoder>> {
    use anyhow::Context;
    fn one(v: &Value, with_channels: bool) -> anyhow::Result<StackDecoder> {
        let id = v
            .get("id")
            .and_then(Value::as_str)
            .context("decoder without id")?;
        let options = v
            .get("options")
            .and_then(Value::as_object)
            .map(|m| m.iter().map(|(k, x)| (k.clone(), x.clone())).collect())
            .unwrap_or_default();
        let mut channels = Vec::new();
        if with_channels {
            for e in v
                .get("channel")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                for (k, x) in e.as_object().into_iter().flatten() {
                    channels.push((k.clone(), x.as_i64().context("channel index")? as i32));
                }
            }
        }
        Ok(StackDecoder {
            id: id.to_owned(),
            options,
            channels,
        })
    }
    let mut out = vec![one(entry, true)?];
    for sub in entry
        .get("stacked decoders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        out.push(one(sub, false)?);
    }
    Ok(out)
}

/// `json_str` of oracle_decode.c (control bytes escaped, the rest raw).
fn json_text(s: &mut String, v: &str) {
    s.push('"');
    for ch in v.chars() {
        match ch {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
}

fn begin_event(s: &mut String, ty: &str, o: &Output<'_>) {
    s.push_str(&format!("{{\"t\":\"{ty}\",\"inst\":"));
    json_text(s, &o.inst.inst_id);
    s.push_str(&format!(",\"ss\":{},\"es\":{}", o.start, o.end));
}

fn event_line(o: &Output<'_>) -> Option<String> {
    let mut s = String::new();
    match &o.data {
        OutputData::Annotation(a) => {
            begin_event(&mut s, "ann", o);
            s.push_str(&format!(
                ",\"cls\":{},\"type\":{},\"texts\":[",
                a.class, a.ann_type
            ));
            for (i, t) in a.texts.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                json_text(&mut s, t);
            }
            s.push(']');
            if !a.hex.is_empty() {
                s.push_str(",\"hex\":");
                json_text(&mut s, &a.hex);
                s.push_str(&format!(",\"num\":{}", a.num));
            }
            s.push('}');
        }
        OutputData::Binary { class, data } => {
            begin_event(&mut s, "bin", o);
            s.push_str(&format!(",\"cls\":{class},\"data\":\""));
            for b in data.iter() {
                s.push_str(&format!("{b:02x}"));
            }
            s.push_str("\"}");
        }
        OutputData::Meta(v) => {
            begin_event(&mut s, "meta", o);
            s.push_str(",\"name\":");
            json_text(&mut s, o.pdo.meta.as_ref().map_or("", |m| m.1.as_str()));
            match v {
                dsview_decode::MetaValue::Int(i) => s.push_str(&format!(",\"value\":{i}}}")),
                dsview_decode::MetaValue::Float(f) => {
                    s.push_str(&format!(",\"value\":{}}}", fmt_g(*f, 17)))
                }
            }
        }
        // DSView shows no logic outputs; the goldens predate them
        OutputData::Python(_) | OutputData::Logic { .. } => return None,
    }
    Some(s)
}

fn error_line(wh: &str, msg: &str) -> String {
    let mut s = String::from("{\"t\":\"error\",\"where\":");
    json_text(&mut s, wh);
    s.push_str(",\"msg\":");
    json_text(&mut s, msg);
    s.push('}');
    s
}

/// Option typing as in StoreSession::load_decoders (see `build_options` in
/// oracle_decode.c).
fn build_options(
    dec: &dsview_decode::DecoderInfo,
    spec: &StackDecoder,
) -> HashMap<String, OptValue> {
    let mut out = HashMap::new();
    for opt in &dec.options {
        let Some((_, v)) = spec.options.iter().rev().find(|(k, _)| *k == opt.id) else {
            continue;
        };
        let (vs, num, is_num) = match v {
            Value::String(s) => (s.as_str(), 0.0, false),
            Value::Number(n) => ("", n.as_f64().unwrap_or(0.0), true),
            _ => ("", 0.0, true),
        };
        let value = match &opt.def {
            Some(OptValue::Float(_)) => {
                let mut d = if is_num { num } else { 0.0 };
                if !vs.is_empty() {
                    d = strtod(vs);
                }
                Some(OptValue::Float(d))
            }
            Some(OptValue::Int(_)) => {
                // QJsonValue::toInt() then QString::toInt(): 32-bit
                let mut i = if is_num { num as i32 } else { 0 };
                if !vs.is_empty() {
                    i = strtoll(vs) as i32;
                }
                Some(OptValue::Int(i as i64))
            }
            Some(OptValue::Str(_)) => Some(OptValue::Str(vs.to_owned())),
            None => None,
        };
        if let Some(v) = value {
            out.insert(opt.id.clone(), v);
        }
    }
    out
}

fn leaf_is_const(bits: &[u8], leaf_samples: u64) -> bool {
    let words = (leaf_samples / 64) as usize;
    let mut last = 0u64;
    for i in 0..words {
        let mut b = [0u8; 8];
        let src = bits.get(i * 8..i * 8 + 8).unwrap_or(&[]);
        b[..src.len()].copy_from_slice(src);
        let w = u64::from_le_bytes(b);
        if i == 0 {
            last = if w & 1 != 0 { !0 } else { 0 };
        }
        if last ^ w != 0 {
            return false;
        }
        last = if w >> 63 != 0 { !0 } else { 0 };
    }
    true
}

fn sample_at(bits: &[u8], i: u64) -> u8 {
    (bits[(i / 8) as usize] >> (i % 8)) & 1
}

/// `oracle_decode_run` on the Rust engine: emits each JSON line through
/// `emit`; returns 0 or -1 like C.
pub fn run(
    stack: &[StackDecoder],
    samplerate: u64,
    total_samples: u64,
    signals: &[Option<Vec<u8>>],
    max_chunk: u64,
    emit: Arc<dyn Fn(String) + Send + Sync>,
) -> i32 {
    let max_chunk = if max_chunk == 0 {
        MAX_CHUNK_SIZE
    } else {
        max_chunk
    };
    if total_samples == 0 {
        emit(error_line("input", "no samples"));
        return -1;
    }
    let session = Session::new();
    let mut prev: Option<Arc<dsview_decode::Instance>> = None;
    let mut bottom = None;
    for spec in stack {
        let Some(dec) = dsview_decode::decoder_by_id(&spec.id) else {
            emit(error_line("stack", &spec.id));
            return -1;
        };
        let opts = build_options(&dec, spec);
        let di = match session.inst_new(&spec.id, Some(opts), None) {
            Ok(d) => d,
            Err(_) => {
                emit(error_line("inst_new", &spec.id));
                return -1;
            }
        };
        if spec
            .channels
            .iter()
            .any(|(_, i)| *i < 0 || *i as usize >= signals.len() || signals[*i as usize].is_none())
        {
            emit(error_line(
                "channels",
                "decoder channel bound to a missing signal",
            ));
            return -1;
        }
        let _ = session.channel_set_all(&di, &spec.channels);
        match &prev {
            Some(p) => session.stack(p, &di),
            None => bottom = Some(Arc::clone(&di)),
        }
        prev = Some(di);
    }
    let bottom = bottom.expect("a stack has a bottom decoder");

    session.metadata_set_samplerate(samplerate);
    let out = Arc::new(Mutex::new(()));
    for t in [
        dsview_decode::module::OUTPUT_ANN,
        dsview_decode::module::OUTPUT_BINARY,
        dsview_decode::module::OUTPUT_META,
    ] {
        let emit = Arc::clone(&emit);
        let lock = Arc::clone(&out);
        session.callback_add(t, move |o| {
            if let Some(line) = event_line(o) {
                let _g = lock.lock();
                emit(line);
            }
        });
    }

    if let Err(e) = session.start() {
        emit(error_line(
            "start",
            if e.is_empty() {
                "srd_session_start failed"
            } else {
                &e
            },
        ));
        return -1;
    }

    // DecoderStack::decode_data for a finished capture
    let map = bottom.channelmap();
    let end_index = total_samples - 1;
    let mut i = 0u64;
    let mut end_time = false;
    let mut ret = 0;
    let mut failed = false;
    while i <= end_index {
        let mut chunk_end = end_index;
        let mut bufs: Vec<Option<&[u8]>> = Vec::with_capacity(map.len());
        let mut consts = Vec::with_capacity(map.len());
        for &sig in &map {
            if sig == -1 {
                bufs.push(None);
                consts.push(0);
                continue;
            }
            let bits = signals[sig as usize].as_deref().expect("bound signal");
            let leaf_start = (i / LEAF_SAMPLES) * LEAF_SAMPLES;
            let leaf_len = LEAF_SAMPLES.min(total_samples - leaf_start);
            let leaf_bits = &bits[(leaf_start / 8) as usize..];
            chunk_end = (leaf_start + LEAF_SAMPLES).min(total_samples);
            bufs.push(if leaf_is_const(leaf_bits, leaf_len) {
                None
            } else {
                Some(&leaf_bits[((i % LEAF_SAMPLES) / 8) as usize..])
            });
            consts.push(sample_at(bits, i));
        }
        if chunk_end >= end_index {
            chunk_end = end_index + 1;
        }
        if chunk_end - i > max_chunk {
            chunk_end = i + max_chunk;
        }
        end_time = chunk_end > end_index;
        if let Err(e) = session.send(i, chunk_end, &bufs, &consts) {
            let msg = match e {
                dsview_decode::SendError::Decoder(m) => m,
                dsview_decode::SendError::Arg(_) => "srd_session_send failed".to_owned(),
            };
            emit(error_line("send", &msg));
            failed = true;
            ret = -1;
            break;
        }
        i = chunk_end;
    }
    if !failed
        && end_time
        && let Err(e) = session.end()
        && !e.is_empty()
    {
        emit(error_line("end", &e));
    }
    drop(session);
    ret
}

fn json_list(s: &mut String, items: &[String]) {
    s.push('[');
    for (i, t) in items.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_text(s, t);
    }
    s.push(']');
}

fn json_opt(s: &mut String, v: Option<&String>) {
    match v {
        Some(t) => json_text(s, t),
        None => s.push_str("null"),
    }
}

fn json_variant(s: &mut String, v: Option<&OptValue>) {
    match v {
        None => s.push_str("null"),
        Some(OptValue::Str(t)) => {
            s.push_str("{\"s\":");
            json_text(s, t);
            s.push('}');
        }
        Some(OptValue::Int(i)) => s.push_str(&format!("{{\"x\":{i}}}")),
        Some(OptValue::Float(f)) => s.push_str(&format!("{{\"d\":{}}}", fmt_g(*f, 17))),
    }
}

fn json_channels(s: &mut String, chans: &[dsview_decode::decoder::ChannelInfo]) {
    s.push('[');
    for (i, c) in chans.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"id\":");
        json_text(s, &c.id);
        s.push_str(",\"name\":");
        json_text(s, &c.name);
        s.push_str(",\"desc\":");
        json_text(s, &c.desc);
        s.push_str(&format!(
            ",\"order\":{},\"type\":{},\"idn\":",
            c.order, c.ty
        ));
        json_opt(s, c.idn.as_ref());
        s.push('}');
    }
    s.push(']');
}

/// One decoder's metadata in `oracle_decode_list`'s format; annotations in
/// C's GSList order (reversed).
pub fn list_line(d: &dsview_decode::DecoderInfo) -> String {
    let mut s = String::from("{\"id\":");
    json_text(&mut s, &d.id);
    for (k, v) in [
        ("name", &d.name),
        ("longname", &d.longname),
        ("desc", &d.desc),
        ("license", &d.license),
    ] {
        s.push_str(&format!(",\"{k}\":"));
        json_text(&mut s, v);
    }
    for (k, v) in [
        ("inputs", &d.inputs),
        ("outputs", &d.outputs),
        ("tags", &d.tags),
    ] {
        s.push_str(&format!(",\"{k}\":"));
        json_list(&mut s, v);
    }
    s.push_str(",\"channels\":");
    json_channels(&mut s, &d.channels);
    s.push_str(",\"opt_channels\":");
    json_channels(&mut s, &d.opt_channels);
    s.push_str(",\"options\":[");
    for (i, o) in d.options.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"id\":");
        json_text(&mut s, &o.id);
        s.push_str(",\"idn\":");
        json_opt(&mut s, o.idn.as_ref());
        s.push_str(",\"desc\":");
        json_opt(&mut s, o.desc.as_ref());
        s.push_str(",\"def\":");
        json_variant(&mut s, o.def.as_ref());
        s.push_str(",\"values\":[");
        for (j, v) in o.values.iter().enumerate() {
            if j > 0 {
                s.push(',');
            }
            json_variant(&mut s, Some(v));
        }
        s.push_str("]}");
    }
    s.push_str("],\"annotations\":[");
    for (i, a) in d.annotations.iter().rev().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_list(&mut s, a);
    }
    s.push_str("],\"ann_types\":[");
    s.push_str(
        &d.ann_types
            .iter()
            .map(i32::to_string)
            .collect::<Vec<_>>()
            .join(","),
    );
    s.push_str("],\"annotation_rows\":[");
    for (i, r) in d.annotation_rows.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"id\":");
        json_text(&mut s, &r.id);
        s.push_str(",\"desc\":");
        json_text(&mut s, &r.desc);
        s.push_str(",\"classes\":[");
        s.push_str(
            &r.classes
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(","),
        );
        s.push_str("]}");
    }
    s.push_str("],\"binary\":[");
    for (i, b) in d.binary.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        json_list(&mut s, b);
    }
    s.push_str("]}");
    s
}
