//! The window's flows around the captured data (`MainWindow`): "Save
//! captured data?" before another device is chosen or the window closes,
//! on a simulated DSLogic.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsview_gui::app::App;
use dsview_gui::hooks::Shared;
use dsview_session::{Paths, Session};
use dsview_usb::Speed;
use dsview_usb::sim::{Hub, SimBackend, SimConfig};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A session on a simulated DSLogic Plus (and the demo devices).
fn dslogic_session() -> Session {
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
            data_dir: root().join("DSView"),
            profile_dir: None,
            version: "test".into(),
        },
        sim,
    );
    let (list, _) = s.devices();
    let e = list
        .iter()
        .find(|e| e.name.contains("DSLogic"))
        .expect("simulated DSLogic listed");
    assert!(s.set_device(e.handle));
    s
}

fn capture(s: &Session) {
    use dsview_core::{ConfValue, key};
    s.set_config(key::LIMIT_SAMPLES, ConfValue::U64(65536), None);
    assert!(s.start_capture(false));
    assert!(s.wait(Duration::from_secs(60), |st| st.ended_seq > 0
        && !st.working));
    assert!(s.have_hardware_data());
}

fn harness(s: &Session) -> (Harness<'static>, Arc<Mutex<Option<App>>>) {
    let app: Arc<Mutex<Option<App>>> = Arc::new(Mutex::new(None));
    let (s2, a2) = (s.clone(), app.clone());
    let h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_ui(move |ui| {
            let mut a = a2.lock().unwrap();
            let app =
                a.get_or_insert_with(|| App::new(s2.clone(), Shared::default(), "test", ui.ctx()));
            app.frame(ui);
        });
    (h, app)
}

/// Another device chosen in the sampling bar: the question comes once per
/// capture; "No" switches.
#[test]
fn device_switch_offers_to_save() {
    let s = dslogic_session();
    capture(&s);
    let (mut h, app) = harness(&s);
    h.run();
    let demo = s
        .devices()
        .0
        .iter()
        .find(|e| e.name == "Demo Logic")
        .unwrap()
        .handle;
    app.lock().unwrap().as_mut().unwrap().bar.switch_to = Some(demo);
    h.run();
    h.get_by_label("Save captured data?");
    assert!(s.state().device.as_ref().unwrap().is_hardware());
    h.get_by_label("No").click();
    h.run();
    assert_eq!(s.state().device.as_ref().unwrap().handle, demo);
    assert!(h.query_by_label("Save captured data?").is_none());
    s.exit();
}

/// Closing the window with unsaved hardware data asks first (and keeps
/// the window); a second close of the same capture does not ask again.
#[test]
fn close_offers_to_save_once() {
    let s = dslogic_session();
    capture(&s);
    let (mut h, _app) = harness(&s);
    h.run();
    let close = |h: &mut Harness<'_>| {
        h.input_mut()
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .events
            .push(egui::ViewportEvent::Close);
        h.run();
    };
    close(&mut h);
    h.get_by_label("Save captured data?");
    // "No" closes the window (`able_to_close` returns true)
    h.get_by_label("No").click();
    let mut closed = false;
    for _ in 0..4 {
        h.step();
        closed |= h
            .output()
            .viewport_output
            .values()
            .any(|v| v.commands.contains(&egui::ViewportCommand::Close));
    }
    assert!(closed, "no Close command after No");
    h.run();
    close(&mut h);
    assert!(h.query_by_label("Save captured data?").is_none());
    s.exit();
}

/// Help > Log Options: "Save To File" and a level, OK applies them (the log
/// file opens; Qt opens it in overwrite mode here).
#[test]
fn log_options_dialog() {
    let dir = std::env::temp_dir().join(format!("dsv-logdlg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("DSView.log");
    dsview_gui::log::set_path(log.clone());
    let s = dslogic_session();
    let (mut h, app) = harness(&s);
    h.run();
    h.get_by_label("Help").click();
    h.run();
    h.get_by_label("Log Options").click();
    h.run();
    h.get_by_label("Save To File").click();
    h.run();
    h.get_by_label("Log Level").click();
    h.run();
    h.get_by_label("4").click();
    h.run();
    h.get_by_label("OK").click();
    h.run();
    {
        let a = app.lock().unwrap();
        let o = &a.as_ref().unwrap().options;
        assert!(o.able_save_log);
        assert_eq!(o.log_level, 4);
    }
    assert_eq!(dsview_gui::log::level(), 4);
    assert!(log.is_file());
    assert!(h.query_by_label("Log Options").is_none());
    s.exit();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Help > About shows the version and architecture; Esc closes it.
#[test]
fn about_window() {
    let s = dslogic_session();
    let (mut h, _app) = harness(&s);
    h.run();
    h.get_by_label("Help").click();
    h.run();
    h.get_by_label("About…").click();
    h.run();
    h.get_by_label_contains("DSView test (");
    h.get_by_label("All members of Sigrok project");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_label_contains("DSView test (").is_none());
    s.exit();
}
