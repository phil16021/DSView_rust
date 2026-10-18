//! The DSLogic driver against the Phase 0 usbmon captures of the C driver
//! (testdata/usb/dslogic, recorded with a DSLogic Plus; not in git, the
//! tests skip without them). The replay device checks every command the
//! Rust driver sends, including the DSL_setting block, against the C
//! driver's, and answers with the recorded responses and sample data.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dsview_core::{Body, ConfValue, Packet, Trigger, key};
use dsview_drivers::dsl::dslogic::DsLogic;
use dsview_drivers::dsl::tables::DSLOGIC_PROFILES;
use dsview_drivers::sys::{self, Clock};
use dsview_drivers::{Device, Step};
use dsview_usb::replay::{Log, ReplayDevice};
use dsview_usb::trace::{self, Kind, Urb, Xfer};
use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};
use serde_json::Value;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/usb/dslogic")
}

/// A clock that moves 1 ms per reading and by the sleep: waits are instant.
struct Fast(AtomicI64);

impl Clock for Fast {
    fn now_us(&self) -> i64 {
        self.0.fetch_add(1000, Ordering::Relaxed)
    }
    fn sleep_us(&self, us: u64) {
        self.0.fetch_add(us as i64, Ordering::Relaxed);
    }
}

fn setup() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    sys::set_clock(Arc::new(Fast(AtomicI64::new(0))));
    if std::env::var("REPLAY_LOG").is_ok() {
        sys::set_log_sink(Box::new(|l, m| eprintln!("drv[{l}]: {m}")));
    }
    g
}

/// Hands out one prepared device.
struct OneDevice(Mutex<Option<Box<dyn UsbDevice>>>);

impl UsbBackend for OneDevice {
    fn list(&self) -> Result<Vec<Info>, UsbError> {
        Ok(Vec::new())
    }
    fn open(&self, _bus: u8, _address: u8, _how: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
        self.0.lock().unwrap().take().ok_or(UsbError::Busy)
    }
}

/// The DSLogic in a capture (the device answering vendor requests).
fn device_of(urbs: &[Urb]) -> (u16, u8) {
    let u = urbs
        .iter()
        .find(|u| u.xfer == Xfer::Control && u.setup.is_some_and(|s| s[0] & 0x60 == 0x40))
        .expect("a vendor request");
    (u.bus, u.dev)
}

/// Bulk IN data of the capture after its first (header) transfer.
fn capture_data(urbs: &[Urb], bus: u16, dev: u8) -> Vec<u8> {
    let mut v = Vec::new();
    let mut first = true;
    for u in urbs.iter().filter(|u| {
        u.bus == bus
            && u.dev == dev
            && u.kind == Kind::Complete
            && u.xfer == Xfer::Bulk
            && u.ep == 0x86
    }) {
        if first {
            first = false;
            continue;
        }
        // what the replay hands out: uncaptured bytes as zeros
        let start = v.len();
        v.extend_from_slice(&u.data);
        v.resize(start + u.length as usize, 0);
    }
    v
}

fn replay(name: &str) -> Option<(ReplayDevice, Log, Vec<Urb>)> {
    let path = dir().join(name);
    if !path.exists() {
        eprintln!("skipping: {} not recorded here", path.display());
        return None;
    }
    let urbs = trace::read(&path).unwrap();
    let (bus, dev) = device_of(&urbs);
    let r = ReplayDevice::new(&urbs, bus, dev);
    let log = r.log();
    Some((r, log, urbs))
}

fn plus() -> &'static dsview_drivers::dsl::Profile {
    DSLOGIC_PROFILES.iter().find(|p| p.pid == 0x0034).unwrap()
}

fn check(log: &Log, what: &str) {
    let l = log.lock().unwrap();
    assert!(
        l.is_empty(),
        "{what}: {} mismatches:\n{}",
        l.len(),
        l.join("\n")
    );
}

#[test]
fn open_matches_c() {
    let _g = setup();
    let Some((r, log, _)) = replay("startup.pcapng") else {
        return;
    };
    let remaining = Arc::new(Mutex::new(None::<usize>));
    let backend = Arc::new(OneDevice(Mutex::new(Some(Box::new(Watch(
        r,
        Arc::clone(&remaining),
    ))))));
    let mut d = DsLogic::dslogic(plus(), backend, 1, 1);
    d.open().expect("open");
    // DSView then applies the stored threshold (1.0 V)
    d.config_set(key::VTH, &ConfValue::F64(1.0), None).unwrap();
    check(&log, "open");
    drop(d);
    assert_eq!(
        *remaining.lock().unwrap(),
        Some(0),
        "capture steps left over"
    );
}

/// Reports the replay's remaining steps when dropped.
struct Watch(ReplayDevice, Arc<Mutex<Option<usize>>>);

impl Drop for Watch {
    fn drop(&mut self) {
        *self.1.lock().unwrap() = Some(self.0.remaining());
    }
}

