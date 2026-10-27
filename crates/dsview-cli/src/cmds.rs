//! The one-shot commands: `devices`, `info`, `capture`, `decode`, `export`,
//! `verify-counter` and `selftest`. They run the control API in process,
//! so they check their arguments as `dsview-mcp` does.

use std::path::Path;
use std::time::Instant;

use dsview_control::ControlBackend;
use dsview_control::api::Api;
use dsview_core::{ConfValue, key};
use dsview_session::Session;
use serde_json::{Map, Value, json};

/// Options of the one-shot commands.
#[derive(Default)]
pub struct Opts {
    /// The positional arguments after the command.
    pub args: Vec<String>,
    pub device: Option<String>,
    pub mode: Option<String>,
    pub samplerate: Option<u64>,
    pub samples: Option<u64>,
    /// `0-7,9`
    pub channels: Option<String>,
    pub timeout_s: f64,
    pub out: Option<String>,
    /// `uart:rx=0,baudrate=115200`, per `--decoder`
    pub decoders: Vec<String>,
    pub json: bool,
}

struct Ctl {
    api: Api,
}

impl Ctl {
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let p = params.as_object().cloned().unwrap_or_default();
        self.api
            .call(method, &p)
            .map_err(|e| format!("{method}: {e}"))
    }
}

/// `--device`: an index of `devices`, or a part of a name (any case).
/// Without one: the first hardware device, else the active one.
fn select(c: &Ctl, s: &Session, want: Option<&str>) -> Result<(), String> {
    let list = c.call("list_devices", json!({}))?;
    let devices = list["devices"].as_array().cloned().unwrap_or_default();
    let index = match want {
        Some(w) => match w.parse::<i64>() {
            Ok(i) => Some(i),
            Err(_) => devices
                .iter()
                .find(|d| {
                    d["name"]
                        .as_str()
                        .is_some_and(|n| n.to_lowercase().contains(&w.to_lowercase()))
                })
                .and_then(|d| d["index"].as_i64()),
        }
        .ok_or_else(|| format!("no device matches '{w}'"))
        .map(Some)?,
        None => {
            let (entries, _) = s.devices();
            entries
                .iter()
                .position(|e| matches!(e.kind, dsview_session::Kind::Usb { .. }))
                .map(|i| i as i64)
        }
    };
    if let Some(i) = index {
        c.call("select_device", json!({ "index": i }))?;
    }
    Ok(())
}

/// `0-7,9` → [0..=7, 9]
fn channel_list(spec: &str) -> Result<Vec<i64>, String> {
    let mut out = Vec::new();
    for part in spec.split(',').filter(|p| !p.is_empty()) {
        let bad = || format!("bad channel list '{spec}'");
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (i64, i64) =
                    (a.parse().map_err(|_| bad())?, b.parse().map_err(|_| bad())?);
                if a > b {
                    return Err(bad());
                }
                out.extend(a..=b);
            }
            None => out.push(part.parse().map_err(|_| bad())?),
        }
    }
    Ok(out)
}

/// `id:key=value,...`: the decoder, its channel map (keys that are channel
/// ids) and its options (the rest; numbers stay numbers).
fn decoder_spec(spec: &str) -> Result<Value, String> {
    // ids carry a colon themselves ("0:uart"); "uart" finds it too
    let find = |id: &str| {
        dsview_decode::decoder_by_id(id).or_else(|| {
            dsview_decode::decoders()
                .into_iter()
                .find(|d| d.id.rsplit(':').next() == Some(id))
        })
    };
    // the longest id that is a prefix wins
    let (info, rest) = spec
        .match_indices(':')
        .map(|(i, _)| (&spec[..i], &spec[i + 1..]))
        .chain(std::iter::once((spec, "")))
        .rev()
        .find_map(|(id, rest)| find(id).map(|d| (d, rest)))
        .ok_or_else(|| format!("unknown decoder in '{spec}'; see `dsviewr-cli decoders`"))?;
    let (mut chans, mut opts) = (Map::new(), Map::new());
    for kv in rest.split(',').filter(|s| !s.is_empty()) {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| format!("'{kv}' in --decoder needs key=value"))?;
        let is_channel = info
            .channels
            .iter()
            .chain(&info.opt_channels)
            .any(|c| c.id == k);
        if is_channel {
            let ch: i64 = v
                .parse()
                .map_err(|_| format!("channel {k}={v} is not a number"))?;
            chans.insert(k.into(), ch.into());
        } else {
            let value = v
                .parse::<i64>()
                .map(Value::from)
                .or_else(|_| v.parse::<f64>().map(Value::from))
                .unwrap_or_else(|_| v.into());
            opts.insert(k.into(), value);
        }
    }
    Ok(json!({"id": info.id, "channels": chans, "options": opts, "run": true}))
}

