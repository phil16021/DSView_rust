//! Runtime choice between the C and Rust implementation of a subsystem
//! during the migration (docs/migration/PLAN.md §1.2).

use std::ffi::{CStr, c_char};

/// True when subsystem `name` ("FORMATS", "STORE", "DECODE", ...) should use
/// Rust: `DSVIEW_IMPL_<name>` if set, else `DSVIEW_IMPL`, equal to "rust".
pub fn use_rust(name: &str) -> bool {
    let specific = std::env::var(format!("DSVIEW_IMPL_{name}"))
        .ok()
        .filter(|v| !v.is_empty());
    let value = specific.or_else(|| std::env::var("DSVIEW_IMPL").ok());
    value.as_deref() == Some("rust")
}

/// C entry point for [`use_rust`]: 1 for Rust, 0 for C.
///
/// # Safety
/// `subsystem` must be NUL-terminated or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_use_rust(subsystem: *const c_char) -> i32 {
    if subsystem.is_null() {
        return 0;
    }
    let name = unsafe { CStr::from_ptr(subsystem) }.to_string_lossy();
    i32::from(use_rust(&name))
}
