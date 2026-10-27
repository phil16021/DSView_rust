//! A decoder that keeps the tuple `wait()` returned (`pins = self.wait()`,
//! as `pjdl` and `ieee488` do) gets a fresh tuple from the next `wait()`
//! (DEVIATIONS F13). C refilled one tuple per instance; with the decoder
//! still holding it, the refill failed and the second `wait()` raised a
//! SystemError.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dsview_decode::{OutputData, Session};

#[test]
fn kept_wait_result_survives_the_next_wait() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/decoders");
    dsview_decode::init(&[dir.as_path()]).expect("init");
    assert!(dsview_decode::load("keep_pins"), "test decoder loads");

    let session = Session::new();
    let di = session.inst_new("keep_pins", None, None).expect("instance");
    session
        .channel_set_all(&di, &[("data".to_owned(), 0)])
        .expect("channels");
    session.metadata_set_samplerate(1_000_000);
    let texts = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&texts);
    session.callback_add(dsview_decode::module::OUTPUT_ANN, move |o| {
        if let OutputData::Annotation(a) = &o.data {
            sink.lock().unwrap().push((o.start, a.texts[0].clone()));
        }
    });
    session.start().expect("start");

    // edges at 8 (up), 16 (down), 24 (up), 32 (down)
    let bits = [0x00u8, 0xff, 0x00, 0xff, 0, 0, 0, 0];
    session
        .send(0, 64, &[Some(&bits)], &[0])
        .expect("the second wait() returns normally");
    session.end().expect("end");
    assert_eq!(
        *texts.lock().unwrap(),
        [
            (16, "1->0".to_owned()),
            (24, "0->1".to_owned()),
            (32, "1->0".to_owned())
        ]
    );
}
