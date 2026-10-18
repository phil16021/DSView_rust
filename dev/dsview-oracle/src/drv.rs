//! Shared environment for the driver oracle: one virtual clock and one random
//! sequence that both the C demo driver (demo.c is built with its timers,
//! `g_usleep`, `rand` and `srand` mapped onto the `oracle_*` functions here)
//! and the Rust one (`dsview_drivers::sys`) use, plus the C scenario API.
//!
//! The clock moves only when a driver sleeps or reads it: every reading
//! advances it by a fixed step, standing in for the time a callback takes.
//! Both drivers read it at the same points, so they see identical times.

use std::ffi::{c_char, c_int, c_uint, c_ulong, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use dsview_drivers::sys::{self, Clock, Rng};

static NOW_US: AtomicI64 = AtomicI64::new(0);
static STEP_US: AtomicI64 = AtomicI64::new(1);
static RNG: AtomicU64 = AtomicU64::new(1);

struct VirtualClock;

impl Clock for VirtualClock {
    fn now_us(&self) -> i64 {
        NOW_US.fetch_add(STEP_US.load(Ordering::Relaxed), Ordering::Relaxed)
    }
    fn sleep_us(&self, us: u64) {
        NOW_US.fetch_add(us as i64, Ordering::Relaxed);
    }
}

/// xorshift64*, top 31 bits: `0..=RAND_MAX`.
fn next_rand() -> i32 {
    let mut x = RNG.load(Ordering::Relaxed);
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    RNG.store(x, Ordering::Relaxed);
    (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as i32
}

struct SharedRng;

impl Rng for SharedRng {
    fn rand(&mut self) -> i32 {
        next_rand()
    }
    /// Seeding is ignored so the sequence stays the scenario's.
    fn srand(&mut self, _seed: u32) {}
}

/// Installs the virtual clock (advancing `step_us` per reading) and the
/// random sequence `seed` for both drivers.
pub fn install(seed: u64, step_us: i64) {
    NOW_US.store(0, Ordering::Relaxed);
    STEP_US.store(step_us, Ordering::Relaxed);
    RNG.store(seed | 1, Ordering::Relaxed);
    sys::set_clock(Arc::new(VirtualClock));
    sys::set_rng(Box::new(SharedRng));
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_clock_now_us() -> i64 {
    VirtualClock.now_us()
}

#[unsafe(no_mangle)]
#[allow(clippy::unnecessary_cast)] // c_ulong is not u64 everywhere
pub extern "C" fn oracle_usleep(us: c_ulong) {
    VirtualClock.sleep_us(us as u64);
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_rand() -> c_int {
    next_rand()
}

#[unsafe(no_mangle)]
pub extern "C" fn oracle_srand(_seed: c_uint) {}

/// csrc/oracle.h `struct oracle_drv_packet`.
#[repr(C)]
pub struct OracleDrvPacket {
    pub ty: c_int,
    pub status: c_int,
    pub data: *const c_void,
    pub len: u64,
    pub num_samples: c_int,
    pub format: c_int,
    pub index: c_int,
    pub order: c_int,
    pub unitsize: c_int,
    pub data_error: c_int,
    pub error_pattern: u64,
    pub mq: c_int,
    pub unit: c_int,
    pub mqflags: u64,
    pub samplerate_tog: c_int,
    pub trig_flag: c_int,
    pub trig_ch: c_int,
    pub unit_bits: c_int,
    pub unit_pitch: c_int,
    pub nprobes: c_int,
    pub feed_version: c_int,
    pub trig_real_pos: u32,
}

pub type OracleDrvPacketCb = extern "C" fn(user: *mut c_void, p: *const OracleDrvPacket);

unsafe extern "C" {
    pub fn oracle_drv_init(user_dir: *const c_char, log_level: c_int) -> c_int;
    pub fn oracle_drv_exit();
    pub fn oracle_drv_activate(name: *const c_char) -> c_int;
    pub fn oracle_drv_open_file(path: *const c_char) -> c_int;
    pub fn oracle_drv_get(key: c_int, ch: c_int) -> *mut c_char;
    pub fn oracle_drv_list(key: c_int) -> *mut c_char;
    pub fn oracle_drv_set(key: c_int, ch: c_int, text: *const c_char) -> c_int;
    pub fn oracle_drv_enable(ch: c_int, enable: c_int) -> c_int;
    pub fn oracle_drv_state() -> *mut c_char;
    pub fn oracle_drv_capture(stop_after: c_int, cb: OracleDrvPacketCb, user: *mut c_void)
    -> c_int;
    pub fn oracle_free(p: *mut c_void);
}

/// Takes a g_malloc'd C string.
///
/// # Safety
/// `p` must be NULL or a g_malloc'd NUL-terminated string.
pub unsafe fn take_string(p: *mut c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = unsafe { std::ffi::CStr::from_ptr(p) }
        .to_string_lossy()
        .into_owned();
    unsafe { oracle_free(p.cast()) };
    s
}
