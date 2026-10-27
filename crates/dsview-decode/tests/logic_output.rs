//! `OUTPUT_LOGIC` (libsigrokdecode's logic output, which DSView's engine
//! lacks): decoders that register it start, and its puts reach a callback.
//! `pca9571` and `tca6408a` failed in `start()` without it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Once};

use dsview_decode::{OutputData, Session};

fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let tests = root.join("tests/decoders");
        let assets = root.join("../../assets/decoders");
        dsview_decode::init(&[tests.as_path(), assets.as_path()]).expect("init");
    });
}

#[test]
fn logic_puts_reach_the_callback() {
    init();
    assert!(dsview_decode::load("logic_out"), "test decoder loads");
    let session = Session::new();
    let di = session.inst_new("logic_out", None, None).expect("instance");
    session
        .channel_set_all(&di, &[("data".to_owned(), 0)])
        .expect("channels");
    session.metadata_set_samplerate(1_000_000);
    let got = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&got);
    session.callback_add(dsview_decode::module::OUTPUT_LOGIC, move |o| {
        if let OutputData::Logic { channel, data } = &o.data {
            sink.lock()
                .unwrap()
                .push((o.start, *channel, data.to_vec()));
        }
    });
    let anns = Arc::new(Mutex::new(0));
    let ann_sink = Arc::clone(&anns);
    session.callback_add(dsview_decode::module::OUTPUT_ANN, move |_| {
        *ann_sink.lock().unwrap() += 1;
    });
    session.start().expect("start");
    // edges at 8 (up) and 16 (down)
    let bits = [0x00u8, 0xff, 0, 0, 0, 0, 0, 0];
    session.send(0, 64, &[Some(&bits)], &[0]).expect("send");
    session.end().expect("end");
    assert_eq!(*got.lock().unwrap(), [(8, 0, vec![1]), (16, 0, vec![0])]);
    assert_eq!(*anns.lock().unwrap(), 2);
}

#[test]
fn expander_decoders_start() {
    init();
    for module in ["1-i2c", "pca9571", "tca6408a"] {
        assert!(dsview_decode::load(module), "{module} loads");
    }
    for id in ["pca9571", "tca6408a"] {
        let session = Session::new();
        // options as the GUI passes them: the defaults filled in
        let i2c = session
            .inst_new("1:i2c", Some(Default::default()), None)
            .expect("i2c");
        session
            .channel_set_all(&i2c, &[("scl".to_owned(), 0), ("sda".to_owned(), 1)])
            .expect("channels");
        let top = session
            .inst_new(id, Some(Default::default()), None)
            .expect(id);
        session.stack(&i2c, &top);
        session.metadata_set_samplerate(1_000_000);
        session
            .start()
            .unwrap_or_else(|e| panic!("{id} start: {e:?}"));
        let idle = [0xffu8; 16];
        session
            .send(0, 128, &[Some(&idle), Some(&idle)], &[0, 1])
            .unwrap_or_else(|e| panic!("{id} decode: {e:?}"));
        session.end().expect("end");
    }
}
