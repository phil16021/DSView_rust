//! Configuration keys and values, as libsigrok4DSL's `config_get`/`config_set`/
//! `config_list` exchange them in GVariants.

/// `enum sr_config_option_id` (libsigrok.h); `tests/keys.rs` checks every value.
pub mod key {
    pub const LOGIC_ANALYZER: i32 = 10000;
    pub const OSCILLOSCOPE: i32 = 10001;
    pub const MULTIMETER: i32 = 10002;
    pub const DEMO_DEV: i32 = 10003;
    pub const SOUNDLEVELMETER: i32 = 10004;
    pub const THERMOMETER: i32 = 10005;
    pub const HYGROMETER: i32 = 10006;
    pub const CONN: i32 = 20000;
    pub const SERIALCOMM: i32 = 20001;
    pub const SAMPLERATE: i32 = 30000;
    pub const CAPTURE_RATIO: i32 = 30001;
    pub const USB_SPEED: i32 = 30002;
    pub const USB30_SUPPORT: i32 = 30003;
    pub const DEVICE_MODE: i32 = 30004;
    pub const INSTANT: i32 = 30005;
    pub const STATUS: i32 = 30006;
    pub const PATTERN_MODE: i32 = 30007;
    pub const RLE: i32 = 30008;
    pub const WAIT_UPLOAD: i32 = 30009;
    pub const TRIGGER_SLOPE: i32 = 30010;
    pub const TRIGGER_SOURCE: i32 = 30011;
    pub const TRIGGER_CHANNEL: i32 = 30012;
    pub const TRIGGER_VALUE: i32 = 30013;
    pub const HORIZ_TRIGGERPOS: i32 = 30014;
    pub const TRIGGER_HOLDOFF: i32 = 30015;
    pub const TRIGGER_MARGIN: i32 = 30016;
    pub const BUFFERSIZE: i32 = 30017;
    pub const MAX_TIMEBASE: i32 = 30018;
    pub const MIN_TIMEBASE: i32 = 30019;
    pub const TIMEBASE: i32 = 30020;
    pub const FILTER: i32 = 30021;
    pub const DSO_SYNC: i32 = 30022;
    pub const UNIT_BITS: i32 = 30023;
    pub const REF_MIN: i32 = 30024;
    pub const REF_MAX: i32 = 30025;
    pub const TOTAL_CH_NUM: i32 = 30026;
    pub const VLD_CH_NUM: i32 = 30027;
    pub const LA_CH32: i32 = 30028;
    pub const HAVE_ZERO: i32 = 30029;
    pub const ZERO: i32 = 30030;
    pub const ZERO_SET: i32 = 30031;
    pub const ZERO_LOAD: i32 = 30032;
    pub const ZERO_DEFAULT: i32 = 30033;
    pub const ZERO_COMB_FGAIN: i32 = 30034;
    pub const ZERO_COMB: i32 = 30035;
    pub const VOCM: i32 = 30036;
    pub const CALI: i32 = 30037;
    pub const STATUS_PERIOD: i32 = 30038;
    pub const STATUS_PCNT: i32 = 30039;
    pub const STATUS_MAX: i32 = 30040;
    pub const STATUS_MIN: i32 = 30041;
    pub const STATUS_PLEN: i32 = 30042;
    pub const STATUS_LLEN: i32 = 30043;
    pub const STATUS_LEVEL: i32 = 30044;
    pub const STATUS_PLEVEL: i32 = 30045;
    pub const STATUS_LOW: i32 = 30046;
    pub const STATUS_HIGH: i32 = 30047;
    pub const STATUS_RLEN: i32 = 30048;
    pub const STATUS_FLEN: i32 = 30049;
    pub const STATUS_RMS: i32 = 30050;
    pub const STATUS_MEAN: i32 = 30051;
    pub const STREAM: i32 = 30052;
    pub const ROLL: i32 = 30053;
    pub const TEST: i32 = 30054;
    pub const EEPROM: i32 = 30055;
    pub const TUNE: i32 = 30056;
    pub const TUNE_SEL: i32 = 30057;
    pub const EXTEND_ID: i32 = 30058;
    pub const EXTEND_DATA: i32 = 30059;
    pub const SAMPLE_INTERVAL: i32 = 30060;
    pub const NUM_TIMEBASE: i32 = 30061;
    pub const NUM_VDIV: i32 = 30062;
    pub const CLOCK_TYPE: i32 = 30063;
    pub const CLOCK_EDGE: i32 = 30064;
    pub const OPERATION_MODE: i32 = 30065;
    pub const BUFFER_OPTIONS: i32 = 30066;
    pub const CHANNEL_MODE: i32 = 30067;
    pub const RLE_SUPPORT: i32 = 30068;
    pub const MAX_HEIGHT: i32 = 30069;
    pub const MAX_HEIGHT_VALUE: i32 = 30070;
    pub const THRESHOLD: i32 = 30071;
    pub const VTH: i32 = 30072;
    pub const MAX_DSO_SAMPLERATE: i32 = 30073;
    pub const MAX_DSO_SAMPLELIMITS: i32 = 30074;
    pub const HW_DEPTH: i32 = 30075;
    pub const BANDWIDTH: i32 = 30076;
    pub const BANDWIDTH_LIMIT: i32 = 30077;
    pub const PROBE_CONFIGS: i32 = 30078;
    pub const PROBE_EN: i32 = 30080;
    pub const PROBE_COUPLING: i32 = 30081;
    pub const PROBE_VDIV: i32 = 30082;
    pub const PROBE_FACTOR: i32 = 30083;
    pub const PROBE_MAP_DEFAULT: i32 = 30084;
    pub const PROBE_MAP_UNIT: i32 = 30085;
    pub const PROBE_MAP_MIN: i32 = 30086;
    pub const PROBE_MAP_MAX: i32 = 30087;
    pub const PROBE_OFFSET: i32 = 30088;
    pub const PROBE_HW_OFFSET: i32 = 30089;
    pub const PROBE_PREOFF: i32 = 30090;
    pub const PROBE_PREOFF_DEFAULT: i32 = 30091;
    pub const PROBE_PREOFF_MARGIN: i32 = 30092;
    pub const PROBE_VGAIN: i32 = 30093;
    pub const PROBE_VGAIN_DEFAULT: i32 = 30094;
    pub const PROBE_VGAIN_RANGE: i32 = 30095;
    pub const PROBE_COMB_COMP_EN: i32 = 30096;
    pub const PROBE_COMB_COMP: i32 = 30097;
    pub const DEVICE_OPTIONS: i32 = 30098;
    pub const DEVICE_SESSIONS: i32 = 30099;
    pub const FILE_VERSION: i32 = 30102;
    pub const CAPTURE_NUM_PROBES: i32 = 30103;
    pub const NUM_BLOCKS: i32 = 30104;
    pub const LOAD_DECODER: i32 = 30105;
    pub const DEMO_INIT: i32 = 30106;
    pub const DEMO_CHANGE: i32 = 30107;
    pub const LIMIT_MSEC: i32 = 50000;
    pub const LIMIT_SAMPLES: i32 = 50001;
    pub const TRIGGER_TIME: i32 = 50002;
    pub const TRIGGER_POS: i32 = 50003;
    pub const ACTUAL_SAMPLES: i32 = 50004;
    pub const LIMIT_FRAMES: i32 = 50005;
    pub const CONTINUOUS: i32 = 50006;
    pub const DATALOG: i32 = 50007;
    pub const EXT_SAMPLERATE: i32 = 50008;
    pub const LOOP_MODE: i32 = 60001;

