//! The protocol dock shows every decoder's table at once, one below the
//! other, and a table can go into its own window and back (Qt showed one
//! table at a time).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsview_formats::dsl::{LogicWriter, WriteChannel};
use dsview_gui::app::App;
use dsview_gui::hooks::Shared;
use dsview_session::{Paths, Session};
use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

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

/// A capture of two clocks with a `timing` decoder on each.
fn session_with_two_decoders() -> (Session, PathBuf) {
    dsview_decode::init(&[root().join("assets/decoders").as_path()]).unwrap();
    dsview_decode::load_all();
    let s = Session::new(
        Paths {
            data_dir: root().join("assets"),
            profile_dir: None,
            version: "test".into(),
        },
        Arc::new(NoUsb),
    );
    let n: u64 = 1 << 16;
    let clock = |half: u64| {
        let mut bits = vec![0u8; (n / 8) as usize];
        for i in 0..n {
            if (i / half) % 2 == 1 {
                bits[(i / 8) as usize] |= 1 << (i % 8);
            }
        }
        bits
    };
    let (a, b) = (clock(64), clock(100));
    let path = std::env::temp_dir().join(format!("dsv-proto-multi-{}.dsl", std::process::id()));
    LogicWriter {
        driver: "virtual-session".into(),
        samplerate: 1_000_000,
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
        decoders_json: r#"[{"id":"timing","channel":[{"data":0}],"options":{}},
                           {"id":"timing","channel":[{"data":1}],"options":{}}]"#
            .into(),
        session_json: "{}".into(),
    }
    .write(std::fs::File::create(&path).unwrap(), &[&a, &b])
    .unwrap();
    s.set_file(path.to_str().unwrap()).unwrap();
    let t0 = Instant::now();
    while (s.is_working() || s.is_decoding()) && t0.elapsed() < Duration::from_secs(120) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(s.state().decoders.len(), 2);
    (s, path)
}

#[test]
fn every_decoder_table_is_shown_and_one_can_go_into_its_own_window() {
    let (s, path) = session_with_two_decoders();
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
    // the protocol dock (D)
    std::thread::sleep(Duration::from_millis(220));
    h.event(egui::Event::Key {
        key: egui::Key::D,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    let tables = |h: &Harness| h.query_all_by_label("#").count();
    let states = |app: &Arc<Mutex<Option<App>>>| {
        let traces = s.state().decoders.clone();
        app.lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .protocol
            .table_states(&traces)
    };
    // both tables at once
    assert_eq!(tables(&h), 2, "both decoder tables shown");
    assert_eq!(states(&app), vec![(false, false), (false, false)]);

    // the first one into its own window (embedded by the harness): both
    // tables are still there, one of them in the window
    h.query_all_by_label("In its own window")
        .next()
        .expect("detach button")
        .click();
    h.run();
    assert_eq!(states(&app), vec![(false, true), (false, false)]);
    assert_eq!(tables(&h), 2, "the detached table is shown in its window");

    // and back into the dock
    h.query_by_label("Back into the dock")
        .expect("dock button")
        .click();
    h.run();
    assert_eq!(states(&app), vec![(false, false), (false, false)]);
    assert_eq!(tables(&h), 2);
    s.exit();
    let _ = std::fs::remove_file(&path);
}
