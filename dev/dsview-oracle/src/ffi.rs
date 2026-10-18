//! Bindings for csrc/oracle.h.

use std::ffi::{c_char, c_int, c_void};

#[repr(C)]
pub struct OracleOpt {
    pub key: *const c_char,
    pub str_: *const c_char,
    pub num: f64,
    pub is_num: c_int,
}

#[repr(C)]
pub struct OracleDec {
    pub id: *const c_char,
    pub opts: *const OracleOpt,
    pub n_opts: c_int,
    pub ch_id: *const *const c_char,
    pub ch_idx: *const c_int,
    pub n_ch: c_int,
}

pub type OracleEmit = extern "C" fn(user: *mut c_void, json_line: *const c_char);

pub const ORACLE_MAX_CHANNELS: usize = 64;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OracleChannel {
    pub index: c_int,
    pub kind: c_int,
    pub enabled: c_int,
    pub name: [c_char; 64],
}

#[repr(C)]
pub struct OracleDslInfo {
    pub mode: c_int,
    pub samplerate: u64,
    pub limit_samples: u64,
    pub n_channels: c_int,
    pub channels: [OracleChannel; ORACLE_MAX_CHANNELS],
}

pub type OraclePacketCb = extern "C" fn(
    user: *mut c_void,
    kind: c_int,
    format: c_int,
    data: *const u8,
    len: u64,
    num_samples: c_int,
);

unsafe extern "C" {
    pub fn oracle_decode_init(decoder_dir: *const c_char, log_level: c_int) -> c_int;
    pub fn oracle_decode_exit();
    pub fn oracle_decode_list(emit: OracleEmit, user: *mut c_void);
    pub fn oracle_decode_run(
        stack: *const OracleDec,
        n_dec: c_int,
        samplerate: u64,
        total_samples: u64,
        signals: *const *const u8,
        n_signals: c_int,
        max_chunk: u64,
        emit: OracleEmit,
        user: *mut c_void,
    ) -> c_int;

    pub fn oracle_dsl_run(
        path: *const c_char,
        log_level: c_int,
        info: *mut OracleDslInfo,
        cb: OraclePacketCb,
        user: *mut c_void,
    ) -> c_int;
}

#[repr(C)]
pub struct OracleExportChannel {
    pub index: c_int,
    pub kind: c_int,
    pub enabled: c_int,
    pub name: *const c_char,
    pub vdiv: u64,
    pub vfactor: u64,
    pub hw_offset: u16,
    pub map_max: f64,
    pub map_min: f64,
    pub map_unit: *const c_char,
}

pub const EXP_META: c_int = 0;
pub const EXP_LOGIC: c_int = 1;
pub const EXP_DSO: c_int = 2;
pub const EXP_ANALOG: c_int = 3;
pub const EXP_END: c_int = 4;

#[repr(C)]
pub struct OracleExportPacket {
    pub kind: c_int,
    pub samplerate: u64,
    pub limit_samples: u64,
    pub has_ref: c_int,
    pub ref_min: u32,
    pub ref_max: u32,
    pub data: *const u8,
    pub len: u64,
    pub unitsize: u16,
    pub num_samples: c_int,
    pub export_original: c_int,
}

unsafe extern "C" {
    pub fn oracle_export_run(
        module_id: *const c_char,
        kind: c_int,
        start_index: u64,
        time_string: *const c_char,
        chans: *const OracleExportChannel,
        n_ch: c_int,
        pkts: *const OracleExportPacket,
        n_pkts: c_int,
        out: *mut *mut c_char,
    ) -> c_int;
    pub fn oracle_free(p: *mut c_void);
}

// enum oracle_packet_kind / oracle_channel_kind in csrc/oracle.h.
pub const PKT_LOGIC: c_int = 1;
pub const PKT_DSO: c_int = 2;
pub const PKT_ANALOG: c_int = 3;
pub const PKT_END: c_int = 4;
pub const CH_LOGIC: c_int = 1;
pub const CH_DSO: c_int = 2;
pub const CH_ANALOG: c_int = 3;
/// `format` of a logic packet: LA_CROSS_DATA.
pub const LOGIC_CROSS: c_int = 0;

