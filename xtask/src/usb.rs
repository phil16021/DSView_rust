//! `cargo xtask usb record`: drives a DSLogic through the C implementation
//! (`DSView --mcp`) scenario by scenario while dumpcap records the USB
//! traffic. These pcapng files are the Phase 3 replay baseline.
//!
//! Needs: dumpcap (wireshark-common), `sudo modprobe usbmon`, read access to
//! /dev/usbmonN for the DSLogic's bus. See tools/usb/README.md.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use dsview_testkit::control::{Client, default_socket_path};
use serde_json::{Value, json};

struct Scenario {
    name: String,
    samplerate: u64,
    channels: usize,
    trigger: Option<(&'static str, i64)>,
    /// Stop after this long instead of waiting for the capture to finish.
    stop_after: Option<Duration>,
}

fn scenarios(available_rates: &[u64]) -> Vec<Scenario> {
    let mut out = Vec::new();
    // Three representative rates from what the device offers in its current
    // mode: the first >= 1 MHz, the middle of the remaining range, the highest.
    let mut rates: Vec<u64> = available_rates
        .iter()
        .copied()
        .filter(|&r| r >= 1_000_000)
        .collect();
    rates.sort_unstable();
    let mut picks: Vec<u64> = Vec::new();
    for r in [rates.first(), rates.get(rates.len() / 2), rates.last()]
        .into_iter()
        .flatten()
    {
        if !picks.contains(r) {
            picks.push(*r);
        }
    }
    for &rate in &picks {
        for ch in [16usize, 8, 4] {
            out.push(Scenario {
                name: format!("rate-{}khz-{ch}ch", rate / 1_000),
                samplerate: rate,
                channels: ch,
                trigger: None,
                stop_after: None,
            });
        }
    }
    let rate = picks.first().copied().unwrap_or(1_000_000);
    out.push(Scenario {
        name: "trigger-rising-ch0-stop-while-waiting".into(),
        samplerate: rate,
        channels: 16,
        trigger: Some(("rising", 0)),
        stop_after: Some(Duration::from_secs(2)),
    });
    out.push(Scenario {
        name: "trigger-high-ch1-stop-while-waiting".into(),
        samplerate: rate,
        channels: 16,
        trigger: Some(("high", 1)),
        stop_after: Some(Duration::from_secs(2)),
    });
    out
}

fn call(c: &mut Client, method: &str, params: Value) -> Result<Value> {
    c.call(method, &params, Duration::from_secs(60))?
        .map_err(|e| anyhow::anyhow!("{method}: {e}"))
}

fn run_steps(c: &mut Client, device: i64, s: &Scenario) -> Result<Value> {
    call(c, "select_device", json!({"index": device}))?;
    call(c, "set_mode", json!({"mode": "logic"}))?;
    let chans: Vec<Value> = (0..16)
        .map(|i| json!({"index": i, "enabled": i < s.channels}))
        .collect();
    call(c, "set_channels", json!({"channels": chans}))?;
    match s.trigger {
        Some((kind, ch)) => call(
            c,
            "set_trigger",
            json!({"type": kind, "channel": ch, "position_percent": 50}),
        )?,
        None => call(c, "set_trigger", json!({"type": "none"}))?,
    };
    call(
        c,
        "set_sampling",
        json!({"samplerate": s.samplerate, "samples": 1_048_576}),
    )?;
    match s.stop_after {
        None => call(c, "start_capture", json!({"timeout_s": 30})),
        Some(d) => {
            call(c, "start_capture", json!({"wait": false}))?;
            std::thread::sleep(d);
            let status = call(c, "capture_status", json!({}))?;
            call(c, "stop_capture", json!({}))?;
            // Stopping is asynchronous: the FPGA flushes its buffer first.
            let t0 = std::time::Instant::now();
            loop {
                let st = call(c, "capture_status", json!({}))?;
                if st["working"] == json!(false) {
                    return Ok(
                        json!({"status_before_stop": status, "stop_ms": t0.elapsed().as_millis() as u64,
                                     "samples_after_stop": st["capture"]["samples"]}),
                    );
                }
                if t0.elapsed() > Duration::from_secs(15) {
                    bail!("capture still running 15 s after stop_capture");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn start_dumpcap(iface: &str, file: &Path) -> Result<Child> {
    let child = Command::new("dumpcap")
        .args(["-q", "-i", iface, "-w"])
        .arg(file)
        .spawn()
        .context("starting dumpcap (install wireshark-common)")?;
    std::thread::sleep(Duration::from_millis(800));
    Ok(child)
}

fn stop_dumpcap(mut child: Child) -> Result<()> {
    std::thread::sleep(Duration::from_millis(500));
    // SIGINT lets dumpcap finish the pcapng file cleanly.
    unsafe { libc_kill(child.id() as i32, 2) };
    child.wait()?;
    Ok(())
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

pub fn record(root: &Path, args: &[&str]) -> Result<()> {
    let mut iface = "usbmon1".to_string();
    let mut out = root.join("testdata/usb/dslogic");
    let mut dry_run = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--iface" => iface = it.next().context("--iface usbmonN")?.to_string(),
            "--out" => out = PathBuf::from(it.next().context("--out DIR")?),
            "--dry-run" => dry_run = true,
            other => bail!("unexpected argument {other}"),
        }
    }

    let mut c = Client::new(default_socket_path());
    let devices = call(&mut c, "list_devices", json!({}))?;
    let dev = devices["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["name"].as_str().is_some_and(|n| n.starts_with("DSLogic")))
        .context("no DSLogic in list_devices; plug it in and start `DSView --mcp`")?;
    let index = dev["index"].as_i64().unwrap();
    let name = dev["name"].as_str().unwrap_or_default().to_string();

    let config = call(&mut c, "select_device", json!({"index": index}))?;
    let rates: Vec<u64> = config["available_samplerates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .collect();
    println!("device: {name}; {} samplerates available", rates.len());

    std::fs::create_dir_all(&out)?;
    let mut summary = Vec::new();
    for s in scenarios(&rates) {
        let file = out.join(format!("{}.pcapng", s.name));
        let cap = if dry_run {
            None
        } else {
            Some(start_dumpcap(&iface, &file)?)
        };
        let result = run_steps(&mut c, index, &s);
        if let Some(child) = cap {
            stop_dumpcap(child)?;
        }
        let ok = result.is_ok();
        println!("{} {}", if ok { "ok  " } else { "FAIL" }, s.name);
        let result = result.unwrap_or_else(|e| json!({"error": e.to_string()}));
        summary.push(json!({
            "scenario": s.name, "samplerate": s.samplerate, "channels": s.channels,
            "trigger": s.trigger.map(|(k, ch)| json!({"type": k, "channel": ch})),
            "pcapng": (!dry_run).then(|| file.file_name().unwrap().to_string_lossy().into_owned()),
            "result": result,
        }));
    }
    let manifest =
        json!({"device": name, "iface": iface, "dry_run": dry_run, "scenarios": summary});
    let path = out.join(if dry_run {
        "dry-run.json"
    } else {
        "manifest.json"
    });
    std::fs::write(&path, serde_json::to_string_pretty(&manifest)? + "\n")?;
    println!("wrote {}", path.display());
    Ok(())
}
