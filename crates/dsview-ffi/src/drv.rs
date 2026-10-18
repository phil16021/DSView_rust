//! `dsv_drv_*`: the Rust drivers (crates/dsview-drivers: demo, file) for libsigrok4DSL's
//! driver shim (`hardware/rust/rs_driver.c`), which wraps each device in an
//! `sr_dev_driver` and translates GVariants and channel structs.
//!
//! Return codes are libsigrok's (`SR_OK` 0, `SR_ERR_*` positive).
//! Acquisition: `dsv_drv_poll` runs one source callback, sleeping without the
//! device lock and forwarding packets through `emit` with no lock held, so
//! the GUI's datafeed callback may call back into the device.

use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::{Arc, Mutex};

use dsview_core::{Body, Channel, ConfValue, ListItem, Packet, SrError};
use dsview_drivers::dsl::device::{self, Family};
use dsview_drivers::{Device, Step, demo, file, sys};
use dsview_usb::UsbBackend;

use crate::error::{guard, guard_or};

const SR_OK: i32 = 0;
const SR_ERR: i32 = 1;
const SR_ERR_ARG: i32 = 3;

fn code(r: Result<(), SrError>) -> i32 {
    match r {
        Ok(()) => SR_OK,
        Err(e) => e as i32,
    }
}

/// A device handle. The device may be shared with the acquisition library
/// (`dsv_lib_device`): each handle is one reference, freed by `dsv_drv_free`.
pub struct DsvDevice {
    dev: Arc<Mutex<Box<dyn Device>>>,
}

impl DsvDevice {
    /// A handle on a device the library keeps.
    pub fn shared(dev: Arc<Mutex<Box<dyn Device>>>) -> *mut DsvDevice {
        Box::into_raw(Box::new(DsvDevice { dev }))
    }
}

thread_local! {
    /// Strings handed out by `dsv_drv_info`/`dsv_drv_channel_get`: valid
    /// until the next such call on the same thread (the GUI and the collect
    /// thread both call them).
    static STRINGS: std::cell::RefCell<Vec<CString>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn clear_strings() {
    STRINGS.with(|s| s.borrow_mut().clear());
}

fn keep(s: &str) -> *const c_char {
    let c = CString::new(s.replace('\0', "")).unwrap_or_default();
    let p = c.as_ptr();
    STRINGS.with(|s| s.borrow_mut().push(c));
    p
}

fn keep_opt(s: Option<&str>) -> *const c_char {
    s.map_or(std::ptr::null(), keep)
}

// ------------------------------------------------------------------ values

/// A config value. `kind`: 0 none, 1 bool (`i`), 2 byte, 3 int16, 4 uint16,
/// 5 int32, 6 uint32, 7 int64 (`i`), 8 uint64 (`u`), 9 double (`d`), 10 string
/// (`s`), 11 strv (`strs`, `n`), 12 `ai` / 13 `at` / 14 `ay` (`arr`, `n`),
/// 15 `a{sv}` (`strs` keys, `items` values, `n`), 16 `sr_list_item` list
/// (`arr` int32 ids, `strs` names, `n`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DsvConf {
    pub kind: i32,
    pub n: u32,
    pub i: i64,
    pub u: u64,
    pub d: f64,
    pub s: *const c_char,
    pub strs: *const *const c_char,
    pub arr: *const c_void,
    pub items: *const DsvConf,
}

impl Default for DsvConf {
    fn default() -> Self {
        DsvConf {
            kind: 0,
            n: 0,
            i: 0,
            u: 0,
            d: 0.0,
            s: std::ptr::null(),
            strs: std::ptr::null(),
            arr: std::ptr::null(),
            items: std::ptr::null(),
        }
    }
}

/// Owner of everything a returned `DsvConf` points to; `c` comes first so
/// the `DsvConf` pointer is the box pointer.
#[repr(C)]
struct ConfBox {
    c: DsvConf,
    keep: Keep,
}

#[derive(Default)]
struct Keep {
    strings: Vec<CString>,
    ptrs: Vec<Vec<*const c_char>>,
    i32s: Vec<Vec<i32>>,
    u64s: Vec<Vec<u64>>,
    bytes: Vec<Vec<u8>>,
    items: Vec<Vec<DsvConf>>,
}

