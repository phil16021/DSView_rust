#![no_main]
//! A `.dsc` profile or a `.dsl` session entry: any JSON loads or is
//! refused, never a panic.

use libfuzzer_sys::fuzz_target;

#[path = "common/session.rs"]
mod session;

fuzz_target!(|data: &[u8]| {
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(data) else {
        return;
    };
    let s = session::session();
    let _ = dsview_session::store::load_config(s, &doc, true);
    session::clear(s);
});
