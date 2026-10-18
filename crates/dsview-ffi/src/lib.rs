//! C ABI (`dsv_*`) of the Rust DSView core, linked into the Qt build with
//! `-DDSVIEW_RUST_CORE=ON`. Header: `include/dsview_ffi.h`.
//!
//! * `dsv_version`, `dsv_last_error`, `dsv_use_rust`
//! * `dsv_decode_*`: the decoder engine behind libsigrokdecode4DSL
//! * `dsv_drv_*`, `dsv_conf_free`: the Rust device drivers behind libsigrok4DSL's driver shim
//! * `dsv_dsl_*`: `.dsl`/`.demo` reading for libsigrok4DSL's session driver
//! * `dsv_export_*`: CSV/VCD export behind libsigrok4DSL's output modules
//! * `dsv_zip_writer_*`, `dsv_dsl_entry`: `.dsl` saving/metadata behind ZipMaker/ZipReader
//! * `dsv_logic_*`: the logic sample store behind `pv::data::LogicSnapshot`
//! * `dsv_dso_*`, `dsv_analog_*`: the DSO/analog stores behind `DsoSnapshot`/`AnalogSnapshot`

pub mod decode;
pub mod drv;
pub mod dsl;
pub mod error;
pub mod export;
pub mod store;
pub mod store_ad;
pub mod switch;
pub mod zip;

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
