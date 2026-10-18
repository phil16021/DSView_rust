//! `dsv_lib_*`: the acquisition library (crates/dsview-session: device list,
//! activation, hotplug, the capture thread) behind libsigrok4DSL's `ds_*`
//! API when `DSVIEW_IMPL_ACQ=rust` (`ds_api_rs.c`).
//!
//! The shim keeps a `struct sr_dev_inst` per device for the GUI, built from
//! `dsv_lib_device()` handles (shared with the library). The library calls
//! back through `struct dsv_lib_host`: events, data packets, and the points
//! where the shim's copy and the device have to be brought in step.

use std::ffi::{CStr, c_char, c_void};
use std::sync::{Arc, Mutex};

use dsview_core::Packet;
use dsview_session::{Entry, Handle, Host, Kind, Library};

use crate::drv::{DsvDevice, emit_all, usb_backend};
use crate::error::{guard, guard_or};

const SR_OK: i32 = 0;
const SR_ERR_ARG: i32 = 3;
const SR_ERR_CALL_STATUS: i32 = 8;
const SR_ERR_HAVE_DONE: i32 = 9;

/// The shim's callbacks; `user` is passed back to each.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DsvLibHost {
    pub user: *mut c_void,
    /// `ds_set_event_callback`'s callback.
    pub event: Option<extern "C" fn(user: *mut c_void, ev: i32)>,
    /// The `emit` context (the shim's `sr_dev_inst`) of a device.
    pub packet_ctx: Option<extern "C" fn(user: *mut c_void, handle: u64) -> *mut c_void>,
    /// Forwards one packet (`rs_emit`).
    pub emit: Option<extern "C" fn(ctx: *mut c_void, p: *const crate::drv::DsvPacket)>,
    /// Copy the shim's channel list into the device.
    pub sync_in: Option<extern "C" fn(user: *mut c_void, handle: u64)>,
    /// Copy the device out (`full`: also mode, status, the channel list).
    pub sync_out: Option<extern "C" fn(user: *mut c_void, handle: u64, full: i32)>,
    /// A capture is about to start (the shim passes the trigger in).
    pub before_start: Option<extern "C" fn(user: *mut c_void, handle: u64)>,
    /// A device left for good: the shim frees its copy.
    pub removed: Option<extern "C" fn(user: *mut c_void, handle: u64)>,
}

struct CHost(DsvLibHost);

// The shim's callbacks are thread-safe (it locks its own state).
unsafe impl Send for CHost {}
unsafe impl Sync for CHost {}

impl Host for CHost {
    fn event(&self, ev: i32) {
        if let Some(f) = self.0.event {
            f(self.0.user, ev);
        }
    }
    fn packets(&self, handle: Handle, packets: &[Packet]) {
        if packets.is_empty() {
            return;
        }
        if let (Some(ctx), Some(emit)) = (self.0.packet_ctx, self.0.emit) {
            let c = ctx(self.0.user, handle);
            emit_all(packets, emit, c);
        }
    }
    fn sync_in(&self, handle: Handle) {
        if let Some(f) = self.0.sync_in {
            f(self.0.user, handle);
        }
    }
    fn sync_out(&self, handle: Handle, full: bool) {
        if let Some(f) = self.0.sync_out {
            f(self.0.user, handle, i32::from(full));
        }
    }
    fn before_start(&self, handle: Handle) {
        if let Some(f) = self.0.before_start {
            f(self.0.user, handle);
        }
    }
    fn removed(&self, handle: Handle) {
        if let Some(f) = self.0.removed {
            f(self.0.user, handle);
        }
    }
}

static LIB: Mutex<Option<Library>> = Mutex::new(None);

fn lib() -> Option<Library> {
    LIB.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn code(r: Result<(), dsview_core::SrError>) -> i32 {
    match r {
        Ok(()) => SR_OK,
        Err(e) => e as i32,
    }
}

/// `struct ds_device_base_info` plus what `ds_get_actived_device_info` needs.
#[repr(C)]
pub struct DsvLibEntry {
    pub handle: u64,
    pub dev_type: i32,
    pub actived_times: i32,
    /// 0 demo, 1 capture file, 2 DSLogic, 3 DSCope
    pub driver: i32,
    pub bus: u8,
    pub address: u8,
    pub name: [c_char; 150],
    pub unique_id: [c_char; 64],
    pub path: [c_char; 256],
}

fn copy_str(dst: &mut [c_char], s: &str) {
    let n = s.len().min(dst.len() - 1);
    for (d, &b) in dst.iter_mut().zip(&s.as_bytes()[..n]) {
        *d = b as c_char;
    }
    dst[n] = 0;
}

fn fill(out: &mut DsvLibEntry, e: &Entry) {
    out.handle = e.handle;
    out.dev_type = e.dev_type();
    out.actived_times = e.actived_times;
    (out.driver, out.bus, out.address) = match e.kind {
        Kind::Demo => (0, 0, 0),
        Kind::File(_) => (1, 0, 0),
        Kind::Usb {
            family,
            bus,
            address,
        } => (
            if family == dsview_drivers::dsl::device::Family::DsLogic {
                2
            } else {
                3
            },
            bus,
            address,
        ),
    };
    copy_str(&mut out.name, &e.name);
    copy_str(&mut out.unique_id, &e.unique_id());
    // sdi->path, which C reports for demo and file devices
    let path = match &e.kind {
        Kind::File(_) | Kind::Demo => e.lock().info().path.unwrap_or_default(),
        Kind::Usb { .. } => String::new(),
    };
    copy_str(&mut out.path, &path);
}

/// `ds_lib_init()`: the library with the shim's callbacks; the USB backend
/// is dsview-ffi's (see `set_usb_backend`).
///
/// # Safety
/// `host` must be readable; its callbacks callable from any thread until
/// `dsv_lib_exit`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_lib_init(host: *const DsvLibHost) -> i32 {
    if host.is_null() {
        return SR_ERR_ARG;
    }
    let host = unsafe { *host };
    guard(|| {
        let mut l = LIB.lock().unwrap_or_else(|e| e.into_inner());
        if l.is_some() {
            return SR_ERR_HAVE_DONE;
        }
        *l = Some(Library::init(Arc::new(CHost(host)), usb_backend()));
        SR_OK
    })
}

