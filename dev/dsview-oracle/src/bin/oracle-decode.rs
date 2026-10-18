//! Runs the original C decoder engine over a capture and prints every decoder
//! output (annotations, binary, meta, errors) as JSON lines.
//!
//! usage: oracle-decode <capture.dsl|.demo> [--max-chunk N] [--stack JSON] [--log LEVEL]
//!
//! Without --stack, every decoder stack saved in the capture is run in turn.
//! --stack takes one stack in DSView's saved form, e.g.
//! '{"id":"1:i2c","channel":[{"scl":1},{"sda":0}],"options":{},"stacked decoders":[]}'

use std::ffi::{CString, c_char, c_void};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use dsview_oracle::{capture, decoder_dir, ffi};
use serde_json::{Value, json};

struct Output(Arc<Mutex<BufWriter<std::io::Stdout>>>);

extern "C" fn emit(user: *mut c_void, line: *const c_char) {
    let out = unsafe { &*(user as *const Output) };
    let line = unsafe { std::ffi::CStr::from_ptr(line) };
    let mut w = out.0.lock().unwrap();
    let _ = w.write_all(line.to_bytes());
    let _ = w.write_all(b"\n");
}

/// Owns the C strings behind one `oracle_dec`.
struct DecSpec {
    id: CString,
    opt_keys: Vec<CString>,
    opt_strs: Vec<CString>,
    opts: Vec<ffi::OracleOpt>,
    ch_ids: Vec<CString>,
    ch_ptrs: Vec<*const c_char>,
    ch_idx: Vec<i32>,
}

impl DecSpec {
    fn new(id: &str, options: Option<&Value>, channels: Option<&Value>) -> Result<Self> {
        let mut spec = DecSpec {
            id: CString::new(id)?,
            opt_keys: vec![],
            opt_strs: vec![],
            opts: vec![],
            ch_ids: vec![],
            ch_ptrs: vec![],
            ch_idx: vec![],
        };
        let mut nums = Vec::new();
        if let Some(Value::Object(map)) = options {
            for (k, v) in map {
                spec.opt_keys.push(CString::new(k.as_str())?);
                let (s, num, is_num) = match v {
                    Value::String(s) => (s.clone(), 0.0, 0),
                    Value::Number(n) => (String::new(), n.as_f64().unwrap_or(0.0), 1),
                    _ => (String::new(), 0.0, 1),
                };
                spec.opt_strs.push(CString::new(s)?);
                nums.push((num, is_num));
            }
        }
        for (i, (num, is_num)) in nums.into_iter().enumerate() {
            spec.opts.push(ffi::OracleOpt {
                key: spec.opt_keys[i].as_ptr(),
                str_: spec.opt_strs[i].as_ptr(),
                num,
                is_num,
            });
        }
        // DSView stores channels as [{"scl": 1}, {"sda": 0}].
        for entry in channels.and_then(Value::as_array).into_iter().flatten() {
            for (k, v) in entry.as_object().into_iter().flatten() {
                spec.ch_ids.push(CString::new(k.as_str())?);
                spec.ch_idx
                    .push(v.as_i64().context("channel index")? as i32);
            }
        }
        spec.ch_ptrs = spec.ch_ids.iter().map(|c| c.as_ptr()).collect();
        Ok(spec)
    }

    fn raw(&self) -> ffi::OracleDec {
        ffi::OracleDec {
            id: self.id.as_ptr(),
            opts: self.opts.as_ptr(),
            n_opts: self.opts.len() as i32,
            ch_id: self.ch_ptrs.as_ptr(),
            ch_idx: self.ch_idx.as_ptr(),
            n_ch: self.ch_idx.len() as i32,
        }
    }
}

