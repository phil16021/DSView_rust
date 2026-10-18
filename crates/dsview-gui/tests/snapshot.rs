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

/// The demo device in `mode`, after one instant capture.
fn demo_in_mode(mode: dsview_core::Mode) -> Session {
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
    s.switch_work_mode(mode as i32);
    if mode == dsview_core::Mode::Analog {
        // analog captures until stopped: stop once data arrived
        assert!(s.start_capture(false));
        let t0 = std::time::Instant::now();
        while s.data().analog.lock().unwrap().sample_count() < 2000 {
            assert!(t0.elapsed() < Duration::from_secs(30), "no analog data");
            std::thread::sleep(Duration::from_millis(20));
        }
        s.stop_capture();
    } else {
        assert!(s.start_capture(true));
    }
    assert!(s.wait(Duration::from_secs(60), |st| st.ended_seq > 0
        && !st.working));
    s
}

#[test]
fn dso_demo_view() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut view = TimeView::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("dso_demo_view");
    s.exit();
}

#[test]
fn analog_demo_view() {
    let s = demo_in_mode(dsview_core::Mode::Analog);
    let mut view = TimeView::default();
    let s2 = s.clone();
    let mut fixed = false;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
            // how much the stream delivered varies: show samples it always has
            if !fixed {
                view.show_region(&s2, 0, 1000);
                fixed = true;
            }
        });
    harness.run();
    harness.snapshot("analog_demo_view");
    s.exit();
}

#[test]
fn dso_trigger_dock() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut dock = dsview_gui::dsotrigger::DsoTriggerDock::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(280.0, 520.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("dso_trigger_dock");
    s.exit();
}

#[test]
fn dso_measure_bar() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut view = TimeView::default();
    // Freq and PK-PK of channel 0, RMS of channel 1
    view.dso.measures[0] = (0, 1);
    view.dso.measures[1] = (0, 16);
    view.dso.measures[2] = (1, 14);
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 60.0))
        .wgpu()
        .build_ui(move |ui| {
            view.dso_measure_bar(ui, &s2);
        });
    harness.run();
    harness.snapshot("dso_measure_bar");
    // the values the control API reports
    let m = dsview_control::api::dso_measurements(&s, 0).unwrap();
    assert_ne!(m["frequency"], "--");
    s.exit();
}

#[test]
fn fft_pane() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut fft = dsview_gui::fftview::FftState::default();
    fft.specs[0].enabled = true;
    fft.specs[0].window = 1;
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 300.0))
        .wgpu()
        .build_ui(move |ui| {
            fft.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("fft_pane");
    s.exit();
}

#[test]
fn dso_math_view() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut view = TimeView::default();
    let m = &mut view.dso.math;
    m.enabled = true;
    m.ty = dsview_dsp::math::MathType::Add;
    (m.src1, m.src2, m.filter_width, m.zero_vrate) = (0, 1, 10, 0.5);
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("dso_math_view");
    s.exit();
}

#[test]
fn lissajous_view() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut view = TimeView::default();
    view.dso.lissajous.enabled = true;
    (
        view.dso.lissajous.x,
        view.dso.lissajous.y,
        view.dso.lissajous.percent,
    ) = (0, 1, 100);
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("lissajous_view");
    s.exit();
}

#[test]
fn dso_reference_view() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut view = TimeView::default();
    let mut samples = {
        let store = s.data().dso.lock().unwrap();
        let n = store.sample_count() as usize;
        store.get_samples(0, 0).unwrap()[..n].to_vec()
    };
    samples.reverse();
    view.dso.refs.waves.push(dsview_viewmodel::dso::RefWave {
        index: 0,
        samples,
        samplerate: s.state().cur_snap_samplerate as f64,
        hw_offset: 128,
        mv_per_code: 1000.0 * 10.0 / 255.0,
        colour: [238, 178, 17],
        name: "Ref 1".into(),
    });
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("dso_reference_view");
    s.exit();
}