impl Keep {
    fn string(&mut self, s: &str) -> *const c_char {
        let c = CString::new(s.replace('\0', "")).unwrap_or_default();
        let p = c.as_ptr();
        self.strings.push(c);
        p
    }
    fn strv<'a>(&mut self, items: impl Iterator<Item = &'a str>) -> (*const *const c_char, u32) {
        let mut v: Vec<*const c_char> = items.map(|s| self.string(s)).collect();
        let n = v.len() as u32;
        v.push(std::ptr::null());
        let p = v.as_ptr();
        self.ptrs.push(v);
        (p, n)
    }
}

fn to_c(v: &ConfValue, keep: &mut Keep) -> DsvConf {
    let mut c = DsvConf::default();
    match v {
        ConfValue::Bool(b) => (c.kind, c.i) = (1, *b as i64),
        ConfValue::Byte(x) => (c.kind, c.i) = (2, *x as i64),
        ConfValue::I16(x) => (c.kind, c.i) = (3, *x as i64),
        ConfValue::U16(x) => (c.kind, c.i) = (4, *x as i64),
        ConfValue::I32(x) => (c.kind, c.i) = (5, *x as i64),
        ConfValue::U32(x) => (c.kind, c.i) = (6, *x as i64),
        ConfValue::I64(x) => (c.kind, c.i) = (7, *x),
        ConfValue::U64(x) => (c.kind, c.u) = (8, *x),
        ConfValue::F64(x) => (c.kind, c.d) = (9, *x),
        ConfValue::Str(s) => (c.kind, c.s) = (10, keep.string(s)),
        ConfValue::StrV(v) => {
            c.kind = 11;
            (c.strs, c.n) = keep.strv(v.iter().map(String::as_str));
        }
        ConfValue::I32s(v) => {
            (c.kind, c.n, c.arr) = (12, v.len() as u32, v.as_ptr().cast());
            keep.i32s.push(v.clone());
            c.arr = keep.i32s.last().unwrap().as_ptr().cast();
        }
        ConfValue::U64s(v) => {
            keep.u64s.push(v.clone());
            (c.kind, c.n, c.arr) = (
                13,
                v.len() as u32,
                keep.u64s.last().unwrap().as_ptr().cast(),
            );
        }
        ConfValue::Bytes(v) => {
            keep.bytes.push(v.clone());
            (c.kind, c.n, c.arr) = (
                14,
                v.len() as u32,
                keep.bytes.last().unwrap().as_ptr().cast(),
            );
        }
        ConfValue::Dict(d) => {
            c.kind = 15;
            (c.strs, c.n) = keep.strv(d.iter().map(|(k, _)| k.as_str()));
            let items: Vec<DsvConf> = d.iter().map(|(_, v)| to_c(v, keep)).collect();
            c.items = items.as_ptr();
            keep.items.push(items);
        }
        ConfValue::List(l) => {
            c.kind = 16;
            (c.strs, c.n) = keep.strv(l.iter().map(|it| it.name.as_str()));
            keep.i32s.push(l.iter().map(|it| it.id).collect());
            c.arr = keep.i32s.last().unwrap().as_ptr().cast();
        }
    }
    c
}

