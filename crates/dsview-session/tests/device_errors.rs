//! A failed device switch leaves its message for the GUI
//! (`SigSession::set_device`), in its own test binary: the resource
//! directory is process-wide.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsview_session::session::device_error_text;
use dsview_session::{Kind, Paths, Session};
use dsview_usb::Speed;
use dsview_usb::sim::{Hub, SimBackend, SimConfig};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn a_missing_bitstream_is_reported_as_missing_firmware() {
    let res = std::env::temp_dir().join(format!("dsv-nores-{}", std::process::id()));
    std::fs::create_dir_all(&res).unwrap();
    dsview_drivers::sys::set_res_dir(res.clone());
    let mut cfg = SimConfig::new(0x2A0E, 0x0034, Speed::High);
    // a cold FPGA needs its bitstream at open
    cfg.fpga_done = false;
    let sim = Arc::new(SimBackend::new(
        Arc::new(Mutex::new(Hub::new(cfg))),
        Arc::new(|d: Duration| std::thread::sleep(d)),
    ));
    let s = Session::new(
        Paths {
            data_dir: root().join("assets"),
            profile_dir: Some(res.clone()),
            version: "test".into(),
        },
        sim,
    );
    let (list, _) = s.devices();
    let e = list
        .iter()
        .find(|e| matches!(e.kind, Kind::Usb { .. }))
        .expect("simulated DSLogic");
    assert!(!s.set_device(e.handle));
    let err = std::mem::take(&mut s.state().device_error);
    assert_eq!(
        device_error_text(err),
        Some("Error: Firmware file is not exist!")
    );
    s.exit();
    let _ = std::fs::remove_dir_all(&res);
}

#[test]
fn the_messages_are_qts() {
    assert_eq!(device_error_text(0), None);
    assert_eq!(device_error_text(7), None);
    assert_eq!(device_error_text(13), Some("Error: USB IO error!"));
}

#[test]
fn the_data_dir_prefers_the_package_layout() {
    let root = std::env::temp_dir().join(format!("dsv-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    assert_eq!(dsview_session::data_dir_for(&root.join("bin")), None);
    std::fs::create_dir_all(root.join("share/DSView/res")).unwrap();
    assert_eq!(
        dsview_session::data_dir_for(&root.join("bin")),
        Some(root.join("share/DSView"))
    );
    std::fs::create_dir_all(root.join("share/dsview/res")).unwrap();
    assert_eq!(
        dsview_session::data_dir_for(&root.join("bin")),
        Some(root.join("share/dsview"))
    );
    std::fs::remove_dir_all(&root).unwrap();
}