/// Opaque `oracle_store` (pv::data::LogicSnapshot).
#[repr(C)]
pub struct OracleStore {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OstoreCounts {
    pub sample_count: u64,
    pub ring_sample_count: u64,
    pub total_sample_count: u64,
    pub loop_offset: u64,
    pub ring_start: u64,
    pub ring_end: u64,
    pub block_num: c_int,
    pub last_ended: c_int,
    pub channel_num: u32,
}

unsafe extern "C" {
    pub fn ostore_new() -> *mut OracleStore;
    pub fn ostore_free(s: *mut OracleStore);
    pub fn ostore_set_loop(s: *mut OracleStore, is_loop: c_int);
    pub fn ostore_first_payload(
        s: *mut OracleStore,
        data: *const u8,
        len: u64,
        total: u64,
        ch: *const u16,
        n_ch: c_int,
        able_free: c_int,
    );
    pub fn ostore_append(s: *mut OracleStore, data: *const u8, len: u64);
    pub fn ostore_capture_ended(s: *mut OracleStore);
    pub fn ostore_clear(s: *mut OracleStore);
    pub fn ostore_counts(s: *mut OracleStore, c: *mut OstoreCounts);
    pub fn ostore_block_size(s: *mut OracleStore, block: c_int) -> u64;
    pub fn ostore_block_buf(
        s: *mut OracleStore,
        block: c_int,
        sig: c_int,
        sample: *mut c_int,
    ) -> *const u8;
    pub fn ostore_get_samples(
        s: *mut OracleStore,
        start: u64,
        end: *mut u64,
        sig: c_int,
        lbp: *mut *mut c_void,
    ) -> *const u8;
    pub fn ostore_get_sample(s: *mut OracleStore, index: u64, sig: c_int) -> c_int;
    pub fn ostore_has_data(s: *mut OracleStore, sig: c_int) -> c_int;
    pub fn ostore_display_edges(
        s: *mut OracleStore,
        start: u64,
        end: u64,
        width: u16,
        max_togs: u16,
        pixels_offset: f64,
        min_length: f64,
        sig: u16,
        edges: *mut u8,
        n_edges: *mut c_int,
        tog_pos: *mut u16,
        tog_val: *mut u8,
        n_togs: *mut c_int,
    ) -> c_int;
    pub fn ostore_nxt_edge(
        s: *mut OracleStore,
        index: *mut u64,
        last_sample: c_int,
        end: u64,
        min_length: f64,
        sig: c_int,
    ) -> c_int;
    pub fn ostore_pre_edge(
        s: *mut OracleStore,
        index: *mut u64,
        last_sample: c_int,
        min_length: f64,
        sig: c_int,
    ) -> c_int;
    pub fn ostore_pattern_search(
        s: *mut OracleStore,
        start: i64,
        end: i64,
        index: *mut i64,
        chs: *const u16,
        flags: *const c_char,
        n: c_int,
        is_next: c_int,
    ) -> c_int;
    pub fn ostore_block_index_with_sample(
        s: *mut OracleStore,
        sample: u64,
        offset: *mut u64,
    ) -> c_int;
    pub fn ostore_decode_end(s: *mut OracleStore);
    pub fn ostore_free_decode_lpb(s: *mut OracleStore, lbp: *mut c_void);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct OstoreInternals {
    pub byte_fraction: u8,
    pub ch_fraction: u16,
    pub dest_order: i64,
    pub dest_root: i64,
    pub dest_pos: i64,
    pub dest_byte: i64,
    pub last_sample: [u64; 64],
    pub last_calc_count: [u64; 64],
    pub cur_ref_root: [u64; 64],
    pub cur_ref_lbp: [u64; 64],
    pub is_loop: c_int,
    pub able_free: c_int,
    pub kept_leaves: u64,
    pub lst_free_block_index: c_int,
    pub n_orders: u64,
}

unsafe extern "C" {
    pub fn ostore_internals(s: *mut OracleStore, o: *mut OstoreInternals);
    pub fn ostore_roots(s: *mut OracleStore, order: c_int) -> c_int;
    pub fn ostore_root(
        s: *mut OracleStore,
        order: c_int,
        root: c_int,
        bits: *mut u64,
        leaves: *mut *const u64,
    );
}

/// Opaque `osdso` / `osana` (pv::data::DsoSnapshot / AnalogSnapshot).
#[repr(C)]
pub struct OsDso {
    _private: [u8; 0],
}
#[repr(C)]
pub struct OsAna {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsdsoCounts {
    pub sample_count: u64,
    pub total_sample_count: u64,
    pub ring_sample_count: u64,
    pub channel_num: u32,
    pub last_ended: c_int,
    pub memory_failed: c_int,
    pub is_file: c_int,
    pub instant: c_int,
    pub out_of_range: c_int,
    pub block_num: c_int,
    pub n_orders: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsanaCounts {
    pub sample_count: u64,
    pub total_sample_count: u64,
    pub ring_sample_count: u64,
    pub capacity: u64,
    pub channel_num: u32,
    pub unit_bytes: u8,
    pub unit_pitch: u16,
    pub last_ended: c_int,
    pub memory_failed: c_int,
    pub block_num: c_int,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct OsanaSection {
    pub start: u64,
    pub scale: u32,
    pub length: u64,
    pub samples_num: u64,
    pub samples: *const u8,
}

unsafe extern "C" {
    pub fn osdso_new() -> *mut OsDso;
    pub fn osdso_free(s: *mut OsDso);
    pub fn osdso_clear(s: *mut OsDso);
    pub fn osdso_init(s: *mut OsDso);
    pub fn osdso_capture_ended(s: *mut OsDso);
    pub fn osdso_set_ref_range(s: *mut OsDso, ref_max: u32, ref_min: u32);
    pub fn osdso_zero(s: *mut OsDso);
    pub fn osdso_counts(s: *mut OsDso, c: *mut OsdsoCounts);
    pub fn osdso_block_size(s: *mut OsDso, block: c_int) -> u64;
    pub fn osdso_has_data(s: *mut OsDso, sig: c_int) -> c_int;
    pub fn osdso_buffer(s: *mut OsDso, order: c_int) -> *const u8;
    pub fn osdso_get_samples(s: *mut OsDso, start: u64, sig: c_int) -> *const u8;
    pub fn osdso_max_min(s: *mut OsDso, order: c_int, maxv: *mut u8, minv: *mut u8) -> c_int;
    pub fn osdso_first_payload(
        s: *mut OsDso,
        data: *const u8,
        num_samples: c_int,
        total: u64,
        idx: *const u16,
        enabled: *const c_int,
        n: c_int,
        instant: c_int,
        is_file: c_int,
    );
    pub fn osdso_append(s: *mut OsDso, data: *const u8, num_samples: c_int);

    pub fn osana_new() -> *mut OsAna;
    pub fn osana_free(s: *mut OsAna);
    pub fn osana_clear(s: *mut OsAna);
    pub fn osana_init(s: *mut OsAna);
    pub fn osana_capture_ended(s: *mut OsAna);
    pub fn osana_zero(s: *mut OsAna);
    pub fn osana_counts(s: *mut OsAna, c: *mut OsanaCounts);
    pub fn osana_block_size(s: *mut OsAna, block: c_int) -> u64;
    pub fn osana_has_data(s: *mut OsAna, sig: c_int) -> c_int;
    pub fn osana_has_enabled_channel(s: *mut OsAna, sig: c_int) -> c_int;
    pub fn osana_ch_order(s: *mut OsAna, sig: c_int) -> c_int;
    pub fn osana_data(s: *mut OsAna) -> *const u8;
    pub fn osana_envelope_slack(s: *mut OsAna, probe: c_int, level: c_int) -> c_int;
    pub fn osana_envelope(
        s: *mut OsAna,
        probe: c_int,
        level: c_int,
        len: *mut u64,
        ring_len: *mut u64,
        count: *mut u64,
        samples: *mut *const u8,
    );
    pub fn osana_section(
        s: *mut OsAna,
        start: u64,
        count: i64,
        min_length: f32,
        probe: c_int,
        out: *mut OsanaSection,
    );
    pub fn osana_first_payload(
        s: *mut OsAna,
        data: *const u8,
        num_samples: c_int,
        unit_bits: u8,
        unit_pitch: u16,
        total: u64,
        idx: *const u16,
        enabled: *const c_int,
        n: c_int,
    );
    pub fn osana_append(s: *mut OsAna, data: *const u8, num_samples: c_int, unit_pitch: u16);
}
