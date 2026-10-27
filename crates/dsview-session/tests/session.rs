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
        dsview_decode::init(&[root().join("assets/decoders").as_path()]).unwrap();
        dsview_decode::load_all();
    });
    let s = Session::new(
        Paths {
            data_dir: root().join("assets"),
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

/// B4: the collect thread hands packets to the ingest thread and goes on
/// reading the device, also while the store is locked (a GUI frame, a live
/// decode): it used to ingest under that lock itself. The data and the
/// capture's end then arrive in order.
#[test]
fn collect_does_not_wait_for_the_store() {
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    let held = s.data().logic.lock().unwrap();
    let t0 = std::time::Instant::now();
    while s.is_collecting() {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "the collect thread waits for the store"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // the device is done, the ingest is not: the capture has not ended
    assert_eq!(s.state().ended_seq, 0);
    drop(held);
    wait(&s, "capture end", |st| st.ended_seq > 0 && !st.working);
    assert_eq!(s.data().logic.lock().unwrap().ring_sample_count(), 131_072);
    s.exit();
}

#[test]
fn logic_capture_estimate() {
    use dsview_session::session::logic_capture_bytes;
    assert_eq!(logic_capture_bytes(0, 16), 0);
    // a partial word still takes a whole one; the mip-map adds 1/63
    assert_eq!(logic_capture_bytes(1, 1), 9);
    assert_eq!(logic_capture_bytes(64 * 63, 1), 8 * 63 + 8);
    // 16 channels x 1G samples: 2 GiB of data, 32.5 MiB of mip-map
    let b = logic_capture_bytes(1 << 30, 16);
    assert!(b > 2 << 30 && b < (2 << 30) + (33 << 20), "{b}");
    assert_eq!(logic_capture_bytes(u64::MAX, 32), u64::MAX);
}

/// A capture that does not fit into the available memory is refused
/// before anything is cleared, with a message the GUI shows.
#[test]
fn capture_beyond_memory_is_refused() {
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    wait(&s, "capture end", |st| st.ended_seq > 0 && !st.working);
    // 131072 samples x 16 channels need 260 KiB; the old capture's buffer
    // counts as available, so 256 MiB (the headroom kept for the rest of the
    // system) leave room for it
    s.set_mem_available(|| Some(256 << 20));
    assert!(s.check_capture_memory().is_ok());
    s.set_config(key::LIMIT_SAMPLES, ConfValue::U64(16 << 20), None);
    s.update_sample_rate_list();
    s.commit_settings();
    let limit = s.sample_limit();
    assert!(limit >= 16 << 20, "{limit}");
    s.set_mem_available(|| Some(288 << 20));
    assert!(!s.start_capture(false));
    let e = s.state().start_error.clone().expect("no message");
    assert!(
        e.contains("GiB") && e.contains("Reduce the sample depth"),
        "{e}"
    );
    assert_eq!(s.state().started_seq, 1);
    // the shown capture is still there
    assert_eq!(s.data().logic.lock().unwrap().ring_sample_count(), 131_072);
    // where the available memory is unknown there is no check
    s.set_mem_available(|| None);
    s.state().start_error = None;
    assert!(s.start_capture(false));
    wait(&s, "capture end", |st| st.ended_seq > 1 && !st.working);
    assert_eq!(s.state().started_seq, 2);
    assert!(s.state().start_error.is_none());
    s.exit();
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
    // Cancel after three lines keeps them; progress rises to 100
    let mut seen = Vec::new();
    dsview_session::store::export_protocol_with(&t, &rows[..2], count, &path, &mut |p| {
        seen.push(p);
        seen.len() < 3
    })
    .unwrap();
    let text2 = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text2.lines().count(), 4);
    assert!(text.starts_with(&text2));
    let mut all = Vec::new();
    dsview_session::store::export_protocol_with(&t, &rows[..2], count, &path, &mut |p| {
        all.push(p);
        true
    })
    .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(all.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(all.last(), Some(&100));
    s.exit();
}

/// The protocol export as `.txt`: the same lines as the CSV, as a table whose
/// columns line up (F27: Qt's TXT was the CSV text).
#[test]
fn protocol_export_txt_is_a_table() {
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
    let count = s.data().logic.lock().unwrap().ring_sample_count();
    let dir = std::env::temp_dir().join(format!("dsv-proto-txt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (csv, txt) = (dir.join("p.csv"), dir.join("p.txt"));
    dsview_session::store::export_protocol(&t, &rows[..2], count, &csv).unwrap();
    dsview_session::store::export_protocol(&t, &rows[..2], count, &txt).unwrap();
    let csv = std::fs::read_to_string(&csv).unwrap();
    let txt = std::fs::read_to_string(&txt).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(csv.lines().count(), txt.lines().count());
    assert!(
        !txt.lines().next().unwrap().contains(','),
        "no CSV separators"
    );
    // each column starts at the same character in every line
    let starts = |l: &str, n: usize| -> Vec<usize> {
        let cells: Vec<&str> = l.split("  ").filter(|c| !c.trim().is_empty()).collect();
        cells
            .iter()
            .take(n)
            .map(|c| l.char_indices().count() - l[l.find(c.trim()).unwrap()..].chars().count())
            .collect()
    };
    let header = txt.lines().next().unwrap();
    assert!(header.starts_with("Id"), "{header}");
    let cols = starts(header, 3);
    assert_eq!(cols.len(), 3, "{header}");
    for l in txt.lines().skip(1).take(50) {
        // the Id and Time columns are always filled
        assert_eq!(starts(l, 2), cols[..2], "{l}");
    }
    // the same values as the CSV's first data line
    let first_csv: Vec<&str> = csv.lines().nth(1).unwrap().split(',').collect();
    let first_txt = txt.lines().nth(1).unwrap();
    assert!(first_txt.starts_with(first_csv[0]), "{first_txt}");
    assert!(first_txt.contains(first_csv[1]), "{first_txt}");
    s.exit();
}

/// A range between cursors: the CSV export holds exactly the full export's
/// rows from the start sample on, and a saved .dsl reloads as the range.
#[test]
fn ranges_for_export_and_save() {
    use dsview_formats::export::Format;
    use dsview_session::store;
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    wait(&s, "capture end", |st| st.ended_seq > 0 && !st.working);
    let dir = std::env::temp_dir().join(format!("dsv-range-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let csv = Format::from_id("csv").unwrap();

    // original data: one row per sample, so rows can be compared by index
    let (a, b) = (40_000u64, 60_000u64);
    store::export_range(&s, &dir.join("all.csv"), csv, None, true).unwrap();
    store::export_range(&s, &dir.join("part.csv"), csv, Some((a, b)), true).unwrap();
    let rows = |p: &str| -> Vec<String> {
        std::fs::read_to_string(dir.join(p))
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with(';') && !l.starts_with("Time"))
            .map(|l| l.split_once(',').map_or(l, |x| x.1).to_owned())
            .collect()
    };
    let (all, part) = (rows("all.csv"), rows("part.csv"));
    // C cuts at whole bytes: from a / 8 × 8 to b / 8 × 8
    let (from, to) = ((a / 8 * 8) as usize, (b / 8 * 8) as usize);
    assert_eq!(part.len(), to - from, "rows");
    assert_eq!(part, all[from..to]);

    // .dsl: 64-sample units, reloaded as a capture of its own
    store::save_dsl_range(&s, &dir.join("part.dsl"), "test", Some((a, b))).unwrap();
    let mut r = dsview_formats::dsl::DslReader::open(&dir.join("part.dsl")).unwrap();
    let (fa, fb) = (a - a % 64, b.div_ceil(64) * 64);
    assert_eq!(r.header().total_samples, fb - fa);
    let store_ = s.data().logic.lock().unwrap();
    for pos in [0usize, 3] {
        let bits = r.logic_channel(pos).unwrap();
        let ch = r.header().channels[pos].index as i32;
        for i in (0..fb - fa).step_by(97) {
            let want = store_.get_sample(fa + i, ch);
            let got = bits[(i / 8) as usize] >> (i % 8) & 1 == 1;
            assert_eq!(got, want, "channel {ch} sample {}", fa + i);
        }
    }
    drop(store_);
    let _ = std::fs::remove_dir_all(&dir);
    s.exit();
}

/// F11: a .dsl whose length is not a multiple of 64 opens unshifted and
/// saves back whole: every sample at its index, before and after the round
/// trip, and the saved header keeps the length.
#[test]
fn unaligned_dsl_round_trip() {
    use dsview_formats::dsl::{DslReader, LogicWriter, WriteChannel};
    use dsview_session::store;
    let (_g, s) = session("Demo Logic");
    let dir = std::env::temp_dir().join(format!("dsv-f11-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // 1001 samples, so the last byte is partial too; channel 0 rises at
    // 500, channel 1 toggles every 7 samples and is high at the end
    let n: u64 = 1001;
    let level = |ch: usize, i: u64| {
        if ch == 0 {
            i >= 500
        } else {
            (i / 7).is_multiple_of(2)
        }
    };
    let bits: Vec<Vec<u8>> = (0..2)
        .map(|ch| {
            // the writer takes whole 64-sample words
            let mut v = vec![0u8; (n.div_ceil(64) * 8) as usize];
            for i in 0..n {
                if level(ch, i) {
                    v[(i / 8) as usize] |= 1 << (i % 8);
                }
            }
            v
        })
        .collect();
    let src = dir.join("src.dsl");
    LogicWriter {
        driver: "virtual-session".into(),
        samplerate: 1_000_000,
        total_samples: n,
        trigger_time_ms: 0,
        trigger_pos: 0,
        channels: (0..2)
            .map(|i| WriteChannel {
                index: i,
                name: i.to_string(),
            })
            .collect(),
        decoders_json: "[]".into(),
        session_json: "{}".into(),
    }
    .write(
        std::fs::File::create(&src).unwrap(),
        &[&bits[0][..], &bits[1][..]],
    )
    .unwrap();
    let check = |s: &Session, what: &str| {
        let st = s.data().logic.lock().unwrap();
        assert_eq!(st.sample_count(), n, "{what}: length");
        for ch in 0..2 {
            for i in 0..n {
                assert_eq!(
                    st.get_sample(i, ch as i32),
                    level(ch, i),
                    "{what}: ch {ch} sample {i}"
                );
            }
        }
    };
    s.set_file(src.to_str().unwrap()).unwrap();
    wait(&s, "load", |st| st.ended_seq > 0 && !st.working);
    check(&s, "opened");

    let out = dir.join("out.dsl");
    store::save_dsl(&s, &out, "test").unwrap();
    let mut r = DslReader::open(&out).unwrap();
    assert_eq!(r.header().total_samples, n);
    for (pos, want) in bits.iter().enumerate() {
        let got = r.logic_channel(pos).unwrap();
        let bytes = n.div_ceil(8) as usize;
        assert!(
            got.len() >= bytes,
            "saved channel {pos}: {} bytes",
            got.len()
        );
        // the bits past the data in the last byte are padding
        let valid = |v: &[u8]| {
            let mut v = v[..bytes].to_vec();
            v[bytes - 1] &= ((1u16 << (n - (bytes as u64 - 1) * 8)) - 1) as u8;
            v
        };
        assert_eq!(valid(&got), valid(want), "saved channel {pos}");
    }
    // a range up to the end: whole words, cut by the header's length
    let part = dir.join("part.dsl");
    store::save_dsl_range(&s, &part, "test", Some((64, n))).unwrap();
    let mut r = DslReader::open(&part).unwrap();
    assert_eq!(r.header().total_samples, n - 64);
    let got = r.logic_channel(1).unwrap();
    assert_eq!(got.len() % 8, 0);
    for i in 0..n - 64 {
        assert_eq!(
            got[(i / 8) as usize] >> (i % 8) & 1 == 1,
            level(1, i + 64),
            "range sample {i}"
        );
    }

    let seq = s.state().ended_seq;
    s.set_file(out.to_str().unwrap()).unwrap();
    wait(&s, "reload", |st| st.ended_seq > seq && !st.working);
    check(&s, "reopened");
    let _ = std::fs::remove_dir_all(&dir);
    s.exit();
}

/// File > Store then Load: the profile brings the settings back; Default
/// loads the shipped one.
#[test]
fn profiles_store_load_default() {
    let (_g, s) = session("Demo Logic");
    let dir = std::env::temp_dir().join(format!("dsv-prof-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("p.dsc");
    let logic = dsview_core::channel_type::LOGIC;
    s.set_signal_name(3, logic, "MOSI");
    s.store_profile(&p).unwrap();
    s.set_signal_name(3, logic, "x");
    s.load_profile(&p).unwrap();
    let name = s
        .state()
        .signals
        .iter()
        .find(|x| x.index == 3)
        .map(|x| x.name.clone());
    assert_eq!(name.as_deref(), Some("MOSI"));
    // the demo's decoders come back with it
    assert_eq!(s.state().decoders.len(), 4);
    let d = s.default_profile_path().unwrap();
    assert!(d.ends_with("res/virtual-demo0.def.dsc"), "{}", d.display());
    let _ = std::fs::remove_dir_all(&dir);
    s.exit();
}

/// A profile the user loads does not set decoder options that run a
/// program or read a file; they wait for the user's yes.
#[test]
fn guarded_decoder_options_wait_for_confirmation() {
    use dsview_decode::OptValue;
    let (_g, s) = session("Demo Logic");
    let dir = std::env::temp_dir().join(format!("dsv-guard-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("p.dsc");
    s.store_profile(&p).unwrap();
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    doc["decoder"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "arm_itm",
            "channel": [],
            "options": {"objdump": "/bin/false", "elffile": "", "objdump_opts": "-x"}
        }));
    std::fs::write(&p, doc.to_string()).unwrap();
    s.load_profile(&p).unwrap();
    let held = s.state().held_decoders.clone();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].settings, ["arm_itm: objdump = /bin/false"]);
    let opts = |s: &Session| {
        let st = s.state();
        let t = st.decoders.last().unwrap();
        t.stack.decoders()[0].options.clone()
    };
    // loaded with the default, the other options as stored
    assert!(!opts(&s).contains_key("objdump"));
    assert_eq!(
        opts(&s).get("objdump_opts"),
        Some(&OptValue::Str("-x".into()))
    );
    let n = s.state().decoders.len();
    assert!(s.apply_held_decoder(&held[0]));
    assert_eq!(s.state().decoders.len(), n);
    assert_eq!(
        opts(&s).get("objdump"),
        Some(&OptValue::Str("/bin/false".into()))
    );
    // DSView's own profiles are trusted
    s.state().held_decoders.clear();
    assert!(dsview_session::store::load_config_from_json(&s, &doc));
    assert!(s.state().held_decoders.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
    s.exit();
}

/// A decode region (the decoder options' start and end cursors): the
/// annotations lie inside it.
#[test]
fn decode_region() {
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    wait(&s, "capture end", |st| st.ended_seq > 0 && !st.working);
    wait(&s, "decoding", |st| !st.decoding);
    let t = s.state().decoders[0].clone();
    let full = t.stack.result_count();
    t.stack.set_region(40_000, 60_000);
    s.start_decode(0);
    wait(&s, "decoding", |st| !st.decoding);
    let part = t.stack.result_count();
    assert!(part > 0 && part < full, "{part} of {full}");
    for (row, _) in t.stack.rows().iter().enumerate() {
        for a in t.stack.row_sorted(row, 131_072) {
            assert!(
                a.start >= 40_000 && a.end <= 60_001,
                "{}..{}",
                a.start,
                a.end
            );
        }
    }
    s.exit();
}

/// The protocol list's number formats: I²C's "Data write: 04" in decimal,
/// binary and ASCII.
#[test]
fn annotation_number_formats() {
    use dsview_session::numfmt::Format;
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    wait(&s, "capture end", |st| st.ended_seq > 0 && !st.working);
    wait(&s, "decoding", |st| !st.decoding);
    let t = s.state().decoders[0].clone();
    let st = &t.stack;
    let a = (0..st.rows().len())
        .flat_map(|r| st.row_sorted(r, 131_072))
        .find(|a| a.text().starts_with("Data write: 04"))
        .expect("an I2C data write");
    assert_eq!(a.text_in(Format::Hex), "Data write: 04");
    assert_eq!(a.text_in(Format::Dec), "Data write: 4");
    assert_eq!(a.text_in(Format::Bin), "Data write: 00000100");
    assert_eq!(a.text_in(Format::Ascii), "Data write: [04]");
    st.set_format(Format::Dec);
    assert_eq!(st.format(), Format::Dec);
    s.exit();
}

/// A hardware device's settings go to `<driver><mode>.ses.dsc`, the file
/// Qt DSView uses; without it the per-language file of an older DSView is
/// read.
#[test]
fn hardware_profile_names() {
    use dsview_usb::Speed;
    use dsview_usb::sim::{Hub, SimBackend, SimConfig};
    let (_g, _demo) = session("Demo Logic");
    _demo.exit();
    let dir = std::env::temp_dir().join(format!("dsv-hwprof-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let open = |language: i64| {
        let hub = Arc::new(Mutex::new(Hub::new(SimConfig::new(
            0x2A0E,
            0x0034,
            Speed::High,
        ))));
        let sim = Arc::new(SimBackend::new(
            hub,
            Arc::new(|d: Duration| std::thread::sleep(d)),
        ));
        let s = Session::new(
            Paths {
                data_dir: root().join("assets"),
                profile_dir: Some(dir.clone()),
                version: "test".into(),
            },
            sim,
        );
        s.state().language = language;
        let (list, _) = s.devices();
        let e = list
            .iter()
            .find(|e| matches!(e.kind, Kind::Usb { .. }))
            .expect("simulated DSLogic");
        assert!(s.set_device(e.handle));
        s
    };
    let logic = dsview_core::channel_type::LOGIC;
    let name3 = |s: &Session| {
        s.state()
            .signals
            .iter()
            .find(|x| x.index == 3)
            .map(|x| x.name.clone())
            .unwrap()
    };
    let s = open(0);
    s.set_signal_name(3, logic, "MOSI");
    s.save_config();
    s.exit();
    let files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].ends_with("0.ses.dsc"), "{files:?}");
    assert_eq!(name3(&open(0)), "MOSI");
    // only an older per-language profile
    let legacy = files[0].replace(".ses.dsc", ".ses25.dsc");
    std::fs::rename(dir.join(&files[0]), dir.join(&legacy)).unwrap();
    let s = open(0);
    assert_eq!(name3(&s), "MOSI");
    s.exit();
    // with the language known, its own file wins over the first by name
    let other = files[0].replace(".ses.dsc", ".ses31.dsc");
    let text = std::fs::read_to_string(dir.join(&legacy))
        .unwrap()
        .replace("\"MOSI\"", "\"MISO\"");
    std::fs::write(dir.join(&other), text).unwrap();
    // (each exit writes the new-format file, which is read first)
    let new = dir.join(&files[0]);
    let _ = std::fs::remove_file(&new);
    let s = open(0);
    assert_eq!(name3(&s), "MOSI", "first by name without a language");
    s.exit();
    let _ = std::fs::remove_file(&new);
    let s = open(31);
    assert_eq!(name3(&s), "MISO");
    s.exit();
    let _ = std::fs::remove_dir_all(&dir);
}

/// srzip export of a logic capture: `version`, `metadata`, then the
/// samples in packets of 8192, all of the capture.
#[test]
fn srzip_export_holds_the_capture() {
    use dsview_session::store::{Progress, export_range_with};
    use std::io::Read;
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    wait(&s, "captured", |st| {
        st.ended_seq > 0 && !st.working && !st.decoding
    });
    let path = std::env::temp_dir().join(format!("dsv-srzip-{}.srzip", std::process::id()));
    let fmt = dsview_formats::export::Format::Srzip;
    export_range_with(&s, &path, fmt, None, false, &Progress::default()).unwrap();
    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut text = |name: &str| {
        let mut v = Vec::new();
        z.by_name(name).unwrap().read_to_end(&mut v).unwrap();
        v
    };
    assert_eq!(text("version"), b"2");
    let meta = String::from_utf8(text("metadata")).unwrap();
    assert!(
        meta.starts_with("[global]\nsigrok version=0.2.0\n\n[device 1]\ncapturefile=logic-1\n"),
        "{meta}"
    );
    let unitsize: usize = meta
        .lines()
        .find_map(|l| l.strip_prefix("unitsize="))
        .unwrap()
        .parse()
        .unwrap();
    let samples = s.data().logic.lock().unwrap().ring_sample_count() as usize;
    let mut total = 0;
    for i in 1.. {
        let mut v = Vec::new();
        match z.by_name(&format!("logic-1-{i}")) {
            Ok(mut e) => e.read_to_end(&mut v).unwrap(),
            Err(_) => break,
        };
        total += v.len();
    }
    assert_eq!(total, samples * unitsize);
    let _ = std::fs::remove_file(&path);
    s.exit();
}

/// Save and export count their progress to 100%; a cancelled save leaves
/// no file, a cancelled export keeps what it wrote.
#[test]
fn store_progress_and_cancel() {
    use dsview_session::store::{Progress, export_range_with, save_dsl_range_with};
    use std::sync::atomic::Ordering;
    let (_g, s) = session("Demo Logic");
    assert!(s.start_capture(false));
    wait(&s, "captured", |st| {
        st.ended_seq > 0 && !st.working && !st.decoding
    });
    let dir = std::env::temp_dir().join(format!("dsv-storeprog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (dsl, csv) = (dir.join("a.dsl"), dir.join("a.csv"));
    for range in [None, Some((1000, 70_000))] {
        let p = Progress::default();
        save_dsl_range_with(&s, &dsl, "test", range, &p).unwrap();
        assert!(dsl.is_file());
        assert_eq!(p.percent(), 100);
        assert_eq!(
            p.done.load(Ordering::Relaxed),
            p.total.load(Ordering::Relaxed)
        );
        assert!(p.total.load(Ordering::Relaxed) > 0);
        let p = Progress::default();
        export_range_with(
            &s,
            &csv,
            dsview_formats::export::Format::Csv,
            range,
            true,
            &p,
        )
        .unwrap();
        assert_eq!(
            p.done.load(Ordering::Relaxed),
            p.total.load(Ordering::Relaxed),
            "{range:?}"
        );
    }
    let p = Progress::default();
    p.cancel.store(true, Ordering::Relaxed);
    std::fs::remove_file(&dsl).unwrap();
    save_dsl_range_with(&s, &dsl, "test", None, &p).unwrap();
    assert!(!dsl.exists());
    export_range_with(
        &s,
        &csv,
        dsview_formats::export::Format::Csv,
        None,
        true,
        &p,
    )
    .unwrap();
    assert!(csv.is_file());
    let _ = std::fs::remove_dir_all(&dir);
    s.exit();
}
