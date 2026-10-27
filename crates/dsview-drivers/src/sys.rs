//! Process-wide services the C drivers take from libc and GLib: the clock
//! (`g_timer_*`, `g_usleep`), `rand()`, logging and the resource paths
//! (`DS_USR_PATH`). Tests install a virtual clock and a shared random
//! sequence, so C and Rust drivers can be run against the same inputs.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Monotonic time in microseconds and sleeping, like `g_get_monotonic_time`
/// and `g_usleep`.
pub trait Clock: Send + Sync {
    fn now_us(&self) -> i64;
    fn sleep_us(&self, us: u64);
}

/// `rand()`/`srand()`: values in `0..=RAND_MAX` (2^31 - 1).
pub trait Rng: Send {
    fn rand(&mut self) -> i32;
    fn srand(&mut self, seed: u32);
}

struct RealClock(Instant);

impl Clock for RealClock {
    fn now_us(&self) -> i64 {
        self.0.elapsed().as_micros() as i64
    }
    fn sleep_us(&self, us: u64) {
        std::thread::sleep(Duration::from_micros(us));
    }
}

/// xorshift64*, top 31 bits.
struct XorShift(u64);

impl Rng for XorShift {
    fn rand(&mut self) -> i32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as i32
    }
    fn srand(&mut self, seed: u32) {
        self.0 = u64::from(seed) | 0x9E37_79B9_0000_0000;
    }
}

fn clock_cell() -> &'static RwLock<Arc<dyn Clock>> {
    static CLOCK: OnceLock<RwLock<Arc<dyn Clock>>> = OnceLock::new();
    CLOCK.get_or_init(|| RwLock::new(Arc::new(RealClock(Instant::now()))))
}

fn rng_cell() -> &'static Mutex<Box<dyn Rng>> {
    static RNG: OnceLock<Mutex<Box<dyn Rng>>> = OnceLock::new();
    RNG.get_or_init(|| {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_nanos() as u64);
        Mutex::new(Box::new(XorShift(seed | 1)))
    })
}

pub fn clock() -> Arc<dyn Clock> {
    clock_cell().read().unwrap().clone()
}

pub fn set_clock(c: Arc<dyn Clock>) {
    *clock_cell().write().unwrap() = c;
}

pub fn set_rng(r: Box<dyn Rng>) {
    *rng_cell().lock().unwrap() = r;
}

pub fn rand() -> i32 {
    rng_cell().lock().unwrap().rand()
}

pub fn srand(seed: u32) {
    rng_cell().lock().unwrap().srand(seed)
}

/// `GTimer`: started on creation and by [`Timer::start`].
#[derive(Clone, Copy, Debug)]
pub struct Timer {
    start: i64,
}

impl Timer {
    pub fn new() -> Timer {
        Timer {
            start: clock().now_us(),
        }
    }
    pub fn start(&mut self) {
        self.start = clock().now_us();
    }
    /// `g_timer_elapsed`: seconds, from whole microseconds.
    pub fn elapsed(&self) -> f64 {
        (clock().now_us() - self.start) as f64 / 1e6
    }
}

impl Default for Timer {
    fn default() -> Self {
        Timer::new()
    }
}

/// `g_usleep(SR_MS(secs))` as the C drivers write it: the double is
/// truncated to whole microseconds.
pub fn usleep_secs(secs: f64) -> u64 {
    (secs * 1_000_000.0) as u64
}

/// Log levels as in `sr_log` (1 error .. 5 detail).
pub type LogSink = Box<dyn Fn(i32, &str) + Send + Sync>;

fn log_cell() -> &'static RwLock<Option<LogSink>> {
    static LOG: OnceLock<RwLock<Option<LogSink>>> = OnceLock::new();
    LOG.get_or_init(|| RwLock::new(None))
}

pub fn set_log_sink(sink: LogSink) {
    *log_cell().write().unwrap() = Some(sink);
}

/// Log lines a crash report shows (the last ones, errors to info).
const RECENT_LINES: usize = 200;

fn recent_cell() -> &'static std::sync::Mutex<std::collections::VecDeque<String>> {
    static RECENT: OnceLock<std::sync::Mutex<std::collections::VecDeque<String>>> = OnceLock::new();
    RECENT.get_or_init(|| std::sync::Mutex::new(std::collections::VecDeque::new()))
}

pub fn log(level: i32, msg: &str) {
    if level <= 3
        && let Ok(mut r) = recent_cell().lock()
    {
        if r.len() == RECENT_LINES {
            r.pop_front();
        }
        r.push_back(format!("[{level}] {msg}"));
    }
    if let Some(sink) = log_cell().read().unwrap().as_ref() {
        sink(level, msg);
    }
}

/// The last log lines up to info level, oldest first, whatever the sink
/// shows (for crash reports). Empty when another thread holds them, so a
/// panic hook never waits.
pub fn recent_log() -> Vec<String> {
    recent_cell()
        .try_lock()
        .map(|r| r.iter().cloned().collect())
        .unwrap_or_default()
}

macro_rules! sr_err { ($($t:tt)*) => { $crate::sys::log(1, &format!($($t)*)) } }
macro_rules! sr_info { ($($t:tt)*) => { $crate::sys::log(3, &format!($($t)*)) } }
macro_rules! sr_dbg { ($($t:tt)*) => { $crate::sys::log(4, &format!($($t)*)) } }
pub(crate) use {sr_dbg, sr_err, sr_info};

fn user_dir_cell() -> &'static RwLock<PathBuf> {
    static DIR: OnceLock<RwLock<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| RwLock::new(PathBuf::new()))
}

/// `DS_USR_PATH` (`ds_set_user_data_dir`): the demo patterns live in `<dir>/demo`.
pub fn set_user_dir(dir: PathBuf) {
    *user_dir_cell().write().unwrap() = dir;
}

pub fn user_dir() -> PathBuf {
    user_dir_cell().read().unwrap().clone()
}

fn res_dir_cell() -> &'static RwLock<PathBuf> {
    static DIR: OnceLock<RwLock<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| RwLock::new(PathBuf::new()))
}

/// `DS_RES_PATH` (`ds_set_firmware_resource_dir`): FX2 firmware and FPGA bitstreams.
pub fn set_res_dir(dir: PathBuf) {
    *res_dir_cell().write().unwrap() = dir;
}

pub fn res_dir() -> PathBuf {
    res_dir_cell().read().unwrap().clone()
}
