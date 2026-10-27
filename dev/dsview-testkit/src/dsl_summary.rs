//! Summary of a capture as read by the Rust `.dsl` reader, in the exact
//! format `oracle-dsl` prints for the C session driver.

use dsview_formats::dsl::{DslReader, Packet};
use dsview_formats::header::Mode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Same fields, hashing rules and packet counts as oracle-dsl.
pub fn summary(path: &std::path::Path) -> Value {
    let mut r = DslReader::open(path).expect("open");
    let h = r.header().clone();
    let kind = |m: Option<Mode>| match m {
        Some(Mode::Dso) => "dso",
        Some(Mode::Analog) => "analog",
        _ => "logic",
    };

    let logic_pos: Vec<usize> = h
        .channels
        .iter()
        .enumerate()
        .filter(|(_, c)| c.enabled && kind(c.mode) == "logic")
        .map(|(i, _)| i)
        .collect();
    let sample_channels = h
        .channels
        .iter()
        .filter(|c| c.enabled && kind(c.mode) != "logic")
        .count()
        .max(1) as u64;

    let mut words: Vec<Vec<u64>> = vec![Vec::new(); logic_pos.len()];
    let mut raw = Sha256::new();
    let mut raw_samples = 0u64;
    let mut packets = [0u64; 3];
    r.stream(|p| {
        packets[match p {
            Packet::Logic(_) => 0,
            Packet::Dso { .. } => 1,
            Packet::Analog { .. } => 2,
        }] += 1;
        match p {
            Packet::Logic(bytes) if !words.is_empty() => {
                let n = words.len();
                for (i, w) in bytes.as_chunks::<8>().0.iter().enumerate() {
                    words[i % n].push(u64::from_le_bytes(*w));
                }
            }
            Packet::Dso { data, num_samples } | Packet::Analog { data, num_samples } => {
                raw.update(&data[..(num_samples * sample_channels) as usize]);
                raw_samples += num_samples;
            }
            _ => {}
        }
    })
    .expect("stream");

    let total = h.total_samples;
    let channels: Vec<Value> = h
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut o = json!({"index": c.index, "type": kind(c.mode), "enabled": c.enabled,
                               "name": c.name.clone().unwrap_or_default()});
            if let Some(pos) = logic_pos.iter().position(|&p| p == i) {
                let w = &words[pos];
                let mut hsh = Sha256::new();
                let full = (total / 64) as usize;
                for x in w.iter().take(full) {
                    hsh.update(x.to_le_bytes());
                }
                let rem = total % 64;
                if rem > 0
                    && let Some(x) = w.get(full)
                {
                    let masked = x & ((1u64 << rem) - 1);
                    hsh.update(&masked.to_le_bytes()[..rem.div_ceil(8) as usize]);
                }
                o["samples"] = json!((w.len() as u64 * 64).min(total));
                o["sha256"] = json!(format!("{:x}", hsh.finalize()));
            }
            o
        })
        .collect();

    let mut s = json!({
        "mode": kind(Some(h.mode)),
        "samplerate": h.samplerate,
        "limit_samples": total,
        "channels": channels,
        "packets": {"logic": packets[0], "dso": packets[1], "analog": packets[2], "end": 1},
    });
    if raw_samples > 0 {
        s["raw_samples"] = json!(raw_samples);
        s["raw_sha256"] = json!(format!("{:x}", raw.finalize()));
    }
    s
}