/// # Safety
/// `c` must be a valid `DsvConf` whose pointers match its kind.
unsafe fn from_c(c: &DsvConf) -> Option<ConfValue> {
    let cstr = |p: *const c_char| -> String {
        if p.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    let slice = |n: u32| -> &[*const c_char] {
        if c.strs.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(c.strs, n as usize) }
        }
    };
    Some(match c.kind {
        1 => ConfValue::Bool(c.i != 0),
        2 => ConfValue::Byte(c.i as u8),
        3 => ConfValue::I16(c.i as i16),
        4 => ConfValue::U16(c.i as u16),
        5 => ConfValue::I32(c.i as i32),
        6 => ConfValue::U32(c.i as u32),
        7 => ConfValue::I64(c.i),
        8 => ConfValue::U64(c.u),
        9 => ConfValue::F64(c.d),
        10 if !c.s.is_null() => ConfValue::Str(cstr(c.s)),
        11 => ConfValue::StrV(slice(c.n).iter().map(|&p| cstr(p)).collect()),
        12 if !c.arr.is_null() => ConfValue::I32s(
            unsafe { std::slice::from_raw_parts(c.arr.cast::<i32>(), c.n as usize) }.to_vec(),
        ),
        13 if !c.arr.is_null() => ConfValue::U64s(
            unsafe { std::slice::from_raw_parts(c.arr.cast::<u64>(), c.n as usize) }.to_vec(),
        ),
        14 if !c.arr.is_null() => ConfValue::Bytes(
            unsafe { std::slice::from_raw_parts(c.arr.cast::<u8>(), c.n as usize) }.to_vec(),
        ),
        16 if !c.arr.is_null() => {
            let ids = unsafe { std::slice::from_raw_parts(c.arr.cast::<i32>(), c.n as usize) };
            ConfValue::List(
                ids.iter()
                    .zip(slice(c.n))
                    .map(|(&id, &p)| ListItem { id, name: cstr(p) })
                    .collect(),
            )
        }
        _ => return None,
    })
}

fn boxed(v: &ConfValue) -> *mut DsvConf {
    let mut keep = Keep::default();
    let c = to_c(v, &mut keep);
    Box::into_raw(Box::new(ConfBox { c, keep })).cast()
}

/// Frees a value returned by `dsv_drv_config_get`/`dsv_drv_config_list`.
///
/// # Safety
/// `v` must come from those functions (or be NULL) and is freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_conf_free(v: *mut DsvConf) {
    if !v.is_null() {
        drop(unsafe { Box::from_raw(v.cast::<ConfBox>()) });
    }
}

// ------------------------------------------------------------- devices

/// Logs through `cb(level, msg)` (sr_log levels, 1 error .. 5 detail).
#[unsafe(no_mangle)]
pub extern "C" fn dsv_drv_set_log(cb: Option<extern "C" fn(i32, *const c_char)>) {
    if let Some(cb) = cb {
        sys::set_log_sink(Box::new(move |level, msg| {
            let c = CString::new(msg.replace('\0', " ")).unwrap_or_default();
            cb(level, c.as_ptr());
        }));
    }
}

/// `DS_USR_PATH`: the demo patterns are read from `<dir>/demo`.
///
/// # Safety
/// `dir` must be NUL-terminated or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_set_user_dir(dir: *const c_char) {
    guard_or((), || {
        if !dir.is_null() {
            let d = unsafe { CStr::from_ptr(dir) }
                .to_string_lossy()
                .into_owned();
            sys::set_user_dir(d.into());
        }
    })
}

fn handle(dev: Box<dyn Device>) -> *mut DsvDevice {
    DsvDevice::shared(Arc::new(Mutex::new(dev)))
}

/// The demo driver's devices (hw_scan): up to `cap` handles into `out`;
/// returns how many.
///
/// # Safety
/// `out` must have room for `cap` pointers.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_demo_scan(out: *mut *mut DsvDevice, cap: u32) -> u32 {
    guard_or(0, || {
        let mut n = 0;
        for d in demo::scan().into_iter().take(cap as usize) {
            unsafe { *out.add(n) = handle(Box::new(d)) };
            n += 1;
        }
        n as u32
    })
}

/// A device for the capture file `path` (sr_new_virtual_device): SR_OK and
/// `*out`, or an error code.
///
/// # Safety
/// `path` must be NUL-terminated, `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_file_new(path: *const c_char, out: *mut *mut DsvDevice) -> i32 {
    guard(|| {
        if path.is_null() || out.is_null() {
            return SR_ERR_ARG;
        }
        let p = unsafe { CStr::from_ptr(path) }
            .to_string_lossy()
            .into_owned();
        match file::FileDevice::new(&p) {
            Ok(d) => {
                unsafe { *out = handle(Box::new(d)) };
                SR_OK
            }
            Err(e) => e as i32,
        }
    })
}

