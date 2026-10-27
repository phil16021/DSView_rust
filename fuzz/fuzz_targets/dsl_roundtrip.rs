#![no_main]
//! `.dsl` writer → reader round trip: any channel names, sample count, rate
//! and bits that `LogicWriter` writes read back the same through `DslReader`.

use dsview_formats::dsl::{DslReader, Limits, LogicWriter, WriteChannel};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&head, rest)) = data.split_first() else { return };
    let channels = (head % 4) as usize + 1;
    let Some((meta, rest)) = rest.split_at_checked(10) else { return };
    let samples = u64::from(u32::from_le_bytes(meta[0..4].try_into().unwrap()) % 200_000) + 1;
    let samplerate = u64::from(u32::from_le_bytes(meta[4..8].try_into().unwrap())).max(1);
    let name_len = (meta[8] % 16) as usize + 1;
    let trigger_pos = u64::from(meta[9]) * samples / 255;
    // names from the input (lossy UTF-8, trimmed: the key file trims values)
    let mut rest = rest;
    let mut names = Vec::new();
    for _ in 0..channels {
        let take = name_len.min(rest.len());
        let n = String::from_utf8_lossy(&rest[..take]).trim().replace(['\n', '\r', '\0'], "_");
        rest = &rest[take..];
        names.push(if n.is_empty() { "x".to_owned() } else { n });
    }
    let words = samples.div_ceil(64) as usize;
    let bits: Vec<Vec<u8>> = (0..channels)
        .map(|c| (0..words * 8).map(|i| rest.get((i + c) % rest.len().max(1)).copied().unwrap_or(0) ^ i as u8).collect())
        .collect();
    let w = LogicWriter {
        driver: "virtual-demo".into(),
        samplerate,
        total_samples: samples,
        trigger_time_ms: 0,
        trigger_pos,
        channels: names
            .iter()
            .enumerate()
            .map(|(i, n)| WriteChannel { index: i as u16, name: n.clone() })
            .collect(),
        decoders_json: "[]".into(),
        session_json: "{}".into(),
    };
    let refs: Vec<&[u8]> = bits.iter().map(Vec::as_slice).collect();
    let file = w.write(std::io::Cursor::new(Vec::new()), &refs).unwrap().into_inner();
    let mut r = DslReader::new(std::io::Cursor::new(file), Limits::default()).expect("written file reads");
    let h = r.header().clone();
    assert_eq!((h.total_samples, h.total_probes, h.trigger_pos), (samples, channels as u64, trigger_pos));
    assert_eq!(h.samplerate, samplerate, "samplerate");
    let read: Vec<String> = h.channels.iter().map(|c| c.name.clone().unwrap_or_default()).collect();
    assert_eq!(read, names);
    for (pos, b) in bits.iter().enumerate() {
        assert_eq!(r.logic_channel(pos).unwrap(), *b, "channel {pos}");
    }
});
