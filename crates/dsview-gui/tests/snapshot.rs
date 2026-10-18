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
