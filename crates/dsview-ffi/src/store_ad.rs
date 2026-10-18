//! `dsv_dso_*` / `dsv_analog_*`: the Rust DSO and analog stores behind
//! `pv::data::DsoSnapshot` and `AnalogSnapshot` (`DSVIEW_IMPL_STORE=rust`).
//!
//! As with the C++ classes, sample and envelope pointers stay valid until
//! the next `clear` or a first payload that changes the capture's shape.

use std::sync::{Mutex, MutexGuard};

use dsview_store::analog::{AnalogChannel, AnalogStore};
use dsview_store::dso::{DsoChannel, DsoStore};

use crate::error::guard;

/// Opaque handles owned by C between `dsv_*_new` and `dsv_*_free`.
pub struct DsvDsoStore(Mutex<DsoStore>);
pub struct DsvAnalogStore(Mutex<AnalogStore>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<S, T>(m: Option<&Mutex<S>>, default: T, f: impl FnOnce(&mut S) -> T) -> T {
    let Some(m) = m else {
        return default;
    };
    let mut out = None;
    guard(|| {
        out = Some(f(&mut lock(m)));
        0
    });
    out.unwrap_or(default)
}

fn dso<'a>(p: *const DsvDsoStore) -> Option<&'a Mutex<DsoStore>> {
    // SAFETY: non-NULL handles come from dsv_dso_new.
    unsafe { p.as_ref() }.map(|h| &h.0)
}

fn analog<'a>(p: *const DsvAnalogStore) -> Option<&'a Mutex<AnalogStore>> {
    // SAFETY: non-NULL handles come from dsv_analog_new.
    unsafe { p.as_ref() }.map(|h| &h.0)
}

/// # Safety
/// `data`: `len` readable bytes or NULL.
unsafe fn bytes<'a>(data: *const u8, len: usize) -> &'a [u8] {
    if data.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: per the contract.
        unsafe { std::slice::from_raw_parts(data, len) }
    }
}

/// # Safety
/// `idx`/`enabled`: `n` entries each (or NULL when `n` is 0).
unsafe fn channel_list(idx: *const u16, enabled: *const bool, n: u32) -> Vec<(u16, bool)> {
    (0..n as usize)
        // SAFETY: per the contract.
        .map(|i| unsafe { (*idx.add(i), *enabled.add(i)) })
        .collect()
}

/// # Safety
/// `out`: room for `cap` entries.
unsafe fn copy_channels(ch: &[u16], out: *mut u16, cap: u32) -> u32 {
    for (i, &c) in ch.iter().take(cap as usize).enumerate() {
        // SAFETY: i < cap.
        unsafe { *out.add(i) = c };
    }
    ch.len() as u32
}

// ------------------------------------------------------------------ DSO

#[repr(C)]
#[derive(Default)]
pub struct DsvDsoCounts {
    pub sample_count: u64,
    pub total_sample_count: u64,
    pub channel_num: u32,
    pub last_ended: bool,
    pub memory_failed: bool,
    pub is_file: bool,
    pub instant: bool,
    pub out_of_range: bool,
}

