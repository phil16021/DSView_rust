//! C ABI (`dsv_*`) of the Rust DSView core.
//!
//! Phase 0 only proves the build plumbing: the Qt build links this static
//! library and logs its version at startup. Later phases add the formats,
//! store, decoder and acquisition APIs here.

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
