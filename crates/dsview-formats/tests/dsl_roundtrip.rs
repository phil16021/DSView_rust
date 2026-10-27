//! `.dsl` writer → reader round trip (M7b B7): whatever `LogicWriter` writes,
//! `DslReader` reads back the same header fields, channel names and bits.

use dsview_formats::dsl::{DslReader, Limits, LogicWriter, WriteChannel};
use proptest::prelude::*;

/// Writes a capture and checks what the reader returns.
fn roundtrip(names: &[String], samples: u64, samplerate: u64, trigger_pos: u64, bits: &[Vec<u8>]) {
    let w = LogicWriter {
        driver: "virtual-demo".into(),
        samplerate,
        total_samples: samples,
        trigger_time_ms: 1_700_000_000_000,
        trigger_pos,
        channels: names
            .iter()
            .enumerate()
            .map(|(i, n)| WriteChannel {
                index: i as u16,
                name: n.clone(),
            })
            .collect(),
        decoders_json: "[]".into(),
        session_json: "{}".into(),
    };
    let refs: Vec<&[u8]> = bits.iter().map(Vec::as_slice).collect();
    let file = w
        .write(std::io::Cursor::new(Vec::new()), &refs)
        .unwrap()
        .into_inner();
    let mut r = DslReader::new(std::io::Cursor::new(file), Limits::default()).unwrap();
    let h = r.header().clone();
    assert_eq!(h.total_samples, samples);
    assert_eq!(h.total_probes, names.len() as u64);
    assert_eq!(h.samplerate, samplerate);
    assert_eq!(h.trigger_pos, trigger_pos);
    let read_names: Vec<String> = h
        .channels
        .iter()
        .map(|c| c.name.clone().unwrap_or_default())
        .collect();
    assert_eq!(read_names, names);
    let words = samples.div_ceil(64) as usize;
    for (pos, b) in bits.iter().enumerate() {
        assert_eq!(
            r.logic_channel(pos).unwrap(),
            b[..words * 8],
            "channel {pos}"
        );
    }
}

fn name() -> impl Strategy<Value = String> {
    // printable names, including the characters the key file escapes
    proptest::string::string_regex("[ -~äöü中文;=\\\\#\\[\\]]{1,12}")
        .unwrap()
        .prop_filter("no surrounding blanks", |s| s.trim() == s)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn written_captures_read_back(
        names in proptest::collection::vec(name(), 1..5),
        samples in 1u64..300_000,
        rate in prop::sample::select(vec![1_000u64, 1_000_000, 25_000_000, 400_000_000, 1_000_000_000]),
        trig in 0u64..300_000,
        seed in any::<u64>(),
    ) {
        let words = samples.div_ceil(64) as usize;
        let mut x = seed | 1;
        let bits: Vec<Vec<u8>> = names
            .iter()
            .map(|_| {
                (0..words * 8)
                    .map(|_| {
                        x ^= x << 13;
                        x ^= x >> 7;
                        x ^= x << 17;
                        x as u8
                    })
                    .collect()
            })
            .collect();
        roundtrip(&names, samples, rate, trig.min(samples), &bits);
    }
}