fn dso_report(s: &DsoStore, out: *mut DsvDsoCounts) {
    if !out.is_null() {
        // SAFETY: caller-provided struct.
        unsafe {
            *out = DsvDsoCounts {
                sample_count: s.sample_count(),
                total_sample_count: s.total_sample_count(),
                channel_num: s.channel_num(),
                last_ended: s.last_ended(),
                memory_failed: s.memory_failed(),
                is_file: s.is_file(),
                instant: s.is_instant(),
                out_of_range: s.data_out_off_range(),
            }
        };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_new() -> *mut DsvDsoStore {
    Box::into_raw(Box::new(DsvDsoStore(Mutex::new(DsoStore::new()))))
}

/// # Safety
/// `p` from `dsv_dso_new` (or NULL), not used after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dso_free(p: *mut DsvDsoStore) {
    if !p.is_null() {
        // SAFETY: per the contract.
        drop(unsafe { Box::from_raw(p) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_get_counts(p: *const DsvDsoStore, out: *mut DsvDsoCounts) {
    with(dso(p), (), |s| dso_report(s, out));
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_clear(p: *const DsvDsoStore, out: *mut DsvDsoCounts) {
    with(dso(p), (), |s| {
        s.clear();
        dso_report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_init(p: *const DsvDsoStore, out: *mut DsvDsoCounts) {
    with(dso(p), (), |s| {
        s.init();
        dso_report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_capture_ended(p: *const DsvDsoStore, out: *mut DsvDsoCounts) {
    with(dso(p), (), |s| {
        s.capture_ended();
        dso_report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_set_ref_range(p: *const DsvDsoStore, ref_max: u32, ref_min: u32) {
    with(dso(p), (), |s| s.set_ref_range(ref_max, ref_min));
}

/// First frame: `num_samples` interleaved samples of the used channels
/// (enabled ones, or all for a file); `idx`/`enabled` list the device's DSO
/// channels.
///
/// # Safety
/// `data`: `num_samples` × used channels bytes; `idx`/`enabled`: `n` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dso_first_payload(
    p: *const DsvDsoStore,
    data: *const u8,
    num_samples: i32,
    total_sample_count: u64,
    idx: *const u16,
    enabled: *const bool,
    n: u32,
    instant: bool,
    is_file: bool,
    samplerate_tog: bool,
    out: *mut DsvDsoCounts,
) {
    // SAFETY: per the contract.
    let chans: Vec<DsoChannel> = unsafe { channel_list(idx, enabled, n) }
        .into_iter()
        .map(|(index, enabled)| DsoChannel { index, enabled })
        .collect();
    let used = chans.iter().filter(|c| c.enabled || is_file).count();
    let samples = num_samples.max(0) as usize;
    // SAFETY: per the contract.
    let data = unsafe { bytes(data, samples * used) };
    with(dso(p), (), |s| {
        s.first_payload(
            data,
            samples as u64,
            total_sample_count,
            &chans,
            instant,
            is_file,
            samplerate_tog,
        );
        dso_report(s, out)
    });
}

/// # Safety
/// `data`: `num_samples` × `channel_num` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dso_append_payload(
    p: *const DsvDsoStore,
    data: *const u8,
    num_samples: i32,
    samplerate_tog: bool,
    out: *mut DsvDsoCounts,
) {
    let samples = num_samples.max(0) as usize;
    with(dso(p), (), |s| {
        // SAFETY: per the contract.
        let data = unsafe { bytes(data, samples * s.channel_num() as usize) };
        s.append_payload(data, samples as u64, samplerate_tog);
        dso_report(s, out)
    });
}

/// `DsoSnapshot::EnvelopeSection`; `samples` points at `length` (min, max)
/// byte pairs.
#[repr(C)]
pub struct DsvDsoEnvSection {
    pub start: u64,
    pub scale: u32,
    pub length: u64,
    pub samples: *const u8,
}

/// `enable_envelope`
#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_enable_envelope(p: *const DsvDsoStore, enable: bool) {
    with(dso(p), (), |s| s.enable_envelope(enable));
}

/// `get_envelope_section`: `probe` is the channel's storage position. The
/// pointer stays valid until the store changes.
///
/// # Safety
/// `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dso_envelope_section(
    p: *const DsvDsoStore,
    start: u64,
    end: u64,
    min_length: f32,
    probe: i32,
    out: *mut DsvDsoEnvSection,
) {
    let r = with(dso(p), None, |s| {
        let e = s.get_envelope_section(start, end, min_length, probe.max(0) as usize);
        Some(DsvDsoEnvSection {
            start: e.start,
            scale: e.scale,
            length: e.length,
            samples: e.samples.as_ptr().cast(),
        })
    });
    if !out.is_null() {
        // SAFETY: per the contract.
        unsafe {
            *out = r.unwrap_or(DsvDsoEnvSection {
                start: 0,
                scale: 0,
                length: 0,
                samples: std::ptr::null(),
            });
        }
    }
}

/// Channel `sig`'s samples from `start`; NULL for an unknown channel.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_get_samples(p: *const DsvDsoStore, start: u64, sig: i32) -> *const u8 {
    with(dso(p), std::ptr::null(), |s| {
        s.get_samples(start, sig)
            .map_or(std::ptr::null(), <[u8]>::as_ptr)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_block_num(p: *const DsvDsoStore) -> i32 {
    with(dso(p), 0, |s| s.get_block_num())
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_dso_block_size(p: *const DsvDsoStore, block: i32) -> u64 {
    with(dso(p), 0, |s| s.get_block_size(block))
}

/// # Safety
/// `maxv`/`minv` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dso_max_min(
    p: *const DsvDsoStore,
    order: i32,
    maxv: *mut u8,
    minv: *mut u8,
) -> bool {
    with(dso(p), false, |s| match s.get_max_min_value(order) {
        Some((mx, mn)) => {
            // SAFETY: per the contract.
            unsafe {
                *maxv = mx;
                *minv = mn;
            }
            true
        }
        None => false,
    })
}

/// # Safety
/// `out`: room for `cap` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dso_channels(p: *const DsvDsoStore, out: *mut u16, cap: u32) -> u32 {
    // SAFETY: per the contract.
    with(dso(p), 0, |s| unsafe {
        copy_channels(s.channel_indexes(), out, cap)
    })
}

// ------------------------------------------------------------------ analog

#[repr(C)]
#[derive(Default)]
pub struct DsvAnalogCounts {
    pub sample_count: u64,
    pub total_sample_count: u64,
    pub ring_sample_count: u64,
    pub channel_num: u32,
    pub unit_bytes: u8,
    pub last_ended: bool,
    pub memory_failed: bool,
}

/// `AnalogSnapshot::EnvelopeSection`; `samples` points at (min, max) byte pairs.
#[repr(C)]
pub struct DsvEnvelopeSection {
    pub start: u64,
    pub scale: u32,
    pub length: u64,
    pub samples_num: u64,
    pub samples: *const u8,
}

fn analog_report(s: &AnalogStore, out: *mut DsvAnalogCounts) {
    if !out.is_null() {
        // SAFETY: caller-provided struct.
        unsafe {
            *out = DsvAnalogCounts {
                sample_count: s.sample_count(),
                total_sample_count: s.total_sample_count(),
                ring_sample_count: s.ring_sample_count(),
                channel_num: s.channel_num(),
                unit_bytes: s.unit_bytes(),
                last_ended: s.last_ended(),
                memory_failed: s.memory_failed(),
            }
        };
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_new() -> *mut DsvAnalogStore {
    Box::into_raw(Box::new(DsvAnalogStore(Mutex::new(AnalogStore::new()))))
}

/// # Safety
/// `p` from `dsv_analog_new` (or NULL), not used after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_analog_free(p: *mut DsvAnalogStore) {
    if !p.is_null() {
        // SAFETY: per the contract.
        drop(unsafe { Box::from_raw(p) });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_get_counts(p: *const DsvAnalogStore, out: *mut DsvAnalogCounts) {
    with(analog(p), (), |s| analog_report(s, out));
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_clear(p: *const DsvAnalogStore, out: *mut DsvAnalogCounts) {
    with(analog(p), (), |s| {
        s.clear();
        analog_report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_init(p: *const DsvAnalogStore, out: *mut DsvAnalogCounts) {
    with(analog(p), (), |s| {
        s.init();
        analog_report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_capture_ended(p: *const DsvAnalogStore, out: *mut DsvAnalogCounts) {
    with(analog(p), (), |s| {
        s.capture_ended();
        analog_report(s, out)
    });
}

/// First packet; `idx`/`enabled` list the device's analog channels (all of
/// them carry data).
///
/// # Safety
/// `data`: `num_samples` × channels × ceil(unit_bits / 8) bytes;
/// `idx`/`enabled`: `n` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_analog_first_payload(
    p: *const DsvAnalogStore,
    data: *const u8,
    num_samples: i32,
    unit_bits: u8,
    unit_pitch: u16,
    total_sample_count: u64,
    idx: *const u16,
    enabled: *const bool,
    n: u32,
    out: *mut DsvAnalogCounts,
) {
    // SAFETY: per the contract.
    let chans: Vec<AnalogChannel> = unsafe { channel_list(idx, enabled, n) }
        .into_iter()
        .map(|(index, enabled)| AnalogChannel { index, enabled })
        .collect();
    let samples = num_samples.max(0) as usize;
    // SAFETY: per the contract.
    let data = unsafe { bytes(data, samples * chans.len() * unit_bits.div_ceil(8) as usize) };
    with(analog(p), (), |s| {
        s.first_payload(
            data,
            samples as u64,
            unit_bits,
            unit_pitch,
            total_sample_count,
            &chans,
        );
        analog_report(s, out)
    });
}

/// # Safety
/// `data`: `num_samples` × channels × unit bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_analog_append_payload(
    p: *const DsvAnalogStore,
    data: *const u8,
    num_samples: i32,
    unit_pitch: u16,
    out: *mut DsvAnalogCounts,
) {
    let samples = num_samples.max(0) as usize;
    with(analog(p), (), |s| {
        let bps = s.channel_num() as usize * s.unit_bytes() as usize;
        // SAFETY: per the contract.
        let data = unsafe { bytes(data, samples * bps) };
        s.append_payload(data, samples as u64, unit_pitch);
        analog_report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_get_samples(p: *const DsvAnalogStore, start: u64) -> *const u8 {
    with(analog(p), std::ptr::null(), |s| {
        s.get_samples(start)
            .map_or(std::ptr::null(), <[u8]>::as_ptr)
    })
}

/// The whole sample ring (NULL before the first packet).
#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_data(p: *const DsvAnalogStore) -> *const u8 {
    with(analog(p), std::ptr::null(), |s| {
        if s.data().is_empty() {
            std::ptr::null()
        } else {
            s.data().as_ptr()
        }
    })
}

/// # Safety
/// `out` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_analog_envelope_section(
    p: *const DsvAnalogStore,
    start: u64,
    count: i64,
    min_length: f32,
    probe: i32,
    out: *mut DsvEnvelopeSection,
) {
    with(analog(p), (), |s| {
        let e = s.get_envelope_section(start, count, min_length, probe);
        // SAFETY: per the contract.
        unsafe {
            *out = DsvEnvelopeSection {
                start: e.start,
                scale: e.scale,
                length: e.length,
                samples_num: e.samples_num,
                samples: e.samples.cast(),
            }
        };
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_ch_order(p: *const DsvAnalogStore, sig: i32) -> i32 {
    with(analog(p), -1, |s| s.ch_order(sig))
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_has_enabled_channel(p: *const DsvAnalogStore, sig: i32) -> bool {
    with(analog(p), false, |s| s.has_enabled_channel(sig))
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_block_num(p: *const DsvAnalogStore) -> i32 {
    with(analog(p), 0, |s| s.get_block_num())
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_analog_block_size(p: *const DsvAnalogStore, block: i32) -> u64 {
    with(analog(p), 0, |s| s.get_block_size(block))
}

/// # Safety
/// `out`: room for `cap` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_analog_channels(
    p: *const DsvAnalogStore,
    out: *mut u16,
    cap: u32,
) -> u32 {
    // SAFETY: per the contract.
    with(analog(p), 0, |s| unsafe {
        copy_channels(s.channel_indexes(), out, cap)
    })
}

/// Indexes of the enabled analog channels.
///
/// # Safety
/// `out`: room for `cap` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_analog_enabled_channels(
    p: *const DsvAnalogStore,
    out: *mut u16,
    cap: u32,
) -> u32 {
    // SAFETY: per the contract.
    with(analog(p), 0, |s| unsafe {
        copy_channels(s.enabled_channel_indexes(), out, cap)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dso_and_analog_through_the_c_abi() {
        let d = dsv_dso_new();
        let mut c = DsvDsoCounts::default();
        unsafe {
            dsv_dso_first_payload(
                d,
                [1u8, 9, 2, 8].as_ptr(),
                2,
                4,
                [0u16, 1].as_ptr(),
                [true, true].as_ptr(),
                2,
                false,
                false,
                false,
                &mut c,
            );
            assert_eq!((c.sample_count, c.channel_num), (2, 2));
            let p = dsv_dso_get_samples(d, 0, 1);
            assert_eq!(std::slice::from_raw_parts(p, 2), [9, 8]);
            dsv_dso_free(d);
        }

        let a = dsv_analog_new();
        let mut c = DsvAnalogCounts::default();
        let data: Vec<u8> = (0..32u8).collect();
        unsafe {
            dsv_analog_first_payload(
                a,
                data.as_ptr(),
                32,
                8,
                1,
                64,
                [0u16].as_ptr(),
                [true].as_ptr(),
                1,
                &mut c,
            );
            assert_eq!((c.sample_count, c.ring_sample_count), (32, 32));
            let mut e = std::mem::zeroed::<DsvEnvelopeSection>();
            dsv_analog_envelope_section(a, 0, 32, 1.0, 0, &mut e);
            assert_eq!((e.scale, e.samples_num), (16, 2));
            assert_eq!(std::slice::from_raw_parts(e.samples, 4), [0, 15, 16, 31]);
            dsv_analog_free(a);
        }
    }
}