fn stack_specs(entry: &Value) -> Result<Vec<DecSpec>> {
    let id = entry
        .get("id")
        .and_then(Value::as_str)
        .context("decoder without id")?;
    let mut specs = vec![DecSpec::new(
        id,
        entry.get("options"),
        entry.get("channel"),
    )?];
    for sub in entry
        .get("stacked decoders")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let sid = sub
            .get("id")
            .and_then(Value::as_str)
            .context("stacked decoder without id")?;
        specs.push(DecSpec::new(sid, sub.get("options"), None)?);
    }
    Ok(specs)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut capture_path: Option<PathBuf> = None;
    let mut max_chunk = 0u64;
    let mut stack: Option<Value> = None;
    let mut log_level = 0;
    let mut list = false;
    let mut first = 0usize;
    let mut count = usize::MAX;
    let mut stack_timeout = 0u64;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--max-chunk" => max_chunk = args.next().context("--max-chunk N")?.parse()?,
            "--stack" => stack = Some(serde_json::from_str(&args.next().context("--stack JSON")?)?),
            "--log" => log_level = args.next().context("--log LEVEL")?.parse()?,
            "--list" => list = true,
            "--first" => first = args.next().context("--first N")?.parse()?,
            "--count" => count = args.next().context("--count N")?.parse()?,
            "--stack-timeout" => {
                stack_timeout = args.next().context("--stack-timeout SECS")?.parse()?
            }
            "-h" | "--help" => {
                println!(
                    "usage: oracle-decode <capture> [--max-chunk N] [--stack JSON] [--log LEVEL]"
                );
                return Ok(());
            }
            p if capture_path.is_none() => capture_path = Some(PathBuf::from(p)),
            other => bail!("unexpected argument {other}"),
        }
    }
    if list {
        let dir = CString::new(decoder_dir().to_string_lossy().as_bytes())?;
        if unsafe { ffi::oracle_decode_init(dir.as_ptr(), log_level) } != 0 {
            bail!("libsigrokdecode init failed");
        }
        let out = Output(Arc::new(Mutex::new(BufWriter::new(std::io::stdout()))));
        unsafe { ffi::oracle_decode_list(emit, &out as *const Output as *mut c_void) };
        out.0.lock().unwrap().flush()?;
        unsafe { ffi::oracle_decode_exit() };
        return Ok(());
    }
    let capture_path = capture_path.context("missing capture path")?;
    let cap = capture::load(&capture_path)?;
    let stacks = match stack {
        Some(s) => vec![s],
        None => cap.decoders.clone(),
    };

    let dir = CString::new(decoder_dir().to_string_lossy().as_bytes())?;
    if unsafe { ffi::oracle_decode_init(dir.as_ptr(), log_level) } != 0 {
        bail!("libsigrokdecode init failed");
    }

    let out: &'static Output = Box::leak(Box::new(Output(Arc::new(Mutex::new(BufWriter::new(
        std::io::stdout(),
    ))))));
    let dog = (stack_timeout > 0).then(|| {
        dsview_oracle::watchdog::Watchdog::start(
            std::time::Duration::from_secs(stack_timeout),
            Arc::clone(&out.0) as Arc<Mutex<dyn Write + Send>>,
        )
    });
    let signal_ptrs: Vec<*const u8> = cap
        .signals
        .iter()
        .map(|s| s.as_ref().map_or(std::ptr::null(), |v| v.as_ptr()))
        .collect();

    for (index, entry) in stacks.iter().enumerate().skip(first).take(count) {
        let specs = stack_specs(entry)?;
        if let Some(d) = &dog {
            d.begin(index);
        }
        let raw: Vec<ffi::OracleDec> = specs.iter().map(DecSpec::raw).collect();
        let ids: Vec<&str> = specs.iter().map(|s| s.id.to_str().unwrap()).collect();
        emit_value(out, &json!({"t": "stack", "index": index, "decoders": ids}));
        let rc = unsafe {
            ffi::oracle_decode_run(
                raw.as_ptr(),
                raw.len() as i32,
                cap.samplerate,
                cap.total_samples,
                signal_ptrs.as_ptr(),
                signal_ptrs.len() as i32,
                max_chunk,
                emit,
                out as *const Output as *mut c_void,
            )
        };
        if let Some(d) = &dog {
            d.end();
        }
        emit_value(out, &json!({"t": "stack_end", "index": index, "rc": rc}));
        // finished stacks survive a kill (decode-diff resumes after them)
        out.0.lock().unwrap().flush()?;
    }
    out.0.lock().unwrap().flush()?;
    unsafe { ffi::oracle_decode_exit() };
    Ok(())
}

fn emit_value(out: &Output, v: &Value) {
    let mut w = out.0.lock().unwrap();
    let _ = writeln!(w, "{v}");
}