impl UsbDevice for Watch {
    fn control_out(
        &mut self,
        s: dsview_usb::Setup,
        d: &[u8],
        t: std::time::Duration,
    ) -> Result<(), UsbError> {
        self.0.control_out(s, d, t)
    }
    fn control_in(
        &mut self,
        s: dsview_usb::Setup,
        l: u16,
        t: std::time::Duration,
    ) -> Result<Vec<u8>, UsbError> {
        self.0.control_in(s, l, t)
    }
    fn bulk_out(&mut self, ep: u8, d: &[u8], t: std::time::Duration) -> Result<usize, UsbError> {
        self.0.bulk_out(ep, d, t)
    }
    fn bulk_in_submit(&mut self, ep: u8, len: usize) -> Result<(), UsbError> {
        self.0.bulk_in_submit(ep, len)
    }
    fn bulk_in_wait(&mut self, ep: u8, t: std::time::Duration) -> Option<dsview_usb::InCompletion> {
        self.0.bulk_in_wait(ep, t)
    }
    fn bulk_in_pending(&self, ep: u8) -> usize {
        self.0.bulk_in_pending(ep)
    }
    fn bulk_in_cancel_all(&mut self, ep: u8) {
        self.0.bulk_in_cancel_all(ep)
    }
}

/// The trigger DSView's simple trigger dialog sets for `t` on `channel`.
fn trigger_for(t: &Value) -> Trigger {
    let mut tr = Trigger {
        trigger_pos: 1,
        ..Trigger::default()
    };
    if let Some(ch) = t["channel"].as_u64() {
        let c = match t["type"].as_str() {
            Some("rising") => b'R',
            Some("falling") => b'F',
            Some("high") => b'1',
            Some("low") => b'0',
            _ => b'X',
        };
        tr.trigger_en = 1;
        // the simple trigger sets trigger0 only
        tr.trigger0[16][ch as usize] = c;
    }
    tr
}

fn run_capture(scenario: &Value) {
    let name = scenario["pcapng"].as_str().unwrap();
    let Some((r, log, urbs)) = replay(name) else {
        return;
    };
    let (bus, dev) = device_of(&urbs);
    let expected = capture_data(&urbs, bus, dev);
    let remaining = Arc::new(Mutex::new(None::<usize>));
    let backend: Arc<dyn UsbBackend> = Arc::new(OneDevice(Mutex::new(None)));
    let mut d = DsLogic::dslogic(plus(), backend, 1, 1);
    d.attach(Box::new(Watch(r, Arc::clone(&remaining))));

    let n = scenario["channels"].as_u64().unwrap() as usize;
    for i in 0..16 {
        d.config_set(key::PROBE_EN, &ConfValue::Bool(i < n), Some(i))
            .unwrap();
    }
    let rate = scenario["samplerate"].as_u64().unwrap();
    d.config_set(key::SAMPLERATE, &ConfValue::U64(rate), None)
        .unwrap();
    d.config_set(key::LIMIT_SAMPLES, &ConfValue::U64(1_000_000), None)
        .unwrap();
    d.set_trigger(&trigger_for(&scenario["trigger"]));

    let mut out: Vec<Packet> = Vec::new();
    if d.start(&mut out).is_err() {
        check(&log, name);
        panic!("{name}: start failed");
    }
    let stop_waiting = !scenario["trigger"].is_null();
    let mut polls = 0;
    loop {
        polls += 1;
        assert!(polls < 1_000_000, "{name}: capture does not end");
        if stop_waiting && polls == 50 {
            // DSView's stop while waiting for the trigger: WAIT_UPLOAD
            let v = d.config_get(key::WAIT_UPLOAD, None).unwrap();
            assert_eq!(v, Some(ConfValue::Bool(true)));
        }
        if d.poll(false, &mut out) == Step::Remove {
            break;
        }
    }
    d.stop(&mut out).unwrap();
    check(&log, name);

    let data: Vec<u8> = out
        .iter()
        .filter_map(|p| match &p.body {
            Body::Logic { data, .. } => Some(data.as_slice()),
            _ => None,
        })
        .flatten()
        .copied()
        .collect();
    assert!(!data.is_empty(), "{name}: no data");
    if scenario["trigger"].is_null() {
        // exactly the capture's samples: limit aligned to 1024, n channels
        let samples = (1_000_000u64 + 1023) & !1023;
        assert_eq!(
            data.len() as u64,
            samples / 64 * n as u64 * 8,
            "{name}: data length"
        );
    }
    assert_eq!(
        data[..],
        expected[..data.len()],
        "{name}: data differs from the capture"
    );
    assert!(matches!(
        out.first().map(|p| &p.body),
        Some(Body::Header { .. })
    ));
    assert!(
        out.iter().any(|p| matches!(p.body, Body::Trigger(_))),
        "{name}: no trigger packet"
    );
    assert_eq!(
        out.last().map(|p| &p.body),
        Some(&Body::End),
        "{name}: no END"
    );
    drop(d);
    assert_eq!(
        *remaining.lock().unwrap(),
        Some(0),
        "{name}: capture steps left over"
    );
}

#[test]
fn captures_match_c() {
    let _g = setup();
    let manifest = dir().join("manifest.json");
    if !manifest.exists() {
        eprintln!("skipping: no captures");
        return;
    }
    let m: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    for s in m["scenarios"].as_array().unwrap() {
        if Path::new(&dir().join(s["pcapng"].as_str().unwrap())).exists() {
            eprintln!("scenario {}", s["scenario"]);
            run_capture(s);
        }
    }
}
