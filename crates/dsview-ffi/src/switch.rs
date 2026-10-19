//! Runtime choice between the C and Rust implementation of a subsystem
//! during the migration (docs/migration/PLAN.md §1.2).
//!
//! Release R1 makes Rust the default: the CMake build of `DSView-qt` enables
//! the `rust-default` feature. Without it (the oracle, the C-vs-Rust checks)
//! C stays the default, so the differential tests keep comparing C to Rust.

use std::ffi::{CStr, c_char};

/// True when subsystem `name` ("FORMATS", "STORE", "DECODE", ...) should use
/// Rust: `DSVIEW_IMPL_<name>` if set, else `DSVIEW_IMPL`, equal to "rust";
/// with neither set, the build's default.
pub fn use_rust(name: &str) -> bool {
    let set = |key: String| std::env::var(key).ok().filter(|v| !v.is_empty());
    match set(format!("DSVIEW_IMPL_{name}")).or_else(|| set("DSVIEW_IMPL".into())) {
        Some(v) => v == "rust",
        None => RUST_DEFAULT,
    }
}

/// Whether Rust is chosen when no `DSVIEW_IMPL*` variable says otherwise.
pub const RUST_DEFAULT: bool = cfg!(feature = "rust-default");

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

#[cfg(test)]
mod tests {
    use super::*;

    // one test: it changes the process environment
    #[test]
    fn variables_override_the_default() {
        // SAFETY: no other test in this crate reads DSVIEW_IMPL*.
        unsafe {
            std::env::remove_var("DSVIEW_IMPL");
            std::env::remove_var("DSVIEW_IMPL_STORE");
        }
        assert_eq!(use_rust("STORE"), RUST_DEFAULT);
        unsafe { std::env::set_var("DSVIEW_IMPL", "c") };
        assert!(!use_rust("STORE"));
        unsafe { std::env::set_var("DSVIEW_IMPL_STORE", "rust") };
        assert!(use_rust("STORE"));
        assert!(!use_rust("DECODE"));
        unsafe { std::env::set_var("DSVIEW_IMPL", "") };
        unsafe { std::env::set_var("DSVIEW_IMPL_STORE", "") };
        assert_eq!(use_rust("STORE"), RUST_DEFAULT);
        unsafe { std::env::set_var("DSVIEW_IMPL", "rust") };
        unsafe { std::env::set_var("DSVIEW_IMPL_STORE", "c") };
        assert!(!use_rust("STORE"));
        assert!(use_rust("DECODE"));
    }
}
