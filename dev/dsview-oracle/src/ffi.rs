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