#[test]
fn dso_histogram_dialog() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut view = TimeView::default();
    view.dso.analysis.histogram = Some((0, false));
    view.dso.analysis.chan_measure = Some((0, 1));
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 900.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("dso_analysis_dialogs");
    s.exit();
}

fn click(h: &mut Harness<'_>, x: f32, y: f32) {
    let pos = egui::pos2(x, y);
    let ev = &mut h.input_mut().events;
    ev.push(egui::Event::PointerMoved(pos));
    for pressed in [true, false] {
        ev.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
}

/// The header controls and the timebase knob change the device.
#[test]
fn dso_controls() {
    use dsview_core::key;
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let mut view = TimeView::default();
    let s2 = s.clone();
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            view.show(ui, &s2);
        });
    h.run();
    let factor = |ch: usize| s.config(key::PROBE_FACTOR, Some(ch)).unwrap().get_u64();
    let vdiv = |ch: usize| s.config(key::PROBE_VDIV, Some(ch)).unwrap().get_u64();
    let pos1 = s
        .channels()
        .iter()
        .position(|c| c.index == 1 && c.ty == dsview_core::channel_type::DSO)
        .unwrap();
    assert_eq!(factor(pos1), 1);
    // channel 1's header row (the layout's second band of the 594 px area
    // below the 46 px ruler) is at y = 498; x10 spans 25..60, y - 10..y + 10
    let y = 498.0;
    click(&mut h, 40.0, y);
    h.run();
    assert_eq!(factor(pos1), 10);
    // the dial's lower right half steps V/div up
    let v0 = vdiv(pos1);
    click(&mut h, 100.0, y + 25.0);
    h.run();
    assert!(vdiv(pos1) > v0, "{} > {v0}", vdiv(pos1));
    // the wheel over the traces steps the timebase
    let tb0 = s.config_u64(key::TIMEBASE).unwrap();
    h.input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(600.0, 300.0)));
    h.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, 1.0),
        modifiers: egui::Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    h.run();
    let tb1 = s.config_u64(key::TIMEBASE).unwrap();
    assert!(tb1 < tb0, "zoom in: {tb1} < {tb0}");
    s.exit();
}

/// A double click and two clicks make the X measurement.
#[test]
fn dso_x_measure() {
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let view = std::rc::Rc::new(std::cell::RefCell::new(TimeView::default()));
    let (v2, s2) = (view.clone(), s.clone());
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            v2.borrow_mut().show(ui, &s2);
        });
    h.run();
    let y = 300.0;
    click(&mut h, 300.0, y);
    click(&mut h, 300.0, y);
    h.run();
    assert_eq!(view.borrow().dso.xm.map(|m| m.stage), Some(1));
    click(&mut h, 480.0, y);
    h.run();
    click(&mut h, 660.0, y);
    h.run();
    let m = view.borrow().dso.xm.unwrap();
    assert!(m.is_complete());
    assert!(
        m.index[0] < m.index[1] && m.index[1] < m.index[2],
        "{:?}",
        m.index
    );
    h.snapshot("dso_x_measure");
    s.exit();
}

/// A new frame leaves the old one as a ghost; a V/div step drops it.
#[test]
fn dso_persistence() {
    use dsview_core::{ConfValue, key};
    let s = demo_in_mode(dsview_core::Mode::Dso);
    let view = std::rc::Rc::new(std::cell::RefCell::new(TimeView::default()));
    view.borrow_mut().dso.persist.ms = 5000;
    let (v2, s2) = (view.clone(), s.clone());
    let mut h = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            v2.borrow_mut().show(ui, &s2);
        });
    h.run();
    assert_eq!(view.borrow().dso.persist.ghost_count(), 0);
    let ended = s.state().ended_seq;
    assert!(s.start_capture(true));
    assert!(s.wait(Duration::from_secs(60), |st| st.ended_seq > ended
        && !st.working));
    h.run();
    assert_eq!(view.borrow().dso.persist.ghost_count(), 1);
    let pos = s
        .channels()
        .iter()
        .position(|c| c.index == 0 && c.ty == dsview_core::channel_type::DSO)
        .unwrap();
    s.set_config(key::PROBE_VDIV, ConfValue::U64(2000), Some(pos));
    h.run();
    assert_eq!(view.borrow().dso.persist.ghost_count(), 0);
    s.exit();
}

