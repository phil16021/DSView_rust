#![no_main]
//! The decoder entries of a file (`StoreSession::load_decoders`), with
//! every decoder loaded: ids, channel maps, options of any type, stacks.

use libfuzzer_sys::fuzz_target;

#[path = "common/session.rs"]
mod session;

fuzz_target!(|data: &[u8]| {
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_slice(data) else {
        return;
    };
    let s = session::session();
    let n = dsview_session::store::load_file_decoders(s, &entries);
    assert!(n <= entries.len());
    let _ = dsview_session::store::decoders_json(s);
    session::clear(s);
});
