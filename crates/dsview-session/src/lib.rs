//! The acquisition library: libsigrok4DSL's `lib_main.c` (device list,
//! activation, hotplug, the collect thread, events) and the part of
//! `session.c` the drivers use. [`Library`] keeps the devices of every
//! driver (demo, capture files, DSLogic, DSCope) and runs one capture at a
//! time on its own thread.
//!
//! The C library's users (the Qt GUI through `ds_api_rs.c`) see the devices
//! through a [`Host`]: it receives the events and data packets, and keeps
//! its own copies of the devices in step at the points C would have shared
//! them.

/// Log lines go where the drivers' go (`dsview_drivers::sys`).
macro_rules! sr_err { ($($t:tt)*) => { dsview_drivers::sys::log(1, &format!($($t)*)) } }
#[allow(unused_macros)]
macro_rules! sr_info { ($($t:tt)*) => { dsview_drivers::sys::log(3, &format!($($t)*)) } }

pub mod decode;
pub mod library;
pub mod numfmt;
pub mod session;
pub mod store;
pub mod trigger;

pub use library::{Entry, Handle, Host, Kind, Library};
pub use session::{Paths, Session};

/// `enum libsigrok_event_type` and the `DS_EV_*` codes.
pub mod event {
    pub const NEW_DEVICE_ATTACH: i32 = 1;
    pub const CURRENT_DEVICE_DETACH: i32 = 2;
    pub const INACTIVE_DEVICE_DETACH: i32 = 3;
    pub const COLLECT_TASK_START: i32 = 101;
    pub const COLLECT_TASK_END: i32 = 102;
    pub const DEVICE_RUNNING: i32 = 103;
    pub const DEVICE_STOPPED: i32 = 104;
    pub const COLLECT_TASK_END_BY_DETACHED: i32 = 105;
    pub const COLLECT_TASK_END_BY_ERROR: i32 = 106;
    pub const DEVICE_SPEED_NOT_MATCH: i32 = 107;
}