#[test]
fn trigger_dock() {
    let s = demo_session();
    {
        let mut st = s.state();
        st.adv_trigger.advanced = true;
        st.adv_trigger.stages = 2;
        st.adv_trigger.stage[0].value0 =
            dsview_session::trigger::normalise("XXXXXXXXXXXXXXR1", "X01RFC");
    }
    let mut dock = dsview_gui::triggerdock::TriggerDock::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(360.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("trigger_dock");
    s.exit();
}

#[test]
fn measure_dock() {
    let s = demo_session();
    let mut view = TimeView::default();
    view.cursors.samples = vec![45_147, 50_223];
    let mut dock = dsview_gui::measuredock::MeasureDock::default();
    dock.add_logic_rows(2, 1, 0);
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(340.0, 520.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2, &mut view);
        });
    harness.run();
    harness.snapshot("measure_dock");
    s.exit();
}

/// Next and previous find the rising edges of channel 0 in protocol.demo.
#[test]
fn search_dock() {
    let s = demo_session();
    let mut dock = dsview_gui::searchdock::SearchDock::default();
    let mut view = TimeView::default();
    view.search = Some((0, false));
    dock.pattern.insert(0, b'R');
    let ctx = egui::Context::default();
    let run = |dock: &mut dsview_gui::searchdock::SearchDock, view: &mut TimeView, next: bool| {
        dock.search(&s, view, next);
        let t0 = std::time::Instant::now();
        while dock.busy() {
            assert!(t0.elapsed() < Duration::from_secs(30));
            let mut out = ctx.run_ui(Default::default(), |ui| dock.poll(ui.ctx(), &s, view));
            out.textures_delta.clear();
            std::thread::sleep(Duration::from_millis(5));
        }
        view.search.unwrap()
    };
    let (a, hit) = run(&mut dock, &mut view, true);
    assert!(hit);
    let (b, _) = run(&mut dock, &mut view, true);
    assert!(b > a, "{b} > {a}");
    // both are rising edges of channel 0
    let store = s.data().logic.lock().unwrap();
    for p in [a, b] {
        assert!(store.get_sample(p, 0) && !store.get_sample(p - 1, 0), "{p}");
    }
    drop(store);
    let (c, _) = run(&mut dock, &mut view, false);
    assert_eq!(c, a);
    assert_eq!(dock.message.take(), None);
    s.exit();
}

fn options_snapshot(s: Session, name: &str) {
    let mut d = dsview_gui::deviceoptions::DeviceOptions::new(&s);
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(560.0, 560.0))
        .wgpu()
        .build_ui(move |ui| {
            d.show(ui.ctx(), &s2);
        });
    harness.run();
    harness.snapshot(name);
    s.exit();
}

#[test]
fn device_options_logic() {
    options_snapshot(demo_session(), "device_options_logic");
}

#[test]
fn device_options_analog() {
    options_snapshot(
        demo_in_mode(dsview_core::Mode::Analog),
        "device_options_analog",
    );
}

