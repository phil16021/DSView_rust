//! `dsv_zip_writer_*` (behind `ZipMaker`) and `dsv_dsl_entry` (behind `ZipReader`).

use std::ffi::{CStr, c_char};
use std::path::Path;

use dsview_formats::zipw::DslZipWriter;

use crate::dsl::DsvDsl;
use crate::error::{DSV_ERR, DSV_ERR_ARG, DSV_OK, guard, set_error};

pub struct DsvZipWriter {
    inner: Option<DslZipWriter>,
}

fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(p) }.to_str().ok()
    }
}

/// Starts writing a `.dsl` at `path` (atomically; see dsview_formats::zipw).
///
/// # Safety
/// `path` NUL-terminated; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_zip_writer_create(
    path: *const c_char,
    out: *mut *mut DsvZipWriter,
) -> i32 {
    guard(|| {
        let (Some(path), false) = (cstr(path), out.is_null()) else {
            set_error("dsv_zip_writer_create: invalid argument");
            return DSV_ERR_ARG;
        };
        match DslZipWriter::create(Path::new(path)) {
            Ok(w) => {
                unsafe { *out = Box::into_raw(Box::new(DsvZipWriter { inner: Some(w) })) };
                DSV_OK
            }
            Err(e) => {
                set_error(format!("{path}: {e}"));
                DSV_ERR
            }
        }
    })
}

/// Adds entry `name` with `len` bytes.
///
/// # Safety
/// `h` from `dsv_zip_writer_create`; `name` NUL-terminated; `data` has `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_zip_writer_add(
    h: *mut DsvZipWriter,
    name: *const c_char,
    data: *const u8,
    len: u64,
) -> i32 {
    guard(|| {
        let (Some(h), Some(name)) = (unsafe { h.as_mut() }, cstr(name)) else {
            set_error("dsv_zip_writer_add: invalid argument");
            return DSV_ERR_ARG;
        };
        let Some(w) = h.inner.as_mut() else {
            set_error("dsv_zip_writer_add: writer already finished");
            return DSV_ERR_ARG;
        };
        let bytes = if len == 0 || data.is_null() {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(data, len as usize) }
        };
        match w.add(name, bytes) {
            Ok(()) => DSV_OK,
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

/// Completes the archive and moves it into place. The handle stays valid
/// for `dsv_zip_writer_free`.
///
/// # Safety
/// `h` from `dsv_zip_writer_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_zip_writer_finish(h: *mut DsvZipWriter) -> i32 {
    guard(|| {
        let Some(w) = unsafe { h.as_mut() }.and_then(|h| h.inner.take()) else {
            set_error("dsv_zip_writer_finish: no open writer");
            return DSV_ERR_ARG;
        };
        match w.finish() {
            Ok(()) => DSV_OK,
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

/// Frees the handle; an unfinished archive is discarded. NULL is ignored.
///
/// # Safety
/// `h` from `dsv_zip_writer_create`, not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_zip_writer_free(h: *mut DsvZipWriter) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

/// Reads metadata entry `name` (`session`, `decoders`, `header`).
/// Returns 1 when found (data valid until the next call or close), 0 when
/// missing, negative on error.
///
/// # Safety
/// `h` from `dsv_dsl_open`; `name` NUL-terminated; outputs writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dsl_entry(
    h: *mut DsvDsl,
    name: *const c_char,
    data: *mut *const u8,
    len: *mut u64,
) -> i32 {
    guard(|| {
        let (Some(h), Some(name), false, false) = (
            unsafe { h.as_mut() },
            cstr(name),
            data.is_null(),
            len.is_null(),
        ) else {
            set_error("dsv_dsl_entry: invalid argument");
            return DSV_ERR_ARG;
        };
        match h.read_meta(name) {
            Ok(Some(bytes)) => {
                unsafe {
                    *data = bytes.as_ptr();
                    *len = bytes.len() as u64;
                }
                1
            }
            Ok(None) => 0,
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn write_then_read_entry_through_the_c_abi() {
        let dir = std::env::temp_dir().join(format!("dsview-ffi-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = CString::new(dir.join("t.dsl").to_str().unwrap()).unwrap();
        let header = b"[version]\nversion = 3\n[header]\ndevice mode = 0\ncapturefile = data\ntotal samples = 64\ntotal probes = 1\ntotal blocks = 1\nsamplerate = 1 MHz\nprobe0 = A\n";

        let mut w = std::ptr::null_mut();
        assert_eq!(
            unsafe { dsv_zip_writer_create(path.as_ptr(), &mut w) },
            DSV_OK
        );
        let add = |name: &str, data: &[u8]| {
            let n = CString::new(name).unwrap();
            unsafe { dsv_zip_writer_add(w, n.as_ptr(), data.as_ptr(), data.len() as u64) }
        };
        assert_eq!(add("header", header), DSV_OK);
        assert_eq!(add("session", b"{\"x\":1}"), DSV_OK);
        assert_eq!(add("L-0/0", &[1u8; 8]), DSV_OK);
        assert_eq!(add("../x", b"no"), DSV_ERR);
        assert_eq!(unsafe { dsv_zip_writer_finish(w) }, DSV_OK);
        unsafe { dsv_zip_writer_free(w) };

        let mut r = std::ptr::null_mut();
        assert_eq!(
            unsafe { crate::dsl::dsv_dsl_open(path.as_ptr(), &mut r) },
            DSV_OK
        );
        let (mut data, mut len) = (std::ptr::null(), 0u64);
        let session = CString::new("session").unwrap();
        assert_eq!(
            unsafe { dsv_dsl_entry(r, session.as_ptr(), &mut data, &mut len) },
            1
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(data, len as usize) },
            b"{\"x\":1}"
        );
        let missing = CString::new("decoders").unwrap();
        assert_eq!(
            unsafe { dsv_dsl_entry(r, missing.as_ptr(), &mut data, &mut len) },
            0
        );
        unsafe { crate::dsl::dsv_dsl_close(r) };
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
