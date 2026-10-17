//! `dsv_logic_*`: the Rust logic sample store behind `pv::data::LogicSnapshot`
//! (selected with `DSVIEW_IMPL_STORE=rust`).
//!
//! Calls lock the store; `dsv_logic_cancel_search` does not, so it can stop
//! a pattern search running on another thread. Pointers returned by
//! `dsv_logic_get_samples`/`dsv_logic_block_buf` point into the store's
//! leaves and stay valid until the leaf is freed (`clear`, a new capture, or
//! for a constant leaf kept for a decoder, `dsv_logic_free_decode_lpb` /
//! `dsv_logic_decode_end`), as with the C++ class.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dsview_store::logic::{LogicStore, Pattern};

use crate::error::guard;

/// Opaque handle owned by C between `dsv_logic_new` and `dsv_logic_free`.
pub struct DsvLogicStore {
    store: Mutex<LogicStore>,
    stop: Arc<AtomicBool>,
}

impl DsvLogicStore {
    fn lock(&self) -> MutexGuard<'_, LogicStore> {
        // a panic inside a call poisons the lock; the store state is still
        // the one the C++ class would have, keep using it
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The `pv::data::Snapshot` fields the C++ wrapper mirrors after each change.
#[repr(C)]
#[derive(Default)]
pub struct DsvLogicCounts {
    pub sample_count: u64,
    pub ring_sample_count: u64,
    pub total_sample_count: u64,
    pub loop_offset: u64,
    pub channel_num: u32,
    pub last_ended: bool,
    pub memory_failed: bool,
    pub able_free: bool,
    pub is_loop: bool,
}

fn counts(s: &LogicStore) -> DsvLogicCounts {
    DsvLogicCounts {
        sample_count: s.sample_count(),
        ring_sample_count: s.ring_sample_count(),
        total_sample_count: s.total_sample_count(),
        loop_offset: s.loop_offset(),
        channel_num: s.channel_num(),
        last_ended: s.last_ended(),
        memory_failed: s.memory_failed(),
        able_free: s.is_able_free(),
        is_loop: s.is_loop(),
    }
}

/// Runs `f` on the store; returns `default` for a NULL handle or a panic.
fn with<T>(p: *const DsvLogicStore, default: T, f: impl FnOnce(&mut LogicStore) -> T) -> T {
    if p.is_null() {
        return default;
    }
    // SAFETY: non-NULL handles come from dsv_logic_new.
    let h = unsafe { &*p };
    let mut out = None;
    guard(|| {
        out = Some(f(&mut h.lock()));
        0
    });
    out.unwrap_or(default)
}

/// Writes the counts to `out` (may be NULL).
fn report(s: &LogicStore, out: *mut DsvLogicCounts) {
    if !out.is_null() {
        // SAFETY: caller-provided struct.
        unsafe { *out = counts(s) };
    }
}

/// # Safety
/// `data` must be NULL or point to `len` readable bytes.
unsafe fn bytes<'a>(data: *const u8, len: u64) -> &'a [u8] {
    if data.is_null() || len == 0 {
        &[]
    } else {
        // SAFETY: per the contract.
        unsafe { std::slice::from_raw_parts(data, len as usize) }
    }
}

/// A new, cleared store. Free with `dsv_logic_free`.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_new() -> *mut DsvLogicStore {
    let store = LogicStore::new();
    let stop = store.search_stop_flag();
    Box::into_raw(Box::new(DsvLogicStore {
        store: Mutex::new(store),
        stop,
    }))
}

/// # Safety
/// `p` must come from `dsv_logic_new` (or be NULL) and not be used after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_free(p: *mut DsvLogicStore) {
    if !p.is_null() {
        // SAFETY: per the contract.
        drop(unsafe { Box::from_raw(p) });
    }
}

/// Counters without changing anything.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_get_counts(p: *const DsvLogicStore, out: *mut DsvLogicCounts) {
    with(p, (), |s| report(s, out));
}

/// C++ `clear()`: frees all data.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_clear(p: *const DsvLogicStore, out: *mut DsvLogicCounts) {
    with(p, (), |s| {
        s.clear();
        report(s, out)
    });
}

