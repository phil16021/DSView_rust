//! Developer tasks for the Rust port.
//!
//!   cargo xtask corpus init      write testdata/corpus.json from DSView/demo
//!   cargo xtask corpus verify    check every corpus file against its hash
//!   cargo xtask golden gen       (re)generate goldens with the C oracle
//!   cargo xtask golden check     regenerate in memory and diff against the stored goldens
//!   cargo xtask control record   run the demo scenario against `DSView --mcp`, store the transcript
//!   cargo xtask control check    run it again and compare with the stored transcript
//!   cargo xtask usb record [--iface usbmonN] [--out DIR] [--dry-run]
//!                                record DSLogic USB traffic per scenario (tools/usb/README.md)

mod usb;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use dsview_testkit::corpus::{Corpus, Entry, sha256_file};
use dsview_testkit::{golden, repo_root};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["corpus", "init"] => corpus_init(&root),
        ["corpus", "verify"] => {
            Corpus::load(&root)?.verify(&root)?;
            println!("corpus ok");
            Ok(())
        }
        ["golden", "gen"] => golden_run(&root, Mode::Generate),
        ["golden", "check"] => golden_run(&root, Mode::Check),
        ["control", "record"] => control_run(&root, Mode::Generate),
        ["control", "check"] => control_run(&root, Mode::Check),
        ["usb", "record", rest @ ..] => usb::record(&root, rest),
        _ => {
            eprintln!(
                "usage: cargo xtask <corpus init|corpus verify|golden gen|golden check|control record|control check|usb record>"
            );
            std::process::exit(2);
        }
    }
}

fn corpus_init(root: &Path) -> Result<()> {
    let mut files = Vec::new();
    for mode in ["logic", "dso", "analog"] {
        let dir = root.join("DSView/demo").join(mode);
        let mut names: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        names.sort();
        for p in names
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "demo"))
        {
            let rel = p.strip_prefix(root)?.to_string_lossy().into_owned();
            files.push(Entry {
                sha256: sha256_file(&p)?,
                path: rel,
                mode: mode.into(),
                decode: mode == "logic",
            });
        }
    }
    let corpus = Corpus { files };
    corpus.save(root)?;
    println!(
        "wrote {} entries to testdata/corpus.json",
        corpus.files.len()
    );
    Ok(())
}

#[derive(PartialEq)]
enum Mode {
    Generate,
    Check,
}

fn oracle_bin(root: &Path, name: &str) -> Result<PathBuf> {
    let status = Command::new(env!("CARGO"))
        .args(["build", "--release", "-p", "dsview-oracle", "--bin", name])
        .current_dir(root)
        .status()?;
    if !status.success() {
        bail!("building {name} failed");
    }
    Ok(root.join("target/release").join(name))
}

fn run(bin: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("run {}", bin.display()))?;
    if !out.status.success() {
        bail!(
            "{} {:?} failed:\n{}",
            bin.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn golden_run(root: &Path, mode: Mode) -> Result<()> {
    let corpus = Corpus::load(root)?;
    corpus.verify(root)?;
    let dsl = oracle_bin(root, "oracle-dsl")?;
    let decode = oracle_bin(root, "oracle-decode")?;

    let mut failures = 0;
    for entry in &corpus.files {
        let name = Corpus::golden_name(entry);
        let input = root.join(&entry.path);
        let input = input.to_str().context("non-UTF-8 path")?;

        let mut outputs = vec![(
            root.join(format!("testdata/golden/dsl/{name}.json")),
            run(&dsl, &[input])?,
        )];
        if entry.decode {
            outputs.push((
                root.join(format!("testdata/golden/decode/{name}.jsonl.gz")),
                run(&decode, &[input])?,
            ));
        }

        for (path, text) in outputs {
            let rel = path.strip_prefix(root)?.display().to_string();
            let gz = rel.ends_with(".gz");
            match mode {
                Mode::Generate => {
                    if gz {
                        golden::write_gz(&path, &text)?;
                    } else {
                        std::fs::create_dir_all(path.parent().unwrap())?;
                        std::fs::write(&path, &text)?;
                    }
                    println!("wrote {rel} ({} lines)", text.lines().count());
                }
                Mode::Check => {
                    let stored = if gz {
                        golden::read_gz(&path)
                    } else {
                        Ok(std::fs::read_to_string(&path)?)
                    }
                    .with_context(|| {
                        format!("missing golden {rel}; run `cargo xtask golden gen`")
                    })?;
                    match golden::first_divergence(&stored, &text) {
                        None => println!("ok   {rel}"),
                        Some(d) => {
                            failures += 1;
                            println!(
                                "FAIL {rel}: {} differing lines\n{d}",
                                golden::count_differences(&stored, &text)
                            );
                        }
                    }
                }
            }
        }
    }
    if failures > 0 {
        bail!("{failures} golden file(s) differ");
    }
    Ok(())
}

/// Control-API conformance: the demo scenario's transcript against a running
/// `DSView --mcp` (socket from DSVIEW_MCP_SOCKET or the default path).
fn control_run(root: &Path, mode: Mode) -> Result<()> {
    use dsview_testkit::control::{
        Client, Recorded, compare, default_socket_path, demo_scenario, run_scenario,
    };

    let out_dir = std::env::temp_dir().join(format!("dsview-control-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir)?;
    let mut client = Client::new(default_socket_path());
    let transcript = run_scenario(&mut client, &demo_scenario(), &out_dir);
    let _ = std::fs::remove_dir_all(&out_dir);
    let transcript = transcript?;

    let path = root.join("testdata/control/demo-scenario.json");
    match mode {
        Mode::Generate => {
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(&path, serde_json::to_string_pretty(&transcript)? + "\n")?;
            println!(
                "wrote {} ({} steps)",
                path.strip_prefix(root)?.display(),
                transcript.len()
            );
        }
        Mode::Check => {
            let stored: Vec<Recorded> = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
            let problems = compare(&stored, &transcript);
            if !problems.is_empty() {
                bail!(
                    "{} step(s) differ:\n{}",
                    problems.len(),
                    problems.join("\n")
                );
            }
            println!("control transcript ok ({} steps)", transcript.len());
        }
    }
    Ok(())
}
