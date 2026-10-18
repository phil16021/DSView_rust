//! A negative `skip` count leaves an OverflowError pending, so `wait()` fails
//! with a SystemError once a condition matches (DEVIATIONS F14). Storing
//! `samplenum` must not clear that error, even when the type attribute cache
//! misses (F17): before the fix this depended on memory layout.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dsview_decode::{OutputData, SendError, Session};

#[test]
fn negative_skip_fails_wait_even_on_type_cache_miss() {
    dsview_decode::log::set_sink(|_, msg| eprintln!("{msg}"));
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/decoders");
    dsview_decode::init(&[dir.as_path()]).expect("init");
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
    let res = session.send(0, 64, &[Some(&bits)], &[0]);
    match res {
        Err(SendError::Decoder(msg)) => assert!(msg.contains("SystemError"), "{msg}"),
        Err(SendError::Arg(a)) => panic!("argument error {a}"),
        Ok(()) => panic!("wait() returned normally"),
    }
    assert_eq!(*anns.lock().unwrap(), 0);
}
