#![no_main]
//! Any byte string offered as a .dsl must be read or rejected without panics
//! or unbounded allocation (small limits keep each run cheap).

use dsview_formats::dsl::{DslReader, Limits};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = Limits { max_meta_bytes: 1 << 16, max_entry_bytes: 1 << 20, max_entries: 1024 };
    let Ok(mut r) = DslReader::new(std::io::Cursor::new(data), limits) else { return };
    let _ = r.decoder_stacks();
    let mut total = 0usize;
    let _ = r.stream(|p| {
        total += match p {
            dsview_formats::dsl::Packet::Logic(b) => b.len(),
            dsview_formats::dsl::Packet::Dso { data, .. } | dsview_formats::dsl::Packet::Analog { data, .. } => data.len(),
        };
    });
});
