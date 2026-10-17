//! C ABI (`dsv_*`) of the Rust DSView core, linked into the Qt build with
//! `-DDSVIEW_RUST_CORE=ON`. Header: `include/dsview_ffi.h`.
//!
//! * `dsv_version`, `dsv_last_error`
//! * `dsv_dsl_*`: `.dsl`/`.demo` reading for libsigrok4DSL's session driver

pub mod dsl;
pub mod error;

use std::ffi::c_char;

static VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();

/// Version of the Rust core as a NUL-terminated static string.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_version() -> *const c_char {
    VERSION.as_ptr().cast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn version_is_nul_terminated() {
        let v = unsafe { CStr::from_ptr(dsv_version()) };
        assert_eq!(v.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }
}
