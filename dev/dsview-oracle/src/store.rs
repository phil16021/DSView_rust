//! Safe wrapper around the original pv::data::LogicSnapshot
//! (csrc/oracle_store.cpp), with the same method shapes as
//! `dsview_store::logic::LogicStore` so tests can drive both alike.

use std::ffi::{c_int, c_void};

use crate::ffi::*;

pub use crate::ffi::OstoreCounts as Counts;

/// One C++ LogicSnapshot. Not Send: the C++ class is used from one thread.
pub struct CLogicStore {
    s: *mut OracleStore,
}

/// Result of [`CLogicStore::get_samples`].
#[derive(Clone, Copy, Debug)]
pub struct CSamples {
    pub data: *const u8,
    pub end_sample: u64,
    pub lbp: *mut c_void,
}

impl Default for CLogicStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CLogicStore {
    /// Constructed and cleared, as DSView uses it.
    pub fn new() -> CLogicStore {
        // SAFETY: plain constructor calls.
        let s = unsafe { ostore_new() };
        unsafe { ostore_clear(s) };
        CLogicStore { s }
    }

    /// The underlying `oracle_store` for direct `ffi::ostore_*` calls.
    pub fn raw(&self) -> *mut OracleStore {
        self.s
    }

    pub fn set_loop(&mut self, is_loop: bool) {
        unsafe { ostore_set_loop(self.s, is_loop as c_int) }
    }

    /// The C++ code reads up to 7 bytes past a packet; the copy is padded
    /// with zeros so those reads are defined and deterministic.
    fn padded(data: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(data.len() + 8);
        v.extend_from_slice(data);
        v.extend_from_slice(&[0; 8]);
        v
    }

    pub fn first_payload(&mut self, data: &[u8], total: u64, channels: &[u16], able_free: bool) {
        let p = Self::padded(data);
        unsafe {
            ostore_first_payload(
                self.s,
                p.as_ptr(),
                data.len() as u64,
                total,
                channels.as_ptr(),
                channels.len() as c_int,
                able_free as c_int,
            )
        }
    }

    pub fn append_payload(&mut self, data: &[u8]) {
        let p = Self::padded(data);
        unsafe { ostore_append(self.s, p.as_ptr(), data.len() as u64) }
    }

    pub fn capture_ended(&mut self) {
        unsafe { ostore_capture_ended(self.s) }
    }

    pub fn clear(&mut self) {
        unsafe { ostore_clear(self.s) }
    }

    pub fn counts(&self) -> Counts {
        let mut c = Counts::default();
        unsafe { ostore_counts(self.s, &mut c) };
        c
    }

    pub fn get_block_size(&self, block: i32) -> u64 {
        unsafe { ostore_block_size(self.s, block) }
    }

    pub fn get_block_buf(&self, block: i32, sig: i32) -> (*const u8, bool) {
        let mut sample = 0;
        let p = unsafe { ostore_block_buf(self.s, block, sig, &mut sample) };
        (p, sample != 0)
    }

    pub fn get_samples(&mut self, start: u64, sig: i32) -> CSamples {
        let mut end = 0;
        let mut lbp = std::ptr::null_mut();
        let data = unsafe { ostore_get_samples(self.s, start, &mut end, sig, &mut lbp) };
        CSamples {
            data,
            end_sample: end,
            lbp,
        }
    }

    pub fn get_sample(&self, index: u64, sig: i32) -> bool {
        unsafe { ostore_get_sample(self.s, index, sig) != 0 }
    }

