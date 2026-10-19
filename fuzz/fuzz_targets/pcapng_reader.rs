#![no_main]
//! Any bytes must parse as a usbmon pcapng trace or fail cleanly.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(urbs) = dsview_usb::trace::parse(data) {
        for u in &urbs {
            let _ = u.is_in();
        }
    }
});