/// `DS_RES_PATH`: FX2 firmware and FPGA bitstreams.
///
/// # Safety
/// `dir` must be NUL-terminated or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_set_res_dir(dir: *const c_char) {
    if !dir.is_null() {
        let d = unsafe { CStr::from_ptr(dir) }
            .to_string_lossy()
            .into_owned();
        guard_or((), || sys::set_res_dir(d.into()));
    }
}

fn backend_cell() -> &'static std::sync::RwLock<Option<std::sync::Arc<dyn UsbBackend>>> {
    static B: std::sync::OnceLock<std::sync::RwLock<Option<std::sync::Arc<dyn UsbBackend>>>> =
        std::sync::OnceLock::new();
    B.get_or_init(|| std::sync::RwLock::new(None))
}

/// The USB backend the hardware drivers use: the real devices (nusb)
/// unless [`set_usb_backend`] chose another one.
pub(crate) fn usb_backend() -> std::sync::Arc<dyn UsbBackend> {
    let cell = backend_cell();
    if let Some(b) = cell.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return b.clone();
    }
    cell.write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(|| std::sync::Arc::new(dsview_usb::nusb_backend::NusbBackend))
        .clone()
}

/// Replaces the USB backend (simulated devices in tests and the oracle);
/// devices opened before keep theirs.
pub fn set_usb_backend(b: std::sync::Arc<dyn UsbBackend>) {
    *backend_cell().write().unwrap_or_else(|e| e.into_inner()) = Some(b);
}

/// A DSLogic or DSCope found on the bus (bus number and device address).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DsvUsbLocation {
    pub bus: u8,
    pub address: u8,
}

fn family_scan(family: Family, out: *mut DsvUsbLocation, cap: u32, slow: *mut i32) -> u32 {
    guard_or(0, || {
        let (list, too_slow) = device::scan(family, usb_backend().as_ref());
        if !slow.is_null() {
            unsafe { *slow = too_slow as i32 };
        }
        let n = list.len().min(cap as usize);
        for (i, c) in list.iter().take(n).enumerate() {
            unsafe {
                *out.add(i) = DsvUsbLocation {
                    bus: c.info.bus,
                    address: c.info.address,
                }
            };
        }
        n as u32
    })
}

fn family_probe(family: Family, bus: u8, address: u8, out: *mut *mut DsvDevice) -> i32 {
    guard(|| {
        let backend = usb_backend();
        let (list, _) = device::scan(family, backend.as_ref());
        let Some(c) = list
            .iter()
            .find(|c| c.info.bus == bus && c.info.address == address)
        else {
            return -1;
        };
        match device::probe(family, backend.clone(), c) {
            device::Probed::Device(d) => {
                unsafe { *out = handle(d) };
                1
            }
            device::Probed::FirmwareUploaded => 0,
            device::Probed::NotReady => -1,
        }
    })
}

/// DSLogic devices on the bus whose profile matches, up to `cap`; `*slow`
/// is set when a DreamSourceLab device runs below high speed.
///
/// # Safety
/// `out` must have room for `cap` entries, `slow` writable or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_dslogic_scan(
    out: *mut DsvUsbLocation,
    cap: u32,
    slow: *mut i32,
) -> u32 {
    family_scan(Family::DsLogic, out, cap, slow)
}

/// Probes the DSLogic at `bus`.`address`: 1 and `*out` when it runs the
/// DSL firmware, 0 after uploading the firmware (it re-enumerates), -1 when
/// it is gone or does not answer.
///
/// # Safety
/// `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_dslogic_probe(
    bus: u8,
    address: u8,
    out: *mut *mut DsvDevice,
) -> i32 {
    family_probe(Family::DsLogic, bus, address, out)
}

/// DSCope devices (also the Hermelin MSO-E8), as [`dsv_drv_dslogic_scan`].
///
/// # Safety
/// `out` must have room for `cap` entries, `slow` writable or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_dscope_scan(
    out: *mut DsvUsbLocation,
    cap: u32,
    slow: *mut i32,
) -> u32 {
    family_scan(Family::DsCope, out, cap, slow)
}