/// C++ `init()`: resets counters, keeps data.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_init(p: *const DsvLogicStore, out: *mut DsvLogicCounts) {
    with(p, (), |s| {
        s.init();
        report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_set_loop(p: *const DsvLogicStore, is_loop: bool) {
    with(p, (), |s| s.set_loop(is_loop));
}

/// First LA_CROSS packet of a capture; `chans` are the enabled logic
/// channels' indexes in device order.
///
/// # Safety
/// `data`: `len` bytes; `chans`: `n_chans` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_first_payload(
    p: *const DsvLogicStore,
    data: *const u8,
    len: u64,
    total_sample_count: u64,
    chans: *const u16,
    n_chans: u32,
    able_free: bool,
    out: *mut DsvLogicCounts,
) {
    // SAFETY: per the contract.
    let data = unsafe { bytes(data, len) };
    let chans: &[u16] = if chans.is_null() {
        &[]
    } else {
        // SAFETY: per the contract.
        unsafe { std::slice::from_raw_parts(chans, n_chans as usize) }
    };
    with(p, (), |s| {
        s.first_payload(data, total_sample_count, chans, able_free);
        report(s, out)
    });
}

/// # Safety
/// `data`: `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_append_payload(
    p: *const DsvLogicStore,
    data: *const u8,
    len: u64,
    out: *mut DsvLogicCounts,
) {
    // SAFETY: per the contract.
    let data = unsafe { bytes(data, len) };
    with(p, (), |s| {
        s.append_payload(data);
        report(s, out)
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_capture_ended(p: *const DsvLogicStore, out: *mut DsvLogicCounts) {
    with(p, (), |s| {
        s.capture_ended();
        report(s, out)
    });
}

/// Copies up to `cap` channel indexes (storage order) to `out`; returns how
/// many channels have data.
///
/// # Safety
/// `out`: room for `cap` entries (may be NULL when `cap` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_channels(
    p: *const DsvLogicStore,
    out: *mut u16,
    cap: u32,
) -> u32 {
    with(p, 0, |s| {
        let ch = s.channel_indexes();
        for (i, &c) in ch.iter().take(cap as usize).enumerate() {
            // SAFETY: i < cap.
            unsafe { *out.add(i) = c };
        }
        ch.len() as u32
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_has_data(p: *const DsvLogicStore, sig: i32) -> bool {
    with(p, false, |s| s.has_data(sig))
}

/// Run of samples from `start` for decoders. Returns the data (NULL for a
/// constant block), sets `*end_sample` and, on success, `*lbp` (the leaf to
/// release with `dsv_logic_free_decode_lpb`).
///
/// # Safety
/// `end_sample` must be valid; `lbp` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_get_samples(
    p: *const DsvLogicStore,
    start: u64,
    sig: i32,
    end_sample: *mut u64,
    lbp: *mut *const u8,
) -> *const u8 {
    with(p, std::ptr::null(), |s| {
        let r = s.get_samples(start, sig);
        // SAFETY: caller-provided pointers.
        unsafe {
            *end_sample = r.end_sample;
            if !r.data.is_null() && !lbp.is_null() {
                *lbp = r.lbp;
            }
        }
        r.data
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_get_sample(p: *const DsvLogicStore, index: u64, sig: i32) -> bool {
    with(p, false, |s| s.get_sample(index, sig))
}

/// C++ `get_display_edges`. `edges` gets `*n_edges` entries of
/// `is_edge << 1 | level` (`width`, or `width + 1` when an edge falls on the
/// last pixel, as in C++); `tog_pos`/`tog_val` get `*n_togs` (at most
/// `max(max_togs, 1)`) toggles. Returns the first sample's level.
///
/// # Safety
/// `edges`: `width + 1` entries; `tog_pos`/`tog_val`: `max_togs + 1`
/// entries; `n_edges`/`n_togs` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_display_edges(
    p: *const DsvLogicStore,
    start: u64,
    end: u64,
    width: u16,
    max_togs: u16,
    pixels_offset: f64,
    min_length: f64,
    sig: u16,
    edges: *mut u8,
    n_edges: *mut u32,
    tog_pos: *mut u16,
    tog_val: *mut u8,
    n_togs: *mut u32,
) -> bool {
    with(p, false, |s| {
        let (mut e, mut t) = (Vec::with_capacity(width as usize), Vec::new());
        let first = s.get_display_edges(
            &mut e,
            &mut t,
            start,
            end,
            width,
            max_togs,
            pixels_offset,
            min_length,
            sig,
        );
        let ne = e.len().min(width as usize + 1);
        let nt = t.len().min(max_togs as usize + 1);
        // SAFETY: bounded by the caller's buffer sizes.
        unsafe {
            for (i, &(edge, level)) in e.iter().take(ne).enumerate() {
                *edges.add(i) = (u8::from(edge) << 1) | u8::from(level);
            }
            for (i, &(pos, level)) in t.iter().take(nt).enumerate() {
                *tog_pos.add(i) = pos;
                *tog_val.add(i) = u8::from(level);
            }
            *n_edges = ne as u32;
            *n_togs = nt as u32;
        }
        first
    })
}

/// # Safety
/// `index` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_nxt_edge(
    p: *const DsvLogicStore,
    index: *mut u64,
    last_sample: bool,
    end: u64,
    min_length: f64,
    sig: i32,
) -> bool {
    with(p, false, |s| {
        // SAFETY: per the contract.
        let i = unsafe { &mut *index };
        s.get_nxt_edge(i, last_sample, end, min_length, sig)
    })
}

/// # Safety
/// `index` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_pre_edge(
    p: *const DsvLogicStore,
    index: *mut u64,
    last_sample: bool,
    min_length: f64,
    sig: i32,
) -> bool {
    with(p, false, |s| {
        // SAFETY: per the contract.
        let i = unsafe { &mut *index };
        s.get_pre_edge(i, last_sample, min_length, sig)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_block_num(p: *const DsvLogicStore) -> i32 {
    with(p, 0, |s| s.get_block_num())
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_block_size(p: *const DsvLogicStore, block: i32) -> u64 {
    with(p, 0, |s| s.get_block_size(block))
}

/// Block data (NULL for a constant block, whose level goes to `*sample`).
///
/// # Safety
/// `sample` must be valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_block_buf(
    p: *const DsvLogicStore,
    block: i32,
    sig: i32,
    sample: *mut bool,
) -> *const u8 {
    with(p, std::ptr::null(), |s| {
        let (ptr, level) = s.get_block_buf(block, sig);
        if ptr.is_null() {
            // SAFETY: per the contract.
            unsafe { *sample = level };
        }
        ptr
    })
}

/// Pattern search; `flags[i]` ('0', '1', 'X', 'R', 'F', 'C') applies to
/// channel `chans[i]`.
///
/// # Safety
/// `index` valid; `chans`/`flags`: `n` entries each.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_pattern_search(
    p: *const DsvLogicStore,
    start: i64,
    end: i64,
    index: *mut i64,
    chans: *const u16,
    flags: *const u8,
    n: u32,
    is_next: bool,
) -> bool {
    let mut pattern = Pattern::new();
    for i in 0..n as usize {
        // SAFETY: per the contract.
        unsafe { pattern.insert(*chans.add(i), *flags.add(i)) };
    }
    with(p, false, |s| {
        // SAFETY: per the contract.
        let i = unsafe { &mut *index };
        s.pattern_search(start, end, i, &pattern, is_next)
    })
}

/// Stops a running `dsv_logic_pattern_search` (does not take the lock).
///
/// # Safety
/// `p` from `dsv_logic_new` or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_cancel_search(p: *const DsvLogicStore) {
    if !p.is_null() {
        // SAFETY: non-NULL handles come from dsv_logic_new.
        unsafe { &*p }.stop.store(true, Ordering::Relaxed);
    }
}

/// # Safety
/// `offset` may be NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_logic_block_index_with_sample(
    p: *const DsvLogicStore,
    sample: u64,
    offset: *mut u64,
) -> i32 {
    with(p, 0, |s| {
        let (b, off) = s.get_block_index_with_sample(sample);
        if !offset.is_null() {
            // SAFETY: per the contract.
            unsafe { *offset = off };
        }
        b
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_decode_end(p: *const DsvLogicStore) {
    with(p, (), |s| s.decode_end());
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_logic_free_decode_lpb(p: *const DsvLogicStore, lbp: *const u8) {
    with(p, (), |s| s.free_decode_lpb(lbp));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_through_the_c_abi() {
        let p = dsv_logic_new();
        let words: Vec<u8> = [0u64, !0, 0xff00, 0]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        let mut c = DsvLogicCounts::default();
        unsafe {
            dsv_logic_first_payload(
                p,
                words.as_ptr(),
                16,
                128,
                [5u16, 7].as_ptr(),
                2,
                true,
                &mut c,
            );
            dsv_logic_append_payload(p, words[16..].as_ptr(), 16, &mut c);
        }
        dsv_logic_capture_ended(p, &mut c);
        assert_eq!(
            (c.sample_count, c.channel_num, c.last_ended),
            (128, 2, true)
        );
        assert!(!dsv_logic_get_sample(p, 0, 5) && dsv_logic_get_sample(p, 0, 7));
        assert!(dsv_logic_get_sample(p, 64 + 8, 5));
        let mut i = 64;
        assert!(unsafe { dsv_logic_nxt_edge(p, &mut i, false, 127, 1.0, 5) });
        assert_eq!(i, 72);
        let mut ch = [0u16; 4];
        assert_eq!(unsafe { dsv_logic_channels(p, ch.as_mut_ptr(), 4) }, 2);
        assert_eq!(&ch[..2], &[5, 7]);
        unsafe {
            dsv_logic_cancel_search(p);
            dsv_logic_free(p);
        }
    }
}
