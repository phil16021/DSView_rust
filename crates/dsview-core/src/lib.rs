//! Types shared by the Rust drivers, the session and the C ABI: config keys
//! and values, channels, device status and datafeed packets. Numeric values
//! are libsigrok4DSL's (libsigrok.h), so they cross the C seam unchanged.

pub mod conf;

pub use conf::{ConfValue, ListItem, key};

/// `enum OPERATION_MODE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum Mode {
    Logic = 0,
    Dso = 1,
    Analog = 2,
}

impl Mode {
    pub fn from_i32(v: i32) -> Option<Mode> {
        match v {
            0 => Some(Mode::Logic),
            1 => Some(Mode::Dso),
            2 => Some(Mode::Analog),
            _ => None,
        }
    }
}

/// `enum CHANNEL_TYPE`.
pub mod channel_type {
    pub const LOGIC: i32 = 10000;
    pub const DSO: i32 = 10001;
    pub const ANALOG: i32 = 10002;
}

/// `enum sr_device_type`.
pub mod dev_type {
    pub const DEMO: i32 = 1;
    pub const FILELOG: i32 = 2;
    pub const USB: i32 = 3;
}

/// `enum sr_device_status`.
pub mod dev_status {
    pub const NOT_FOUND: i32 = 10000;
    pub const INITIALIZING: i32 = 10001;
    pub const INACTIVE: i32 = 10002;
    pub const INCOMPATIBLE: i32 = 10003;
    pub const ACTIVE: i32 = 10004;
    pub const STOPPING: i32 = 10005;
}

/// libsigrok error codes (`SR_ERR_*`, positive in this fork).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum SrError {
    Err = 1,
    Malloc = 2,
    Arg = 3,
    Bug = 4,
    Samplerate = 5,
    Na = 6,
    DeviceClosed = 7,
    CallStatus = 8,
}

pub type SrResult<T> = Result<T, SrError>;

/// `struct sr_channel`, minus the DSCope calibration pointer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Channel {
    pub index: u16,
    pub ty: i32,
    pub enabled: bool,
    pub name: String,
    pub trigger: Option<String>,
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
    pub map_default: bool,
    /// `map_unit` (C: a borrowed `const char *`, NULL when unset).
    pub map_unit: Option<String>,
    pub map_min: f64,
    pub map_max: f64,
}

impl Channel {
    /// `sr_channel_new`: everything else zero.
    pub fn new(index: u16, ty: i32, enabled: bool, name: &str) -> Channel {
        Channel {
            index,
            ty,
            enabled,
            name: name.to_owned(),
            ..Channel::default()
        }
    }
}

/// `struct sr_status`, same layout (gboolean = i32).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct Status {
    pub trig_hit: u8,
    pub captured_cnt3: u8,
    pub captured_cnt2: u8,
    pub captured_cnt1: u8,
    pub captured_cnt0: u8,

    pub pkt_id: u16,
    pub vlen: u32,
    pub stream_mode: i32,
    pub measure_valid: i32,
    pub sample_divider: u32,
    pub sample_divider_tog: i32,
    pub trig_flag: i32,
    pub trig_ch: u8,
    pub trig_offset: u8,

    pub ch0_max: u8,
    pub ch0_min: u8,
    pub ch0_cyc_cnt: u32,
    pub ch0_cyc_tlen: u32,
    pub ch0_cyc_plen: u32,
    pub ch0_cyc_llen: u32,
    pub ch0_level_valid: i32,
    pub ch0_plevel: i32,
    pub ch0_low_level: u8,
    pub ch0_high_level: u8,
    pub ch0_cyc_rlen: u32,
    pub ch0_cyc_flen: u32,
    pub ch0_acc_square: u64,
    pub ch0_acc_mean: u32,
    pub ch0_acc_mean_p1: u32,
    pub ch0_acc_mean_p2: u32,
    pub ch0_acc_mean_p3: u32,

    pub ch1_max: u8,
    pub ch1_min: u8,
    pub ch1_cyc_cnt: u32,
    pub ch1_cyc_tlen: u32,
    pub ch1_cyc_plen: u32,
    pub ch1_cyc_llen: u32,
    pub ch1_level_valid: i32,
    pub ch1_plevel: i32,
    pub ch1_low_level: u8,
    pub ch1_high_level: u8,
    pub ch1_cyc_rlen: u32,
    pub ch1_cyc_flen: u32,
    pub ch1_acc_square: u64,
    pub ch1_acc_mean: u32,
    pub ch1_acc_mean_p1: u32,
    pub ch1_acc_mean_p2: u32,
    pub ch1_acc_mean_p3: u32,
}

/// `SR_PKT_*`.
pub mod pkt_status {
    pub const OK: u16 = 0;
    pub const SOURCE_ERROR: u16 = 1;
    pub const DATA_ERROR: u16 = 2;
}

/// `SR_MQ_VOLTAGE`, `SR_UNIT_VOLT`, `SR_MQFLAG_AC`.
pub const MQ_VOLTAGE: i32 = 10000;
pub const UNIT_VOLT: i32 = 10000;
pub const MQFLAG_AC: u64 = 0x01;

/// A datafeed packet (`struct sr_datafeed_packet` and its payloads). Fields
/// some C drivers leave uninitialized are zero here.
#[derive(Clone, Debug, PartialEq)]
pub struct Packet {
    pub status: u16,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// `sr_datafeed_header`: feed version and `gettimeofday` start time.
    Header {
        feed_version: i32,
        start_sec: i64,
        start_usec: i64,
    },
    End,
    /// `struct ds_trigger_pos`.
    Trigger(TriggerPos),
    /// `sr_datafeed_logic`; `data.len()` is `length`.
    Logic {
        format: i32,
        index: u16,
        order: u16,
        unitsize: u16,
        data_error: u16,
        error_pattern: u64,
        data: Vec<u8>,
    },
    /// `sr_datafeed_dso`; `probes` is the device's channel list.
    Dso {
        num_samples: i32,
        mq: i32,
        unit: i32,
        mqflags: u64,
        samplerate_tog: bool,
        trig_flag: bool,
        trig_ch: u8,
        data: Vec<u8>,
    },
    /// `sr_datafeed_analog`; `probes` is the device's channel list.
    Analog {
        num_samples: i32,
        unit_bits: u8,
        unit_pitch: u16,
        mq: i32,
        unit: i32,
        mqflags: u64,
        data: Vec<u8>,
    },
}

impl Packet {
    pub fn ok(body: Body) -> Packet {
        Packet {
            status: pkt_status::OK,
            body,
        }
    }

    /// `SR_DF_END` with `status`.
    pub fn end(status: u16) -> Packet {
        Packet {
            status,
            body: Body::End,
        }
    }

    /// `enum sr_datafeed_packet_type` value.
    pub fn type_id(&self) -> u16 {
        match self.body {
            Body::Header { .. } => 10000,
            Body::End => 10001,
            Body::Trigger(_) => 10003,
            Body::Logic { .. } => 10004,
            Body::Dso { .. } => 10005,
            Body::Analog { .. } => 10006,
        }
    }
}

/// `struct ds_trigger_pos`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TriggerPos {
    pub check_id: u32,
    pub real_pos: u32,
    pub ram_saddr: u32,
    pub remain_cnt_l: u32,
    pub remain_cnt_h: u32,
    pub status: u32,
}

/// `LA_CROSS_DATA`.
pub const LA_CROSS_DATA: i32 = 0;