/// A session on a simulated DSCope (`dsview_usb::sim`).
fn dscope_session() -> Session {
    use dsview_usb::Speed;
    use dsview_usb::sim::{Hub, SimBackend, SimConfig};
    let hub = Arc::new(std::sync::Mutex::new(Hub::new(SimConfig::new(
        0x2A0E,
        0x0002,
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
        .find(|e| e.name.contains("DSCope"))
        .expect("simulated DSCope listed");
    assert!(s.set_device(e.handle));
    s
}

#[test]
fn calibration_dialog() {
    use dsview_core::{ConfValue, key};
    use egui_kittest::kittest::Queryable;
    let s = dscope_session();
    assert_eq!(s.mode(), dsview_core::Mode::Dso as i32);
    s.set_config(key::CALI, ConfValue::Bool(true), None);
    let cal = std::rc::Rc::new(std::cell::RefCell::new(
        dsview_gui::calibration::Calibration::open(&s),
    ));
    let (c2, s2) = (cal.clone(), s.clone());
    let mut h = Harness::builder()
        .with_size(egui::vec2(700.0, 420.0))
        .wgpu()
        .build_ui(move |ui| {
            c2.borrow_mut().show(ui.ctx(), &s2);
        });
    h.run();
    h.snapshot("calibration_dialog");
    // Exit leaves calibration mode
    h.get_by_label("Exit").click();
    h.run();
    assert!(!cal.borrow().open);
    assert!(!s.config(key::CALI, None).unwrap().get_bool());
    s.exit();
}

#[test]
fn store_dialog() {
    let s = demo_session();
    let mut d = dsview_gui::storedlg::StoreDialog::new("/tmp/capture.csv".into(), true, false);
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(560.0, 300.0))
        .wgpu()
        .build_ui(move |ui| {
            d.show(ui.ctx(), &s2, &[45_147, 50_223], "test");
        });
    harness.run();
    harness.snapshot("store_dialog");
    s.exit();
}

#[test]
fn options_dialog() {
    let mut o = dsview_gui::options::AppOptions::default();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(480.0, 620.0))
        .wgpu()
        .build_ui(move |ui| {
            o.dialog(ui);
        });
    harness.run();
    harness.snapshot("options_dialog");
}

/// Auto calibration on the simulated DSCope: 2 µs/div, a running capture
/// until the driver clears ZERO.
#[test]
fn zero_calibration() {
    use dsview_core::{ConfValue, key};
    let s = dscope_session();
    s.set_config(key::ZERO, ConfValue::Bool(true), None);
    let mut z = dsview_gui::zerocal::ZeroCal::start(&s);
    assert_eq!(s.config_u64(key::TIMEBASE), Some(2000));
    assert!(s.is_working());
    let ctx = egui::Context::default();
    let t0 = std::time::Instant::now();
    while s.config(key::ZERO, None).unwrap().get_bool() {
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "calibration did not finish"
        );
        let mut out = ctx.run_ui(Default::default(), |ui| {
            z.show(ui.ctx(), &s);
        });
        out.textures_delta.clear();
        std::thread::sleep(Duration::from_millis(50));
    }
    s.stop_capture();
    s.exit();
}

#[test]
fn protocol_dock() {
    let s = demo_session();
    let mut dock = dsview_gui::protocol::ProtocolDock::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(420.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2);
        });
    harness.run();
    harness.snapshot("protocol_dock");
    s.exit();
}

#[test]
fn protocol_list_options() {
    use egui_kittest::kittest::Queryable;
    let s = demo_session();
    let mut dock = dsview_gui::protocol::ProtocolDock::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(520.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2);
        });
    harness.run();
    harness.get_by_label("List…").click();
    harness.run();
    let t = s.state().decoders[0].clone();
    // the first row listed by default (bit and warning rows are not)
    let k = (0..t.stack.rows().len())
        .find(|&i| t.stack.row_list_shown(i))
        .unwrap();
    let title = t.stack.rows()[k].title();
    harness.get_by_label(&title).click();
    harness.run();
    assert!(!t.stack.row_list_shown(k), "{title} still listed");
    harness.snapshot("protocol_list_options");
    s.exit();
}

#[test]
fn protocol_export_dialog() {
    use egui_kittest::kittest::Queryable;
    let s = demo_session();
    let mut dock = dsview_gui::protocol::ProtocolDock::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(520.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2);
        });
    harness.run();
    harness.get_by_label("Export…").click();
    harness.run();
    harness.get_by_label("Export Format: ");
    harness.snapshot("protocol_export_dialog");
    harness.get_by_label("Cancel").click();
    harness.run();
    assert!(harness.query_by_label("Export Format: ").is_none());
    s.exit();
}

