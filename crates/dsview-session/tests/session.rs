//! The capture session on the demo devices: the sampling bar's depth
//! rounding, a single capture with the demo's decoders, repeat and loop
//! mode, and a DSO capture.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::time::Duration;

use dsview_core::{ConfValue, key};
use dsview_session::session::CollectMode;
use dsview_session::{Kind, Paths, Session};
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

/// Sessions share the drivers' global state: one at a time.
fn session(device: &str) -> (MutexGuard<'static, ()>, Session) {
    static LOCK: Mutex<()> = Mutex::new(());
    static INIT: Once = Once::new();
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    INIT.call_once(|| {
        dsview_decode::init(&[root().join("libsigrokdecode4DSL/decoders").as_path()]).unwrap();
        dsview_decode::load_all();
    });
    let s = Session::new(
        Paths {
            data_dir: root().join("DSView"),
            profile_dir: None,
            version: "test".into(),
        },
        Arc::new(NoUsb),
    );
    let (list, _) = s.devices();
    let e = list
        .iter()
        .find(|e| e.kind == Kind::Demo && e.name == device)
        .expect("demo device");
    assert!(s.set_device(e.handle));
    (g, s)
}

fn wait(s: &Session, what: &str, cond: impl Fn(&dsview_session::session::State) -> bool) {
    assert!(s.wait(Duration::from_secs(60), cond), "timed out: {what}");
}

#[test]
fn sample_count_snaps_up_to_1024() {
    let (_g, s) = session("Demo Logic");
    s.set_config(key::LIMIT_SAMPLES, ConfValue::U64(1_000_000), None);
    s.update_sample_rate_list();
    s.commit_settings();
    assert_eq!(s.sample_limit(), 1_000_448);
    s.exit();
}

#[test]
fn single_capture_runs_the_demo_decoders() {
    let (_g, s) = session("Demo Logic");
    let names: Vec<String> = s.state().decoders.iter().map(|d| d.name()).collect();
    assert_eq!(names, ["1:I²C", "1:UART", "1:SPI", "CAN-FD"]);
    assert!(s.start_capture(false));
    wait(&s, "capture end", |st| st.ended_seq > 0 && !st.working);
    wait(&s, "decoding", |st| !st.decoding);
    assert_eq!(s.data().logic.lock().unwrap().ring_sample_count(), 131_072);
    let counts: Vec<(u64, i32)> = s
        .state()
        .decoders
        .iter()
        .map(|d| (d.stack.result_count(), d.stack.progress()))
        .collect();
    assert_eq!(counts, [(405, 100), (517, 100), (5192, 100), (2265, 100)]);
    s.exit();
}

#[test]
fn repeat_mode_captures_until_stopped() {
    let (_g, s) = session("Demo Logic");
    s.set_collect_mode(CollectMode::Repeat, 0.0);
    assert!(s.start_capture(false));
    wait(&s, "three captures", |st| st.captures >= 3);
    assert!(s.is_working());
    s.stop_capture();
    wait(&s, "stopped", |st| !st.working && !st.running);
    assert!(s.have_view_data());
    s.exit();
}

#[test]
fn loop_mode_streams_into_the_ring() {
    let (_g, s) = session("Demo Logic");
    assert!(s.set_demo_pattern("random"));
    s.set_collect_mode(CollectMode::Loop, 0.0);
    assert!(s.start_capture(false));
    std::thread::sleep(Duration::from_millis(1500));
    assert!(s.is_working(), "a loop capture runs until stopped");
    assert_eq!(s.state().collect_mode, CollectMode::Loop);
    s.stop_capture();
    wait(&s, "stopped", |st| !st.working && !st.running);
    assert!(s.data().logic.lock().unwrap().ring_sample_count() > 0);
    s.exit();
}

#[test]
fn dso_single_shot() {
    let (_g, s) = session("Demo Oscilloscope");
    assert!(s.start_capture(true));
    wait(&s, "frame", |st| st.ended_seq > 0 && !st.working);
    assert_eq!(s.data().dso.lock().unwrap().sample_count(), 10_000);
    s.exit();
}

#[test]
fn events_follow_a_capture() {
    use dsview_session::session::SessionEvent;
    let (_g, s) = session("Demo Logic");
    let rx = s.subscribe();
    assert!(s.start_capture(false));
    wait(&s, "decoding done", |st| {
        st.ended_seq > 0 && !st.working && !st.decoding
    });
    // the decode thread sends DecodeDone after clearing the flag
    std::thread::sleep(Duration::from_millis(100));
    let evs: Vec<SessionEvent> = rx.try_iter().collect();
    let pos = |e: &SessionEvent| evs.iter().position(|x| x == e);
    let started = pos(&SessionEvent::CaptureStarted).expect("started");
    let data = pos(&SessionEvent::DataAppended).expect("data");
    let ended = pos(&SessionEvent::CaptureEnded).expect("ended");
    let done = pos(&SessionEvent::DecodeDone).expect("decoded");
    assert!(started < data && data < ended, "{evs:?}");
    assert!(done > data, "{evs:?}");
    s.exit();
}

#[test]
fn protocol_export_merges_rows_by_start() {
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    wait(&s, "decoded", |st| {
        st.ended_seq > 0 && !st.working && !st.decoding
    });
    let t = s.state().decoders[0].clone();
    let rows: Vec<usize> = t
        .stack
        .rows()
        .iter()
        .enumerate()
        .filter(|(_, r)| r.list_shown)
        .map(|(i, _)| i)
        .collect();
    let path = std::env::temp_dir().join(format!("dsv-proto-{}.csv", std::process::id()));
    let count = s.data().logic.lock().unwrap().ring_sample_count();
    dsview_session::store::export_protocol(&t, &rows[..2], count, &path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    let mut lines = text.lines();
    let titles: Vec<String> = rows[..2]
        .iter()
        .map(|&r| t.stack.rows()[r].title())
        .collect();
    assert_eq!(
        lines.next().unwrap(),
        format!("Id,Time[ns],{}", titles.join(","))
    );
    // the I2C start at sample 45147 (25 MHz: 40 ns per sample)
    let first = lines.next().unwrap();
    assert!(first.starts_with("1,1805880.00,"), "{first}");
    s.exit();
}
