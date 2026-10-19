//! M6c's exit check: the protocol table of a decoder with more than 100 000
//! annotations stays interactive (a clock in a capture file, decoded by
//! `timing`). The columns are sorted once per result
//! set (`ProtocolDock::cols_cache`), the rows drawn are the visible ones.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsview_formats::dsl::{LogicWriter, WriteChannel};
use dsview_gui::app::App;
use dsview_gui::hooks::Shared;
use dsview_session::{Paths, Session};
use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};
use egui_kittest::Harness;

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

#[test]
fn a_table_of_100k_rows_stays_interactive() {
    dsview_decode::init(&[root().join("libsigrokdecode4DSL/decoders").as_path()]).unwrap();
    dsview_decode::load_all();
    let s = Session::new(
        Paths {
            data_dir: root().join("DSView"),
            profile_dir: None,
            version: "test".into(),
        },
        Arc::new(NoUsb),
    );
    // a capture of a clock (an edge every 16 samples) with the timing
    // decoder, which annotates every edge
    let n: u64 = 4 << 20;
    let mut bits = vec![0u8; (n / 8) as usize];
    for i in 0..n {
        if (i / 16) % 2 == 1 {
            bits[(i / 8) as usize] |= 1 << (i % 8);
        }
    }
    let path = std::env::temp_dir().join(format!("dsv-proto-large-{}.dsl", std::process::id()));
    LogicWriter {
        driver: "virtual-session".into(),
        samplerate: 10_000_000,
        total_samples: n,
        trigger_time_ms: 0,
        trigger_pos: 0,
        channels: vec![WriteChannel {
            index: 0,
            name: "CLK".into(),
        }],
        decoders_json: r#"[{"id":"timing","channel":[{"data":0}],"options":{}}]"#.into(),
        session_json: "{}".into(),
    }
    .write(std::fs::File::create(&path).unwrap(), &[&bits])
    .unwrap();
    s.set_file(path.to_str().unwrap()).unwrap();
    let t0 = Instant::now();
    while (s.is_working() || s.is_decoding()) && t0.elapsed() < Duration::from_secs(600) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let t = s.state().decoders[0].clone();
    let count = s.data().logic.lock().unwrap().ring_sample_count();
    let rows: Vec<usize> = (0..t.stack.rows().len())
        .map(|r| t.stack.row_sorted(r, count).len())
        .collect();
    let most = rows.iter().copied().max().unwrap_or(0);
    // what each frame would cost without the cache
    let sort = Instant::now();
    for r in 0..t.stack.rows().len() {
        let _ = t.stack.row_sorted(r, count);
    }
    let sort = sort.elapsed();
    assert!(most > 100_000, "rows {rows:?} of {count} samples");

    let app: Arc<Mutex<Option<App>>> = Arc::new(Mutex::new(None));
    let (s2, a2) = (s.clone(), app.clone());
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .with_max_steps(16)
        .build_ui(move |ui| {
            let mut a = a2.lock().unwrap();
            let app =
                a.get_or_insert_with(|| App::new(s2.clone(), Shared::default(), "test", ui.ctx()));
            app.frame(ui);
        });
    h.run();
    // the protocol dock (D), then frames with the table shown
    std::thread::sleep(Duration::from_millis(220));
    h.event(egui::Event::Key {
        key: egui::Key::D,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    // the table is shown: its header and its first row
    use egui_kittest::kittest::Queryable;
    assert!(
        h.query_all_by_label("#").next().is_some(),
        "no protocol table"
    );
    let first = Instant::now();
    h.step();
    let first = first.elapsed();
    let n = 20;
    let t1 = Instant::now();
    for _ in 0..n {
        h.step();
    }
    let per_frame = t1.elapsed() / n;
    eprintln!(
        "protocol table: {most} rows; first frame {first:?}, then {per_frame:?} per frame \
         (collecting and sorting the rows once: {sort:?})"
    );
    // a frame after the first must not sort the columns again
    let limit = if cfg!(debug_assertions) { 250 } else { 16 };
    assert!(
        per_frame < Duration::from_millis(limit),
        "{per_frame:?} per frame with {most} rows"
    );
    s.exit();
    let _ = std::fs::remove_file(&path);
}