/// Probes a DSCope, as [`dsv_drv_dslogic_probe`].
///
/// # Safety
/// `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_dscope_probe(
    bus: u8,
    address: u8,
    out: *mut *mut DsvDevice,
) -> i32 {
    family_probe(Family::DsCope, bus, address, out)
}

/// A USB device's bus and address after it reconnected (`sdi->conn`).
///
/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_set_location(h: *const DsvDevice, bus: u8, address: u8) {
    guard_or((), || dev(h).lock().set_location(bus, address));
}

/// The library's `struct ds_trigger` (`size` must be its size).
///
/// # Safety
/// `h` must be a live handle, `t` readable for `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_set_trigger(
    h: *const DsvDevice,
    t: *const c_void,
    size: usize,
) -> i32 {
    if t.is_null() || size != std::mem::size_of::<dsview_core::Trigger>() {
        return SR_ERR_ARG;
    }
    guard(|| {
        let trig = unsafe { t.cast::<dsview_core::Trigger>().read_unaligned() };
        dev(h).lock().set_trigger(&trig);
        SR_OK
    })
}

/// The `ds_set_last_error` code of the device's last failure (0: none).
///
/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_last_error(h: *const DsvDevice) -> i32 {
    guard_or(0, || dev(h).lock().last_error())
}

/// # Safety
/// `h` must come from a scan and is freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_free(h: *mut DsvDevice) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

/// `struct sr_dev_inst` identity fields.
#[repr(C)]
pub struct DsvDevInfo {
    pub vendor: *const c_char,
    pub model: *const c_char,
    /// NULL when unset
    pub version: *const c_char,
    pub dev_type: i32,
    pub status: i32,
    pub mode: i32,
    /// NULL when unset
    pub path: *const c_char,
}

fn dev<'a>(h: *const DsvDevice) -> &'a DsvDevice {
    unsafe { &*h }
}

impl DsvDevice {
    /// The device; a panic in an earlier call (caught by `guard`) does not
    /// make it unusable.
    fn lock(&self) -> std::sync::MutexGuard<'_, Box<dyn Device>> {
        self.dev.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// # Safety
/// `h` must be a live handle, `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_info(h: *const DsvDevice, out: *mut DsvDevInfo) {
    guard_or((), || {
        let d = dev(h);
        let info = d.lock().info();
        clear_strings();
        unsafe {
            *out = DsvDevInfo {
                vendor: keep(&info.vendor),
                model: keep(&info.model),
                version: keep_opt(info.version.as_deref()),
                dev_type: info.dev_type,
                status: info.status,
                mode: info.mode as i32,
                path: keep_opt(info.path.as_deref()),
            }
        };
    })
}

/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_set_status(h: *const DsvDevice, status: i32) {
    guard_or((), || {
        dev(h).lock().set_status(status);
    })
}

/// The device's modes (`enum OPERATION_MODE`) in `sr_mode_list` order; returns the count.
///
/// # Safety
/// `h` must be a live handle, `out` must have room for `cap` ints.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_modes(h: *const DsvDevice, out: *mut i32, cap: u32) -> u32 {
    guard_or(0, || {
        let modes = dev(h).lock().mode_list();
        let n = modes.len().min(cap as usize);
        for (i, m) in modes.iter().take(n).enumerate() {
            unsafe { *out.add(i) = *m as i32 };
        }
        n as u32
    })
}

/// `struct sr_channel` without the DSCope VGA pointer. Strings may be NULL.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DsvChannel {
    pub index: u16,
    pub ty: i32,
    pub enabled: i32,
    pub name: *const c_char,
    pub trigger: *const c_char,
    pub bits: u8,
    pub vdiv: u64,
    pub vfactor: u64,
    pub offset: u16,
    pub zero_offset: u16,
    pub hw_offset: u16,
    pub vpos_trans: u16,
    pub coupling: u8,
    pub trig_value: u8,
    pub comb_diff_top: i8,
    pub comb_diff_bom: i8,
    pub comb_comp: i8,
    pub digi_fgain: u16,
    pub cali_fgain: [f64; 4],
    pub cali_comb_fgain: [f64; 4],
    pub map_default: i32,
    pub map_unit: *const c_char,
    pub map_min: f64,
    pub map_max: f64,
}