    pub fn has_data(&self, sig: i32) -> bool {
        unsafe { ostore_has_data(self.s, sig) != 0 }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn get_display_edges(
        &self,
        edges: &mut Vec<(bool, bool)>,
        togs: &mut Vec<(u16, bool)>,
        start: u64,
        end: u64,
        width: u16,
        max_togs: u16,
        pixels_offset: f64,
        min_length: f64,
        sig: u16,
    ) -> bool {
        let mut e = vec![0u8; width as usize + 1];
        let mut tp = vec![0u16; max_togs as usize + 2];
        let mut tv = vec![0u8; max_togs as usize + 2];
        let (mut ne, mut nt) = (0, 0);
        let first = unsafe {
            ostore_display_edges(
                self.s,
                start,
                end,
                width,
                max_togs,
                pixels_offset,
                min_length,
                sig,
                e.as_mut_ptr(),
                &mut ne,
                tp.as_mut_ptr(),
                tv.as_mut_ptr(),
                &mut nt,
            )
        };
        edges.clear();
        edges.extend(e[..ne as usize].iter().map(|&b| (b & 2 != 0, b & 1 != 0)));
        togs.clear();
        togs.extend((0..nt as usize).map(|i| (tp[i], tv[i] != 0)));
        first != 0
    }

    pub fn get_nxt_edge(
        &self,
        index: &mut u64,
        last_sample: bool,
        end: u64,
        min_length: f64,
        sig: i32,
    ) -> bool {
        unsafe { ostore_nxt_edge(self.s, index, last_sample as c_int, end, min_length, sig) != 0 }
    }

    pub fn get_pre_edge(
        &self,
        index: &mut u64,
        last_sample: bool,
        min_length: f64,
        sig: i32,
    ) -> bool {
        unsafe { ostore_pre_edge(self.s, index, last_sample as c_int, min_length, sig) != 0 }
    }

    /// `pattern`: (channel, flag) pairs in channel order.
    pub fn pattern_search(
        &self,
        start: i64,
        end: i64,
        index: &mut i64,
        pattern: &[(u16, u8)],
        is_next: bool,
    ) -> bool {
        let chs: Vec<u16> = pattern.iter().map(|p| p.0).collect();
        let flags: Vec<std::ffi::c_char> =
            pattern.iter().map(|p| p.1 as std::ffi::c_char).collect();
        unsafe {
            ostore_pattern_search(
                self.s,
                start,
                end,
                index,
                chs.as_ptr(),
                flags.as_ptr(),
                pattern.len() as c_int,
                is_next as c_int,
            ) != 0
        }
    }

    pub fn get_block_index_with_sample(&self, sample: u64) -> (i32, u64) {
        let mut off = 0;
        let b = unsafe { ostore_block_index_with_sample(self.s, sample, &mut off) };
        (b, off)
    }

    pub fn decode_end(&mut self) {
        unsafe { ostore_decode_end(self.s) }
    }

    /// `lbp` is only compared with the kept leaves, never dereferenced.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn free_decode_lpb(&mut self, lbp: *mut c_void) {
        unsafe { ostore_free_decode_lpb(self.s, lbp) }
    }
}

/// Internal state of a store, comparable between C++ and Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Internals {
    pub byte_fraction: u8,
    pub ch_fraction: u16,
    /// (order, root, leaf, byte) of the unfinished word's destination.
    pub dest: Option<(usize, usize, usize, usize)>,
    /// Per enabled channel.
    pub last_sample: Vec<u64>,
    pub last_calc_count: Vec<u64>,
    pub cur_ref: Vec<(u64, u64)>,
    pub is_loop: bool,
    pub able_free: bool,
    pub kept_leaves: usize,
    pub lst_free_block_index: i32,
    pub orders: usize,
}

/// One root node: tog/first/last bits and its 64 leaves (words incl. mip-map).
pub struct Root<'a> {
    pub bits: (u64, u64, u64),
    pub leaves: Vec<Option<&'a [u64]>>,
}

/// Words per leaf (C++ `LeafBlockSpace / 8`).
pub const LEAF_WORDS: usize = 266_305;

impl CLogicStore {
    pub fn internals(&self, channels: usize) -> Internals {
        let mut o = std::mem::MaybeUninit::<OstoreInternals>::zeroed();
        // SAFETY: fills the struct.
        let o = unsafe {
            ostore_internals(self.s, o.as_mut_ptr());
            o.assume_init()
        };
        let n = channels.min(64);
        Internals {
            byte_fraction: o.byte_fraction,
            ch_fraction: o.ch_fraction,
            dest: (o.dest_order >= 0).then_some((
                o.dest_order as usize,
                o.dest_root as usize,
                o.dest_pos as usize,
                o.dest_byte as usize,
            )),
            last_sample: o.last_sample[..n].to_vec(),
            last_calc_count: o.last_calc_count[..n].to_vec(),
            cur_ref: (0..n)
                .map(|i| (o.cur_ref_root[i], o.cur_ref_lbp[i]))
                .collect(),
            is_loop: o.is_loop != 0,
            able_free: o.able_free != 0,
            kept_leaves: o.kept_leaves as usize,
            lst_free_block_index: o.lst_free_block_index,
            orders: o.n_orders as usize,
        }
    }

    pub fn roots(&self, order: usize) -> usize {
        unsafe { ostore_roots(self.s, order as c_int) as usize }
    }

