//! Chunking invariance: a decoder gives the same annotations whatever the
//! chunks the samples arrive in (DecoderStack sends what the capture has;
//! a stream sends many small pieces). UART and I²C, from two samples to the
//! whole capture per chunk; 1-Wire and SDA2506 for `skip: 0` right after a
//! match (F38). libsigrokdecode moved an annotation by one sample when a
//! wait() ended on a chunk's first sample, and chunks of one sample decoded
//! nothing (F32); the engine gives the same output for every chunking.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dsview_decode::{OptValue, OutputData, Session};

fn decoders_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/decoders")
}

fn init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        dsview_decode::init(&[decoders_dir().as_path()]).expect("init");
        dsview_decode::load_all();
    });
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

/// 1-Wire at 1 MHz on channel 0 (µs = samples): reset, presence, then
/// `bits` as write slots. A 0 is held low past the slot minimum, so its slot
/// end is a `skip: 0` right after the rising edge matched.
fn onewire(bits: &[bool]) -> Vec<[bool; 2]> {
    let mut v = vec![[true, true]; 100];
    let put = |l: bool, n: usize, v: &mut Vec<[bool; 2]>| {
        v.extend(std::iter::repeat_n([l, true], n));
    };
    put(false, 500, &mut v);
    put(true, 15, &mut v);
    put(false, 120, &mut v);
    put(true, 400, &mut v);
    for &b in bits {
        if b {
            put(false, 6, &mut v);
            put(true, 64, &mut v);
        } else {
            put(false, 70, &mut v);
            put(true, 10, &mut v);
        }
    }
    put(true, 200, &mut v);
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
    init();
    let cases: [Case; 3] = [
        ("1:uart", &[("rxtx", 0)], uart(b"chunks!\x00\xff")),
        ("0:i2c", &[("scl", 0), ("sda", 1)], i2c(&[0xA0, 0x12, 0x5A])),
        (
            "onewire_link",
            &[("owr", 0)],
            onewire(&[
                false, true, false, false, true, true, false, true, false, false,
            ]),
        ),
    ];
    for (id, channels, levels) in &cases {
        let whole = decode(id, channels, levels, levels.len() as u64);
        assert!(
            whole.len() >= 9,
            "{id}: {} annotations {whole:?}",
            whole.len()
        );
        for chunk in [1, 2, 3, 7, 8, 9, 64, 100, 511, 4096] {
            let got = decode(id, channels, levels, chunk);
            assert_eq!(got, whole, "{id} in chunks of {chunk}");
        }
    }
}

/// `skip: 0` right after a match also returns that sample's pins when the
/// match was a chunk's last sample (F38). `sda2506` at 10 MHz reads each data
/// bit with `skip: 0` on the falling clock edge; DATA changes one sample
/// after the edge, so the sample after the match would give the next bit.
#[test]
fn skip_zero_pins_do_not_depend_on_the_chunks() {
    init();
    // CLK, DATA, CE# (low: data mode); 5 samples per half clock
    let bits = [
        true, false, false, true, true, false, true, false, true, true,
    ];
    let mut lv: Vec<[bool; 3]> = vec![[true, false, false]; 20];
    let mut d = false;
    for &b in &bits {
        lv.push([false, d, false]);
        d = b;
        lv.extend(std::iter::repeat_n([false, d, false], 4));
        lv.extend(std::iter::repeat_n([true, d, false], 5));
    }
    lv.extend(std::iter::repeat_n([true, d, false], 20));
    let n = lv.len().div_ceil(64) * 8;
    let mut bufs = vec![vec![0u8; n]; 3];
    for (i, l) in lv.iter().enumerate() {
        for c in 0..3 {
            if l[c] {
                bufs[c][i / 8] |= 1 << (i % 8);
            }
        }
    }
    let run = |chunk: u64| -> Vec<Ann> {
        let session = Session::new();
        let di = session.inst_new("sda2506", None, None).expect("instance");
        let ch: Vec<(String, i32)> = [("clk", 0), ("d", 1), ("ce", 2)]
            .iter()
            .map(|&(k, v)| (k.to_owned(), v))
            .collect();
        session.channel_set_all(&di, &ch).expect("channels");
        session.metadata_set_samplerate(10_000_000);
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
        let total = lv.len() as u64;
        let mut i = 0;
        while i < total {
            let end = (i + chunk).min(total);
            let from = (i / 8) as usize;
            let refs: Vec<Option<&[u8]>> = bufs.iter().map(|b| Some(&b[from..])).collect();
            session.send(i, end, &refs, &[0, 0, 0]).expect("send");
            i = end;
        }
        session.end().expect("end");
        out.lock().unwrap().clone()
    };
    let whole = run(lv.len() as u64);
    let data: Vec<&str> = whole
        .iter()
        .filter(|a| a.3[0].len() == 1)
        .map(|a| a.3[0].as_str())
        .collect();
    // each bit is read on its own falling edge, before DATA moves on
    assert_eq!(
        data,
        ["0", "1", "0", "0", "1", "1", "0", "1", "0", "1"],
        "{whole:?}"
    );
    for chunk in [1, 2, 3, 5, 7, 8, 64] {
        assert_eq!(run(chunk), whole, "sda2506 in chunks of {chunk}");
    }
}