/// The decoder options' show buttons hide a row at once and stay with the
/// stack when OK rebuilds it.
#[test]
fn decoder_show_buttons() {
    use egui_kittest::kittest::Queryable;
    let s = demo_session();
    let mut dock = dsview_gui::protocol::ProtocolDock::default();
    let s2 = s.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(560.0, 900.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2);
        });
    harness.run();
    harness.get_all_by_label("Settings").next().unwrap().click();
    harness.run();
    let t = s.state().decoders[0].clone();
    let k = (0..t.stack.rows().len())
        .find(|&i| t.stack.rows()[i].title().ends_with("Address/Data"))
        .unwrap();
    let title = t.stack.rows()[k].title();
    assert!(t.stack.row_shown(k));
    harness.get_by_label(&title).click();
    harness.run();
    assert!(!t.stack.row_shown(k), "{title} still shown");
    harness.snapshot("decoder_show_buttons");
    harness.get_by_label("OK").click();
    harness.run();
    let t2 = s.state().decoders[0].clone();
    assert!(!Arc::ptr_eq(&t, &t2), "the stack was rebuilt");
    let k2 = t2
        .stack
        .rows()
        .iter()
        .position(|r| r.title() == title)
        .unwrap();
    assert!(!t2.stack.row_shown(k2));
    s.exit();
}

/// A press and a release of `button` at (x, y), one frame each.
fn click_with(h: &mut Harness<'_>, x: f32, y: f32, button: egui::PointerButton) {
    let pos = egui::pos2(x, y);
    for pressed in [true, false] {
        let ev = &mut h.input_mut().events;
        ev.push(egui::Event::PointerMoved(pos));
        ev.push(egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
    }
}

/// The ruler's strips: a left click opens the selector, its first box adds
/// a cursor there; a right click opens the go-to strip, whose boxes centre
/// a cursor; in the selector a right click deletes one.
#[test]
fn ruler_cursor_strips() {
    use egui::PointerButton::{Primary, Secondary};
    let s = demo_session();
    let view = Arc::new(std::sync::Mutex::new(TimeView::default()));
    let (s2, v2) = (s.clone(), view.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            let mut view = v2.lock().unwrap();
            view.show(ui, &s2);
            if view.cursors.samples.is_empty() {
                view.show_region(&s2, 40_000, 60_000);
                view.cursors.samples = vec![45_147, 50_223];
            }
        });
    harness.run();
    let header = dsview_gui::timeview::HEADER_WIDTH;
    let h = dsview_gui::timeview::RULER_HEIGHT;
    let box_y = h - 1.3 * 20.0 + 10.0;
    // add at local x 450
    click_with(&mut harness, header + 450.0, 10.0, Primary);
    click_with(&mut harness, header + 450.0, box_y, Primary);
    let added = {
        let v = view.lock().unwrap();
        assert_eq!(v.cursors.samples.len(), 3);
        v.cursors.samples[2]
    };
    // go to cursor 1: it lands in the middle
    click_with(&mut harness, header + 100.0, 10.0, Secondary);
    click_with(&mut harness, header + 100.0 + 20.0, box_y, Primary);
    {
        let v = view.lock().unwrap();
        let tb = dsview_gui::timeview::timebase(&s);
        let x = v.vp.index_to_pixel(v.cursors.samples[0], false, &tb);
        assert!(
            (x - (v.vp.width / 2) as f64).abs() <= 1.0,
            "cursor 1 at {x}"
        );
    }
    // delete cursor 2 in the selector
    click_with(&mut harness, header + 300.0, 10.0, Primary);
    click_with(&mut harness, header + 300.0 + 40.0, box_y, Secondary);
    {
        let v = view.lock().unwrap();
        assert_eq!(v.cursors.samples.len(), 2);
        assert_eq!(v.cursors.samples[1], added);
    }
    // the selector open, for the picture
    let pos = egui::pos2(header + 300.0 + 20.0, box_y);
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(pos));
    harness.run();
    harness.snapshot("ruler_cursor_strips");
    s.exit();
}