    /// Every key with its C name, for the header check.
    pub const ALL: &[(&str, i32)] = &[
        ("LOGIC_ANALYZER", LOGIC_ANALYZER),
        ("OSCILLOSCOPE", OSCILLOSCOPE),
        ("MULTIMETER", MULTIMETER),
        ("DEMO_DEV", DEMO_DEV),
        ("SOUNDLEVELMETER", SOUNDLEVELMETER),
        ("THERMOMETER", THERMOMETER),
        ("HYGROMETER", HYGROMETER),
        ("CONN", CONN),
        ("SERIALCOMM", SERIALCOMM),
        ("SAMPLERATE", SAMPLERATE),
        ("CAPTURE_RATIO", CAPTURE_RATIO),
        ("USB_SPEED", USB_SPEED),
        ("USB30_SUPPORT", USB30_SUPPORT),
        ("DEVICE_MODE", DEVICE_MODE),
        ("INSTANT", INSTANT),
        ("STATUS", STATUS),
        ("PATTERN_MODE", PATTERN_MODE),
        ("RLE", RLE),
        ("WAIT_UPLOAD", WAIT_UPLOAD),
        ("TRIGGER_SLOPE", TRIGGER_SLOPE),
        ("TRIGGER_SOURCE", TRIGGER_SOURCE),
        ("TRIGGER_CHANNEL", TRIGGER_CHANNEL),
        ("TRIGGER_VALUE", TRIGGER_VALUE),
        ("HORIZ_TRIGGERPOS", HORIZ_TRIGGERPOS),
        ("TRIGGER_HOLDOFF", TRIGGER_HOLDOFF),
        ("TRIGGER_MARGIN", TRIGGER_MARGIN),
        ("BUFFERSIZE", BUFFERSIZE),
        ("MAX_TIMEBASE", MAX_TIMEBASE),
        ("MIN_TIMEBASE", MIN_TIMEBASE),
        ("TIMEBASE", TIMEBASE),
        ("FILTER", FILTER),
        ("DSO_SYNC", DSO_SYNC),
        ("UNIT_BITS", UNIT_BITS),
        ("REF_MIN", REF_MIN),
        ("REF_MAX", REF_MAX),
        ("TOTAL_CH_NUM", TOTAL_CH_NUM),
        ("VLD_CH_NUM", VLD_CH_NUM),
        ("LA_CH32", LA_CH32),
        ("HAVE_ZERO", HAVE_ZERO),
        ("ZERO", ZERO),
        ("ZERO_SET", ZERO_SET),
        ("ZERO_LOAD", ZERO_LOAD),
        ("ZERO_DEFAULT", ZERO_DEFAULT),
        ("ZERO_COMB_FGAIN", ZERO_COMB_FGAIN),
        ("ZERO_COMB", ZERO_COMB),
        ("VOCM", VOCM),
        ("CALI", CALI),
        ("STATUS_PERIOD", STATUS_PERIOD),
        ("STATUS_PCNT", STATUS_PCNT),
        ("STATUS_MAX", STATUS_MAX),
        ("STATUS_MIN", STATUS_MIN),
        ("STATUS_PLEN", STATUS_PLEN),
        ("STATUS_LLEN", STATUS_LLEN),
        ("STATUS_LEVEL", STATUS_LEVEL),
        ("STATUS_PLEVEL", STATUS_PLEVEL),
        ("STATUS_LOW", STATUS_LOW),
        ("STATUS_HIGH", STATUS_HIGH),
        ("STATUS_RLEN", STATUS_RLEN),
        ("STATUS_FLEN", STATUS_FLEN),
        ("STATUS_RMS", STATUS_RMS),
        ("STATUS_MEAN", STATUS_MEAN),
        ("STREAM", STREAM),
        ("ROLL", ROLL),
        ("TEST", TEST),
        ("EEPROM", EEPROM),
        ("TUNE", TUNE),
        ("TUNE_SEL", TUNE_SEL),
        ("EXTEND_ID", EXTEND_ID),
        ("EXTEND_DATA", EXTEND_DATA),
        ("SAMPLE_INTERVAL", SAMPLE_INTERVAL),
        ("NUM_TIMEBASE", NUM_TIMEBASE),
        ("NUM_VDIV", NUM_VDIV),
        ("CLOCK_TYPE", CLOCK_TYPE),
        ("CLOCK_EDGE", CLOCK_EDGE),
        ("OPERATION_MODE", OPERATION_MODE),
        ("BUFFER_OPTIONS", BUFFER_OPTIONS),
        ("CHANNEL_MODE", CHANNEL_MODE),
        ("RLE_SUPPORT", RLE_SUPPORT),
        ("MAX_HEIGHT", MAX_HEIGHT),
        ("MAX_HEIGHT_VALUE", MAX_HEIGHT_VALUE),
        ("THRESHOLD", THRESHOLD),
        ("VTH", VTH),
        ("MAX_DSO_SAMPLERATE", MAX_DSO_SAMPLERATE),
        ("MAX_DSO_SAMPLELIMITS", MAX_DSO_SAMPLELIMITS),
        ("HW_DEPTH", HW_DEPTH),
        ("BANDWIDTH", BANDWIDTH),
        ("BANDWIDTH_LIMIT", BANDWIDTH_LIMIT),
        ("PROBE_CONFIGS", PROBE_CONFIGS),
        ("PROBE_EN", PROBE_EN),
        ("PROBE_COUPLING", PROBE_COUPLING),
        ("PROBE_VDIV", PROBE_VDIV),
        ("PROBE_FACTOR", PROBE_FACTOR),
        ("PROBE_MAP_DEFAULT", PROBE_MAP_DEFAULT),
        ("PROBE_MAP_UNIT", PROBE_MAP_UNIT),
        ("PROBE_MAP_MIN", PROBE_MAP_MIN),
        ("PROBE_MAP_MAX", PROBE_MAP_MAX),
        ("PROBE_OFFSET", PROBE_OFFSET),
        ("PROBE_HW_OFFSET", PROBE_HW_OFFSET),
        ("PROBE_PREOFF", PROBE_PREOFF),
        ("PROBE_PREOFF_DEFAULT", PROBE_PREOFF_DEFAULT),
        ("PROBE_PREOFF_MARGIN", PROBE_PREOFF_MARGIN),
        ("PROBE_VGAIN", PROBE_VGAIN),
        ("PROBE_VGAIN_DEFAULT", PROBE_VGAIN_DEFAULT),
        ("PROBE_VGAIN_RANGE", PROBE_VGAIN_RANGE),
        ("PROBE_COMB_COMP_EN", PROBE_COMB_COMP_EN),
        ("PROBE_COMB_COMP", PROBE_COMB_COMP),
        ("DEVICE_OPTIONS", DEVICE_OPTIONS),
        ("DEVICE_SESSIONS", DEVICE_SESSIONS),
        ("FILE_VERSION", FILE_VERSION),
        ("CAPTURE_NUM_PROBES", CAPTURE_NUM_PROBES),
        ("NUM_BLOCKS", NUM_BLOCKS),
        ("LOAD_DECODER", LOAD_DECODER),
        ("DEMO_INIT", DEMO_INIT),
        ("DEMO_CHANGE", DEMO_CHANGE),
        ("LIMIT_MSEC", LIMIT_MSEC),
        ("LIMIT_SAMPLES", LIMIT_SAMPLES),
        ("TRIGGER_TIME", TRIGGER_TIME),
        ("TRIGGER_POS", TRIGGER_POS),
        ("ACTUAL_SAMPLES", ACTUAL_SAMPLES),
        ("LIMIT_FRAMES", LIMIT_FRAMES),
        ("CONTINUOUS", CONTINUOUS),
        ("DATALOG", DATALOG),
        ("EXT_SAMPLERATE", EXT_SAMPLERATE),
        ("LOOP_MODE", LOOP_MODE),
    ];
}