    pub fn root(&self, order: usize, root: usize) -> Root<'_> {
        let mut bits = [0u64; 3];
        let mut leaves = [std::ptr::null::<u64>(); 64];
        unsafe {
            ostore_root(
                self.s,
                order as c_int,
                root as c_int,
                bits.as_mut_ptr(),
                leaves.as_mut_ptr(),
            )
        };
        Root {
            bits: (bits[0], bits[1], bits[2]),
            leaves: leaves
                .iter()
                // SAFETY: a live leaf has LEAF_WORDS words; borrowed from &self.
                .map(|&p| {
                    (!p.is_null()).then(|| unsafe { std::slice::from_raw_parts(p, LEAF_WORDS) })
                })
                .collect(),
        }
    }
}

impl Drop for CLogicStore {
    fn drop(&mut self) {
        unsafe { ostore_free(self.s) }
    }
}

/// The original pv::data::DsoSnapshot.
pub struct CDsoStore {
    s: *mut OsDso,
}

impl Default for CDsoStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CDsoStore {
    pub fn new() -> CDsoStore {
        CDsoStore {
            s: unsafe { osdso_new() },
        }
    }
    pub fn clear(&mut self) {
        unsafe { osdso_clear(self.s) }
    }
    pub fn init(&mut self) {
        unsafe { osdso_init(self.s) }
    }
    pub fn capture_ended(&mut self) {
        unsafe { osdso_capture_ended(self.s) }
    }
    pub fn set_ref_range(&mut self, ref_max: u32, ref_min: u32) {
        unsafe { osdso_set_ref_range(self.s, ref_max, ref_min) }
    }
    /// Zero-fills the channel buffers (C++ leaves them uninitialised).
    pub fn zero(&mut self) {
        unsafe { osdso_zero(self.s) }
    }
    pub fn counts(&self) -> OsdsoCounts {
        let mut c = OsdsoCounts::default();
        unsafe { osdso_counts(self.s, &mut c) };
        c
    }
    pub fn get_block_size(&self, block: i32) -> u64 {
        unsafe { osdso_block_size(self.s, block) }
    }
    pub fn has_data(&self, sig: i32) -> bool {
        unsafe { osdso_has_data(self.s, sig) != 0 }
    }
    /// Channel buffer of storage order `order` (`total + 1` bytes).
    pub fn buffer(&self, order: usize) -> Option<&[u8]> {
        let p = unsafe { osdso_buffer(self.s, order as c_int) };
        let n = self.counts().total_sample_count as usize + 1;
        // SAFETY: C++ allocates total + 1 bytes per channel.
        (!p.is_null()).then(|| unsafe { std::slice::from_raw_parts(p, n) })
    }
    pub fn get_samples(&self, start: u64, sig: i32) -> *const u8 {
        unsafe { osdso_get_samples(self.s, start, sig) }
    }
    pub fn get_max_min_value(&self, order: i32) -> Option<(u8, u8)> {
        let (mut mx, mut mn) = (0, 0);
        (unsafe { osdso_max_min(self.s, order, &mut mx, &mut mn) } != 0).then_some((mx, mn))
    }
    /// `channels`: (index, enabled) of the device's DSO channels.
    #[allow(clippy::too_many_arguments)]
    pub fn first_payload(
        &mut self,
        data: &[u8],
        num_samples: i32,
        total: u64,
        channels: &[(u16, bool)],
        instant: bool,
        is_file: bool,
        samplerate_tog: bool,
    ) {
        let idx: Vec<u16> = channels.iter().map(|c| c.0).collect();
        let en: Vec<c_int> = channels.iter().map(|c| c.1 as c_int).collect();
        unsafe {
            osdso_first_payload(
                self.s,
                data.as_ptr(),
                num_samples,
                total,
                idx.as_ptr(),
                en.as_ptr(),
                idx.len() as c_int,
                instant as c_int,
                is_file as c_int,
                samplerate_tog as c_int,
            )
        }
    }
    pub fn append_payload(&mut self, data: &[u8], num_samples: i32, samplerate_tog: bool) {
        unsafe { osdso_append(self.s, data.as_ptr(), num_samples, samplerate_tog as c_int) }
    }
    pub fn enable_envelope(&mut self, enable: bool) {
        unsafe { osdso_enable_envelope(self.s, enable as c_int) }
    }
    /// (start, scale, entries as (min, max)) of `get_envelope_section`.
    pub fn envelope_section(
        &self,
        start: u64,
        end: u64,
        min_length: f32,
        probe: i32,
    ) -> (u64, u32, Vec<(u8, u8)>) {
        let (mut s0, mut scale, mut len, mut p) = (0u64, 0u32, 0u64, std::ptr::null());
        unsafe {
            osdso_envelope_section(
                self.s, start, end, min_length, probe, &mut s0, &mut scale, &mut len, &mut p,
            )
        };
        let entries = if len == 0 || p.is_null() {
            Vec::new()
        } else {
            // SAFETY: `len` (min, max) pairs from the C++ envelope.
            let b = unsafe { std::slice::from_raw_parts(p, len as usize * 2) };
            b.chunks(2).map(|c| (c[0], c[1])).collect()
        };
        (s0, scale, entries)
    }
}