/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_channel_count(h: *const DsvDevice) -> u32 {
    guard_or(0, || dev(h).lock().channels().len() as u32)
}

/// Changes whenever the device rebuilt its channel list.
///
/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_channel_generation(h: *const DsvDevice) -> u64 {
    guard_or(0, || dev(h).lock().channel_generation())
}

/// Channel `i` of the device's list. Strings stay valid until the next
/// `dsv_drv_channel_get`/`dsv_drv_info` on this thread.
///
/// # Safety
/// `h` must be a live handle, `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_channel_get(
    h: *const DsvDevice,
    i: u32,
    out: *mut DsvChannel,
) -> i32 {
    guard_or(SR_ERR, || {
        let d = dev(h);
        let Some(c) = d.lock().channels().get(i as usize).cloned() else {
            return SR_ERR_ARG;
        };
        clear_strings();
        unsafe {
            *out = DsvChannel {
                index: c.index,
                ty: c.ty,
                enabled: c.enabled as i32,
                name: keep(&c.name),
                trigger: keep_opt(c.trigger.as_deref()),
                bits: c.bits,
                vdiv: c.vdiv,
                vfactor: c.vfactor,
                offset: c.offset,
                zero_offset: c.zero_offset,
                hw_offset: c.hw_offset,
                vpos_trans: c.vpos_trans,
                coupling: c.coupling,
                trig_value: c.trig_value,
                comb_diff_top: c.comb_diff_top,
                comb_diff_bom: c.comb_diff_bom,
                comb_comp: c.comb_comp,
                digi_fgain: c.digi_fgain,
                cali_fgain: c.cali_fgain,
                cali_comb_fgain: c.cali_comb_fgain,
                map_default: c.map_default as i32,
                map_unit: keep_opt(c.map_unit.as_deref()),
                map_min: c.map_min,
                map_max: c.map_max,
            }
        };
        SR_OK
    })
}

/// Copies the C side's channel `i` (which the GUI writes directly) into the device.
///
/// # Safety
/// `h` must be a live handle, `c` a valid channel with valid strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_channel_put(
    h: *const DsvDevice,
    i: u32,
    c: *const DsvChannel,
) -> i32 {
    guard_or(SR_ERR, || {
        let c = unsafe { &*c };
        let s = |p: *const c_char| {
            (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        };
        let mut d = dev(h).lock();
        let Some(ch) = d.channels_mut().get_mut(i as usize) else {
            return SR_ERR_ARG;
        };
        *ch = Channel {
            index: c.index,
            ty: c.ty,
            enabled: c.enabled != 0,
            name: s(c.name).unwrap_or_default(),
            trigger: s(c.trigger),
            bits: c.bits,
            vdiv: c.vdiv,
            vfactor: c.vfactor,
            offset: c.offset,
            zero_offset: c.zero_offset,
            hw_offset: c.hw_offset,
            vpos_trans: c.vpos_trans,
            coupling: c.coupling,
            trig_value: c.trig_value,
            comb_diff_top: c.comb_diff_top,
            comb_diff_bom: c.comb_diff_bom,
            comb_comp: c.comb_comp,
            digi_fgain: c.digi_fgain,
            cali_fgain: c.cali_fgain,
            cali_comb_fgain: c.cali_comb_fgain,
            map_default: c.map_default != 0,
            map_unit: s(c.map_unit),
            map_min: c.map_min,
            map_max: c.map_max,
        };
        SR_OK
    })
}

/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_open(h: *const DsvDevice) -> i32 {
    guard(|| code(dev(h).lock().open()))
}

/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_close(h: *const DsvDevice) -> i32 {
    guard(|| code(dev(h).lock().close()))
}

/// `config_get` for channel position `ch` (-1: none). On SR_OK `*out` is a
/// value to free with `dsv_conf_free`, or NULL when the driver set none.
///
/// # Safety
/// `h` must be a live handle, `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_config_get(
    h: *const DsvDevice,
    key: i32,
    ch: i32,
    out: *mut *mut DsvConf,
) -> i32 {
    guard(|| {
        let ch = usize::try_from(ch).ok();
        match dev(h).lock().config_get(key, ch) {
            Ok(v) => {
                unsafe { *out = v.as_ref().map_or(std::ptr::null_mut(), boxed) };
                SR_OK
            }
            Err(e) => e as i32,
        }
    })
}

