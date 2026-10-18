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
    assert!(m.index[0] < m.index[1] && m.index[1] < m.index[2], "{:?}", m.index);
    h.snapshot("dso_x_measure");
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
