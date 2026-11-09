//! F44: a capture with more logic channels enabled than the channel mode
//! allows is refused with a message, before the USB buffers for all of them
//! exceed the kernel's usbfs limit (errno 12 on the DSLogic).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsview_core::{ConfValue, key};
use dsview_session::{Kind, Paths, Session};
use dsview_usb::Speed;
use dsview_usb::sim::{Hub, SimBackend, SimConfig};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn more_channels_than_the_mode_allows_are_refused() {
    let sim = Arc::new(SimBackend::new(
        Arc::new(Mutex::new(Hub::new(SimConfig::new(
            0x2A0E,
            0x0034,
            Speed::High,
        )))),
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
        .find(|e| matches!(e.kind, Kind::Usb { .. }))
        .expect("simulated DSLogic");
    assert!(s.set_device(e.handle));
    // stream, 3 channels at up to 100 MHz (DSL_STREAM100x3); the list keeps
    // all 16 channels, as in C
    assert!(s.set_config(key::CHANNEL_MODE, ConfValue::I16(3), None));
    let all: Vec<u16> = s.channels().iter().map(|c| c.index).collect();
    assert_eq!(all.len(), 16);
    for &c in &all {
        s.enable_channel(c, true);
    }
    s.reload_signals();
    assert!(
        !s.start_capture(false),
        "a capture on 16 channels in a 3-channel mode"
    );
    let msg = s
        .state()
        .start_error
        .take()
        .expect("a message for the user");
    assert!(
        msg.contains("16 channels") && msg.contains("at most 3"),
        "{msg}"
    );

    for &c in &all[3..] {
        s.enable_channel(c, false);
    }
    s.reload_signals();
    assert_eq!(s.check_channel_count(), Ok(()));
    s.exit();
}
