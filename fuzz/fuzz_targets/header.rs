#![no_main]
//! Any header text must parse or fail cleanly.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    if let Ok(h) = dsview_formats::header::Header::parse(&text) {
        assert!(h.total_probes >= 1 && h.total_probes <= 64);
        assert!(h.channels.len() as u64 >= h.total_probes);
    }
});
