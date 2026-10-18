//! Developer tasks for the Rust port.
//!
//!   cargo xtask corpus init      write testdata/corpus.json from DSView/demo
//!   cargo xtask corpus verify    check every corpus file against its hash
//!   cargo xtask golden gen       (re)generate goldens with the C oracle
//!   cargo xtask golden check     regenerate in memory and diff against the stored goldens
//!   cargo xtask control record   run the demo scenario against `DSView --mcp`, store the transcript
//!   cargo xtask control check    run it again and compare with the stored transcript
//!   cargo xtask control dump OUT  run it and write the transcript to OUT (A/B of two DSViews)
//!   cargo xtask control ab --bin <DSView>   same probes with DSVIEW_IMPL=c and rust; must match
//!   cargo xtask gen-header [--check]   regenerate crates/dsview-ffi/include/dsview_ffi.h (cbindgen)
//!   cargo xtask usb record [--iface usbmonN] [--out DIR] [--dry-run] [--only NAME]
//!                                record DSLogic USB traffic per scenario (tools/usb/README.md)

mod ab;
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
        ["control", "dump", out] => control_dump(Path::new(out)),
        ["control", "ab", rest @ ..] => ab::run(&root, rest),
        ["usb", "record", rest @ ..] => usb::record(&root, rest),
        ["gen-header"] => gen_header(&root, false),
        ["gen-header", "--check"] => gen_header(&root, true),
        _ => {
            eprintln!(
                "usage: cargo xtask <corpus init|corpus verify|golden gen|golden check|control record|control check|control ab|gen-header [--check]|usb record>"
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
    let rust_decode = oracle_bin(root, "rust-decode")?;

    let mut failures = 0;
    for entry in &corpus.files {
        let name = Corpus::golden_name(entry);
        let input = root.join(&entry.path);
        let input = input.to_str().context("non-UTF-8 path")?;

        let mut outputs = vec![(
            root.join(format!("testdata/golden/dsl/{name}.json")),
            run(&dsl, &[input])?,
            "",
        )];
        if entry.decode {
            let golden = root.join(format!("testdata/golden/decode/{name}.jsonl.gz"));
            outputs.push((golden.clone(), run(&decode, &[input])?, ""));
            // the Rust decoder engine must reproduce the same golden
            if mode == Mode::Check {
                outputs.push((
                    golden,
                    run(&rust_decode, &[input])?,
                    " (Rust decoder engine)",
                ));
            }
        }

        // Rust writes, C reads: the C loader must see the same samples in a
        // capture re-written by dsview-formats.
        if mode == Mode::Check && entry.mode == "logic" {
            let tmp =
                std::env::temp_dir().join(format!("dsview-rewrite-{}.dsl", std::process::id()));
            rewrite_logic(&root.join(&entry.path), &tmp)?;
            let rewritten = run(&dsl, &[tmp.to_str().context("temp path")?]);
            let _ = std::fs::remove_file(&tmp);
            let golden: serde_json::Value = serde_json::from_str(&outputs[0].1)?;
            let mut rewritten: serde_json::Value = serde_json::from_str(&rewritten?)?;
            let mut golden = golden;
            golden.as_object_mut().unwrap().remove("packets");
            rewritten.as_object_mut().unwrap().remove("packets");
            if golden == rewritten {
                println!(
                    "ok   rust-written {} reads back identically in C",
                    entry.path
                );
            } else {
                failures += 1;
                println!("FAIL rust-written {}: C reads different data", entry.path);
            }
        }

        for (path, text, label) in outputs {
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
                        None => println!("ok   {rel}{label}"),
                        Some(d) => {
                            failures += 1;
                            println!(
                                "FAIL {rel}{label}: {} differing lines\n{d}",
                                golden::count_differences(&stored, &text)
                            );
                        }
                    }
                }
            }
        }
    }
    if mode == Mode::Check {
        failures += synthetic_logic_check(&dsl)?;
    }
    if failures > 0 {
        bail!("{failures} golden file(s) differ");
    }
    Ok(())
}

/// Deterministic logic data: runs of random length (xorshift64*), per channel.
fn synthetic_bits(samples: u64, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    let mut next = move || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let words = samples.div_ceil(64) as usize;
    let mut out = vec![0u8; words * 8];
    let (mut level, mut i) = (false, 0u64);
    while i < samples {
        let run = 1 + next() % 5000;
        if level {
            for s in i..(i + run).min(samples) {
                out[(s / 8) as usize] |= 1 << (s % 8);
            }
        }
        level = !level;
        i += run;
    }
    out
}

