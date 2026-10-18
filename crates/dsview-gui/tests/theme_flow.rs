//! A theme switch through the window (Display > Themes), in its own test
//! binary: the icon set follows the theme globally.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

#[test]
fn latte_theme() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = dsview_decode::init(&[root().join("libsigrokdecode4DSL/decoders").as_path()]);
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
    let e = list.iter().find(|e| e.name == "Demo Logic").unwrap();
    assert!(s.set_device(e.handle));
    assert!(s.start_capture(false));
    assert!(s.wait(Duration::from_secs(60), |st| st.ended_seq > 0
        && !st.working
        && !st.decoding));
    let app: Arc<Mutex<Option<App>>> = Arc::new(Mutex::new(None));
    let (s2, a2) = (s.clone(), app.clone());
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 600.0))
        .wgpu()
        .build_ui(move |ui| {
            let mut a = a2.lock().unwrap();
            let app =
                a.get_or_insert_with(|| App::new(s2.clone(), Shared::default(), "test", ui.ctx()));
            app.frame(ui);
        });
    h.run();
    h.get_by_label("Display").click();
    h.run();
    h.get_by_label("Themes").click();
    h.run();
    h.get_by_label("Latte").click();
    h.run();
    {
        let a = app.lock().unwrap();
        let a = a.as_ref().unwrap();
        assert_eq!(a.options.frame.style, "latte");
        assert_eq!(a.view.palette, dsview_gui::theme::Theme::Latte.palette());
    }
    // the pointer away from the menus
    h.input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(1090.0, 590.0)));
    h.run();
    h.snapshot("theme_latte");
    s.exit();
}
