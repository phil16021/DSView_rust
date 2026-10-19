//! Chunking invariance: a decoder gives the same annotations whatever the
//! chunks the samples arrive in (DecoderStack sends what the capture has;
//! a stream sends many small pieces). UART and I²C, from two samples to the
//! whole capture per chunk. The engine keeps libsigrokdecode's chunk-edge
//! behaviour (F32): positions may move by one sample.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dsview_decode::{OptValue, OutputData, Session};

fn decoders_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../libsigrokdecode4DSL/decoders")
}

/// Packs per-sample levels of two channels into one bit buffer each.
fn pack(levels: &[[bool; 2]]) -> [Vec<u8>; 2] {
    let n = levels.len().div_ceil(64) * 8;
    let mut out = [vec![0u8; n], vec![0u8; n]];
    for (i, l) in levels.iter().enumerate() {
        for (c, buf) in out.iter_mut().enumerate() {
            if l[c] {
                buf[i / 8] |= 1 << (i % 8);
            }
        }
    }
    out
}

/// UART 8N1 at 10 samples per bit on channel 0; channel 1 idle.
fn uart(bytes: &[u8]) -> Vec<[bool; 2]> {
    let mut v = vec![[true, true]; 100];
    for &b in bytes {
        v.extend(std::iter::repeat_n([false, true], 10));
        for i in 0..8 {
            v.extend(std::iter::repeat_n([b >> i & 1 == 1, true], 10));
        }
        v.extend(std::iter::repeat_n([true, true], 20));
    }
    v
}

/// I²C writes (SCL ch0, SDA ch1), 8 samples per half clock.
fn i2c(bytes: &[u8]) -> Vec<[bool; 2]> {
    let h = 8;
    let mut v = vec![[true, true]; 40];
    let put = |scl: bool, sda: bool, n: usize, v: &mut Vec<[bool; 2]>| {
        v.extend(std::iter::repeat_n([scl, sda], n));
    };
    // start: SDA falls while SCL is high
    put(true, false, h, &mut v);
    let bit = |b: bool, v: &mut Vec<[bool; 2]>| {
        put(false, b, h, v);
        put(true, b, h, v);
    };
    for &byte in bytes {
        for i in (0..8).rev() {
            bit(byte >> i & 1 == 1, &mut v);
        }
        bit(false, &mut v); // ACK
    }
    // stop: SDA rises while SCL is high
    put(false, false, h, &mut v);
    put(true, false, h, &mut v);
    put(true, true, 4 * h, &mut v);
    v
}

type Ann = (u64, u64, i32, Vec<String>);
/// A decoder id, its channel map and the samples.
type Case<'a> = (&'a str, &'a [(&'a str, i32)], Vec<[bool; 2]>);

fn decode(id: &str, channels: &[(&str, i32)], levels: &[[bool; 2]], chunk: u64) -> Vec<Ann> {
    let session = Session::new();
    let mut opts = HashMap::new();
    if id.ends_with("uart") {
        opts.insert("baudrate".to_owned(), OptValue::Int(100_000));
    }
    let di = session.inst_new(id, Some(opts), None).expect("instance");
    let ch: Vec<(String, i32)> = channels.iter().map(|&(k, v)| (k.to_owned(), v)).collect();
    session.channel_set_all(&di, &ch).expect("channels");
    session.metadata_set_samplerate(1_000_000);
    let out = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&out);
    session.callback_add(dsview_decode::module::OUTPUT_ANN, move |o| {
        if let OutputData::Annotation(a) = &o.data {
            sink.lock()
                .unwrap()
                .push((o.start, o.end, a.ann_type, a.texts.clone()));
        }
    });
    session.start().expect("start");
    let bufs = pack(levels);
    let total = levels.len() as u64;
    let mut i = 0;
    while i < total {
        let end = (i + chunk).min(total);
        let from = (i / 8) as usize;
        session
            .send(
                i,
                end,
                &[Some(&bufs[0][from..]), Some(&bufs[1][from..])],
                &[0, 0],
            )
            .expect("send");
        i = end;
    }
    session.end().expect("end");
    out.lock().unwrap().clone()
}

#[test]
fn annotations_do_not_depend_on_the_chunks() {
    dsview_decode::init(&[decoders_dir().as_path()]).expect("init");
    dsview_decode::load_all();
    let cases: [Case; 2] = [
        ("1:uart", &[("rxtx", 0)], uart(b"chunks!\x00\xff")),
        ("0:i2c", &[("scl", 0), ("sda", 1)], i2c(&[0xA0, 0x12, 0x5A])),
    ];
    for (id, channels, levels) in &cases {
        let whole = decode(id, channels, levels, levels.len() as u64);
        assert!(
            whole.len() >= 9,
            "{id}: {} annotations {whole:?}",
            whole.len()
        );
        // chunks of one sample decode nothing, in C as well (F32)
        for chunk in [2, 3, 7, 8, 9, 64, 100, 511, 4096] {
            let got = decode(id, channels, levels, chunk);
            assert_eq!(got.len(), whole.len(), "{id} in chunks of {chunk}");
            for (a, b) in got.iter().zip(&whole) {
                // the same annotations; one that ends a wait() on a chunk's
                // first sample moves by one sample, as in C (F32)
                let near = a.0.abs_diff(b.0) <= 1 && a.1.abs_diff(b.1) <= 1;
                assert!(
                    near && a.2 == b.2 && a.3 == b.3,
                    "{id} in chunks of {chunk}: {a:?} vs {b:?}"
                );
            }
        }
    }
}
