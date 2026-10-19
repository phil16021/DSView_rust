#![no_main]
//! The DSLogic driver on fuzzed bulk IN data: the trigger header and the
//! sample stream.

use libfuzzer_sys::fuzz_target;

#[path = "common/usb_in.rs"]
mod usb_in;

fuzz_target!(|data: &[u8]| {
    usb_in::run(dsview_drivers::dsl::device::Family::DsLogic, data);
});
