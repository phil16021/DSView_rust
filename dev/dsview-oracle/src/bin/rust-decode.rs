//! The Rust decoder engine (`dsview-decode`) over a capture, printing every
//! decoder output as JSON lines in exactly `oracle-decode`'s format.
//!
//! usage: rust-decode <capture.dsl|.demo> [--max-chunk N] [--stack JSON]

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use dsview_oracle::{capture, decoder_dir, rust_run};
use serde_json::{Value, json};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut capture_path: Option<PathBuf> = None;
    let mut max_chunk = 0u64;
    let mut stack: Option<Value> = None;
    let mut list = false;
    let mut first = 0usize;
    let mut count = usize::MAX;
    let mut stack_timeout = 0u64;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--max-chunk" => max_chunk = args.next().context("--max-chunk N")?.parse()?,
            "--stack" => stack = Some(serde_json::from_str(&args.next().context("--stack JSON")?)?),
            "--log" => {
                let level: u8 = args.next().context("--log LEVEL")?.parse()?;
                dsview_decode::log::set_sink(move |l, m| {
                    if (l as u8) <= level {
                        eprintln!("srd: {m}");
                    }
                });
            }
            "--list" => list = true,
            "--first" => first = args.next().context("--first N")?.parse()?,
            "--count" => count = args.next().context("--count N")?.parse()?,
            "--stack-timeout" => {
                stack_timeout = args.next().context("--stack-timeout SECS")?.parse()?
            }
            "-h" | "--help" => {
                println!(
                    "usage: rust-decode <capture> [--max-chunk N] [--stack JSON] [--log LEVEL]"
                );
                return Ok(());
            }
            p if capture_path.is_none() => capture_path = Some(PathBuf::from(p)),
            other => bail!("unexpected argument {other}"),
        }
    }
    if list {
        dsview_decode::init(&[decoder_dir().as_path()]).map_err(anyhow::Error::msg)?;
        dsview_decode::load_all();
        let mut w = BufWriter::new(std::io::stdout());
        for d in dsview_decode::decoders() {
            writeln!(w, "{}", rust_run::list_line(&d))?;
        }
        return Ok(w.flush()?);
    }
    let capture_path = capture_path.context("missing capture path")?;
    let cap = capture::load(&capture_path)?;
    let stacks = match stack {
        Some(s) => vec![s],
        None => cap.decoders.clone(),
    };

    let dir = decoder_dir();
    dsview_decode::init(&[dir.as_path()]).map_err(anyhow::Error::msg)?;
    dsview_decode::load_all();

    let out = Arc::new(Mutex::new(BufWriter::new(std::io::stdout())));
    let dog = (stack_timeout > 0).then(|| {
        dsview_oracle::watchdog::Watchdog::start(
            std::time::Duration::from_secs(stack_timeout),
            Arc::clone(&out) as Arc<Mutex<dyn Write + Send>>,
        )
    });
    let emit: Arc<dyn Fn(String) + Send + Sync> = {
        let out = Arc::clone(&out);
        Arc::new(move |line: String| {
            let mut w = out.lock().unwrap();
            let _ = w.write_all(line.as_bytes());
            let _ = w.write_all(b"\n");
        })
    };
    for (index, entry) in stacks.iter().enumerate().skip(first).take(count) {
        let specs = rust_run::parse_stack(entry)?;
        if let Some(d) = &dog {
            d.begin(index);
        }
        let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
        emit(json!({"t": "stack", "index": index, "decoders": ids}).to_string());
        let rc = rust_run::run(
            &specs,
            cap.samplerate,
            cap.total_samples,
            &cap.signals,
            max_chunk,
            Arc::clone(&emit),
        );
        if let Some(d) = &dog {
            d.end();
        }
        emit(json!({"t": "stack_end", "index": index, "rc": rc}).to_string());
        // finished stacks survive a kill (decode-diff resumes after them)
        out.lock().unwrap().flush()?;
    }
    out.lock().unwrap().flush()?;
    Ok(())
}
