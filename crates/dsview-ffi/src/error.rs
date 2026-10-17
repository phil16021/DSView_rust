//! Error reporting across the C ABI: functions return a negative code and
//! leave a message for `dsv_last_error()` (like `ds_get_last_error`).

use std::cell::RefCell;
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};

pub const DSV_OK: i32 = 0;
pub const DSV_ERR: i32 = -1;
pub const DSV_ERR_ARG: i32 = -2;
pub const DSV_ERR_PANIC: i32 = -99;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

pub fn set_error(msg: impl std::fmt::Display) {
    let text = CString::new(msg.to_string().replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = text);
}

/// Runs `f`, turning a panic into `DSV_ERR_PANIC` so it never unwinds into C.
pub fn guard(f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(p) => {
            let msg = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".into());
            set_error(format!("internal error: {msg}"));
            DSV_ERR_PANIC
        }
    }
}

/// Message of the last failed call on this thread ("" if none). Valid until the next failing call.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}