/// Multi-block/multi-packet logic captures written by the Rust writer must
/// read identically (data and packet boundaries) through C and Rust.
fn synthetic_logic_check(oracle_dsl: &Path) -> Result<usize> {
    use dsview_formats::dsl::{LOGIC_BLOCK_SAMPLES, LogicWriter, WriteChannel};

    let cases: [(&str, &[u16], u64); 3] = [
        (
            "3ch-3blocks-odd",
            &[0, 3, 7],
            2 * LOGIC_BLOCK_SAMPLES + 4096 + 37,
        ),
        (
            "16ch-1M",
            &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
            1_000_000,
        ),
        ("1ch-exact-block", &[5], LOGIC_BLOCK_SAMPLES),
    ];
    let mut failures = 0;
    for (name, idx, samples) in cases {
        let path =
            std::env::temp_dir().join(format!("dsview-synth-{name}-{}.dsl", std::process::id()));
        let bits: Vec<Vec<u8>> = idx
            .iter()
            .map(|&i| synthetic_bits(samples, 0x9e37_79b9 ^ u64::from(i)))
            .collect();
        let writer = LogicWriter {
            driver: "virtual-demo".into(),
            samplerate: 100_000_000,
            total_samples: samples,
            trigger_time_ms: 0,
            trigger_pos: 0,
            channels: idx
                .iter()
                .map(|&i| WriteChannel {
                    index: i,
                    name: format!("ch{i}"),
                })
                .collect(),
            decoders_json: "[]".into(),
            session_json: "{}".into(),
        };
        let refs: Vec<&[u8]> = bits.iter().map(Vec::as_slice).collect();
        writer.write(std::fs::File::create(&path)?, &refs)?;

        let c: serde_json::Value =
            serde_json::from_str(&run(oracle_dsl, &[path.to_str().context("temp path")?])?)?;
        let rust = dsview_testkit::dsl_summary::summary(&path);
        let _ = std::fs::remove_file(&path);
        if c == rust {
            println!("ok   synthetic {name}: C and Rust read identical data and packets");
        } else {
            failures += 1;
            println!("FAIL synthetic {name}\n  C:    {c}\n  Rust: {rust}");
        }
    }
    Ok(failures)
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

fn control_dump(out: &Path) -> Result<()> {
    use dsview_testkit::control::{Client, default_socket_path, demo_scenario, run_scenario};

    let dir = std::env::temp_dir().join(format!("dsview-control-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let mut client = Client::new(default_socket_path());
    let transcript = run_scenario(&mut client, &demo_scenario(), &dir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::write(out, serde_json::to_string_pretty(&transcript?)? + "\n")?;
    Ok(())
}

/// Re-writes a logic capture with the Rust writer (header, decoders, session, data).
fn rewrite_logic(src: &Path, dst: &Path) -> Result<()> {
    use dsview_formats::dsl::{DslReader, LogicWriter, WriteChannel};

    let mut r = DslReader::open(src)?;
    let h = r.header().clone();
    let bits: Vec<Vec<u8>> = (0..h.channels.len())
        .map(|i| r.logic_channel(i))
        .collect::<Result<_, _>>()?;
    let writer = LogicWriter {
        driver: h.driver.clone().unwrap_or_default(),
        samplerate: h.samplerate,
        total_samples: h.total_samples,
        trigger_time_ms: h.trigger_time.unwrap_or(0),
        trigger_pos: h.trigger_pos,
        channels: h
            .channels
            .iter()
            .map(|c| WriteChannel {
                index: c.index,
                name: c.name.clone().unwrap_or_default(),
            })
            .collect(),
        decoders_json: r
            .decoders_json()?
            .map(|v| v.to_string())
            .unwrap_or_else(|| "[]".into()),
        session_json: r
            .session_json()?
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".into()),
    };
    let refs: Vec<&[u8]> = bits.iter().map(Vec::as_slice).collect();
    writer.write(std::fs::File::create(dst)?, &refs)?;
    Ok(())
}

/// The C header of dsview-ffi is generated; `--check` fails if it is stale.
fn gen_header(root: &Path, check: bool) -> Result<()> {
    let header = root.join("crates/dsview-ffi/include/dsview_ffi.h");
    let tmp = std::env::temp_dir().join(format!("dsview_ffi-{}.h", std::process::id()));
    let status = Command::new("cbindgen")
        .args([
            "--quiet",
            "--config",
            "crates/dsview-ffi/cbindgen.toml",
            "--crate",
            "dsview-ffi",
            "--output",
        ])
        .arg(&tmp)
        .arg("crates/dsview-ffi")
        .current_dir(root)
        .status()
        .context("running cbindgen (cargo install --locked cbindgen)")?;
    if !status.success() {
        bail!("cbindgen failed");
    }
    let generated = std::fs::read_to_string(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    if check {
        if std::fs::read_to_string(&header)? != generated {
            bail!("crates/dsview-ffi/include/dsview_ffi.h is stale; run `cargo xtask gen-header`");
        }
        println!("dsview_ffi.h is up to date");
    } else {
        std::fs::write(&header, generated)?;
        println!("wrote crates/dsview-ffi/include/dsview_ffi.h");
    }
    Ok(())
}
