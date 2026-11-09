//! F30 follow-up: when the monitors change while DSView runs, a window that
//! left them all comes onto one, and a layout seen before gets the window's
//! place in it back (a laptop docked and undocked).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsview_gui::app::{App, MonitorLayout};
use dsview_gui::hooks::Shared;
use dsview_session::{Paths, Session};
use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};
use egui::{Rect, ViewportCommand, ViewportId, pos2, vec2};
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

/// The window as the window manager reports it: outer place, inner size.
fn place(h: &mut Harness, x: f32, y: f32) {
    let v = h.input_mut().viewports.entry(ViewportId::ROOT).or_default();
    v.outer_rect = Some(Rect::from_min_size(pos2(x, y), vec2(1600.0, 900.0)));
    v.inner_rect = Some(Rect::from_min_size(pos2(x, y), vec2(1600.0, 900.0)));
    v.maximized = Some(false);
}

/// The root viewport's move in the last frame, if any.
fn moved_to(h: &Harness) -> Option<(f32, f32)> {
    h.output()
        .viewport_output
        .get(&ViewportId::ROOT)?
        .commands
        .iter()
        .find_map(|c| match c {
            ViewportCommand::OuterPosition(p) => Some((p.x, p.y)),
            _ => None,
        })
}

#[test]
fn the_window_follows_the_monitors() {
    let s = Session::new(
        Paths {
            data_dir: root().join("assets"),
            profile_dir: None,
            version: "test".into(),
        },
        Arc::new(NoUsb),
    );
    s.set_default_device();
    let monitors = Arc::new(Mutex::new(None::<MonitorLayout>));
    let app: Arc<Mutex<Option<App>>> = Arc::new(Mutex::new(None));
    let (s2, a2, m2) = (s.clone(), app.clone(), monitors.clone());
    let mut h = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .with_pixels_per_point(1.0)
        .build_ui(move |ui| {
            let mut a = a2.lock().unwrap();
            let app = a.get_or_insert_with(|| {
                let mut app = App::new(s2.clone(), Shared::default(), "test", ui.ctx());
                app.monitors = m2.clone();
                app
            });
            app.frame(ui);
        });
    let docked = MonitorLayout {
        monitors: vec![[0, 0, 1920, 1080], [1920, 0, 2560, 1440]],
        primary: 0,
    };
    let laptop = MonitorLayout {
        monitors: vec![[0, 0, 1920, 1080]],
        primary: 0,
    };

    // docked, the window on the second screen: it stays
    *monitors.lock().unwrap() = Some(docked.clone());
    place(&mut h, 2500.0, 100.0);
    h.step();
    assert_eq!(moved_to(&h), None, "a window that shows was moved");
    h.step();

    // undocked: it left every screen and comes onto the laptop's
    *monitors.lock().unwrap() = Some(laptop);
    h.step();
    assert_eq!(moved_to(&h), Some((160.0, 90.0)));
    // the window manager carried it out; its place is recorded for this
    // layout once the move settled
    place(&mut h, 160.0, 90.0);
    std::thread::sleep(Duration::from_millis(1100));
    h.step();
    h.step();

    // docked again: back to its place on the second screen
    *monitors.lock().unwrap() = Some(docked);
    h.step();
    assert_eq!(moved_to(&h), Some((2500.0, 100.0)));
    s.exit();
}
