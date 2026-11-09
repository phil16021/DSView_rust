//! Every decoder gets its own worker: two decoders over one capture run at
//! the same time (C, and DSViewR before, decoded them one after another, so
//! in a long live capture the later tables stayed empty), and each gives
//! the same annotations as alone.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsview_formats::dsl::{LogicWriter, WriteChannel};
use dsview_session::{Paths, Session};
use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};

struct NoUsb;

impl UsbBackend for NoUsb {
    fn list(&self) -> Result<Vec<Info>, UsbError> {
        Ok(Vec::new())
    }
    fn open(&self, _: u8, _: u8, _: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
        Err(UsbError::NotFound)
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A session on a file of two clocks, with `decoders_json`.
fn session_on(decoders_json: &str, tag: &str) -> (Session, PathBuf) {
    let s = Session::new(
        Paths {
            data_dir: root().join("assets"),
            profile_dir: None,
            version: "test".into(),
        },
        Arc::new(NoUsb),
    );
    let n: u64 = 1 << 21;
    let clock = |half: u64| {
        let mut bits = vec![0u8; (n / 8) as usize];
        for i in 0..n {
            if (i / half) % 2 == 1 {
                bits[(i / 8) as usize] |= 1 << (i % 8);
            }
        }
        bits
    };
    let (a, b) = (clock(16), clock(24));
    let path = std::env::temp_dir().join(format!("dsv-par-{tag}-{}.dsl", std::process::id()));
    LogicWriter {
        driver: "virtual-session".into(),
        samplerate: 10_000_000,
        total_samples: n,
        trigger_time_ms: 0,
        trigger_pos: 0,
        channels: vec![
            WriteChannel {
                index: 0,
                name: "A".into(),
            },
            WriteChannel {
                index: 1,
                name: "B".into(),
            },
        ],
        decoders_json: decoders_json.into(),
        session_json: "{}".into(),
    }
    .write(std::fs::File::create(&path).unwrap(), &[&a, &b])
    .unwrap();
    s.set_file(path.to_str().unwrap()).unwrap();
    (s, path)
}

/// Each decoder's rows of (start, end, text).
type Annotations = Vec<Vec<Vec<(u64, u64, String)>>>;

/// Every row's annotations of each decoder, once done; and whether two
/// decoders were ever in progress at once.
fn run(s: &Session) -> (Annotations, bool) {
    let t0 = Instant::now();
    let mut together = false;
    loop {
        let st = s.state();
        let (working, decoding) = (st.working, st.decoding);
        let busy = st
            .decoders
            .iter()
            .filter(|d| (1..100).contains(&d.stack.progress()))
            .count();
        drop(st);
        together |= busy >= 2;
        if !working && !decoding {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(300),
            "decoding took too long"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let count = s.data().logic.lock().unwrap().ring_sample_count();
    let anns = s
        .state()
        .decoders
        .iter()
        .map(|d| {
            (0..d.stack.rows().len())
                .map(|r| {
                    d.stack
                        .row_sorted(r, count)
                        .iter()
                        .map(|a| (a.start, a.end, a.text().to_owned()))
                        .collect()
                })
                .collect()
        })
        .collect();
    (anns, together)
}

#[test]
fn decoders_run_side_by_side_with_the_same_results() {
    dsview_decode::init(&[root().join("assets/decoders").as_path()]).unwrap();
    dsview_decode::load_all();
    let a = r#"{"id":"timing","channel":[{"data":0}],"options":{}}"#;
    let b = r#"{"id":"timing","channel":[{"data":1}],"options":{}}"#;

    let (s, p) = session_on(&format!("[{a},{b}]"), "both");
    let (both, together) = run(&s);
    s.exit();
    let _ = std::fs::remove_file(p);
    assert_eq!(both.len(), 2);
    assert!(together, "the two decoders never ran at the same time");

    // each alone gives the same annotations
    for (k, one) in [a, b].iter().enumerate() {
        let (s, p) = session_on(&format!("[{one}]"), &format!("one{k}"));
        let (alone, _) = run(&s);
        s.exit();
        let _ = std::fs::remove_file(p);
        let n: usize = alone[0].iter().map(Vec::len).sum();
        assert!(n > 10_000, "decoder {k}: {n} annotations");
        assert_eq!(
            alone[0], both[k],
            "decoder {k} differs when run beside the other"
        );
    }
}