/// Ctrl+Shift+wheel zooms the trace heights and shows the Y-scale badge;
/// a click on the badge resets the zoom.
#[test]
fn yscale_badge() {
    let s = demo_session();
    let view = Arc::new(std::sync::Mutex::new(TimeView::default()));
    let (s2, v2) = (s.clone(), view.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            let mut view = v2.lock().unwrap();
            view.show(ui, &s2);
            if view.cursors.samples.is_empty() {
                view.show_region(&s2, 40_000, 60_000);
                view.cursors.samples = vec![45_147];
            }
        });
    harness.run();
    let mods = egui::Modifiers {
        ctrl: true,
        shift: true,
        command: true,
        ..Default::default()
    };
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(600.0, 300.0)));
    harness.event_modifiers(
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 2.0),
            modifiers: mods,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        },
        mods,
    );
    harness.run();
    let f = view.lock().unwrap().height_factor;
    assert!((f - 1.5).abs() < 1e-9, "factor {f}");
    harness.run();
    harness.snapshot("yscale_badge");
    // the badge at the top right of the traces
    click(&mut harness, 1100.0 - 40.0, 46.0 + 14.0);
    harness.run();
    assert_eq!(view.lock().unwrap().height_factor, 1.0);
    s.exit();
}

/// Dragging a header label moves the trace on the 10 px grid and renumbers
/// the view order; Ctrl+click selects a second trace and both move.
#[test]
fn header_drag_and_multi_select() {
    use dsview_session::session::TraceRef;
    let s = demo_session();
    let view = Arc::new(std::sync::Mutex::new(TimeView::default()));
    let (s2, v2) = (s.clone(), view.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            v2.lock().unwrap().show(ui, &s2);
        });
    harness.run();
    let rows = view.lock().unwrap().shown_rows().to_vec();
    let order = |v: &TimeView| {
        v.shown_rows()
            .iter()
            .map(|r| r.0)
            .collect::<Vec<TraceRef>>()
    };
    let first = order(&view.lock().unwrap());
    let x = 60.0;
    let drag = |h: &mut Harness<'_>, y0: f32, y1: f32, mods: egui::Modifiers| {
        let ev = |h: &mut Harness<'_>, e: egui::Event| h.input_mut().events.push(e);
        ev(h, egui::Event::PointerMoved(egui::pos2(x, y0)));
        h.event_modifiers(
            egui::Event::PointerButton {
                pos: egui::pos2(x, y0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: mods,
            },
            mods,
        );
        h.run();
        for k in 1..=4 {
            let y = y0 + (y1 - y0) * k as f32 / 4.0;
            ev(h, egui::Event::PointerMoved(egui::pos2(x, y)));
            h.run();
        }
        ev(
            h,
            egui::Event::PointerButton {
                pos: egui::pos2(x, y1),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        );
        h.run();
        h.run();
    };
    // the first trace below the third one
    drag(
        &mut harness,
        rows[0].1,
        rows[2].1 + 8.0,
        egui::Modifiers::NONE,
    );
    let second = order(&view.lock().unwrap());
    assert_eq!(second[2], first[0], "{second:?}");
    assert_eq!(second[0], first[1]);
    // Ctrl+click the (new) first trace, then press the second with Ctrl
    // and drag it without: both go to the end
    let rows = view.lock().unwrap().shown_rows().to_vec();
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    let ev = |h: &mut Harness<'_>, e: egui::Event| h.input_mut().events.push(e);
    let button = |y: f32, pressed: bool, modifiers: egui::Modifiers| egui::Event::PointerButton {
        pos: egui::pos2(x, y),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    for (k, y) in [rows[0].1, rows[1].1].into_iter().enumerate() {
        ev(&mut harness, egui::Event::PointerMoved(egui::pos2(x, y)));
        ev(&mut harness, egui::Event::ModifiersChanged(ctrl));
        ev(&mut harness, button(y, true, ctrl));
        harness.run();
        if k == 0 {
            ev(&mut harness, button(y, false, ctrl));
            harness.run();
        }
    }
    ev(
        &mut harness,
        egui::Event::ModifiersChanged(egui::Modifiers::NONE),
    );
    // the pair keeps its spacing: far enough for the upper one to pass the
    // last trace too
    let last = rows.last().unwrap().1 + (rows[1].1 - rows[0].1);
    for k in 1..=4 {
        let y = rows[1].1 + (last + 30.0 - rows[1].1) * k as f32 / 4.0;
        ev(&mut harness, egui::Event::PointerMoved(egui::pos2(x, y)));
        harness.run();
    }
    ev(
        &mut harness,
        button(last + 30.0, false, egui::Modifiers::NONE),
    );
    harness.run();
    harness.run();
    let third = order(&view.lock().unwrap());
    let n = third.len();
    assert_eq!(&third[n - 2..], &[second[0], second[1]], "{third:?}");
    s.exit();
}

/// A touchpad pinch zooms the logic view around the pointer.
#[test]
fn pinch_zoom() {
    let s = demo_session();
    let view = Arc::new(std::sync::Mutex::new(TimeView::default()));
    let (s2, v2) = (s.clone(), view.clone());
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            v2.lock().unwrap().show(ui, &s2);
        });
    harness.run();
    let before = view.lock().unwrap().vp.scale;
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(600.0, 300.0)));
    harness.run();
    for _ in 0..3 {
        harness.input_mut().events.push(egui::Event::Zoom(1.3));
        harness.run();
    }
    let after = view.lock().unwrap().vp.scale;
    assert!(after < before, "zoomed in: {after} < {before}");
    s.exit();
}

