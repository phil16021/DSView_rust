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
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = dsview_decode::init(&[root().join("assets/decoders").as_path()]);
        dsview_decode::load_all();
    });
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
        // windows that settle in a few frames (sizing, a first texture)
        .with_max_steps(16)
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
    let log = dir.join("DSViewR.log");
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
    h.get_by_label_contains("DSViewR test (");
    h.get_by_label("All members of Sigrok project");
    h.key_press(egui::Key::Escape);
    h.run();
    assert!(h.query_by_label_contains("DSViewR test (").is_none());
    s.exit();
}

/// MainWindow's shortcuts: keys with Ctrl are ignored, a second key within
/// 200 ms too; T/D/M/R toggle only the docks the work mode offers, and
/// each mode keeps its own; O opens the device options.
#[test]
fn shortcuts_and_docks_per_mode() {
    let s = dslogic_session();
    let (mut h, app) = harness(&s);
    h.run();
    let press = |h: &mut Harness<'_>, k: egui::Key, m: egui::Modifiers| {
        std::thread::sleep(Duration::from_millis(220));
        h.key_press_modifiers(m, k);
        h.run();
    };
    let docks = |app: &Arc<Mutex<Option<App>>>| {
        let a = app.lock().unwrap();
        let f = &a.as_ref().unwrap().options.frame;
        (f.logic_dock.clone(), f.dso_dock.clone())
    };
    let none = egui::Modifiers::NONE;
    // logic: D opens the protocol dock
    press(&mut h, egui::Key::D, none);
    assert!(docks(&app).0.decode);
    // Ctrl+D is not a shortcut
    press(&mut h, egui::Key::D, egui::Modifiers::CTRL);
    assert!(docks(&app).0.decode);
    // two keys within 200 ms (here in one frame): the second is dropped
    std::thread::sleep(Duration::from_millis(220));
    for _ in 0..2 {
        // raw events: kittest's key_press queues each for its own frame
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: none,
        });
    }
    h.run();
    assert!(docks(&app).0.measure);
    // the oscilloscope: no protocol dock; M is its own
    let dso = s
        .devices()
        .0
        .iter()
        .find(|e| e.name == "Demo Oscilloscope")
        .unwrap()
        .handle;
    assert!(s.set_device(dso));
    h.run();
    press(&mut h, egui::Key::D, none);
    press(&mut h, egui::Key::T, none);
    let (logic, dso_docks) = docks(&app);
    assert!(!dso_docks.decode && dso_docks.trigger && !dso_docks.measure);
    assert!(logic.decode && logic.measure && !logic.trigger);
    // 0 makes channel 0's dial active, ↑ steps its V/div
    use dsview_core::key;
    let vdiv = |s: &Session| s.config(key::PROBE_VDIV, Some(0)).map(|v| v.get_u64());
    let before = vdiv(&s);
    press(&mut h, egui::Key::ArrowUp, none);
    assert_eq!(vdiv(&s), before, "no active dial yet");
    press(&mut h, egui::Key::Num0, none);
    press(&mut h, egui::Key::ArrowUp, none);
    assert_ne!(vdiv(&s), before);
    press(&mut h, egui::Key::ArrowDown, none);
    assert_eq!(vdiv(&s), before);
    // O: the device options
    press(&mut h, egui::Key::O, none);
    h.get_by_label("Device Options");
    s.exit();
}

/// At start-up: the driver note when no udev rule is installed, then the
/// document hint; "Not Show Again" turns the hint off.
#[test]
fn startup_dialogs() {
    let s = dslogic_session();
    let (mut h, app) = harness(&s);
    h.run();
    {
        let mut a = app.lock().unwrap();
        let a = a.as_mut().unwrap();
        a.startup = Some(dsview_gui::startup::Startup::new(
            &a.options,
            &root().join("assets"),
        ));
    }
    h.run();
    let rule = dsview_gui::startup::RULE_PATHS
        .iter()
        .any(|p| std::path::Path::new(p).exists());
    if !rule {
        h.get_by_label_contains("needs a udev rule");
        h.get_by_label("Ignore").click();
        h.run();
    }
    h.get_by_label("Not Show Again").click();
    h.run();
    {
        let a = app.lock().unwrap();
        let a = a.as_ref().unwrap();
        assert!(!a.options.history.show_documents);
        assert!(a.options.history.show_driver_hint);
        assert!(a.startup.is_none());
    }
    s.exit();
}

/// AccessKit audit: every dialog the window opens is drawn once; the
/// harness's accessibility check fails on a widget without a name.
#[test]
fn accessibility_of_the_dialogs() {
    let s = dslogic_session();
    let (mut h, _app) = harness(&s);
    h.run();
    let open = |h: &mut Harness<'_>, path: &[&str], window: &str| {
        for item in path {
            h.get_by_label(item).click();
            h.run();
        }
        assert!(
            h.query_all_by_label(window).next().is_some(),
            "{window} did not open"
        );
        h.key_press(egui::Key::Escape);
        h.run();
        h.run();
    };
    // logic
    open(&mut h, &["Display", "Options…"], "Application Options");
    open(&mut h, &["Channels"], "Channels");
    open(&mut h, &["Options"], "Device Options");
    // the search dock's pattern dialog
    std::thread::sleep(Duration::from_millis(220));
    h.key_press(egui::Key::R);
    h.run();
    h.get_by_label_contains("Search: ").click();
    h.run();
    assert!(h.query_all_by_label("Search Options").next().is_some());
    h.key_press(egui::Key::Escape);
    h.run();
    // the decoder options, from the picker
    std::thread::sleep(Duration::from_millis(220));
    h.key_press(egui::Key::D);
    h.run();
    h.get_by_label("Search decoders").click();
    h.run();
    h.get_by_label("Search decoders").type_text("uart");
    h.run();
    h.get_all_by_label_contains("(1:uart)")
        .next()
        .unwrap()
        .click();
    h.run();
    assert!(h.query_all_by_label_contains("options").next().is_some());
    h.key_press(egui::Key::Escape);
    h.run();
    // the oscilloscope's functions
    let dso = s
        .devices()
        .0
        .iter()
        .find(|e| e.name == "Demo Oscilloscope")
        .unwrap()
        .handle;
    assert!(s.set_device(dso));
    h.run();
    open(&mut h, &["Function", "FFT…"], "FFT Options");
    open(&mut h, &["Function", "Math…"], "Math Options");
    open(&mut h, &["Function", "Lissajous…"], "Lissajous Options");
    open(&mut h, &["Function", "Reference…"], "Reference Waveforms");
    open(&mut h, &["Function", "Histogram…"], "Histogram");
    open(
        &mut h,
        &["Function", "Channel measure…"],
        "Channel-to-channel measure",
    );
    open(&mut h, &["Options"], "Device Options");
    s.exit();
}
