//! The simulated USB device both driver implementations talk to in the
//! oracle (dsview_usb::sim): the C driver through the fake libusb in
//! csrc/fake_libusb.c (the `oracle_sim_*` functions here), the Rust one
//! through `SimBackend`, injected into dsview-ffi. Idle waits on either side
//! advance the virtual clock of [`crate::drv`].

use std::ffi::{c_char, c_int};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsview_drivers::sys;
use dsview_usb::sim::{Hub, SharedHub, SimBackend, SimConfig};
use dsview_usb::{Setup, UsbError};

static HUB: Mutex<Option<SharedHub>> = Mutex::new(None);
/// The data of the transfer `oracle_sim_take` handed out last.
static TAKEN: Mutex<Vec<u8>> = Mutex::new(Vec::new());

fn hub() -> Option<SharedHub> {
    HUB.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// IN transfers a capture may still complete before the session is told
/// to stop (negative: no limit).
static BUDGET: AtomicI64 = AtomicI64::new(-1);

unsafe extern "C" {
    fn sr_session_stop() -> c_int;
}

/// Stops the next capture after `n` completed IN transfers, from inside the
/// driver's event handling: the same point in the C and the Rust driver
/// (both then see the request after that receive_data()). A capture whose
/// data the driver drops (invalid DSO frames) would otherwise never end.
pub fn set_in_budget(n: i64) {
    BUDGET.store(n, Ordering::Relaxed);
}

/// Plugs in the simulated device (before ds_lib_init, which scans) and
/// gives the Rust drivers the same device.
pub fn install(cfg: SimConfig) -> SharedHub {
    let mut hub = Hub::new(cfg);
    hub.on_tick = Some(Box::new(|n| {
        let left = BUDGET.load(Ordering::Relaxed);
        if left < 0 || n == 0 {
            return;
        }
        let left = left - n as i64;
        if left <= 0 {
            BUDGET.store(-1, Ordering::Relaxed);
            unsafe { sr_session_stop() };
        } else {
            BUDGET.store(left, Ordering::Relaxed);
        }
    }));
    let h: SharedHub = Arc::new(Mutex::new(hub));
    *HUB.lock().unwrap_or_else(|e| e.into_inner()) = Some(h.clone());
    let idle = Arc::new(|d: Duration| sys::clock().sleep_us(d.as_micros() as u64));
    dsview_ffi::drv::set_usb_backend(Arc::new(SimBackend {
        hub: h.clone(),
        idle,
    }));
    h
}

/// The transcript since the last call.
pub fn take_log() -> Vec<String> {
    hub().map_or_else(Vec::new, |h| {
        std::mem::take(&mut h.lock().unwrap_or_else(|e| e.into_inner()).log)
    })
}

/// libusb's error codes.
fn usb_err(e: &UsbError) -> c_int {
    match e {
        UsbError::Timeout => -7,
        UsbError::Stall => -9,
        UsbError::Disconnected => -4,
        UsbError::Busy => -6,
        UsbError::NotFound => -5,
        UsbError::Access => -3,
        _ => -1,
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_sim_present() -> c_int {
    c_int::from(hub().is_some())
}

/// vid, pid, speed (libusb_speed), bus, address.
///
/// # Safety
/// All pointers writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oracle_sim_info(
    vid: *mut u16,
    pid: *mut u16,
    speed: *mut c_int,
    bus: *mut u8,
    address: *mut u8,
) {
    let Some(h) = hub() else { return };
    let i = h.lock().unwrap().info();
    unsafe {
        *vid = i.vid;
        *pid = i.pid;
        *speed = i.speed as c_int;
        *bus = i.bus;
        *address = i.address;
    }
}

/// String descriptor `idx` (1 manufacturer, 2 product) into `buf`.
///
/// # Safety
/// `buf` writable for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oracle_sim_string(idx: u8, buf: *mut c_char, len: c_int) -> c_int {
    let Some(h) = hub() else { return -4 };
    let i = h.lock().unwrap().info();
    let s = match idx {
        1 => i.manufacturer,
        2 => i.product,
        _ => None,
    }
    .unwrap_or_default();
    let n = s.len().min(len.max(1) as usize - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf.cast(), n);
        *buf.add(n) = 0;
    }
    n as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_sim_open() {
    if let Some(h) = hub() {
        h.lock().unwrap().open();
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_sim_close() {
    if let Some(h) = hub() {
        h.lock().unwrap().close();
    }
}

/// A control transfer; IN data goes to `data`. Returns the length or a
/// libusb error.
///
/// # Safety
/// `data` valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oracle_sim_control(
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    data: *mut u8,
    len: u16,
) -> c_int {
    let Some(h) = hub() else { return -4 };
    let setup = Setup {
        request_type,
        request,
        value,
        index,
    };
    let mut h = h.lock().unwrap();
    if request_type & 0x80 != 0 {
        match h.control_in(setup, len) {
            Ok(v) => {
                unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), data, v.len()) };
                v.len() as c_int
            }
            Err(e) => usb_err(&e),
        }
    } else {
        let d = if len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(data, usize::from(len)) }
        };
        match h.control_out(setup, d) {
            Ok(()) => c_int::from(len),
            Err(e) => usb_err(&e),
        }
    }
}

/// # Safety
/// `data` valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oracle_sim_bulk_out(ep: u8, data: *const u8, len: c_int) -> c_int {
    let Some(h) = hub() else { return -4 };
    let d = unsafe { std::slice::from_raw_parts(data, len.max(0) as usize) };
    h.lock().unwrap().bulk_out(ep & 0x7f, d) as c_int
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_sim_submit(id: u64, len: c_int) {
    if let Some(h) = hub() {
        h.lock().unwrap().submit(id, len.max(0) as usize);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_sim_cancel(id: u64) {
    if let Some(h) = hub() {
        h.lock().unwrap().cancel(Some(id));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_sim_can_complete() -> c_int {
    hub().map_or(0, |h| c_int::from(h.lock().unwrap().can_complete()))
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_sim_tick() {
    if let Some(h) = hub() {
        h.lock().unwrap().tick();
    }
}

/// The next completed transfer: 1 with its id, status (0 ok, else
/// cancelled), data and length (valid until the next call), or 0.
///
/// # Safety
/// All pointers writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn oracle_sim_take(
    id: *mut u64,
    status: *mut c_int,
    data: *mut *const u8,
    len: *mut c_int,
) -> c_int {
    let Some(h) = hub() else { return 0 };
    let Some(d) = h.lock().unwrap().take_ready() else {
        return 0;
    };
    let mut taken = TAKEN.lock().unwrap_or_else(|e| e.into_inner());
    *taken = d.data;
    unsafe {
        *id = d.id;
        *status = c_int::from(d.status.is_err());
        *data = taken.as_ptr();
        *len = taken.len() as c_int;
    }
    1
}