/// `ds_lib_exit()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_exit() {
    let l = LIB.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(l) = l {
        guard_or((), || l.exit());
    }
}

/// The devices in list order, up to `cap`; returns how many there are.
///
/// # Safety
/// `out` must have room for `cap` entries (or be NULL with `cap` 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_lib_devices(out: *mut DsvLibEntry, cap: u32) -> u32 {
    guard_or(0, || {
        let Some(l) = lib() else { return 0 };
        let list = l.devices();
        for (i, e) in list.iter().take(cap as usize).enumerate() {
            fill(unsafe { &mut *out.add(i) }, e);
        }
        list.len() as u32
    })
}

/// The active device (also one that left the list): 1 and `*out`, or 0.
///
/// # Safety
/// `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_lib_active(out: *mut DsvLibEntry) -> i32 {
    guard_or(0, || {
        let Some(a) = lib().and_then(|l| l.active()) else {
            return 0;
        };
        if !out.is_null() {
            fill(unsafe { &mut *out }, &a);
        }
        1
    })
}

/// `ds_get_actived_device_index()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_active_index() -> i32 {
    guard_or(-1, || lib().map_or(-1, |l| l.active_index()))
}

/// A new handle on device `handle` for the shim's copy (free it with
/// `dsv_drv_free`), or NULL.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_device(handle: u64) -> *mut DsvDevice {
    guard_or(std::ptr::null_mut(), || {
        lib()
            .and_then(|l| l.entry(handle))
            .map_or(std::ptr::null_mut(), |e| DsvDevice::shared(e.dev))
    })
}

/// `ds_active_device()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_activate(handle: u64) -> i32 {
    guard(|| lib().map_or(SR_ERR_CALL_STATUS, |l| code(l.activate(handle))))
}

/// `ds_active_device_by_index()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_activate_index(index: i32) -> i32 {
    guard(|| lib().map_or(SR_ERR_CALL_STATUS, |l| code(l.activate_index(index))))
}

/// `ds_device_from_file()`
///
/// # Safety
/// `path` NUL-terminated or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_lib_device_from_file(path: *const c_char) -> i32 {
    let p = if path.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(path) }
            .to_string_lossy()
            .into_owned()
    };
    guard(|| {
        lib().map_or(SR_ERR_CALL_STATUS, |l| {
            code(l.device_from_file(&p).map(|_| ()))
        })
    })
}

/// `ds_remove_device()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_remove(handle: u64) -> i32 {
    guard(|| lib().map_or(SR_ERR_CALL_STATUS, |l| code(l.remove(handle))))
}

/// `ds_release_actived_device()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_release_active() -> i32 {
    guard(|| lib().map_or(SR_ERR_CALL_STATUS, |l| code(l.release_actived_device())))
}

/// `ds_close_all_device()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_close_all() -> i32 {
    guard(|| {
        if let Some(l) = lib() {
            l.close_all();
        }
        SR_OK
    })
}

/// `ds_reload_device_list()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_reload() -> i32 {
    guard(|| {
        if let Some(l) = lib() {
            l.reload();
        }
        SR_OK
    })
}

/// `ds_start_collect()`; `has_datafeed`: a data callback is set.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_start_collect(has_datafeed: i32) -> i32 {
    guard(|| {
        lib().map_or(SR_ERR_CALL_STATUS, |l| {
            code(l.start_collect(has_datafeed != 0))
        })
    })
}

/// `ds_stop_collect()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_stop_collect() -> i32 {
    guard(|| lib().map_or(SR_ERR_CALL_STATUS, |l| code(l.stop_collect())))
}

/// `sr_session_stop()`: asks the capture to stop without waiting.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_request_stop() {
    guard_or((), || {
        if let Some(l) = lib() {
            l.request_stop();
        }
    });
}

/// `ds_is_collecting()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_is_collecting() -> i32 {
    guard_or(0, || lib().map_or(0, |l| i32::from(l.is_collecting())))
}

/// `ds_get_last_error()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_last_error() -> i32 {
    guard_or(0, || lib().map_or(0, |l| l.last_error()))
}

/// `ds_set_last_error()`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_lib_set_last_error(e: i32) {
    guard_or((), || {
        if let Some(l) = lib() {
            l.set_last_error(e);
        }
    });
}
