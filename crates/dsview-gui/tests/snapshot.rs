//! The time view of the protocol.demo capture, rendered off-screen
//! (egui_kittest on wgpu; lavapipe without a GPU) and compared with the
//! stored snapshots in `tests/snapshots` (UPDATE_SNAPSHOTS=1 rewrites them).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dsview_gui::timeview::TimeView;
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

fn demo_session() -> Session {
    let _ = dsview_decode::init(&[root().join("libsigrokdecode4DSL/decoders").as_path()]);
    dsview_decode::load_all();
    let s = Session::new(
        Paths {
            data_dir: root().join("DSView"),
            profile_dir: None,
            version: "test".into(),
        },
        Arc::new(NoUsb),
    );
    let (list, _) = s.devices();
    let e = list.iter().find(|e| e.name == "Demo Logic").unwrap();
    assert!(s.set_device(e.handle));
    assert!(s.start_capture(false));
    assert!(s.wait(Duration::from_secs(60), |st| st.ended_seq > 0
        && !st.working
        && !st.decoding));
    s
}

#[test]
fn protocol_demo_view() {
    let s = demo_session();
    let mut view = TimeView::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
            // 40..60 k samples, with two cursors (as the transcript does)
            if view.cursors.samples.is_empty() {
                view.show_region(&s2, 40_000, 60_000);
                view.cursors.samples = vec![45_147, 50_223];
            }
        });
    harness.run();
    harness.snapshot("protocol_demo_view");
    s.exit();
}
