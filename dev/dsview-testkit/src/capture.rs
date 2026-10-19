//! Minimal reader for v3 `.dsl`/`.demo` logic captures, enough to feed the
//! decode oracle. The production reader is `dsview-formats` (Phase 1); the
//! `oracle-dsl` binary checks the C loader independently.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

pub struct Capture {
    pub samplerate: u64,
    pub total_samples: u64,
    /// Bit-packed, LSB-first samples per probe index; zero-padded to whole u64 words.
    pub signals: Vec<Option<Vec<u8>>>,
    /// Decoder stacks as saved by DSView (`session.decoder`, else the `decoders` entry).
    pub decoders: Vec<Value>,
}

/// Parses "25 MHz", "1 GHz", "500 kHz", "100 Hz".
pub fn parse_samplerate(s: &str) -> Result<u64> {
    let s = s.trim();
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let value: f64 = s[..num_end]
        .parse()
        .with_context(|| format!("samplerate {s:?}"))?;
    let unit = s[num_end..].trim().to_ascii_lowercase();
    let factor = match unit.trim_end_matches("hz").trim() {
        "" => 1.0,
        "k" => 1e3,
        "m" => 1e6,
        "g" => 1e9,
        other => bail!("unknown samplerate unit {other:?}"),
    };
    Ok((value * factor).round() as u64)
}

fn ini_header(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut in_header = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_header = line == "[header]";
            continue;
        }
        if let (true, Some((k, v))) = (in_header, line.split_once('=')) {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

fn read_entry(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Result<Option<Vec<u8>>> {
    match zip.by_name(name) {
        Ok(mut f) => {
            let mut buf = Vec::new();
            f.read_to_end(&mut buf)?;
            Ok(Some(buf))
        }
        Err(zip::result::ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn load(path: &Path) -> Result<Capture> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file)?;

    let header = read_entry(&mut zip, "header")?.context("missing header entry")?;
    let header = ini_header(&String::from_utf8_lossy(&header));
    if header.get("device mode").map(String::as_str) != Some("0") {
        bail!("only logic captures (device mode 0) can be decoded");
    }
    let samplerate = parse_samplerate(header.get("samplerate").context("no samplerate")?)?;
    let total_samples: u64 = header
        .get("total samples")
        .context("no total samples")?
        .parse()?;

    // L-<probe>/<block>: 2^24-sample blocks per probe, in block order.
    let mut blocks: BTreeMap<usize, BTreeMap<usize, Vec<u8>>> = BTreeMap::new();
    let names: Vec<String> = zip
        .file_names()
        .map(|n| n.map(|n| n.into_owned()))
        .collect::<Result<_, _>>()?;
    for name in names {
        let Some(rest) = name.strip_prefix("L-") else {
            continue;
        };
        let Some((probe, block)) = rest.split_once('/') else {
            continue;
        };
        let (Ok(probe), Ok(block)) = (probe.parse::<usize>(), block.parse::<usize>()) else {
            continue;
        };
        if probe >= 64 {
            bail!("probe index {probe} out of range");
        }
        let data = read_entry(&mut zip, &name)?.unwrap_or_default();
        blocks.entry(probe).or_default().insert(block, data);
    }

    let words = total_samples.div_ceil(64) as usize;
    let mut signals: Vec<Option<Vec<u8>>> = Vec::new();
    for (probe, parts) in blocks {
        let mut bits: Vec<u8> = parts.into_values().flatten().collect();
        bits.resize(words * 8, 0);
        if signals.len() <= probe {
            signals.resize(probe + 1, None);
        }
        signals[probe] = Some(bits);
    }

    let mut decoders = Vec::new();
    if let Some(session) = read_entry(&mut zip, "session")? {
        let session: Value = serde_json::from_slice(&session).context("session JSON")?;
        if let Some(list) = session.get("decoder").and_then(Value::as_array) {
            decoders = list.clone();
        }
    }
    if decoders.is_empty()
        && let Some(raw) = read_entry(&mut zip, "decoders")?
        && let Ok(Value::Array(list)) = serde_json::from_slice(&raw)
    {
        decoders = list;
    }

    Ok(Capture {
        samplerate,
        total_samples,
        signals,
        decoders,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samplerates() {
        assert_eq!(parse_samplerate("25 MHz").unwrap(), 25_000_000);
        assert_eq!(parse_samplerate("1 GHz").unwrap(), 1_000_000_000);
        assert_eq!(parse_samplerate("500 kHz").unwrap(), 500_000);
        assert_eq!(parse_samplerate("100 Hz").unwrap(), 100);
        assert_eq!(parse_samplerate("2.5 MHz").unwrap(), 2_500_000);
        assert!(parse_samplerate("fast").is_err());
    }
}
