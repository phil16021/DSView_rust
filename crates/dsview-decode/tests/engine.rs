//! End-to-end check of the engine on the repository's decoders: a UART byte
//! stream decodes to the expected data annotations, in chunks, with the
//! numeric field DSView shows. (The differential tests against the C engine
//! live in dev/dsview-oracle.)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dsview_decode::{OptValue, OutputData, Session};

fn decoders_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/decoders")
}

/// 8N1 UART frames, LSB first, `spb` samples per bit, idle high.
fn uart_bits(bytes: &[u8], spb: usize) -> (Vec<u8>, u64) {
    let mut levels = vec![true; spb * 10];
    for &b in bytes {
        levels.extend(std::iter::repeat_n(false, spb));
        for i in 0..8 {
            levels.extend(std::iter::repeat_n(b >> i & 1 == 1, spb));
        }
        levels.extend(std::iter::repeat_n(true, spb * 2));
    }
    let n = levels.len() as u64;
    let mut bits = vec![0u8; levels.len().div_ceil(64) * 8];
    for (i, l) in levels.iter().enumerate() {
        if *l {
            bits[i / 8] |= 1 << (i % 8);
        }
    }
    (bits, n)
}

#[test]
fn uart_bytes_decode_to_data_annotations() {
    dsview_decode::init(&[decoders_dir().as_path()]).expect("init");
    dsview_decode::load_all();
    assert!(dsview_decode::decoders().len() > 100, "decoders loaded");

    let samplerate = 1_000_000u64;
    let (bits, total) = uart_bits(b"Hi!", 10);
    let session = Session::new();
    let mut opts = HashMap::new();
    opts.insert("baudrate".to_owned(), OptValue::Int(100_000));
    let di = session
        .inst_new("1:uart", Some(opts), None)
        .expect("instance");
    session
        .channel_set_all(&di, &[("rxtx".to_owned(), 0)])
        .expect("channels");
    session.metadata_set_samplerate(samplerate);

    let data = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&data);
    session.callback_add(dsview_decode::module::OUTPUT_ANN, move |o| {
        if let OutputData::Annotation(a) = &o.data
            && a.ann_type == 108
        {
            sink.lock().unwrap().push(a.hex.clone());
        }
    });
    session.start().expect("start");

    // DecoderStack-style chunks of at most 100 samples
    let mut i = 0;
    while i < total {
        let end = (i + 100).min(total);
        let buf = &bits[(i / 8) as usize..];
        session.send(i, end, &[Some(buf)], &[0]).expect("send");
        i = end;
    }
    session.end().expect("end");
    assert_eq!(*data.lock().unwrap(), ["48", "69", "21"]);
}