impl Drop for CDsoStore {
    fn drop(&mut self) {
        unsafe { osdso_free(self.s) }
    }
}

/// The original pv::data::AnalogSnapshot.
pub struct CAnalogStore {
    s: *mut OsAna,
}

impl Default for CAnalogStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CAnalogStore {
    pub fn new() -> CAnalogStore {
        CAnalogStore {
            s: unsafe { osana_new() },
        }
    }
    pub fn clear(&mut self) {
        unsafe { osana_clear(self.s) }
    }
    pub fn init(&mut self) {
        unsafe { osana_init(self.s) }
    }
    pub fn capture_ended(&mut self) {
        unsafe { osana_capture_ended(self.s) }
    }
    /// Zero-fills the sample ring and envelopes (C++ leaves them uninitialised).
    pub fn zero(&mut self) {
        unsafe { osana_zero(self.s) }
    }
    pub fn counts(&self) -> OsanaCounts {
        let mut c = OsanaCounts::default();
        unsafe { osana_counts(self.s, &mut c) };
        c
    }
    pub fn get_block_size(&self, block: i32) -> u64 {
        unsafe { osana_block_size(self.s, block) }
    }
    pub fn has_data(&self, sig: i32) -> bool {
        unsafe { osana_has_data(self.s, sig) != 0 }
    }
    pub fn has_enabled_channel(&self, sig: i32) -> bool {
        unsafe { osana_has_enabled_channel(self.s, sig) != 0 }
    }
    pub fn ch_order(&self, sig: i32) -> i32 {
        unsafe { osana_ch_order(self.s, sig) }
    }
    /// The whole sample ring (`capacity` bytes).
    pub fn data(&self) -> &[u8] {
        let p = unsafe { osana_data(self.s) };
        let n = self.counts().capacity as usize;
        if p.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(p, n) }
        }
    }
    /// One envelope level: length, ring length, count, `count` (min, max) pairs.
    pub fn envelope(&self, probe: usize, level: usize) -> (u64, u64, u64, &[u8]) {
        let (mut l, mut r, mut c, mut p) = (0, 0, 0, std::ptr::null());
        unsafe {
            osana_envelope(
                self.s,
                probe as c_int,
                level as c_int,
                &mut l,
                &mut r,
                &mut c,
                &mut p,
            )
        };
        let v = if p.is_null() {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(p, 2 * c as usize) }
        };
        (l, r, c, v)
    }
    /// Bytes after an envelope ring that `zero` filled with neutral entries.
    pub fn envelope_slack(&self, probe: usize, level: usize) -> i32 {
        unsafe { osana_envelope_slack(self.s, probe as c_int, level as c_int) }
    }
    pub fn get_envelope_section(
        &self,
        start: u64,
        count: i64,
        min_length: f32,
        probe: i32,
    ) -> OsanaSection {
        let mut out = std::mem::MaybeUninit::<OsanaSection>::zeroed();
        unsafe {
            osana_section(self.s, start, count, min_length, probe, out.as_mut_ptr());
            out.assume_init()
        }
    }
    /// `channels`: (index, enabled) of the device's analog channels.
    #[allow(clippy::too_many_arguments)]
    pub fn first_payload(
        &mut self,
        data: &[u8],
        num_samples: i32,
        unit_bits: u8,
        unit_pitch: u16,
        total: u64,
        channels: &[(u16, bool)],
    ) {
        let idx: Vec<u16> = channels.iter().map(|c| c.0).collect();
        let en: Vec<c_int> = channels.iter().map(|c| c.1 as c_int).collect();
        unsafe {
            osana_first_payload(
                self.s,
                data.as_ptr(),
                num_samples,
                unit_bits,
                unit_pitch,
                total,
                idx.as_ptr(),
                en.as_ptr(),
                idx.len() as c_int,
            )
        }
    }
    pub fn append_payload(&mut self, data: &[u8], num_samples: i32, unit_pitch: u16) {
        unsafe { osana_append(self.s, data.as_ptr(), num_samples, unit_pitch) }
    }
}

impl Drop for CAnalogStore {
    fn drop(&mut self) {
        unsafe { osana_free(self.s) }
    }
}
