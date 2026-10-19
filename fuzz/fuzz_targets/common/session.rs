//! One demo logic session with the repository's decoders, for the targets
//! that feed it JSON (the process may start Python once).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use dsview_session::{Kind, Paths, Session};
use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};

struct NoUsb;

impl UsbBackend for NoUsb {
    fn list(&self) -> Result<Vec<Info>, UsbError> {
        Ok(Vec::new())
    }
    fn open(&self, _: u8, _: u8, _: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
        Err(UsbError::NotFound)
    }
}

pub fn session() -> &'static Session {
    static S: OnceLock<Session> = OnceLock::new();
    S.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        dsview_drivers::sys::set_log_sink(Box::new(|_, _| {}));
        dsview_decode::log::set_sink(|_, _| {});
        let _ = dsview_decode::init(&[root.join("libsigrokdecode4DSL/decoders").as_path()]);
        dsview_decode::load_all();
        let s = Session::new(
            Paths {
                data_dir: root.join("DSView"),
                profile_dir: Some(std::env::temp_dir().join("dsv-fuzz-profiles")),
                version: "fuzz".into(),
            },
            Arc::new(NoUsb),
        );
        let (list, _) = s.devices();
        if let Some(e) = list
            .iter()
            .find(|e| e.kind == Kind::Demo && e.name == "Demo Logic")
        {
            s.set_device(e.handle);
        }
        s
    })
}

/// Back to no decoders.
pub fn clear(s: &Session) {
    while !s.state().decoders.is_empty() {
        s.remove_decoder(0);
    }
    s.state().held_decoders.clear();
}
