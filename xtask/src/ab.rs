//! `cargo xtask control ab --bin <DSView>`: runs the same probes against
//! DSView once per implementation (DSVIEW_IMPL=c, then rust) and requires
//! identical answers. The binary must be built with -DDSVIEW_RUST_CORE=ON and
//! sit in an install layout (<prefix>/bin/DSView next to <prefix>/share/DSView).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use dsview_testkit::control::Client;
use dsview_testkit::corpus::Corpus;
use serde_json::{Value, json};

struct Instance {
    child: Child,
    socket: PathBuf,
    home: PathBuf,
}

impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Starts DSView headless with an isolated profile (first-run dialogs off).
fn launch(bin: &Path, iml: &str) -> Result<Instance> {
    let tag = format!("{}-{iml}", std::process::id());
    let home = std::env::temp_dir().join(format!("dsview-ab-home-{tag}"));
    let conf = home.join(".config/DreamSourceLab");
    std::fs::create_dir_all(&conf)?;
    std::fs::write(
        conf.join("DSView.conf"),
        "[UserHistory]\nshowDocuments=false\nshowDriverHint=false\n",
    )?;
    // Short path: sockaddr_un holds 107 bytes.
    let socket = PathBuf::from(format!("/tmp/dsview-ab-{tag}.sock"));
    let _ = std::fs::remove_file(&socket);

    let child = Command::new(bin)
        .arg("--mcp")
        .env("DSVIEW_IMPL", iml)
        .env("DSVIEW_MCP_SOCKET", &socket)
        .env("QT_QPA_PLATFORM", "offscreen")
        .env_remove("QT_QPA_PLATFORMTHEME")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", bin.display()))?;
    let inst = Instance {
        child,
        socket,
        home,
    };

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut client = Client::new(&inst.socket);
    while Instant::now() < deadline {
        if client
            .call("version", &json!({}), Duration::from_secs(2))
            .is_ok()
        {
            return Ok(inst);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!("DSView ({iml}) did not open its control socket");
}

fn call(c: &mut Client, method: &str, params: Value) -> Value {
    match c.call(method, &params, Duration::from_secs(120)) {
        Ok(Ok(v)) => json!({"ok": v}),
        Ok(Err(e)) => json!({"err": e}),
        Err(e) => json!({"transport": e.to_string()}),
    }
}

/// What DSView reports after opening each corpus file.
fn probe(socket: &Path, root: &Path, corpus: &Corpus) -> Value {
    let mut c = Client::new(socket);
    let mut out = serde_json::Map::new();
    for e in &corpus.files {
        let path = root.join(&e.path);
        let mut r = serde_json::Map::new();
        r.insert(
            "open_file".into(),
            call(&mut c, "open_file", json!({"path": path})),
        );
        r.insert(
            "capture_info".into(),
            call(&mut c, "capture_info", json!({})),
        );
        match e.mode.as_str() {
            "logic" => {
                r.insert(
                    "read_logic".into(),
                    call(&mut c, "read_logic", json!({"max_transitions": 200})),
                );
                r.insert(
                    "get_edges".into(),
                    call(&mut c, "get_edges", json!({"channel": 0, "limit": 50})),
                );
                r.insert(
                    "list_active_decoders".into(),
                    call(&mut c, "list_active_decoders", json!({})),
                );
                r.insert(
                    "annotations".into(),
                    call(
                        &mut c,
                        "get_annotations",
                        json!({"decoder": 0, "limit": 40}),
                    ),
                );
            }
            "dso" => {
                for ch in [0, 1] {
                    r.insert(
                        format!("read_waveform_{ch}"),
                        call(
                            &mut c,
                            "read_waveform",
                            json!({"channel": ch, "max_points": 64}),
                        ),
                    );
                }
            }
            _ => {}
        }
        out.insert(e.path.clone(), Value::Object(r));
    }
    Value::Object(out)
}

pub fn run(root: &Path, args: &[&str]) -> Result<()> {
    let bin = match args {
        ["--bin", b] => PathBuf::from(b),
        _ => bail!(
            "usage: cargo xtask control ab --bin <path/to/DSView built with -DDSVIEW_RUST_CORE=ON>"
        ),
    };
    let corpus = Corpus::load(root)?;
    corpus.verify(root)?;

    let mut results = Vec::new();
    for imp in ["c", "rust"] {
        let inst = launch(&bin, imp)?;
        let r = probe(&inst.socket, root, &corpus);
        drop(inst);
        results.push(r);
    }

    let mut diffs = 0;
    for e in &corpus.files {
        let (a, b) = (&results[0][&e.path], &results[1][&e.path]);
        // Both sides failing the same way is not a pass.
        let errors = a.to_string().contains("\"transport\"")
            || b.to_string().contains("\"transport\"")
            || a["open_file"].get("ok").is_none()
            || b["open_file"].get("ok").is_none();
        if a == b && !errors {
            println!("ok   {} (C and Rust reader identical in DSView)", e.path);
        } else {
            diffs += 1;
            println!(
                "FAIL {}\n  c:    {}\n  rust: {}",
                e.path,
                truncate(a),
                truncate(b)
            );
        }
    }
    if diffs > 0 {
        bail!("{diffs} capture(s) differ between DSVIEW_IMPL=c and rust");
    }
    Ok(())
}

fn truncate(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 600 {
        format!("{}…", s.chars().take(600).collect::<String>())
    } else {
        s
    }
}
