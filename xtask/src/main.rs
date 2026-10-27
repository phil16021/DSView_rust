//! Developer tasks for the Rust port.
//!
//!   cargo xtask corpus init      write testdata/corpus.json from assets/demo
//!   cargo xtask corpus verify    check every corpus file against its hash
//!   cargo xtask golden check     run the Rust reader and decoder engine and diff against
//!                                the stored goldens (written by the C oracle up to tag
//!                                `c-final`)
//!   cargo xtask golden bless     write the goldens from the Rust implementation (the
//!                                reader summary and rust-decode), for intentional
//!                                changes once they are in DEVIATIONS.md
//!   cargo xtask control record   run the demo scenario against `--mcp`, store the transcript
//!   cargo xtask control check    run it again and compare with the stored transcript
//!   cargo xtask control dump OUT  run it and write the transcript to OUT
//!   cargo xtask usb record [--iface usbmonN] [--out DIR] [--dry-run] [--only NAME]
//!                                record DSLogic USB traffic per scenario (tools/usb/README.md)
//!   cargo xtask decoders soak [--runs N] [--seed S] [--jobs J] [--stack-timeout SECS]
//!                             [--max-mem MB] [--keep DIR] [--allow ID,..]
//!                                every decoder on random captures under a time and
//!                                memory limit (`decoder-soak`); fails on a hang

mod mcp;
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
        ["golden", "check"] => golden_run(&root, Mode::Check),
        ["golden", "bless"] => golden_run(&root, Mode::Bless),
        ["control", "record"] => control_run(&root, Mode::Generate),
        ["control", "check"] => control_run(&root, Mode::Check),
        ["control", "dump", out] => control_dump(Path::new(out)),
        ["mcp", "suite", bin] => mcp::suite(&root, bin, None),
        ["mcp", "suite", bin, out] => mcp::suite(&root, bin, Some(Path::new(out))),
        ["mcp", "compare", a, b] => mcp::compare(Path::new(a), Path::new(b)),
        ["usb", "record", rest @ ..] => usb::record(&root, rest),
        ["decoders", "soak", rest @ ..] => decoders_soak(&root, rest),
        _ => {
            eprintln!(
                "usage: cargo xtask <corpus init|corpus verify|golden check|golden bless|control record|control check|control dump OUT|mcp suite BIN [OUT]|mcp compare A B|usb record|decoders soak>"
            );
            std::process::exit(2);
        }
    }
}

fn corpus_init(root: &Path) -> Result<()> {
    let mut files = Vec::new();
    for mode in ["logic", "dso", "analog"] {
        let dir = root.join("assets/demo").join(mode);
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
    /// Write the goldens from the Rust side.
    Bless,
}

fn testkit_bin(root: &Path, name: &str) -> Result<PathBuf> {
    let status = Command::new(env!("CARGO"))
        .args(["build", "--release", "-p", "dsview-testkit", "--bin", name])
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
            "{} {:?} failed ({}):\n{}",
            bin.display(),
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn golden_run(root: &Path, mode: Mode) -> Result<()> {
    let corpus = Corpus::load(root)?;
    corpus.verify(root)?;
    let rust_decode = testkit_bin(root, "rust-decode")?;

    let mut failures = 0;
    for entry in &corpus.files {
        let name = Corpus::golden_name(entry);
        let input = root.join(&entry.path);
        let input = input.to_str().context("non-UTF-8 path")?;

        // the C loader's summary (oracle-dsl), from the Rust reader
        let summary = dsview_testkit::dsl_summary::summary(&root.join(&entry.path));
        let mut outputs = vec![(
            root.join(format!("testdata/golden/dsl/{name}.json")),
            format!("{}\n", serde_json::to_string_pretty(&summary)?),
        )];
        if entry.decode {
            outputs.push((
                root.join(format!("testdata/golden/decode/{name}.jsonl.gz")),
                run(&rust_decode, &[input])?,
            ));
        }

        // Rust writes, Rust reads: a capture re-written by dsview-formats
        // reads back with the same samples as the original
        if mode == Mode::Check && entry.mode == "logic" {
            let tmp =
                std::env::temp_dir().join(format!("dsview-rewrite-{}.dsl", std::process::id()));
            rewrite_logic(&root.join(&entry.path), &tmp)?;
            let mut rewritten = dsview_testkit::dsl_summary::summary(&tmp);
            let _ = std::fs::remove_file(&tmp);
            let mut original = summary.clone();
            original.as_object_mut().unwrap().remove("packets");
            rewritten.as_object_mut().unwrap().remove("packets");
            if original == rewritten {
                println!("ok   rust-written {} reads back identically", entry.path);
            } else {
                failures += 1;
                println!(
                    "FAIL rust-written {}: reads back different data",
                    entry.path
                );
            }
        }

        for (path, text) in outputs {
            let rel = path.strip_prefix(root)?.display().to_string();
            let gz = rel.ends_with(".gz");
            match mode {
                Mode::Generate | Mode::Bless => {
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
                    .with_context(|| format!("missing golden {rel}"))?;
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
    if mode == Mode::Check {
        failures += synthetic_logic_check()?;
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

/// Multi-block logic captures written by the Rust writer must read back with
/// the bits written (until tag `c-final` the C loader read them too).
fn synthetic_logic_check() -> Result<usize> {
    use dsview_formats::dsl::{DslReader, LOGIC_BLOCK_SAMPLES, LogicWriter, WriteChannel};

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

        let mut reader = DslReader::open(&path)?;
        let read: Vec<Vec<u8>> = (0..idx.len())
            .map(|i| reader.logic_channel(i))
            .collect::<Result<_, _>>()?;
        let _ = std::fs::remove_file(&path);
        if read == bits {
            println!("ok   synthetic {name}: the Rust reader returns the bits written");
        } else {
            failures += 1;
            println!("FAIL synthetic {name}: the Rust reader returns other bits");
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
        // the transcript is recorded against Qt's GUI either way
        Mode::Generate | Mode::Bless => {
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

/// `decoders soak`: builds `rust-decode` and `decoder-soak` (release) and runs
/// the soak with `args`.
fn decoders_soak(root: &Path, args: &[&str]) -> Result<()> {
    let st = Command::new(env!("CARGO"))
        .args(["build", "--release", "-p", "dsview-testkit", "--bins"])
        .current_dir(root)
        .status()?;
    if !st.success() {
        bail!("building the decoder soak failed");
    }
    let st = Command::new(root.join("target/release/decoder-soak"))
        .args(args)
        .current_dir(root)
        .status()
        .context("running decoder-soak")?;
    if !st.success() {
        bail!("decoder soak failed");
    }
    Ok(())
}