fn print(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

fn capture(c: &Ctl, s: &Session, o: &Opts) -> Result<Value, String> {
    select(c, s, o.device.as_deref())?;
    if let Some(m) = &o.mode {
        c.call("set_mode", json!({ "mode": m }))?;
    }
    if o.samplerate.is_some() || o.samples.is_some() {
        let mut p = Map::new();
        if let Some(r) = o.samplerate {
            p.insert("samplerate".into(), r.into());
        }
        if let Some(n) = o.samples {
            p.insert("samples".into(), n.into());
        }
        c.call("set_sampling", Value::Object(p))?;
    }
    if let Some(spec) = &o.channels {
        let on = channel_list(spec)?;
        let all: Vec<Value> = s
            .channels()
            .iter()
            .map(|ch| json!({"index": ch.index, "enabled": on.contains(&i64::from(ch.index))}))
            .collect();
        c.call("set_channels", json!({ "channels": all }))?;
    }
    c.call("start_capture", json!({ "timeout_s": o.timeout_s }))
}

/// Writes the capture: `.dsl` saves it, `.csv`/`.vcd`/`.srzip` export it.
fn write(c: &Ctl, path: &str) -> Result<Value, String> {
    if path.ends_with(".dsl") {
        c.call("save_session", json!({ "path": path }))
    } else {
        c.call("export_data", json!({ "path": path }))
    }
}

/// Runs a one-shot command; the exit code.
pub fn run(cmd: &str, s: &Session, version: &str, o: &Opts) -> i32 {
    let c = Ctl {
        api: Api::new(s.clone(), version),
    };
    let r = match cmd {
        "devices" => c.call("list_devices", json!({})).map(|v| {
            if o.json {
                print(&v);
            } else {
                for d in v["devices"].as_array().into_iter().flatten() {
                    println!(
                        "{:>3}  {}{}",
                        d["index"].as_i64().unwrap_or(-1),
                        d["name"].as_str().unwrap_or(""),
                        if d["active"] == true {
                            "  (active)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }),
        "decoders" => c.call("list_decoders", json!({})).map(|v| {
            if o.json {
                print(&v);
            } else {
                for d in v["decoders"].as_array().into_iter().flatten() {
                    println!(
                        "{:<16} {}",
                        d["id"].as_str().unwrap_or(""),
                        d["longname"].as_str().unwrap_or("")
                    );
                }
            }
        }),
        "info" => select(&c, s, o.device.as_deref())
            .and_then(|_| c.call("get_config", json!({})))
            .map(|v| print(&v)),
        "capture" => (|| {
            let out = o.out.as_deref().ok_or("capture needs --out FILE")?;
            let info = capture(&c, s, o)?;
            write(&c, out)?;
            if o.json {
                print(&info);
            } else {
                eprintln!("captured into {out}");
            }
            Ok(())
        })(),
        "export" => (|| {
            let [input, out] = o.args.as_slice() else {
                return Err("usage: dsviewr-cli export FILE.dsl OUT.csv|vcd|srzip".to_string());
            };
            c.call("open_file", json!({ "path": input }))?;
            write(&c, out).map(|_| ())
        })(),
        "decode" => (|| {
            let [input] = o.args.as_slice() else {
                return Err("usage: dsviewr-cli decode FILE.dsl --decoder ID[:k=v,...]".to_string());
            };
            if o.decoders.is_empty() {
                return Err("decode needs at least one --decoder".into());
            }
            c.call("open_file", json!({ "path": input }))?;
            let mut all = Vec::new();
            for spec in &o.decoders {
                c.call("add_decoder", decoder_spec(spec)?)?;
                // after the file's own decoders
                let i = s.state().decoders.len() - 1;
                let a = c.call(
                    "get_annotations",
                    json!({ "decoder": i, "limit": 1_000_000 }),
                )?;
                all.push(a);
            }
            if o.json {
                print(&Value::Array(all));
            } else {
                for a in &all {
                    for x in a["annotations"].as_array().into_iter().flatten() {
                        println!(
                            "{}-{} {}: {}",
                            x["start_sample"],
                            x["end_sample"],
                            x["row"].as_str().unwrap_or(""),
                            x["text"].as_str().unwrap_or("")
                        );
                    }
                }
            }
            Ok(())
        })(),
        "diff-dsl" => crate::diff::dsl(&o.args),
        "diff-ann" => crate::diff::ann_files(&o.args),
        "verify-counter" => verify_counter(&c, s, o),
        "selftest" => selftest(&c, s, version, o),
        _ => Err(format!("unknown command {cmd}")),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("dsviewr-cli {cmd}: {e}");
            1
        }
    }
}

/// The DSLogic's internal test mode: the FPGA sends a binary counter
/// across the enabled channels (bit k on the k-th). Every sample must be
/// the previous one plus one.
fn verify_counter(c: &Ctl, s: &Session, o: &Opts) -> Result<(), String> {
    select(c, s, o.device.as_deref())?;
    let hw = s.state().device.as_ref().is_some_and(|d| d.is_hardware());
    if !hw
        || !s
            .state()
            .device
            .as_ref()
            .is_some_and(|d| d.driver.starts_with("DSLogic"))
    {
        return Err("verify-counter needs a DSLogic".into());
    }
    let op = s
        .config(key::OPERATION_MODE, None)
        .map_or(1, |v| v.get_i16());
    s.set_config(key::OPERATION_MODE, ConfValue::I16(2), None);
    let r = (|| {
        let mut p = Map::new();
        if let Some(r) = o.samplerate {
            p.insert("samplerate".into(), r.into());
        }
        if let Some(n) = o.samples {
            p.insert("samples".into(), n.into());
        }
        if !p.is_empty() {
            c.call("set_sampling", Value::Object(p))?;
        }
        if let Some(spec) = &o.channels {
            let on = channel_list(spec)?;
            let all: Vec<Value> = s
                .channels()
                .iter()
                .map(|ch| json!({"index": ch.index, "enabled": on.contains(&i64::from(ch.index))}))
                .collect();
            c.call("set_channels", json!({ "channels": all }))?;
        }
        let info = c.call("start_capture", json!({ "timeout_s": o.timeout_s }))?;
        if o.json {
            print(&info);
        }
        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
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
        let mut bad = 0u64;
        let mut prev = value(0);
        for i in 1..len {
            let v = value(i);
            if v != prev.wrapping_add(1) & mask {
                bad += 1;
            }
            prev = v;
        }
        println!("counter check: {n} channels, {len} samples, {bad} breaks");
        if len == 0 || bad > 0 {
            Err(format!("{bad} breaks in {len} samples"))
        } else {
            Ok(())
        }
    })();
    s.set_config(key::OPERATION_MODE, ConfValue::I16(op), None);
    r
}

/// A bundle for owners of devices the developers do not have (DSCope,
/// MSO-E8): versions, the device list, each hardware device's settings, a
/// short capture and the full log, in `--out DIR`.
fn selftest(c: &Ctl, s: &Session, version: &str, o: &Opts) -> Result<(), String> {
    let dir = Path::new(o.out.as_deref().ok_or("selftest needs --out DIR")?);
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let t0 = Instant::now();
    let mut report = Map::new();
    report.insert("dsview".into(), version.into());
    report.insert("version".into(), c.call("version", json!({}))?);
    report.insert(
        "os".into(),
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH).into(),
    );
    report.insert("devices".into(), c.call("list_devices", json!({}))?);
    let mut runs = Vec::new();
    let (entries, _) = s.devices();
    for (i, e) in entries.iter().enumerate() {
        if !matches!(e.kind, dsview_session::Kind::Usb { .. }) {
            continue;
        }
        let mut r = Map::new();
        r.insert("name".into(), e.name.clone().into());
        let step = |r: &mut Map<String, Value>, k: &str, v: Result<Value, String>| match v {
            Ok(v) => {
                r.insert(k.into(), v);
                true
            }
            Err(e) => {
                r.insert(format!("{k}_error"), e.into());
                false
            }
        };
        if step(
            &mut r,
            "select",
            c.call("select_device", json!({ "index": i })),
        ) && step(&mut r, "config", c.call("get_config", json!({})))
            && step(
                &mut r,
                "capture",
                c.call("start_capture", json!({ "timeout_s": o.timeout_s })),
            )
        {
            let file = dir.join(format!("capture-{i}.dsl"));
            step(
                &mut r,
                "saved",
                c.call("save_session", json!({ "path": file.to_string_lossy() })),
            );
        }
        runs.push(Value::Object(r));
    }
    if runs.is_empty() {
        report.insert("note".into(), "no hardware device found".into());
    }
    report.insert("hardware".into(), runs.into());
    report.insert("seconds".into(), t0.elapsed().as_secs_f64().into());
    let path = dir.join("selftest.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&report).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    eprintln!(
        "selftest: {} written; please attach the whole directory {}",
        path.display(),
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_lists() {
        assert_eq!(channel_list("0-3,7").unwrap(), [0, 1, 2, 3, 7]);
        assert_eq!(channel_list("5").unwrap(), [5]);
        assert!(channel_list("3-1").is_err());
        assert!(channel_list("a").is_err());
    }
}
