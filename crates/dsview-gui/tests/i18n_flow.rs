//! The language switch through the window (Help > Language), in its own
//! test binary: the shown language is global, and the other window tests
//! look for English labels.

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
fn switch_to_chinese_and_back() {
    if dsview_gui::i18n::cjk_font().is_none() {
        eprintln!("no CJK font on this system: skipped");
        return;
    }
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
    let app: Arc<Mutex<Option<App>>> = Arc::new(Mutex::new(None));
    let (s2, a2) = (s.clone(), app.clone());
    let mut h = Harness::builder()
        .with_size(egui::vec2(900.0, 120.0))
        .wgpu()
        .build_ui(move |ui| {
            let mut a = a2.lock().unwrap();
            let app = a.get_or_insert_with(|| {
                dsview_gui::i18n::add_cjk_font(ui.ctx());
                App::new(s2.clone(), Shared::default(), "test", ui.ctx())
            });
            app.frame(ui);
        });
    h.run();
    h.get_by_label("Help").click();
    h.run();
    h.get_by_label("Language").click();
    h.run();
    h.get_by_label("中文").click();
    h.run();
    std::thread::sleep(Duration::from_millis(50));
    h.run();
    // `IDS_TOOLBAR_FILE`, `IDS_TOOLBAR_RUN_START` without their accelerators
    h.get_by_label("文件");
    h.get_by_label("开始");
    assert_eq!(dsview_gui::i18n::language(), 25);
    assert_eq!(
        app.lock().unwrap().as_ref().unwrap().options.frame.language,
        25
    );
    h.snapshot("app_chinese");
    // and back
    h.get_by_label("帮助").click();
    h.run();
    h.get_by_label("语言").click();
    h.run();
    h.get_by_label("English").click();
    h.run();
    h.get_by_label("File");
    s.exit();
}
