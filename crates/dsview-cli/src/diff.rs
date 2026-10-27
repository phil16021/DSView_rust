//! `diff-dsl` and `diff-ann`: where two captures or two annotation lists
//! part (PLAN §4, debug plan). Both print the first divergence with a window
//! around it and fail when anything differs.

use std::path::Path;

use dsview_formats::dsl::{DslReader, Packet};
use dsview_formats::header::Mode;
use serde_json::Value;

/// Samples shown on each side of a divergence.
const WINDOW: u64 = 16;

/// Sample `i` of a bit-packed (LSB first) channel.
fn bit(b: &[u8], i: u64) -> bool {
    b.get((i / 8) as usize)
        .is_some_and(|v| v >> (i % 8) & 1 == 1)
}

/// Samples of `0..total` where the two channels differ: (count, first).
pub fn diff_bits(a: &[u8], b: &[u8], total: u64) -> (u64, Option<u64>) {
    let bytes = total.div_ceil(8) as usize;
    let mut count = 0;
    let mut first = None;
    for i in 0..bytes {
        let (x, y) = (
            a.get(i).copied().unwrap_or(0),
            b.get(i).copied().unwrap_or(0),
        );
        let mut d = x ^ y;
        if i + 1 == bytes && !total.is_multiple_of(8) {
            d &= (1u8 << (total % 8)) - 1;
        }
        if d != 0 {
            first.get_or_insert(i as u64 * 8 + u64::from(d.trailing_zeros()));
            count += u64::from(d.count_ones());
        }
    }
    (count, first)
}

/// `0101…` of `start..end`, a `|` before sample `mark`.
fn levels(b: &[u8], start: u64, end: u64, mark: u64) -> String {
    (start..end)
        .map(|i| {
            let c = if bit(b, i) { '1' } else { '0' };
            if i == mark {
                format!("|{c}")
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// Every data byte of a capture in packet order (DSO and analog files).
fn stream_bytes(r: &mut DslReader<std::fs::File>) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    r.stream(|p| match p {
        Packet::Logic(b) => out.extend_from_slice(b),
        Packet::Dso { data, .. } | Packet::Analog { data, .. } => out.extend_from_slice(data),
    })
    .map_err(|e| e.to_string())?;
    Ok(out)
}

pub fn dsl(args: &[String]) -> Result<(), String> {
    let [a, b] = args else {
        return Err("usage: dsviewr-cli diff-dsl A.dsl B.dsl".into());
    };
    let open = |p: &String| DslReader::open(Path::new(p)).map_err(|e| format!("{p}: {e}"));
    let (mut ra, mut rb) = (open(a)?, open(b)?);
    let (ha, hb) = (ra.header().clone(), rb.header().clone());
    let mut differs = false;
    let mut field = |name: &str, x: String, y: String| {
        if x != y {
            println!("{name}: {x} | {y}");
            differs = true;
        }
    };
    field("mode", format!("{:?}", ha.mode), format!("{:?}", hb.mode));
    field(
        "samplerate",
        ha.samplerate.to_string(),
        hb.samplerate.to_string(),
    );
    field(
        "total samples",
        ha.total_samples.to_string(),
        hb.total_samples.to_string(),
    );
    field(
        "probes",
        ha.total_probes.to_string(),
        hb.total_probes.to_string(),
    );
    field(
        "trigger pos",
        ha.trigger_pos.to_string(),
        hb.trigger_pos.to_string(),
    );
    let name = |h: &dsview_formats::header::Header, i: usize| {
        h.channels
            .get(i)
            .map(|c| format!("{} {}", c.index, c.name.clone().unwrap_or_default()))
            .unwrap_or_else(|| "-".into())
    };
    let n = ha.channels.len().max(hb.channels.len());
    for i in 0..n {
        field(&format!("channel {i}"), name(&ha, i), name(&hb, i));
    }
    if ha.mode == Mode::Logic && hb.mode == Mode::Logic {
        let total = ha.total_samples.min(hb.total_samples);
        for pos in 0..ha.channels.len().min(hb.channels.len()) {
            let (x, y) = (
                ra.logic_channel(pos).map_err(|e| format!("{a}: {e}"))?,
                rb.logic_channel(pos).map_err(|e| format!("{b}: {e}"))?,
            );
            let (count, first) = diff_bits(&x, &y, total);
            if let Some(f) = first {
                differs = true;
                let (s, e) = (f.saturating_sub(WINDOW), (f + WINDOW).min(total));
                println!(
                    "channel {}: {count} of {total} samples differ, first at {f}\n  {s}..{e}\n  A {}\n  B {}",
                    name(&ha, pos),
                    levels(&x, s, e, f),
                    levels(&y, s, e, f)
                );
            }
        }
    } else if ha.mode == hb.mode {
        let (x, y) = (stream_bytes(&mut ra)?, stream_bytes(&mut rb)?);
        let first = x.iter().zip(&y).position(|(p, q)| p != q);
        if let Some(f) = first.or((x.len() != y.len()).then(|| x.len().min(y.len()))) {
            differs = true;
            let (s, e) = (f.saturating_sub(8), (f + 8).min(x.len().max(y.len())));
            println!(
                "data: {} | {} bytes, first difference at byte {f}\n  A {:?}\n  B {:?}",
                x.len(),
                y.len(),
                x.get(s..e.min(x.len())).unwrap_or(&[]),
                y.get(s..e.min(y.len())).unwrap_or(&[])
            );
        }
    }
    if differs {
        Err("the captures differ".into())
    } else {
        println!("identical");
        Ok(())
    }
}

/// One annotation as `decode --json` (the control API's `get_annotations`)
/// writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ann {
    pub start: u64,
    pub end: u64,
    pub row: String,
    pub text: String,
}

impl std::fmt::Display for Ann {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{} {}: {}", self.start, self.end, self.row, self.text)
    }
}

fn ann(v: &Value) -> Option<Ann> {
    Some(Ann {
        start: v["start_sample"].as_u64()?,
        end: v["end_sample"].as_u64()?,
        row: v["row"].as_str().unwrap_or("").to_owned(),
        text: v["text"].as_str().unwrap_or("").to_owned(),
    })
}

/// The annotations of a `decode --json` output (an array of
/// `get_annotations` replies, or one), or of JSON lines with one annotation
/// each, in file order.
pub fn parse_anns(text: &str) -> Result<Vec<Ann>, String> {
    let from_reply = |r: &Value| -> Vec<Ann> {
        r["annotations"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(ann)
            .collect()
    };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Array(replies)) => Ok(replies.iter().flat_map(from_reply).collect()),
        Ok(v @ Value::Object(_)) if v.get("annotations").is_some() => Ok(from_reply(&v)),
        _ => text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .enumerate()
            .map(|(i, l)| {
                serde_json::from_str::<Value>(l)
                    .ok()
                    .as_ref()
                    .and_then(ann)
                    .ok_or_else(|| format!("line {}: not an annotation", i + 1))
            })
            .collect(),
    }
}

