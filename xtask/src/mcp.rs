//! `cargo xtask mcp suite BIN [OUT]`: every `dsview-mcp` tool through the
//! MCP protocol (stdio, newline-delimited JSON-RPC) against the DSView
//! executable BIN (`dsview`, `dsview-cli`, Qt `DSView`), which `dsview-mcp`
//! starts itself. Each step must succeed, or fail where the scenario expects
//! it. The results go to OUT for `cargo xtask mcp compare A B`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

/// DSView itself, started by the suite (so it can be stopped after it):
/// `dsview-cli serve --mcp`, else `BIN --mcp`, plus `DSVIEW_ARGS`.
struct DsView(Child);

impl DsView {
    fn start(bin: &str, socket: &Path) -> Result<DsView> {
        let headless = Path::new(bin)
            .file_name()
            .is_some_and(|n| n == "dsview-cli");
        let mut args: Vec<String> = if headless {
            vec!["serve".into(), "--mcp".into()]
        } else {
            vec!["--mcp".into()]
        };
        if let Ok(extra) = std::env::var("DSVIEW_ARGS") {
            args.extend(extra.split_whitespace().map(str::to_owned));
        }
        let child = Command::new(bin)
            .args(&args)
            .env("DSVIEW_MCP_SOCKET", socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("starting {bin}"))?;
        let t0 = std::time::Instant::now();
        while !socket.exists() {
            if t0.elapsed() > std::time::Duration::from_secs(60) {
                bail!("{bin} did not open {}", socket.display());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        Ok(DsView(child))
    }
}

impl Drop for DsView {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    id: u64,
}

impl Mcp {
    fn start(root: &Path, dir: &Path, socket: &Path) -> Result<Mcp> {
        let exe = root.join("target/debug/dsview-mcp");
        let mut child = Command::new(&exe)
            .env("DSVIEW_MCP_NO_LAUNCH", "1")
            .env("DSVIEW_MCP_ALLOWED_DIRS", dir)
            .env("DSVIEW_MCP_SOCKET", socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| {
                format!(
                    "starting {} (cargo build -p dsview-mcp first)",
                    exe.display()
                )
            })?;
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            stdin,
            out,
            id: 0,
        };
        m.request(
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {},
                   "clientInfo": {"name": "xtask", "version": "0"}}),
        )?;
        writeln!(
            m.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )?;
        Ok(m)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        let req = json!({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params});
        writeln!(self.stdin, "{req}")?;
        self.stdin.flush()?;
        loop {
            let mut line = String::new();
            if self.out.read_line(&mut line)? == 0 {
                bail!("dsview-mcp closed its output");
            }
            let v: Value = serde_json::from_str(line.trim())?;
            if v.get("id") == Some(&json!(self.id)) {
                if let Some(e) = v.get("error") {
                    bail!("{method}: {e}");
                }
                return Ok(v["result"].clone());
            }
        }
    }

    /// A tool call: (is_error, the text content parsed as JSON if it is).
    fn call(&mut self, name: &str, args: Value) -> Result<(bool, Value)> {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}))?;
        let is_error = r["isError"].as_bool().unwrap_or(false);
        let text = r["content"][0]["text"].as_str().unwrap_or("").to_owned();
        let body = serde_json::from_str(&text).unwrap_or(Value::String(text));
        Ok((is_error, body))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The scenario: (tool, arguments, an error is expected).
fn scenario(dir: &Path) -> Vec<(&'static str, Value, bool)> {
    let p = |f: &str| dir.join(f).to_string_lossy().into_owned();
    vec![
        ("list_devices", json!({}), false),
        ("select_device", json!({"index": "$Demo Logic"}), false),
        ("set_mode", json!({"mode": "logic"}), false),
        ("get_config", json!({}), false),
        (
            "set_sampling",
            json!({"samplerate": 25_000_000, "samples": 131072}),
            false,
        ),
        (
            "set_channels",
            json!({"channels": [{"index": 0, "name": "SCL"}]}),
            false,
        ),
        ("set_trigger", json!({"type": "none"}), false),
        // the demo has no threshold
        ("set_threshold", json!({"volts": 1.5}), true),
        (
            "start_capture",
            json!({"wait": true, "timeout_s": 60}),
            false,
        ),
        ("status", json!({}), false),
        ("capture_info", json!({}), false),
        (
            "get_edges",
            json!({"channel": 0, "start": 0, "end": 20000, "limit": 20}),
            false,
        ),
        (
            "read_logic",
            json!({"channels": [0, 1], "start": 0, "end": 2000}),
            false,
        ),
        ("measure_logic", json!({"channel": 0}), false),
        ("list_decoders", json!({"filter": "i2c"}), false),
        ("describe_decoder", json!({"id": "0:i2c"}), false),
        ("list_active_decoders", json!({}), false),
        (
            "add_decoder",
            json!({"id": "1:uart", "channels": {"rxtx": 2}, "options": {"baudrate": 115200}, "run": false}),
            false,
        ),
        ("run_decoders", json!({"timeout_s": 120}), false),
        ("get_annotations", json!({"decoder": 0, "limit": 10}), false),
        ("remove_decoder", json!({"index": 4}), false),
        ("show_region", json!({"start": 40000, "end": 60000}), false),
        ("set_cursors", json!({"samples": [45147, 50223]}), false),
        ("save_session", json!({"path": p("suite.dsl")}), false),
        ("export_data", json!({"path": p("suite.csv")}), false),
        ("open_file", json!({"path": p("suite.dsl")}), false),
        ("capture_info", json!({}), false),
        // outside the allowed directories
        ("save_session", json!({"path": "/etc/suite.dsl"}), true),
        ("list_devices", json!({}), false),
        ("select_device", json!({"index": "$Demo Logic"}), false),
        ("set_mode", json!({"mode": "dso"}), false),
        (
            "set_dso",
            json!({"timebase_ns": 1000, "channel": 0, "vdiv_mv": 500, "coupling": "dc"}),
            false,
        ),
        (
            "start_capture",
            json!({"wait": true, "timeout_s": 60, "instant": true}),
            false,
        ),
        (
            "read_waveform",
            json!({"channel": 0, "max_points": 50}),
            false,
        ),
        ("measure_dso", json!({"channel": 0}), false),
        ("stop_capture", json!({}), false),
        ("set_mode", json!({"mode": "logic"}), false),
    ]
}