/// The pointer over a pulse: width and period arrows, the edge lines and
/// the floating measurement popup.
#[test]
fn mouse_measure_popup() {
    let s = demo_session();
    let view = Arc::new(std::sync::Mutex::new(TimeView::default()));
    let (s2, v2) = (s.clone(), view.clone());
    let mut shown = false;
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            let mut view = v2.lock().unwrap();
            view.show(ui, &s2);
            if !std::mem::replace(&mut shown, true) {
                view.show_region(&s2, 45_000, 47_000);
            }
        });
    harness.run();
    // channel 0 (SCL) in the middle of the view
    harness.run();
    let rows = view.lock().unwrap().shown_rows().to_vec();
    let y = rows
        .iter()
        .find(|r| {
            matches!(
                r.0,
                dsview_session::session::TraceRef::Signal { index: 0, .. }
            )
        })
        .unwrap()
        .1;
    harness
        .input_mut()
        .events
        .push(egui::Event::PointerMoved(egui::pos2(620.0, y)));
    harness.run();
    harness.run();
    let m = view
        .lock()
        .unwrap()
        .measure
        .clone()
        .expect("a pulse under the pointer");
    assert!(m.end > m.start);
    harness.snapshot("mouse_measure_popup");
    s.exit();
}

/// A click on an annotation in the protocol list marks it on its trace
/// (`set_mark_index`) and asks the view to show it.
#[test]
fn protocol_table_marks_annotation() {
    use egui_kittest::kittest::Queryable;
    let s = demo_session();
    let mut dock = dsview_gui::protocol::ProtocolDock::default();
    let s2 = s.clone();
    let jumps = Arc::new(std::sync::Mutex::new(Vec::new()));
    let j2 = jumps.clone();
    let mut harness = Harness::builder()
        .with_size(egui::vec2(520.0, 640.0))
        .wgpu()
        .build_ui(move |ui| {
            dock.show(ui, &s2);
            if let Some(j) = dock.jump.take() {
                j2.lock().unwrap().push(j);
            }
        });
    harness.run();
    let t = s.state().decoders[0].clone();
    assert_eq!(t.stack.mark(), -1);
    harness
        .get_all_by_label("Address write: 62")
        .next()
        .unwrap()
        .click();
    harness.run();
    let (a, b) = jumps.lock().unwrap()[0];
    assert_eq!(t.stack.mark(), ((a + b) / 2) as i64);
    s.exit();
}