/// `config_set`; `value` NULL or of an unsupported kind reads as a GVariant of
/// the wrong type (zero values).
///
/// # Safety
/// `h` must be a live handle, `value` NULL or valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_config_set(
    h: *const DsvDevice,
    key: i32,
    ch: i32,
    value: *const DsvConf,
) -> i32 {
    guard(|| {
        let v = if value.is_null() {
            None
        } else {
            unsafe { from_c(&*value) }
        };
        // a value of no known kind: every typed getter yields zero
        let v = v.unwrap_or(ConfValue::StrV(Vec::new()));
        let ch = usize::try_from(ch).ok();
        code(dev(h).lock().config_set(key, &v, ch))
    })
}

/// `config_list`; on SR_OK `*out` must be freed with `dsv_conf_free`.
///
/// # Safety
/// `h` must be a live handle, `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_config_list(
    h: *const DsvDevice,
    key: i32,
    out: *mut *mut DsvConf,
) -> i32 {
    guard(|| match dev(h).lock().config_list(key) {
        Ok(v) => {
            unsafe { *out = boxed(&v) };
            SR_OK
        }
        Err(e) => e as i32,
    })
}

/// `dev_status_get` into a `struct sr_status` (`size` must be its size).
///
/// # Safety
/// `h` must be a live handle, `out` writable for `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_status(
    h: *const DsvDevice,
    prg: i32,
    out: *mut c_void,
    size: usize,
) -> i32 {
    if size != std::mem::size_of::<dsview_core::Status>() {
        return SR_ERR;
    }
    guard(|| match dev(h).lock().status(prg != 0) {
        Ok(st) => {
            unsafe { out.cast::<dsview_core::Status>().write_unaligned(st) };
            SR_OK
        }
        Err(e) => e as i32,
    })
}

/// A datafeed packet for `ds_data_forward`: `type` is `SR_DF_*`, `data`/`len`
/// the payload bytes (logic: `length`), other fields as in the payload structs.
#[repr(C)]
pub struct DsvPacket {
    pub ty: u16,
    pub status: u16,
    pub data: *const u8,
    pub len: u64,
    pub num_samples: i32,
    pub format: i32,
    pub index: u16,
    pub order: u16,
    pub unitsize: u16,
    pub data_error: u16,
    pub error_pattern: u64,
    pub mq: i32,
    pub unit: i32,
    pub mqflags: u64,
    pub samplerate_tog: i32,
    pub trig_flag: i32,
    pub trig_ch: u8,
    pub unit_bits: u8,
    pub unit_pitch: u16,
    /// SR_DF_HEADER
    pub feed_version: i32,
    pub start_sec: i64,
    pub start_usec: i64,
    /// SR_DF_TRIGGER (`struct ds_trigger_pos`)
    pub trig_check_id: u32,
    pub trig_real_pos: u32,
    pub trig_ram_saddr: u32,
    pub trig_remain_cnt_l: u32,
    pub trig_remain_cnt_h: u32,
    pub trig_status: u32,
}

pub type DsvEmit = extern "C" fn(ctx: *mut c_void, p: *const DsvPacket);