/// One `sr_list_item` of the lists some keys return by pointer.
#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub id: i32,
    pub name: String,
}

/// A config value with the exact GVariant type the C driver uses for it; the
/// GUI asserts on those types.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfValue {
    /// `b`
    Bool(bool),
    /// `y`
    Byte(u8),
    /// `n`
    I16(i16),
    /// `q`
    U16(u16),
    /// `i`
    I32(i32),
    /// `u`
    U32(u32),
    /// `x`
    I64(i64),
    /// `t`
    U64(u64),
    /// `d`
    F64(f64),
    /// `s`
    Str(String),
    /// `as`
    StrV(Vec<String>),
    /// `ai`
    I32s(Vec<i32>),
    /// `at`
    U64s(Vec<u64>),
    /// `ay`
    Bytes(Vec<u8>),
    /// `a{sv}`
    Dict(Vec<(String, ConfValue)>),
    /// A `struct sr_list_item[]` terminated by id -1, passed as a `t` pointer.
    List(Vec<ListItem>),
}

/// Strict getters like `g_variant_get_<type>`: a value of another type gives
/// the zero value, as GLib returns after its type-check warning.
impl ConfValue {
    pub fn get_bool(&self) -> bool {
        matches!(*self, ConfValue::Bool(true))
    }
    pub fn get_byte(&self) -> u8 {
        match *self {
            ConfValue::Byte(v) => v,
            _ => 0,
        }
    }
    pub fn get_i16(&self) -> i16 {
        match *self {
            ConfValue::I16(v) => v,
            _ => 0,
        }
    }
    pub fn get_u16(&self) -> u16 {
        match *self {
            ConfValue::U16(v) => v,
            _ => 0,
        }
    }
    pub fn get_i32(&self) -> i32 {
        match *self {
            ConfValue::I32(v) => v,
            _ => 0,
        }
    }
    pub fn get_u32(&self) -> u32 {
        match *self {
            ConfValue::U32(v) => v,
            _ => 0,
        }
    }
    pub fn get_i64(&self) -> i64 {
        match *self {
            ConfValue::I64(v) => v,
            _ => 0,
        }
    }
    pub fn get_u64(&self) -> u64 {
        match *self {
            ConfValue::U64(v) => v,
            _ => 0,
        }
    }
    pub fn get_f64(&self) -> f64 {
        match *self {
            ConfValue::F64(v) => v,
            _ => 0.0,
        }
    }
    /// `g_variant_get_string`; C gets NULL for other types.
    pub fn get_str(&self) -> Option<&str> {
        match self {
            ConfValue::Str(s) => Some(s),
            _ => None,
        }
    }
    /// GVariant type string, as `g_variant_get_type_string` gives it (lists: `t`).
    pub fn type_string(&self) -> &'static str {
        match self {
            ConfValue::Bool(_) => "b",
            ConfValue::Byte(_) => "y",
            ConfValue::I16(_) => "n",
            ConfValue::U16(_) => "q",
            ConfValue::I32(_) => "i",
            ConfValue::U32(_) => "u",
            ConfValue::I64(_) => "x",
            ConfValue::U64(_) | ConfValue::List(_) => "t",
            ConfValue::F64(_) => "d",
            ConfValue::Str(_) => "s",
            ConfValue::StrV(_) => "as",
            ConfValue::I32s(_) => "ai",
            ConfValue::U64s(_) => "at",
            ConfValue::Bytes(_) => "ay",
            ConfValue::Dict(_) => "a{sv}",
        }
    }
}
