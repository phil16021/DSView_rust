//! A negative `skip` count (`flexray` computes `pos - self.samplenum`)
//! counts as `skip: 0`: the decoder carries on (DEVIATIONS F14). C left an
//! OverflowError pending, so `wait()` failed with a SystemError once a
//! condition matched, or did not, depending on CPython's type attribute
//! cache (F17). A count too large for 64 bits fails `wait()` at once.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dsview_decode::{OutputData, SendError, Session};

/// The engine with the test decoders, once per process.
fn init() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        dsview_decode::log::set_sink(|_, msg| eprintln!("{msg}"));
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/decoders");
        dsview_decode::init(&[dir.as_path()]).expect("init");
    });
}

#[test]
fn negative_skip_counts_as_zero() {
    init();
    assert!(dsview_decode::load("pending_error"), "test decoder loads");

    let session = Session::new();
    let di = session
        .inst_new("pending_error", None, None)
        .expect("instance");
    session
        .channel_set_all(&di, &[("data".to_owned(), 0)])
        .expect("channels");
    session.metadata_set_samplerate(1_000_000);
    let anns = Arc::new(Mutex::new(0));
    let sink = Arc::clone(&anns);
    session.callback_add(dsview_decode::module::OUTPUT_ANN, move |o| {
        if let OutputData::Annotation(_) = &o.data {
            *sink.lock().unwrap() += 1;
        }
    });
    session.start().expect("start");

    // low, a rising edge at 8, a falling edge at 32
    let mut bits = [0u8; 8];
    bits[1] = 0xff;
    bits[2] = 0xff;
    session
        .send(0, 64, &[Some(&bits)], &[0])
        .expect("wait() with a negative skip returns normally");
    assert_eq!(*anns.lock().unwrap(), 1);
}

#[test]
fn skip_past_64_bits_fails_wait_clearly() {
    init();
    assert!(dsview_decode::load("huge_skip"), "test decoder loads");
    let session = Session::new();
    let di = session.inst_new("huge_skip", None, None).expect("instance");
    session
        .channel_set_all(&di, &[("data".to_owned(), 0)])
        .expect("channels");
    session.metadata_set_samplerate(1_000_000);
    session.start().expect("start");
    let mut bits = [0u8; 8];
    bits[1] = 0xff;
    bits[2] = 0xff;
    match session.send(0, 64, &[Some(&bits)], &[0]) {
        Err(SendError::Decoder(msg)) => {
            // the engine's error for a bad condition, at this wait()
            assert!(
                msg.contains("Failed to get number of samples to skip"),
                "{msg}"
            );
        }
        Err(SendError::Arg(a)) => panic!("argument error {a}"),
        Ok(()) => panic!("wait() returned normally"),
    }
}
