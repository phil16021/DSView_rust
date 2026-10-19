#![no_main]
//! The DSCope driver on fuzzed bulk IN data: DSO frames and the status
//! block behind each.

use libfuzzer_sys::fuzz_target;

#[path = "common/usb_in.rs"]
mod usb_in;

fuzz_target!(|data: &[u8]| {
    usb_in::run(dsview_drivers::dsl::device::Family::DsCope, data);
});