pub fn suite(root: &Path, bin: &str, out: Option<&Path>) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("dsview-mcp-suite-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let socket = dir.join("mcp.sock");
    let dsview = DsView::start(bin, &socket)?;
    let mut m = Mcp::start(root, &dir, &socket)?;
    let tools = m.request("tools/list", json!({}))?;
    let names: Vec<String> = tools["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let steps = scenario(&dir);
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut devices = Value::Null;
    for (i, (tool, args, expect_err)) in steps.iter().enumerate() {
        // "$name": the index of that device in the last list_devices
        let mut args = args.clone();
        if let Some(name) = args["index"].as_str().and_then(|n| n.strip_prefix('$')) {
            let idx = devices["devices"]
                .as_array()
                .and_then(|a| a.iter().find(|d| d["name"] == name))
                .map_or(-1, |d| d["index"].as_i64().unwrap_or(-1));
            args["index"] = json!(idx);
        }
        let (is_error, body) = m.call(tool, args.clone())?;
        if *tool == "list_devices" {
            devices = body.clone();
        }
        let ok = is_error == *expect_err;
        println!(
            "{} {i:2} {tool}{}",
            if ok { "ok  " } else { "FAIL" },
            if is_error { " (error)" } else { "" }
        );
        if !ok {
            failures.push(format!("step {i} {tool}: {body}"));
        }
        results.push(json!({"tool": tool, "args": args, "error": is_error, "result": body}));
    }
    let covered: std::collections::BTreeSet<&str> = steps.iter().map(|s| s.0).collect();
    let missing: Vec<&String> = names
        .iter()
        .filter(|n| !covered.contains(n.as_str()))
        .collect();
    drop(m);
    drop(dsview);
    if let Some(out) = out {
        let doc = json!({"bin": bin, "tools": names, "steps": results});
        std::fs::write(out, serde_json::to_string_pretty(&doc)? + "\n")?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    if !missing.is_empty() {
        failures.push(format!("tools not in the scenario: {missing:?}"));
    }
    if !failures.is_empty() {
        bail!("{} problem(s):\n{}", failures.len(), failures.join("\n"));
    }
    println!(
        "mcp suite ok: {} tools, {} steps against {bin}",
        names.len(),
        steps.len()
    );
    Ok(())
}

/// Drops what differs between runs: paths, times, versions.
fn normalise(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for k in [
                "path",
                "file",
                "version",
                "elapsed_s",
                "duration_s",
                "session_time",
                "trigger_time",
                "note",
            ] {
                o.remove(k);
            }
            for x in o.values_mut() {
                normalise(x);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(normalise),
        Value::String(s) if s.contains("/tmp/") => *s = "<path>".into(),
        _ => {}
    }
}

pub fn compare(a: &Path, b: &Path) -> Result<()> {
    let read =
        |p: &Path| -> Result<Value> { Ok(serde_json::from_str(&std::fs::read_to_string(p)?)?) };
    let (mut x, mut y) = (read(a)?, read(b)?);
    normalise(&mut x);
    normalise(&mut y);
    let (sx, sy) = (
        x["steps"].as_array().cloned().unwrap_or_default(),
        y["steps"].as_array().cloned().unwrap_or_default(),
    );
    let mut diffs = 0;
    for (i, (p, q)) in sx.iter().zip(&sy).enumerate() {
        if p != q {
            diffs += 1;
            println!(
                "step {i} {}:\n  {}\n  {}",
                p["tool"], p["result"], q["result"]
            );
        }
    }
    if sx.len() != sy.len() {
        bail!("{} vs {} steps", sx.len(), sy.len());
    }
    if diffs > 0 {
        bail!("{diffs} step(s) differ");
    }
    println!("same results in all {} steps", sx.len());
    Ok(())
}