pub(crate) fn emit_all(packets: &[Packet], emit: DsvEmit, ctx: *mut c_void) {
    for p in packets {
        let mut c = DsvPacket {
            ty: p.type_id(),
            status: p.status,
            data: std::ptr::null(),
            len: 0,
            num_samples: 0,
            format: 0,
            index: 0,
            order: 0,
            unitsize: 0,
            data_error: 0,
            error_pattern: 0,
            mq: 0,
            unit: 0,
            mqflags: 0,
            samplerate_tog: 0,
            trig_flag: 0,
            trig_ch: 0,
            unit_bits: 0,
            unit_pitch: 0,
            feed_version: 0,
            start_sec: 0,
            start_usec: 0,
            trig_check_id: 0,
            trig_real_pos: 0,
            trig_ram_saddr: 0,
            trig_remain_cnt_l: 0,
            trig_remain_cnt_h: 0,
            trig_status: 0,
        };
        match &p.body {
            Body::End | Body::Overflow => {}
            Body::Header {
                feed_version,
                start_sec,
                start_usec,
            } => {
                (c.feed_version, c.start_sec, c.start_usec) =
                    (*feed_version, *start_sec, *start_usec)
            }
            Body::Trigger(t) => {
                (c.trig_check_id, c.trig_real_pos, c.trig_ram_saddr) =
                    (t.check_id, t.real_pos, t.ram_saddr);
                (c.trig_remain_cnt_l, c.trig_remain_cnt_h, c.trig_status) =
                    (t.remain_cnt_l, t.remain_cnt_h, t.status);
            }
            Body::Logic {
                format,
                index,
                order,
                unitsize,
                data_error,
                error_pattern,
                data,
            } => {
                (c.data, c.len) = (data.as_ptr(), data.len() as u64);
                (c.format, c.index, c.order, c.unitsize) = (*format, *index, *order, *unitsize);
                (c.data_error, c.error_pattern) = (*data_error, *error_pattern);
            }
            Body::Dso {
                num_samples,
                mq,
                unit,
                mqflags,
                samplerate_tog,
                trig_flag,
                trig_ch,
                data,
            } => {
                (c.data, c.len, c.num_samples) = (data.as_ptr(), data.len() as u64, *num_samples);
                (c.mq, c.unit, c.mqflags) = (*mq, *unit, *mqflags);
                (c.samplerate_tog, c.trig_flag, c.trig_ch) =
                    (*samplerate_tog as i32, *trig_flag as i32, *trig_ch);
            }
            Body::Analog {
                num_samples,
                unit_bits,
                unit_pitch,
                mq,
                unit,
                mqflags,
                data,
            } => {
                (c.data, c.len, c.num_samples) = (data.as_ptr(), data.len() as u64, *num_samples);
                (c.unit_bits, c.unit_pitch) = (*unit_bits, *unit_pitch);
                (c.mq, c.unit, c.mqflags) = (*mq, *unit, *mqflags);
            }
        }
        emit(ctx, &c);
    }
}

/// `dev_acquisition_start`; packets the driver sends here go through `emit`.
///
/// # Safety
/// `h` must be a live handle; `emit` is called with `ctx` and no lock held.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_start(
    h: *const DsvDevice,
    emit: DsvEmit,
    ctx: *mut c_void,
) -> i32 {
    guard(|| {
        let mut out = Vec::new();
        let r = dev(h).lock().start(&mut out);
        emit_all(&out, emit, ctx);
        code(r)
    })
}

/// One source callback: returns 1 while the source stays, 0 once it is removed.
///
/// # Safety
/// `h` must be a live handle; `emit` is called with `ctx` and no lock held.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_poll(
    h: *const DsvDevice,
    abort: i32,
    emit: DsvEmit,
    ctx: *mut c_void,
) -> i32 {
    guard(|| {
        let mut out = Vec::new();
        loop {
            let step = dev(h).lock().poll(abort != 0, &mut out);
            emit_all(&out, emit, ctx);
            out.clear();
            match step {
                Step::Continue => return 1,
                Step::Remove => return 0,
                Step::SleepThenPoll(us) => sys::clock().sleep_us(us),
                Step::SleepThenReturn(us) => {
                    sys::clock().sleep_us(us);
                    return 1;
                }
            }
        }
    })
}

/// `dev_acquisition_stop`.
///
/// # Safety
/// `h` must be a live handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_drv_stop(h: *const DsvDevice, emit: DsvEmit, ctx: *mut c_void) -> i32 {
    guard(|| {
        let mut out = Vec::new();
        let r = dev(h).lock().stop(&mut out);
        emit_all(&out, emit, ctx);
        code(r)
    })
}