/// Index of the first annotation that differs, if any (a shorter list
/// differs where it ends).
pub fn first_divergence(a: &[Ann], b: &[Ann]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x != y)
        .or((a.len() != b.len()).then(|| a.len().min(b.len())))
}

pub fn ann_files(args: &[String]) -> Result<(), String> {
    let [a, b] = args else {
        return Err("usage: dsviewr-cli diff-ann A.json B.json".into());
    };
    let read = |p: &String| -> Result<Vec<Ann>, String> {
        let t = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
        parse_anns(&t).map_err(|e| format!("{p}: {e}"))
    };
    let (x, y) = (read(a)?, read(b)?);
    let Some(i) = first_divergence(&x, &y) else {
        println!("identical: {} annotations", x.len());
        return Ok(());
    };
    println!(
        "{} | {} annotations, first difference at #{i}",
        x.len(),
        y.len()
    );
    let (s, e) = (i.saturating_sub(3), i + 4);
    for (tag, list) in [("A", &x), ("B", &y)] {
        println!("{tag}:");
        for (k, a) in list.iter().enumerate().take(e).skip(s) {
            println!("{} #{k} {a}", if k == i { ">" } else { " " });
        }
    }
    Err("the annotations differ".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_count_and_first() {
        let a = [0b0000_0000u8, 0xff];
        let b = [0b0010_0000u8, 0x7f];
        assert_eq!(diff_bits(&a, &b, 16), (2, Some(5)));
        // samples past the total do not count
        assert_eq!(diff_bits(&a, &b, 15), (1, Some(5)));
        assert_eq!(diff_bits(&a, &a, 16), (0, None));
        assert_eq!(levels(&b, 3, 8, 5), "00|100");
    }

    #[test]
    fn annotations_from_replies_and_lines() {
        let reply = r#"[{"decoder":0,"annotations":[
            {"start_sample":1,"end_sample":5,"row":"Bits","text":"1"},
            {"start_sample":5,"end_sample":9,"row":"Bits","text":"0"}]}]"#;
        let a = parse_anns(reply).unwrap();
        assert_eq!(a.len(), 2);
        let lines = "{\"start_sample\":1,\"end_sample\":5,\"row\":\"Bits\",\"text\":\"1\"}\n\
                     {\"start_sample\":5,\"end_sample\":9,\"row\":\"Bits\",\"text\":\"1\"}\n";
        let b = parse_anns(lines).unwrap();
        assert_eq!(first_divergence(&a, &b), Some(1));
        assert_eq!(first_divergence(&a, &a), None);
        assert_eq!(first_divergence(&a, &a[..1]), Some(1));
        assert!(parse_anns("{\"x\":1}\n").is_err());
    }
}
