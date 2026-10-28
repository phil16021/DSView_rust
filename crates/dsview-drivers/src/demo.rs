//! The "virtual-demo" driver, transliterated from libsigrok4DSL/hardware/demo/
//! demo.c (+ demo.h). Two devices ("Demo Oscilloscope", "Demo Logic") play
//! the `.demo` patterns in `<user dir>/demo/{logic,dso,analog}` or generate
//! random data.
//!
//! Behaviour, including timing and the order of `rand()` calls, follows the
//! C driver so the oracle can compare packets one to one under a virtual
//! clock. Departures (DEVIATIONS.md, Phase 3): packet fields C leaves
//! uninitialized are 0, out-of-bounds reads of C read zeros here, and calls C
//! would crash on (NULL channel, NULL string) return `SR_ERR_ARG`.

// Kept in demo.c's shape (index loops, `% n == 0`, nested ifs) so the two
// can be read side by side.
#![allow(
    clippy::needless_range_loop,
    clippy::manual_is_multiple_of,
    clippy::manual_range_contains,
    clippy::collapsible_if
)]

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use dsview_core::{
    Body, Channel, ConfValue, LA_CROSS_DATA, ListItem, MQ_VOLTAGE, MQFLAG_AC, Mode, Packet,
    SrError, SrResult, Status, UNIT_VOLT, channel_type, dev_status, dev_type, key, pkt_status,
};
use dsview_formats::dsl::{DslReader, Packet as DslPacket, StreamState};
use dsview_formats::header::strtoull;
use dsview_formats::keyfile::KeyFile;

use crate::sys::{self, Timer, rand, sr_dbg, sr_err, sr_info, usleep_secs};
use crate::{Device, DeviceInfo, Step};

// ------------------------------------------------------------------ demo.h

const DEFAULT_LOGIC_FILE: &str = "protocol";
const DEFAULT_ANALOG_FILE: &str = "sine";
const DEFAULT_DSO_FILE: &str = "sine";

const LOGIC_BUF_LEN: u64 = 2 * 1048576;
const LOGIC_BLOCK_LEN: u64 = 2 * 1048576;
const LOGIC_MEMORY_LIMIT: u64 = 500 * 1048576;
const LOGIC_MIN_PACKET_LEN: u64 = 8;
const LOGIC_HW_DEPTH: u64 = 16_000_000_000;
const LOGIC_HIGH_LEVEL: u8 = 255;
const LOGIC_LOW_LEVEL: u8 = 0;
const LOGIC_MAX_PROBE_NUM: usize = 16;

const DSO_PACKET_TIME: f64 = 1.0 / 200.0;
const DSO_PACKET_LEN: u64 = 20000;
const DSO_BUF_LEN: u64 = 1048576;
const DSO_DEFAULT_VDIV: u64 = 1000;
const DSO_MID_VAL: u8 = 128;
const DSO_MAX_VAL: u16 = 0;
const DSO_MIN_VAL: u16 = 255;
const DSO_LIMIT: u16 = 255;
const DSO_WAVE_PERIOD_LEN: i64 = 200;
const DSO_WAVE_PERIOD_LEN_PER_PROBE: i64 = 100;

const ANALOG_HW_DEPTH: u64 = 12_500_000;
const ANALOG_DEFAULT_VDIV: u64 = 1000;
const ANALOG_MID_VAL: u8 = 128;
const ANALOG_MAX_VAL: u8 = 0;
const ANALOG_MIN_VAL: u8 = 255;
const ANALOG_AC_COUL_OFFSET: u16 = 128;
const ANALOG_DC_COUL_OFFSET: u16 = 178;
const ANALOG_CYCLE_RATIO: f64 = 103.0 / 2048.0;
const ANALOG_DATA_LEN_PER_CYCLE: u64 = 206;
const ANALOG_PROBE_NUM: u64 = 2;
const ANALOG_PACKET_NUM_PER_SEC: u64 = 200;
const ANALOG_MIN_PACKET_LEN: u64 = 2;

const LOGIC_DEFAULT_SAMPLERATE: u64 = 1_000_000;
const LOGIC_DEFAULT_TOTAL_SAMPLES: u64 = 1_000_000;
const DSO_DEFAULT_SAMPLERATE: u64 = 100_000_000;
const DSO_DEFAULT_TOTAL_SAMPLES: u64 = 10_000;
const DSO_DEFAULT_NUM_PROBE: i32 = 2;
const ANALOG_DEFAULT_SAMPLERATE: u64 = 1_000_000;
const ANALOG_DEFAULT_TOTAL_SAMPLES: u64 = 1_000_000;
const ANALOG_DEFAULT_NUM_PROBE: i32 = 2;

const MAX_TIMEBASE: u64 = 10_000_000_000;
const MIN_TIMEBASE: u64 = 10;

/// enum DEMO_PATTERN
const PATTERN_RANDOM: u8 = 0;
const PATTERN_COUNT: usize = 20;
const RANDOM_NAME: &str = "random";

/// session_driver.c's SESSION_MAX_CHANNEL_COUNT, the packet buffer's slots.
const SESSION_MAX_CHANNEL_COUNT: usize = 512;

const CAPS_MODE_LOGIC: u64 = 1 << 0;
const CAPS_MODE_ANALOG: u64 = 1 << 1;
const CAPS_MODE_DSO: u64 = 1 << 2;

/// enum DEMO_LOGIC_CHANNEL_ID
const DEMO_LOGIC125X16: i32 = 16;

const SAMPLERATES: [u64; 36] = [
    10,
    20,
    50,
    100,
    200,
    500,
    1_000,
    2_000,
    5_000,
    10_000,
    20_000,
    40_000,
    50_000,
    100_000,
    200_000,
    400_000,
    500_000,
    1_000_000,
    2_000_000,
    4_000_000,
    5_000_000,
    10_000_000,
    20_000_000,
    25_000_000,
    40_000_000,
    50_000_000,
    100_000_000,
    125_000_000,
    250_000_000,
    400_000_000,
    500_000_000,
    800_000_000,
    1_000_000_000,
    2_000_000_000,
    5_000_000_000,
    10_000_000_000,
];

/// vdivs10to2000 without its 0 terminator.
const VDIVS: [u64; 8] = [10, 20, 50, 100, 200, 500, 1000, 2000];

const MAX_HEIGHTS: [&str; 5] = ["1X", "2X", "3X", "4X", "5X"];

const PROBE_NAMES: [&str; 16] = [
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14", "15",
];

const PROBE_MAP_UNITS: [&str; 7] = ["V", "A", "°C", "°F", "g", "m", "m/s"];

const HWOPTIONS: [i32; 2] = [key::PATTERN_MODE, key::MAX_HEIGHT];
const SESSIONS: [i32; 4] = [
    key::SAMPLERATE,
    key::LIMIT_SAMPLES,
    key::PATTERN_MODE,
    key::MAX_HEIGHT,
];
const PROBE_OPTIONS: [i32; 6] = [
    key::PROBE_COUPLING,
    key::PROBE_VDIV,
    key::PROBE_MAP_DEFAULT,
    key::PROBE_MAP_UNIT,
    key::PROBE_MAP_MIN,
    key::PROBE_MAP_MAX,
];
const PROBE_COUPLING: [u8; 2] = [0, 1];

/// struct DEMO_channels (the fields the driver reads).
struct ChannelMode {
    id: i32,
    num: u16,
    min_samplerate: u64,
    max_samplerate: u64,
    descr: &'static str,
}

const LOGIC_CHANNEL_MODES: [ChannelMode; 4] = [
    ChannelMode {
        id: 16,
        num: 16,
        min_samplerate: 50_000,
        max_samplerate: 125_000_000,
        descr: "Use 16 Channels (Max 125MHz)",
    },
    ChannelMode {
        id: 17,
        num: 12,
        min_samplerate: 50_000,
        max_samplerate: 250_000_000,
        descr: "Use 12 Channels (Max 250MHz)",
    },
    ChannelMode {
        id: 18,
        num: 6,
        min_samplerate: 50_000,
        max_samplerate: 500_000_000,
        descr: "Use 6 Channels (Max 500MHz)",
    },
    ChannelMode {
        id: 19,
        num: 3,
        min_samplerate: 50_000,
        max_samplerate: 1_000_000_000,
        descr: "Use 3 Channels (Max 1GHz)",
    },
];

/// channel_modes[]: logic stream, DAQ, OSC (min/max samplerate only).
const CHANNEL_MODES: [(u64, u64); 3] = [
    (50_000, 1_000_000_000),
    (10, 10_000_000),
    (100, 200_000_000),
];

/// struct DEMO_profile
pub struct Profile {
    pub vendor: &'static str,
    pub model: &'static str,
    mode_caps: u64,
}

/// supported_Demo[]: the oscilloscope first, the logic analyzer last (DSView
/// selects the last device at launch).
pub const PROFILES: [Profile; 2] = [
    Profile {
        vendor: "DreamSourceLab",
        model: "Demo Oscilloscope",
        mode_caps: CAPS_MODE_DSO,
    },
    Profile {
        vendor: "DreamSourceLab",
        model: "Demo Logic",
        mode_caps: CAPS_MODE_LOGIC | CAPS_MODE_ANALOG | CAPS_MODE_DSO,
    },
];

// ------------------------------------------------------- driver-wide state

/// The C driver's file-scope statics, shared by both demo devices.
struct Shared {
    /// demo_pattern_array + b_load_directory: read once per process.
    patterns: Option<[Vec<String>; 3]>,
    samplerates_file: u64,
    max_probe_num: i32,
    /// init_random_data's `static int rand_k`.
    rand_k: i32,
}

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            Mutex::new(Shared {
                patterns: None,
                samplerates_file: 0,
                max_probe_num: 0,
                rand_k: 0,
            })
        })
        .lock()
        .unwrap()
}

/// demo_mode_names[], indexed by `enum OPERATION_MODE`.
fn mode_dir(mode: Mode) -> &'static str {
    match mode {
        Mode::Logic => "logic",
        Mode::Dso => "dso",
        Mode::Analog => "analog",
    }
}

/// get_pattern_mode_from_file(): "random" plus up to 19 `*.demo` names in
/// directory order. Like C, any name containing ".demo" counts and loses its
/// last five bytes.
fn read_patterns(dir: &Path) -> Vec<String> {
    let mut out = vec![RANDOM_NAME.to_owned()];
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => {
            sr_err!("Faild to open dir:{}", dir.display());
            return out;
        }
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.contains(".demo") {
            // C: g_strndup(name, strlen - 5) on the raw bytes
            let raw = name.as_bytes();
            out.push(String::from_utf8_lossy(&raw[..raw.len() - 5]).into_owned());
            if out.len() >= PATTERN_COUNT {
                break;
            }
        }
    }
    out
}

fn pattern_index(sh: &Shared, mode: Mode, name: &str) -> Option<usize> {
    sh.patterns.as_ref()?[mode as usize]
        .iter()
        .position(|p| p == name)
}

// ------------------------------------------------------------ the device

/// struct session_packet_buffer
struct PacketBuffer {
    post_buf: Vec<u8>,
    post_buf_len: u64,
    post_len: u64,
    block_buf_len: u64,
    block_chan_read_pos: u64,
    block_data_len: u64,
    block_bufs: Vec<Vec<u8>>,
    block_read_positions: Vec<u64>,
}

impl PacketBuffer {
    fn new() -> PacketBuffer {
        PacketBuffer {
            post_buf: Vec::new(),
            post_buf_len: 0,
            post_len: 0,
            block_buf_len: 0,
            block_chan_read_pos: 0,
            block_data_len: 0,
            block_bufs: vec![Vec::new(); SESSION_MAX_CHANNEL_COUNT],
            block_read_positions: vec![0; SESSION_MAX_CHANNEL_COUNT],
        }
    }
}

/// Which source callback `hw_dev_acquisition_start` installed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    None,
    LogicRandom,
    LogicFile,
    Dso,
    Analog,
}

/// A packet waiting for its pacing sleep (C sleeps between building and
/// forwarding it).
struct Pending {
    packets: Vec<Packet>,
    remove: bool,
}

/// struct session_vdev plus the parts of struct sr_dev_inst the driver owns.
pub struct Demo {
    profile: &'static Profile,
    mode: Mode,
    status: i32,
    path: Option<String>,
    channels: Vec<Channel>,
    generation: u64,

    archive: Option<DslReader>,
    /// The logic file playback's position (C: the packet buffer's blocks).
    logic_stream: Option<StreamState>,
    samplerates_min_index: usize,
    samplerates_max_index: usize,
    sample_generator: u8,
    data_buf: Vec<u8>,
    analog_post_buf: Vec<u8>,
    analog_post_buf_len: u64,
    analog_read_pos: u64,
    cur_block: i32,
    num_blocks: i32,
    samplerate: u64,
    total_samples: u64,
    num_probes: i32,
    enabled_probes: i32,
    timebase: u64,
    unit_bits: u8,
    ref_min: u32,
    ref_max: u32,
    max_height: u8,
    mstatus: Status,
    packet_buffer: Option<PacketBuffer>,
    trigger_hrate: u8,
    packet_len: u64,
    packet_time: f64,
    instant: bool,
    load_data: bool,
    vdiv_change: bool,
    offset_change: bool,
    timebase_change: bool,
    channel_mode_change: bool,
    logic_mem_limit: bool,
    post_data_len: u64,
    logci_cur_packet_num: u64,
    logic_sel_probe_list: [u8; LOGIC_MAX_PROBE_NUM],
    logic_sel_probe_num: u8,
    logic_ch_mode: i32,
    logic_ch_mode_index: usize,
    is_loop: bool,

    /// logic_post_buf (a file-scope static in C)
    logic_post_buf: Vec<u8>,
    /// packet_interval / run_time (file-scope GTimers in C, made at open)
    packet_interval: Option<Timer>,
    run_time: Option<Timer>,
    source: Source,
    pending: Option<Pending>,
    /// receive_data_dso() restarts packet_interval after its pacing sleep,
    /// which the caller does after the poll: done at the next poll instead.
    restart_interval_after_sleep: bool,
}

/// hw_scan(): one device per profile.
pub fn scan() -> Vec<Demo> {
    PROFILES
        .iter()
        .map(|p| {
            let mode = if p.mode_caps & CAPS_MODE_LOGIC != 0 {
                Mode::Logic
            } else if p.mode_caps & CAPS_MODE_DSO != 0 {
                Mode::Dso
            } else {
                Mode::Analog
            };
            Demo::new(p, mode)
        })
        .collect()
}

impl Demo {
    fn new(profile: &'static Profile, mode: Mode) -> Demo {
        Demo {
            profile,
            mode,
            status: dev_status::INACTIVE,
            path: None,
            channels: Vec::new(),
            generation: 0,
            archive: None,
            logic_stream: None,
            samplerates_min_index: 0,
            samplerates_max_index: 0,
            sample_generator: 0,
            data_buf: Vec::new(),
            analog_post_buf: Vec::new(),
            analog_post_buf_len: 0,
            analog_read_pos: 0,
            cur_block: 0,
            num_blocks: 0,
            samplerate: 0,
            total_samples: 0,
            num_probes: 0,
            enabled_probes: 0,
            timebase: 0,
            unit_bits: 0,
            ref_min: 0,
            ref_max: 0,
            max_height: 0,
            mstatus: Status::default(),
            packet_buffer: None,
            trigger_hrate: 0,
            packet_len: 0,
            packet_time: 0.0,
            instant: false,
            load_data: false,
            vdiv_change: false,
            offset_change: false,
            timebase_change: false,
            channel_mode_change: false,
            logic_mem_limit: false,
            post_data_len: 0,
            logci_cur_packet_num: 0,
            logic_sel_probe_list: [0; LOGIC_MAX_PROBE_NUM],
            logic_sel_probe_num: 0,
            logic_ch_mode: 0,
            logic_ch_mode_index: 0,
            is_loop: false,
            logic_post_buf: Vec::new(),
            packet_interval: None,
            run_time: None,
            source: Source::None,
            pending: None,
            restart_interval_after_sleep: false,
        }
    }

    fn free_probes(&mut self) {
        self.channels.clear();
        self.generation += 1;
    }

    fn add_probe(&mut self, ch: Channel) {
        self.channels.push(ch);
        self.generation += 1;
    }

    /// vdev_init()
    fn vdev_init(&mut self) {
        self.archive = None;
        self.samplerates_min_index = 0;
        self.samplerates_max_index = 0;
        self.data_buf = Vec::new();
        self.analog_read_pos = 0;
        self.cur_block = 0;
        self.num_blocks = 0;
        self.samplerate = 0;
        self.total_samples = 0;
        self.num_probes = 0;
        self.enabled_probes = 0;
        self.timebase = 500;
        self.ref_min = 1;
        self.ref_max = 255;
        self.max_height = 0;
        self.mstatus.measure_valid = 1;
        self.packet_buffer = None;
        self.packet_len = 0;
        self.packet_time = 0.0;
        self.instant = false;
        self.load_data = false;
        self.vdiv_change = false;
        self.offset_change = false;
        self.timebase_change = false;
        self.channel_mode_change = false;
        self.post_data_len = 0;
        self.logic_sel_probe_list = [0; LOGIC_MAX_PROBE_NUM];
        self.logic_sel_probe_num = 0;
        self.logic_ch_mode = DEMO_LOGIC125X16;
        self.logic_ch_mode_index = 0;
        self.is_loop = false;
        self.unit_bits = if self.mode == Mode::Logic { 1 } else { 8 };
    }

    /// scan_dsl_file()
    fn scan_dsl_file(&mut self) {
        let mut sh = shared();
        if sh.patterns.is_none() {
            let base = sys::user_dir().join("demo");
            sh.patterns = Some(
                [Mode::Logic, Mode::Dso, Mode::Analog]
                    .map(|m| read_patterns(&base.join(mode_dir(m)))),
            );
        }
        let caps = self.profile.mode_caps;
        let (init_mode, default_file) = if caps & CAPS_MODE_LOGIC != 0 {
            (Mode::Logic, DEFAULT_LOGIC_FILE)
        } else if caps & CAPS_MODE_DSO != 0 {
            (Mode::Dso, DEFAULT_DSO_FILE)
        } else {
            (Mode::Analog, DEFAULT_ANALOG_FILE)
        };
        let dex = pattern_index(&sh, init_mode, default_file).map_or(PATTERN_RANDOM, |d| d as u8);
        drop(sh);
        self.sample_generator = dex;
        self.mode = init_mode;
        self.reset_dsl_path(dex);
    }

    /// reset_dsl_path()
    fn reset_dsl_path(&mut self, pattern: u8) {
        self.path = Some(if pattern != PATTERN_RANDOM {
            let sh = shared();
            let name = &sh.patterns.as_ref().unwrap()[self.mode as usize][pattern as usize];
            format!(
                "{}/demo/{}/{}.demo",
                sys::user_dir().display(),
                mode_dir(self.mode),
                name
            )
        } else {
            String::new()
        });
    }

    fn pattern_name(&self) -> String {
        let sh = shared();
        sh.patterns
            .as_ref()
            .and_then(|p| p[self.mode as usize].get(self.sample_generator as usize))
            .cloned()
            .unwrap_or_default()
    }

    /// Clamps the samplerate into `[min, max]` of the mode's range (adjust_samplerate()'s core).
    fn clamp_samplerate(&mut self, min: u64, max: u64) {
        self.samplerates_max_index = SAMPLERATES.len() - 1;
        while SAMPLERATES[self.samplerates_max_index] > max {
            self.samplerates_max_index -= 1;
        }
        self.samplerates_min_index = 0;
        while SAMPLERATES[self.samplerates_min_index] < min {
            self.samplerates_min_index += 1;
        }
        if self.samplerate > SAMPLERATES[self.samplerates_max_index] {
            self.samplerate = SAMPLERATES[self.samplerates_max_index];
        }
        if self.samplerate < SAMPLERATES[self.samplerates_min_index] {
            self.samplerate = SAMPLERATES[self.samplerates_min_index];
        }
    }

    /// logic_adjust_samplerate()
    fn logic_adjust_samplerate(&mut self) {
        let m = &LOGIC_CHANNEL_MODES[self.logic_ch_mode_index];
        self.clamp_samplerate(m.min_samplerate, m.max_samplerate);
    }

    /// adjust_samplerate()
    fn adjust_samplerate(&mut self) {
        if self.mode == Mode::Logic && self.sample_generator > PATTERN_RANDOM {
            return;
        }
        let cur = match self.mode {
            Mode::Logic => 0,
            Mode::Dso => 2,
            Mode::Analog => 1,
        };
        let (min, max) = CHANNEL_MODES[cur];
        self.clamp_samplerate(min, max);
    }

    /// load_virtual_device_session()
    fn load_session(&mut self) -> SrResult<()> {
        match self.mode {
            Mode::Logic if self.sample_generator != PATTERN_RANDOM => {
                let path = self.path.clone().unwrap_or_default();
                let text = match read_header(Path::new(&path)) {
                    Ok(t) => t,
                    Err(e) => {
                        sr_err!("load_virtual_device_session: Load file error:\"{path}\": {e}");
                        return Err(SrError::Err);
                    }
                };
                let Ok(kf) = KeyFile::parse(&text) else {
                    sr_err!("Failed to parse metadata.");
                    return Err(SrError::Err);
                };
                for group in kf.groups() {
                    if !group.name.starts_with("header") {
                        continue;
                    }
                    for (k, val) in group.iter() {
                        let val = val.unwrap_or_default();
                        match k {
                            "samplerate" => {
                                self.samplerate = parse_sizestring(&val);
                                shared().samplerates_file = self.samplerate;
                            }
                            "total samples" => self.total_samples = strtoull(&val),
                            "total blocks" => self.num_blocks = strtoull(&val) as i32,
                            "total probes" => {
                                self.free_probes();
                                self.num_probes = strtoull(&val) as i32;
                            }
                            k if k.starts_with("probe") => {
                                let index = strtoull(&k[5..]) as u16;
                                self.add_probe(Channel::new(
                                    index,
                                    channel_type::LOGIC,
                                    true,
                                    &val,
                                ));
                            }
                            _ => {}
                        }
                    }
                    self.adjust_samplerate();
                }
            }
            Mode::Logic => {
                self.samplerate = LOGIC_DEFAULT_SAMPLERATE;
                self.total_samples = LOGIC_DEFAULT_TOTAL_SAMPLES;
                self.num_probes = i32::from(LOGIC_CHANNEL_MODES[self.logic_ch_mode_index].num);
                self.free_probes();
                for i in 0..self.num_probes as usize {
                    self.add_probe(Channel::new(
                        i as u16,
                        channel_type::LOGIC,
                        true,
                        PROBE_NAMES[i],
                    ));
                }
                self.logic_adjust_samplerate();
            }
            Mode::Dso => {
                self.samplerate = DSO_DEFAULT_SAMPLERATE;
                self.total_samples = DSO_DEFAULT_TOTAL_SAMPLES;
                self.num_probes = DSO_DEFAULT_NUM_PROBE;
                self.num_blocks = 1;
                self.free_probes();
                for i in 0..DSO_DEFAULT_NUM_PROBE as usize {
                    let mut p = Channel::new(i as u16, channel_type::DSO, true, PROBE_NAMES[i]);
                    p.coupling = 1;
                    p.vdiv = 1000;
                    p.vfactor = 1000;
                    p.hw_offset = 128;
                    p.offset = 128;
                    p.trig_value = 128;
                    self.add_probe(p);
                }
                self.adjust_samplerate();
            }
            Mode::Analog => {
                self.samplerate = ANALOG_DEFAULT_SAMPLERATE;
                self.total_samples = ANALOG_DEFAULT_TOTAL_SAMPLES;
                self.num_probes = ANALOG_DEFAULT_NUM_PROBE;
                self.num_blocks = 1;
                self.free_probes();
                for i in 0..ANALOG_DEFAULT_NUM_PROBE as usize {
                    let mut p = Channel::new(i as u16, channel_type::ANALOG, true, PROBE_NAMES[i]);
                    p.bits = 8;
                    p.coupling = 1;
                    p.vdiv = 1000;
                    p.vfactor = 1;
                    p.hw_offset = 128;
                    p.offset = 128;
                    p.trig_value = 128;
                    p.map_default = true;
                    p.map_unit = Some("V".into());
                    p.map_min = -5.0;
                    p.map_max = 5.0;
                    self.add_probe(p);
                }
                self.adjust_samplerate();
            }
        }
        Ok(())
    }

    fn ch(&self, ch: Option<usize>) -> Option<&Channel> {
        ch.and_then(|i| self.channels.get(i))
    }

    fn ch_mut(&mut self, ch: Option<usize>) -> SrResult<&mut Channel> {
        // C dereferences the channel unchecked (NULL crashes)
        ch.and_then(|i| self.channels.get_mut(i))
            .ok_or(SrError::Arg)
    }

    /// close_archive()
    fn close_archive(&mut self) {
        if self.sample_generator != PATTERN_RANDOM {
            if self.archive.is_none() {
                sr_err!("close zip archive error!");
            }
            self.archive = None;
            self.logic_stream = None;
        }
    }

    /// free_temp_buffer()
    fn free_temp_buffer(&mut self) {
        self.packet_buffer = None;
        self.data_buf = Vec::new();
        self.analog_post_buf = Vec::new();
        self.logic_stream = None;
    }

    /// send_error_packet(): END with SR_PKT_SOURCE_ERROR, source removed.
    fn error_end(&mut self, out: &mut Vec<Packet>) -> Step {
        out.push(Packet::end(pkt_status::SOURCE_ERROR));
        self.close_archive();
        Step::Remove
    }

    fn elapsed(t: &Option<Timer>) -> f64 {
        t.as_ref().map_or(0.0, Timer::elapsed)
    }

    fn timer_start(t: &mut Option<Timer>) {
        if let Some(t) = t {
            t.start();
        }
    }

    /// delay_time(): the rest of `packet_time` since `packet_interval` started.
    fn delay_time(&self) -> u64 {
        let wait = self.packet_time - Self::elapsed(&self.packet_interval);
        if wait > 0.0 { usleep_secs(wait) } else { 0 }
    }

    /// logic_delay_time()
    fn logic_delay_time(&self) -> u64 {
        let mut ideal = (self.samplerate / 8) as f64;
        ideal = self.packet_len as f64 / ideal;
        ideal *= self.logci_cur_packet_num as f64;
        let wait = ideal - Self::elapsed(&self.run_time);
        if wait > 0.0 { usleep_secs(wait) } else { 0 }
    }

    /// instant_delay_time()
    fn instant_delay_time(&self) -> u64 {
        let cur_packet_num = (self.post_data_len / self.packet_len) as u16;
        let ideal = f64::from(cur_packet_num) * self.packet_time;
        let wait = ideal - Self::elapsed(&self.run_time);
        if wait > 0.0 { usleep_secs(wait) } else { 0 }
    }

    /// init_random_data()
    fn init_random_data(&mut self) {
        let enabled = self.enabled_probes.max(0) as usize;
        let mut probe_count = [0i32; LOGIC_MAX_PROBE_NUM];
        let mut probe_status = [LOGIC_HIGH_LEVEL; LOGIC_MAX_PROBE_NUM];
        self.data_buf[..LOGIC_BUF_LEN as usize].fill(0);
        // srand(time(NULL) + rand() + rand_k++)
        let k = {
            let mut sh = shared();
            sh.rand_k += 1;
            sh.rand_k - 1
        };
        let r = rand();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        sys::srand((now as u32).wrapping_add(r as u32).wrapping_add(k as u32));
        for c in probe_count.iter_mut().take(enabled) {
            *c = rand() % 1024;
        }
        let mut cur = 0usize;
        for i in 0..self.data_buf.len() {
            if i % 8 == 0 && i != 0 {
                cur += 1;
                if cur as i64 > enabled as i64 - 1 {
                    cur = 0;
                }
            }
            if probe_count[cur] > 0 {
                self.data_buf[i] = probe_status[cur];
                probe_count[cur] -= 1;
            } else {
                probe_status[cur] = if probe_status[cur] == LOGIC_HIGH_LEVEL {
                    LOGIC_LOW_LEVEL
                } else {
                    LOGIC_HIGH_LEVEL
                };
                probe_count[cur] = rand() % 1024;
                self.data_buf[i] = probe_status[cur];
                probe_count[cur] -= 1;
            }
        }
    }

    /// init_analog_random_data()
    fn init_analog_random_data(&mut self) {
        self.data_buf = vec![0; DSO_BUF_LEN as usize];
        for i in 0..self.data_buf.len() {
            self.data_buf[i] = if i % 2 == 0 {
                (rand() % 120 + 68) as u8
            } else {
                self.data_buf[i - 1]
            };
        }
    }

    // ------------------------------------------------------ logic, random

    /// receive_data_logic()
    fn poll_logic_random(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step {
        if let Some(p) = self.pending.take() {
            out.extend(p.packets);
            self.after_logic_random_packet();
            return self.logic_random_tail(abort, false, out);
        }
        let chan_num = self.enabled_probes;
        if chan_num < 1 {
            sr_err!("receive_data_logic: channel count < 1.");
            return Step::Continue;
        }
        if chan_num as usize > SESSION_MAX_CHANNEL_COUNT {
            sr_err!("receive_data_logic: channel count is to big.");
            return Step::Continue;
        }
        Self::timer_start(&mut self.packet_interval);

        let mut to_end = false;
        if !self.is_loop && self.post_data_len >= self.total_samples / 8 {
            to_end = true;
        }
        if to_end {
            return self.logic_random_tail(abort, true, out);
        }

        let enabled = chan_num as u64;
        let mut length = enabled * self.packet_len;
        if !self.is_loop {
            self.post_data_len += length / enabled;
            if self.post_data_len >= self.total_samples / 8 {
                // get_last_packet_len()
                let mut last = self.post_data_len as i32 - (length / enabled) as i32;
                last = (self.total_samples / 8) as i32 - last;
                length = (last as i64 * enabled as i64) as u64;
                self.post_data_len = self.total_samples / 8;
            }
        }
        let fill = (enabled * self.packet_len) as usize;
        self.logic_post_buf[..fill].fill(LOGIC_LOW_LEVEL);
        if self.logic_mem_limit {
            for i in 0..self.logic_sel_probe_num as usize {
                let probe = u64::from(self.logic_sel_probe_list[i]);
                for j in 0..(self.packet_len / 8) as u16 {
                    let cur = (probe * 8 + u64::from(j) * enabled * 8) as usize;
                    self.logic_post_buf[cur..cur + 8].fill(LOGIC_HIGH_LEVEL);
                }
            }
        } else {
            // Packets of 2 MiB or more (rates the channel modes do not offer)
            // overrun the random buffer; C then divides by zero or reads
            // wild memory (F18). Here they start at 0 and are zero-padded.
            let span = (self.data_buf.len() as u64).saturating_sub(length);
            let r = rand() as u64;
            let mut random = if span == 0 { 0 } else { r % span };
            let index = enabled * 8;
            random = random / index * index;
            let (r, l) = (random as usize, length as usize);
            if self.logic_post_buf.len() < l {
                self.logic_post_buf.resize(l, 0);
            }
            copy_padded(&mut self.logic_post_buf[..l], &self.data_buf, r);
        }
        let data = self.logic_post_buf[..length as usize].to_vec();
        self.pending = Some(Pending {
            packets: vec![logic_packet(data)],
            remove: false,
        });
        Step::SleepThenPoll(self.logic_delay_time())
    }

    fn after_logic_random_packet(&mut self) {
        if self.logic_mem_limit {
            let target = (LOGIC_BLOCK_LEN / self.packet_len) as u16;
            if target != 0 && self.logci_cur_packet_num % u64::from(target) == 0 {
                let enabled = self.enabled_probes;
                self.logic_sel_probe_num = (rand() % enabled + 1) as u8;
                self.logic_sel_probe_list[..enabled as usize].fill(0);
                for i in 0..self.logic_sel_probe_num as usize {
                    self.logic_sel_probe_list[i] = (rand() % enabled) as u8;
                }
            }
        }
        self.logci_cur_packet_num += 1;
    }

    fn logic_random_tail(&mut self, abort: bool, to_end: bool, out: &mut Vec<Packet>) -> Step {
        if to_end || abort {
            out.push(Packet::end(pkt_status::OK));
            return Step::Remove;
        }
        Step::Continue
    }

    // -------------------------------------------------------- logic, file

    /// receive_data_logic_decoder()
    fn poll_logic_file(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step {
        if let Some(p) = self.pending.take() {
            out.extend(p.packets);
            if p.remove || abort {
                return self.logic_file_end(out);
            }
            return Step::Continue;
        }
        let chan_num = self.enabled_probes;
        if chan_num < 1 {
            sr_err!("receive_data_logic_decoder: channel count < 1.");
            return Step::Continue;
        }
        if chan_num as usize > SESSION_MAX_CHANNEL_COUNT {
            sr_err!("receive_data_logic_decoder: channel count is to big.");
            return Step::Continue;
        }
        Self::timer_start(&mut self.packet_interval);
        shared().max_probe_num = chan_num;

        let Some(reader) = self.archive.as_mut() else {
            return self.error_end(out);
        };
        // each enabled channel plays its own `L-<index>`; C read L-0 ..
        // L-<enabled-1>, shifting the channels after a disabled one (F21)
        let dirs: Vec<u32> = self
            .channels
            .iter()
            .filter(|c| c.enabled && c.ty == dsview_core::channel_type::LOGIC)
            .map(|c| u32::from(c.index))
            .collect();
        let stream = self.logic_stream.get_or_insert_with(|| {
            reader.logic_stream(
                dirs,
                self.num_blocks.max(0) as u64,
                (self.packet_len / 8) as usize,
            )
        });
        let packet = match reader.next_packet(stream) {
            Ok(Some(DslPacket::Logic(data))) => Some(data.to_vec()),
            Ok(_) => None,
            Err(e) => {
                sr_err!("receive_data_logic_decoder: {e}");
                return self.error_end(out);
            }
        };
        let done = packet.is_none() || stream.is_done();
        match packet {
            Some(data) => {
                self.pending = Some(Pending {
                    packets: vec![logic_packet(data)],
                    remove: done,
                });
                Step::SleepThenPoll(self.delay_time())
            }
            None => self.logic_file_end(out),
        }
    }

    fn logic_file_end(&mut self, out: &mut Vec<Packet>) -> Step {
        out.push(Packet::end(pkt_status::OK));
        self.close_archive();
        self.free_temp_buffer();
        Step::Remove
    }

    // ----------------------------------------------------------------- DSO

    /// The first part of receive_data_dso(): buffer setup and (re)loading the
    /// waveform after a vdiv/timebase/offset change. `Err` = error END sent.
    fn dso_prepare(&mut self, out: &mut Vec<Packet>) -> Result<(), Step> {
        let chan_num = self.num_probes as u16 as u64;
        let byte_align: u64 = if self.mode == Mode::Logic { 8 } else { 1 };

        if self.packet_buffer.is_none() {
            self.cur_block = 0;
            let mut pb = PacketBuffer::new();
            pb.post_buf_len = chan_num * 10000;
            pb.post_buf = vec![0; pb.post_buf_len as usize];
            self.packet_buffer = Some(pb);
        }
        let pb = self.packet_buffer.as_mut().unwrap();
        if pb.post_buf_len != chan_num * 10000 {
            pb.post_buf_len = chan_num * 10000;
            pb.post_buf = vec![0; pb.post_buf_len as usize];
            pb.post_len = 0;
        }
        {
            let mut sh = shared();
            if chan_num as i32 != sh.max_probe_num {
                for ch in 0..chan_num as usize {
                    pb.block_bufs[ch] = Vec::new();
                    pb.block_read_positions[ch] = 0;
                }
                pb.block_buf_len = 0;
                pb.block_data_len = 0;
                pb.block_chan_read_pos = 0;
                sh.max_probe_num = chan_num as i32;
            }
        }

        if !(self.vdiv_change || self.timebase_change || self.offset_change) {
            return Ok(());
        }
        if self.sample_generator == PATTERN_RANDOM {
            let pb = self.packet_buffer.as_mut().unwrap();
            for i in 0..pb.post_buf_len as usize {
                pb.post_buf[i] = if i % 2 == 0 {
                    (rand() % 120 + 68) as u8
                } else {
                    pb.post_buf[i - 1]
                };
            }
            pb.post_len = pb.post_buf_len;
        } else if self.load_data {
            let mut read_chan_index = 0usize;
            self.packet_buffer.as_mut().unwrap().post_len = 0;
            loop {
                let pb = self.packet_buffer.as_ref().unwrap();
                if pb.post_len >= pb.post_buf_len {
                    break;
                }
                if pb.block_chan_read_pos >= pb.block_data_len {
                    if self.cur_block >= self.num_blocks {
                        self.cur_block = 0;
                    }
                    for ch in 0..chan_num as usize {
                        let name = format!("O-{ch}/0");
                        let data = match self.archive.as_mut().map(|a| a.entry(&name)) {
                            Some(Ok(d)) => d,
                            _ => {
                                sr_err!("cant't locate zip inner file:\"{name}\"");
                                return Err(self.error_end(out));
                            }
                        };
                        let pb = self.packet_buffer.as_mut().unwrap();
                        if ch == 0 {
                            pb.block_data_len = data.len() as u64;
                            if pb.block_data_len > pb.block_buf_len {
                                pb.block_buf_len = pb.block_data_len;
                            }
                        } else if pb.block_data_len != data.len() as u64 {
                            sr_err!("The block size is not coincident:{name}");
                            return Err(self.error_end(out));
                        }
                        pb.block_bufs[ch] = data;
                        pb.block_read_positions[ch] = 0;
                    }
                    self.cur_block += 1;
                    self.packet_buffer.as_mut().unwrap().block_chan_read_pos = 0;
                }
                let pb = self.packet_buffer.as_mut().unwrap();
                let pos = pb.block_read_positions[read_chan_index] as usize;
                let byte = pb.block_bufs[read_chan_index]
                    .get(pos)
                    .copied()
                    .unwrap_or(0);
                let w = pb.post_len as usize;
                pb.post_buf[w] = byte;
                pb.post_len += 1;
                pb.block_read_positions[read_chan_index] += 1;
                let rp = pb.block_read_positions[read_chan_index];
                if rp % byte_align == 0 || rp == pb.block_data_len {
                    read_chan_index += 1;
                    if pb.block_read_positions[read_chan_index] == pb.block_data_len
                        && self.cur_block < self.num_blocks
                    {
                        break;
                    }
                    if read_chan_index as u64 == chan_num {
                        read_chan_index = 0;
                        pb.block_chan_read_pos += byte_align;
                    }
                }
            }
            let pb = self.packet_buffer.as_ref().unwrap();
            let n = DSO_PACKET_LEN as usize;
            if self.data_buf.len() < n {
                self.data_buf.resize(n, 0);
            }
            copy_padded(&mut self.data_buf[..n], &pb.post_buf, 0);
            self.load_data = false;
        } else {
            let pb = self.packet_buffer.as_mut().unwrap();
            let n = DSO_PACKET_LEN as usize;
            if pb.post_buf.len() < n {
                pb.post_buf.resize(n, 0);
            }
            copy_padded(&mut pb.post_buf[..n], &self.data_buf, 0);
            pb.post_buf_len = DSO_PACKET_LEN;
        }

        self.dso_wavelength_update();
        self.dso_rescale();
        self.offset_change = false;
        self.timebase_change = false;
        self.vdiv_change = false;
        Ok(())
    }

    /// The vdiv/offset rescaling loop of receive_data_dso().
    fn dso_rescale(&mut self) {
        let pb = self.packet_buffer.as_mut().unwrap();
        for i in 0..pb.post_buf_len as usize {
            let (mine, other) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
            let mut probe = &self.channels[mine];
            if !probe.enabled {
                probe = &self.channels[other];
            }
            let vdiv = probe.vdiv;
            let mut temp_val = pb.post_buf[i];
            if vdiv > 200 {
                if temp_val > DSO_MID_VAL {
                    let val = temp_val - DSO_MID_VAL;
                    let tem = (u64::from(val) * DSO_DEFAULT_VDIV / vdiv) as u16;
                    temp_val = (u16::from(DSO_MID_VAL).wrapping_add(tem)) as u8;
                } else if temp_val < DSO_MID_VAL {
                    let val = DSO_MID_VAL - temp_val;
                    let tem = (u64::from(val) * DSO_DEFAULT_VDIV / vdiv) as u16;
                    temp_val = (i32::from(DSO_MID_VAL) - i32::from(tem)) as u8;
                }
                pb.post_buf[i] = temp_val;
            } else {
                let mid = expand_mid(200 / vdiv);
                let mut tem: u16;
                if temp_val > DSO_MID_VAL {
                    let val = temp_val - DSO_MID_VAL;
                    tem = (i32::from(val) * 1000 / i32::from(vdiv as u16)) as u16;
                    tem = mid.wrapping_add(tem);
                } else if temp_val < DSO_MID_VAL {
                    let val = DSO_MID_VAL - temp_val;
                    tem = (i32::from(val) * 1000 / i32::from(vdiv as u16)) as u16;
                    tem = mid.wrapping_sub(tem);
                } else {
                    tem = mid;
                }
                let mut high_gate = mid;
                high_gate = high_gate.wrapping_sub(probe.offset);
                let low_gate = high_gate.wrapping_add(DSO_LIMIT);
                if tem <= high_gate {
                    tem = DSO_MAX_VAL;
                } else if tem >= low_gate {
                    tem = DSO_MIN_VAL;
                } else {
                    tem -= high_gate;
                }
                pb.post_buf[i] = tem as u8;
            }
        }
    }

    /// dso_wavelength_updata()
    fn dso_wavelength_update(&mut self) {
        if self.sample_generator == PATTERN_RANDOM {
            return;
        }
        let bit = get_bit(self.timebase) as i64;
        let pattern = {
            let sh = shared();
            sh.patterns
                .as_ref()
                .and_then(|p| p[Mode::Dso as usize].get(self.sample_generator as usize))
                .cloned()
                .unwrap_or_default()
        };
        let sawtooth = pattern == "sawtooth";
        let pb = self.packet_buffer.as_mut().unwrap();
        let mut l: i64 = 0;
        if pattern != "square" {
            for i in (0..DSO_PACKET_LEN as i64 / 2).step_by(2) {
                if pb.post_buf.get(i as usize) == Some(&78) {
                    l = i;
                    break;
                }
            }
        }
        let mut tmp = vec![0u8; bit as usize];
        for i in 0..bit {
            let index = if i % 2 == 0 {
                if sawtooth && i == bit - 2 {
                    DSO_WAVE_PERIOD_LEN - 2
                } else if !sawtooth && bit == 10 {
                    i * 16
                } else {
                    i * DSO_WAVE_PERIOD_LEN_PER_PROBE / (bit / 2)
                }
            } else if sawtooth && i == bit - 1 {
                DSO_WAVE_PERIOD_LEN - 1
            } else if !sawtooth && bit == 10 {
                (i - 1) * 16 + 1
            } else {
                (i - 1) * DSO_WAVE_PERIOD_LEN_PER_PROBE / (bit / 2) + 1
            };
            tmp[i as usize] = pb.post_buf.get((index + l) as usize).copied().unwrap_or(0);
        }
        let n = DSO_PACKET_LEN as i64 / bit;
        for i in 0..n {
            let at = (i * bit) as usize;
            pb.post_buf[at..at + bit as usize].copy_from_slice(&tmp);
        }
    }

    /// receive_data_dso()
    fn poll_dso(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step {
        if std::mem::take(&mut self.restart_interval_after_sleep) {
            Self::timer_start(&mut self.packet_interval);
        }
        if let Some(p) = self.pending.take() {
            // the END that followed the last packet's sleep
            out.extend(p.packets);
            return Step::Remove;
        }
        let chan_num = self.num_probes as u16;
        if chan_num < 1 {
            sr_err!("receive_data_dso: channel count < 1.");
            return Step::Continue;
        }
        if chan_num as usize > SESSION_MAX_CHANNEL_COUNT {
            sr_err!("receive_data_dso: channel count is to big.");
            return Step::Continue;
        }
        if let Err(step) = self.dso_prepare(out) {
            return step;
        }

        let byte_align: u64 = if self.mode == Mode::Logic { 8 } else { 1 };
        let total_time = self.timebase as f64 / 1e9 * 10.0;
        let elapsed = Self::elapsed(&self.run_time);
        let mut to_end = false;
        let random = self.sample_generator == PATTERN_RANDOM;
        let timebase = self.timebase;
        let pb = self.packet_buffer.as_mut().unwrap();
        if !self.instant {
            if elapsed < total_time {
                let percent = elapsed / total_time;
                let mut buf_len = (percent * DSO_PACKET_LEN as f64) as i32;
                if buf_len % 2 != 0 {
                    buf_len += 1;
                }
                pb.post_len = buf_len as u64;
            } else {
                let at = |i: i64| pb.post_buf.get(i as usize).copied().unwrap_or(0);
                let (top0, top1) = if random {
                    (
                        at(pb.post_buf_len as i64 - 2),
                        at(pb.post_buf_len as i64 - 1),
                    )
                } else {
                    let b = i64::from(get_bit(timebase));
                    (at(b - 2), at(b - 1))
                };
                if pb.post_buf.len() < pb.post_len as usize {
                    pb.post_buf.resize(pb.post_len as usize, 0);
                }
                let mut i = pb.post_len as i64 as i32 - 1;
                while i > 1 {
                    pb.post_buf[i as usize] = pb.post_buf[i as usize - 2];
                    i -= 2;
                }
                let mut i = pb.post_len as i64 as i32 - 2;
                while i > 0 {
                    pb.post_buf[i as usize] = pb.post_buf[i as usize - 2];
                    i -= 2;
                }
                pb.post_buf[0] = top0;
                pb.post_buf[1] = top1;
                pb.post_len = DSO_PACKET_LEN;
            }
        } else if DSO_PACKET_LEN > self.post_data_len {
            pb.post_len = self.packet_len;
            self.post_data_len += self.packet_len;
        } else {
            to_end = true;
            self.instant = false;
        }

        let mut sleep = None;
        if pb.post_len >= byte_align * u64::from(chan_num) && !to_end {
            let num_samples = (pb.post_len / u64::from(chan_num)) as i32;
            let start = if self.instant {
                self.post_data_len as usize
            } else {
                0
            };
            // C hands the GUI `num_samples * channels` bytes from `start`; past
            // the buffer (the last instant packet) it reads heap garbage.
            let len = num_samples as usize * chan_num as usize;
            let mut data = vec![0u8; len];
            copy_padded(&mut data, &pb.post_buf, start);
            out.push(Packet::ok(Body::Dso {
                num_samples,
                mq: MQ_VOLTAGE,
                unit: UNIT_VOLT,
                mqflags: MQFLAG_AC,
                samplerate_tog: false,
                trig_flag: false,
                trig_ch: 0,
                data,
            }));
            if self.instant {
                sleep = Some(self.instant_delay_time());
            } else {
                sleep = Some(self.delay_time());
            }
            self.dso_status_update();
        }

        if to_end || abort {
            let end = Packet::end(pkt_status::OK);
            let Some(us) = sleep else {
                out.push(end);
                return Step::Remove;
            };
            // C sends END after the packet's sleep and timer restart
            self.restart_interval_after_sleep = !self.instant;
            self.pending = Some(Pending {
                packets: vec![end],
                remove: true,
            });
            return Step::SleepThenPoll(us);
        }
        match sleep {
            Some(us) => {
                self.restart_interval_after_sleep = !self.instant;
                Step::SleepThenReturn(us)
            }
            None => Step::Continue,
        }
    }

    /// dso_status_update()
    fn dso_status_update(&mut self) {
        let pb = self.packet_buffer.as_ref().unwrap();
        let at = |i: usize| pb.post_buf.get(i).copied().unwrap_or(0);
        // each channel's zero code, which DSView's mean and RMS are about
        let offset = |ch: usize| {
            self.channels
                .get(ch)
                .map_or(f64::from(DSO_MID_VAL), |c| f64::from(c.hw_offset))
        };
        let offsets = [offset(0), offset(1)];
        // the samples per channel the session holds for this frame (its
        // divisor N): instant mode appends the packets
        let frame_samples = if self.instant {
            self.post_data_len / 2
        } else {
            pb.post_len / 2
        } as f64;
        let st = &mut self.mstatus;
        let mut ch_max = DSO_MID_VAL;
        let mut ch_min = DSO_MID_VAL;
        // per channel: samples scanned, sum of codes, sum of squares about
        // the zero code
        let (mut scanned, mut sum, mut sumsq) = (0u64, [0f64; 2], [0f64; 2]);
        let mut temp_plevel = false;
        let mut first_plevel = false;
        let mut temp_llen: u32 = 0;

        for i in (0..DSO_PACKET_LEN as usize).step_by(2) {
            let val = at(i);
            ch_max = ch_max.max(val);
            ch_min = ch_min.min(val);
        }
        st.ch0_max = ch_max;
        st.ch1_max = ch_max;
        st.ch0_high_level = ch_max;
        st.ch1_high_level = ch_max;
        st.ch0_min = ch_min;
        st.ch1_min = ch_min;
        st.ch0_low_level = ch_min;
        st.ch1_low_level = ch_min;
        st.ch0_cyc_tlen = 0;
        st.ch1_cyc_tlen = 0;
        st.ch0_cyc_llen = 0;
        st.ch1_cyc_llen = 0;
        st.ch0_cyc_cnt = 0;
        st.ch1_cyc_cnt = 0;
        st.ch0_cyc_rlen = 0;
        st.ch1_cyc_rlen = 0;
        st.ch0_cyc_flen = 0;
        st.ch1_cyc_flen = 0;
        st.ch0_cyc_plen = 0;
        st.ch1_cyc_plen = 0;
        st.ch0_level_valid = 1;
        st.ch1_level_valid = 1;
        st.ch0_acc_mean = 0;
        st.ch1_acc_mean = 0;

        for i in (0..DSO_PACKET_LEN as usize).step_by(2) {
            if !(i as u64 <= pb.post_len || self.instant) {
                break;
            }
            let val = at(i);
            if first_plevel {
                if val <= DSO_MID_VAL {
                    st.ch0_cyc_plen += 1;
                    st.ch1_cyc_plen += 1;
                }
                temp_llen += 1;
                let edge = if temp_plevel {
                    st.ch0_cyc_rlen += 1;
                    st.ch1_cyc_rlen += 1;
                    val == ch_max
                } else {
                    st.ch0_cyc_flen += 1;
                    st.ch1_cyc_flen += 1;
                    val == ch_min
                };
                if edge {
                    temp_plevel = !temp_plevel;
                    if (st.ch0_plevel != 0) == temp_plevel {
                        st.ch0_cyc_cnt += 1;
                        st.ch1_cyc_cnt += 1;
                        st.ch1_cyc_tlen = st.ch1_cyc_tlen.wrapping_add(temp_llen);
                        st.ch0_cyc_tlen = st.ch1_cyc_tlen;
                        st.ch0_cyc_llen = 0;
                        st.ch1_cyc_llen = 0;
                        temp_llen = 0;
                    } else {
                        st.ch0_cyc_llen = temp_llen;
                        st.ch1_cyc_llen = temp_llen;
                    }
                }
            } else if val == ch_max || val == ch_min {
                temp_plevel = val != ch_max;
                st.ch0_plevel = i32::from(temp_plevel);
                st.ch1_plevel = i32::from(temp_plevel);
                first_plevel = true;
            }
            scanned += 1;
            for (ch, v) in [val, at(i + 1)].into_iter().enumerate() {
                let v = f64::from(v);
                sum[ch] += v;
                sumsq[ch] += (offsets[ch] - v).powi(2);
            }
        }
        st.ch1_cyc_rlen /= 2;
        st.ch0_cyc_rlen = st.ch1_cyc_rlen;
        st.ch1_cyc_flen /= 2;
        st.ch0_cyc_flen = st.ch1_cyc_flen;

        // DSViewR: C wrote placeholder sums from the last packet's length
        // (`DSO_MID_VAL * post_len / 2`, `ch_max * post_len / 2 * 7`), which
        // DSView divides by the frame's samples; in instant mode a packet is
        // one sample, so the demo showed 13.90 mV RMS and 5.04 V mean for its
        // ±1.97 V sine. The sums are now the scanned samples' own, scaled to
        // the frame.
        if scanned > 0 {
            let scale = frame_samples / scanned as f64;
            let acc = |x: f64| (x * scale).round();
            st.ch0_acc_mean = acc(sum[0]) as u32;
            st.ch1_acc_mean = acc(sum[1]) as u32;
            st.ch0_acc_square = acc(sumsq[0]) as u64;
            st.ch1_acc_square = acc(sumsq[1]) as u64;
        }
    }

    // -------------------------------------------------------------- analog

    /// receive_data_analog()
    fn poll_analog(&mut self, out: &mut Vec<Packet>) -> Step {
        if let Some(p) = self.pending.take() {
            out.extend(p.packets);
            return Step::Continue;
        }
        Self::timer_start(&mut self.packet_interval);
        if self.load_data {
            if self.sample_generator != PATTERN_RANDOM {
                let name = "A-0/0";
                let entry = match self.archive.as_mut().map(|a| a.entry(name)) {
                    Some(Ok(d)) => d,
                    _ => {
                        sr_err!("can't locate zip inner file:\"{name}\"");
                        return self.error_end(out);
                    }
                };
                // unzReadCurrentFile into a zeroed 206-byte buffer
                let mut analog_data = [0u8; ANALOG_DATA_LEN_PER_CYCLE as usize];
                let n = entry.len().min(analog_data.len());
                analog_data[..n].copy_from_slice(&entry[..n]);

                let mut total = (ANALOG_CYCLE_RATIO
                    * self.total_samples as f64
                    * ANALOG_PROBE_NUM as f64) as u64;
                if total % ANALOG_DATA_LEN_PER_CYCLE != 0 {
                    total = total / ANALOG_DATA_LEN_PER_CYCLE * ANALOG_DATA_LEN_PER_CYCLE;
                }
                // under 2048 samples this is none: C's g_try_malloc0(0) is
                // NULL, every callback fails and the session spins with no
                // data (F19). One whole cycle is the least
                let total = total.max(ANALOG_DATA_LEN_PER_CYCLE);
                self.data_buf = vec![0; total as usize];
                let per_block = total / ANALOG_DATA_LEN_PER_CYCLE;
                let p0_vdiv = self.channels.first().map_or(0, |c| c.vdiv);
                let p1_vdiv = self.channels.get(1).map_or(0, |c| c.vdiv);
                for (i, &raw) in analog_data.iter().enumerate() {
                    let vdiv = if i % 2 == 0 { p0_vdiv } else { p1_vdiv };
                    let mut v = raw;
                    if v > ANALOG_MID_VAL {
                        let val = v - ANALOG_MID_VAL;
                        let tem = (u64::from(val) * ANALOG_DEFAULT_VDIV / vdiv) as u16;
                        v = if tem >= u16::from(ANALOG_MID_VAL) {
                            ANALOG_MIN_VAL
                        } else {
                            ANALOG_MID_VAL + tem as u8
                        };
                    } else if v < ANALOG_MID_VAL {
                        let val = ANALOG_MID_VAL - v;
                        let tem = (u64::from(val) * ANALOG_DEFAULT_VDIV / vdiv) as u16;
                        v = if tem >= u16::from(ANALOG_MID_VAL) {
                            ANALOG_MAX_VAL
                        } else {
                            ANALOG_MID_VAL - tem as u8
                        };
                    }
                    let i = i as u64;
                    for j in 0..per_block {
                        let cur = if i % 2 == 0 {
                            i * per_block + j * 2
                        } else {
                            1 + (i - 1) * per_block + j * 2
                        };
                        self.data_buf[cur as usize] = v;
                    }
                }
            }
            self.load_data = false;
        }

        if self.analog_post_buf_len != self.packet_len {
            self.analog_post_buf = vec![0; self.packet_len as usize];
            self.analog_post_buf_len = self.packet_len;
        }
        let data_len = self.data_buf.len() as u64;
        let plen = self.packet_len as usize;
        if self.analog_read_pos + self.packet_len >= data_len.wrapping_sub(1) {
            let back = data_len.saturating_sub(self.analog_read_pos) as usize;
            let front = plen.saturating_sub(back);
            let rp = self.analog_read_pos as usize;
            copy_padded(
                &mut self.analog_post_buf[..back.min(plen)],
                &self.data_buf,
                rp,
            );
            if front > 0 {
                copy_padded(&mut self.analog_post_buf[back..], &self.data_buf, 0);
            }
            self.analog_read_pos = front as u64;
        } else {
            let rp = self.analog_read_pos as usize;
            copy_padded(&mut self.analog_post_buf[..plen], &self.data_buf, rp);
            self.analog_read_pos += self.packet_len;
        }
        let num_samples = (self.packet_len / self.num_probes.max(1) as u64) as i32;
        self.pending = Some(Pending {
            packets: vec![Packet::ok(Body::Analog {
                num_samples,
                unit_bits: self.unit_bits,
                unit_pitch: 0,
                mq: MQ_VOLTAGE,
                unit: UNIT_VOLT,
                mqflags: MQFLAG_AC,
                data: self.analog_post_buf.clone(),
            })],
            remove: false,
        });
        Step::SleepThenPoll(self.delay_time())
    }
}

/// `DSO_EXPAND_MID_VAL(i)`: `i * 256`, truncated to the u16 it is stored in.
fn expand_mid(i: u64) -> u16 {
    i.wrapping_mul(256) as u16
}

/// get_bit()
fn get_bit(timebase: u64) -> i32 {
    const MS: u64 = 1_000_000;
    const US: u64 = 1_000;
    const SEC: u64 = 1_000_000_000;
    if timebase < MS && timebase >= 20 * US {
        (MS / timebase * 2) as i32
    } else if timebase < 200 * MS && timebase >= MS {
        (200 * MS / timebase * 2) as i32
    } else if timebase <= 10 * SEC && timebase > 200 * MS {
        (40 * SEC / timebase * 2) as i32
    } else {
        200
    }
}

/// Copies `src[start..]` into `dst`, zero past the end of `src`.
fn copy_padded(dst: &mut [u8], src: &[u8], start: usize) {
    let avail = src.len().saturating_sub(start).min(dst.len());
    if avail > 0 {
        dst[..avail].copy_from_slice(&src[start..start + avail]);
    }
    dst[avail..].fill(0);
}

fn logic_packet(data: Vec<u8>) -> Packet {
    Packet::ok(Body::Logic {
        format: LA_CROSS_DATA,
        index: 0,
        order: 0,
        unitsize: 0,
        data_error: 0,
        error_pattern: 0,
        data,
    })
}

/// The `header` entry of a `.demo` file as text.
fn read_header(path: &Path) -> Result<String, dsview_formats::Error> {
    let mut r = DslReader::open(path)?;
    let raw = r
        .meta_entry("header")?
        .ok_or(dsview_formats::Error::Missing("header"))?;
    Ok(String::from_utf8_lossy(&raw).into_owned())
}

/// sr_parse_sizestring(); the demo ignores its error result.
fn parse_sizestring(s: &str) -> u64 {
    let mut size = strtoull(s);
    // the number's extent, as strtoull's end pointer gives it
    let t = s.trim_start();
    let t = t.strip_prefix(['+', '-']).unwrap_or(t);
    let mut rest = t.trim_start_matches(|c: char| c.is_ascii_digit()).chars();
    let mut multiplier = 0u64;
    for c in rest.by_ref() {
        match c {
            ' ' => continue,
            'k' | 'K' => multiplier = 1_000,
            'm' | 'M' => multiplier = 1_000_000,
            'g' | 'G' => multiplier = 1_000_000_000,
            _ => {}
        }
        break;
    }
    if multiplier > 0 {
        size = size.wrapping_mul(multiplier);
    }
    size
}

impl Device for Demo {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor: self.profile.vendor.to_owned(),
            model: self.profile.model.to_owned(),
            version: None,
            dev_type: dev_type::DEMO,
            status: self.status,
            mode: self.mode,
            path: self.path.clone(),
        }
    }

    fn set_status(&mut self, status: i32) {
        self.status = status;
    }

    /// hw_dev_mode_list()
    fn mode_list(&self) -> Vec<Mode> {
        [
            (CAPS_MODE_LOGIC, Mode::Logic),
            (CAPS_MODE_ANALOG, Mode::Analog),
            (CAPS_MODE_DSO, Mode::Dso),
        ]
        .into_iter()
        .filter(|&(cap, _)| self.profile.mode_caps & cap != 0)
        .map(|(_, m)| m)
        .collect()
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn channels_mut(&mut self) -> &mut [Channel] {
        &mut self.channels
    }

    fn channel_generation(&self) -> u64 {
        self.generation
    }

    /// hw_dev_open()
    fn open(&mut self) -> SrResult<()> {
        if self.status == dev_status::ACTIVE {
            return Ok(());
        }
        self.scan_dsl_file();
        self.vdev_init();
        self.status = dev_status::ACTIVE;
        self.packet_interval = Some(Timer::new());
        self.run_time = Some(Timer::new());
        if let Err(e) = self.load_session() {
            sr_err!("Error!Load session file failed.");
            return Err(e);
        }
        Ok(())
    }

    /// hw_dev_close()
    fn close(&mut self) -> SrResult<()> {
        self.packet_buffer = None;
        self.data_buf = Vec::new();
        self.path = None;
        self.packet_interval = None;
        self.run_time = None;
        self.analog_post_buf = Vec::new();
        self.status = dev_status::INACTIVE;
        Ok(())
    }

    fn config_get(&mut self, id: i32, ch: Option<usize>) -> SrResult<Option<ConfValue>> {
        use ConfValue::*;
        let ch = self.ch(ch);
        let v = match id {
            key::SAMPLERATE => U64(self.samplerate),
            key::LIMIT_SAMPLES => U64(self.total_samples),
            key::DEVICE_MODE => I16(self.mode as i16),
            key::INSTANT => Bool(self.instant),
            key::PATTERN_MODE => Str(self.pattern_name()),
            key::MAX_HEIGHT => Str(MAX_HEIGHTS[self.max_height as usize].to_owned()),
            key::MAX_HEIGHT_VALUE => Byte(self.max_height),
            key::PROBE_OFFSET => return Ok(ch.map(|c| U16(c.offset))),
            key::PROBE_HW_OFFSET => return Ok(ch.map(|c| U16(c.hw_offset))),
            key::PROBE_VDIV => return Ok(ch.map(|c| U64(c.vdiv))),
            key::PROBE_FACTOR => return Ok(ch.map(|c| U64(c.vfactor))),
            key::TIMEBASE => U64(self.timebase),
            key::MAX_TIMEBASE => U64(MAX_TIMEBASE),
            key::MIN_TIMEBASE => U64(MIN_TIMEBASE),
            key::PROBE_COUPLING => return Ok(ch.map(|c| Byte(c.coupling))),
            key::TRIGGER_VALUE => return Ok(ch.map(|c| Byte(c.trig_value))),
            key::PROBE_EN => return Ok(ch.map(|c| Bool(c.enabled))),
            key::MAX_DSO_SAMPLERATE => U64(200_000_000),
            key::MAX_DSO_SAMPLELIMITS => U64(20_000),
            key::HW_DEPTH => U64(match self.mode {
                Mode::Logic if self.sample_generator == PATTERN_RANDOM => LOGIC_HW_DEPTH,
                Mode::Logic | Mode::Dso => self.total_samples,
                Mode::Analog => ANALOG_HW_DEPTH,
            }),
            key::UNIT_BITS => Byte(self.unit_bits),
            key::PROBE_MAP_DEFAULT => Bool(ch.ok_or(SrError::Err)?.map_default),
            key::PROBE_MAP_UNIT => {
                // C: g_variant_new_string(NULL), i.e. SR_OK without a value,
                // for channels without a unit (DSO channels)
                let c = ch.ok_or(SrError::Err)?;
                return Ok(c.map_unit.clone().map(Str));
            }
            key::PROBE_MAP_MIN => F64(ch.ok_or(SrError::Err)?.map_min),
            key::PROBE_MAP_MAX => F64(ch.ok_or(SrError::Err)?.map_max),
            key::VLD_CH_NUM => I16(self.num_probes as i16),
            key::HAVE_ZERO => Bool(false),
            key::LOAD_DECODER => Bool(self.sample_generator != PATTERN_RANDOM),
            key::REF_MIN => U32(self.ref_min),
            key::REF_MAX => U32(self.ref_max),
            key::CHANNEL_MODE => I16(self.logic_ch_mode as i16),
            key::HORIZ_TRIGGERPOS => Byte(self.trigger_hrate),
            _ => return Err(SrError::Na),
        };
        Ok(Some(v))
    }

    fn config_set(&mut self, id: i32, data: &ConfValue, ch: Option<usize>) -> SrResult<()> {
        match id {
            key::SAMPLERATE => {
                self.samplerate = data.get_u64();
                sr_dbg!("Setting samplerate to {}.", self.samplerate);
            }
            key::LIMIT_SAMPLES => {
                self.total_samples = data.get_u64();
                sr_dbg!("Setting limit samples to {}.", self.total_samples);
            }
            key::LIMIT_MSEC => {}
            key::DEVICE_MODE => {
                let Some(mode) = Mode::from_i32(i32::from(data.get_i16())) else {
                    // C indexes its tables with any mode value
                    return Err(SrError::Arg);
                };
                self.mode = mode;
                let default = match mode {
                    Mode::Logic => DEFAULT_LOGIC_FILE,
                    Mode::Dso => DEFAULT_DSO_FILE,
                    Mode::Analog => DEFAULT_ANALOG_FILE,
                };
                let nv = pattern_index(&shared(), mode, default);
                self.sample_generator = nv.map_or(PATTERN_RANDOM, |n| n as u8);
                self.reset_dsl_path(self.sample_generator);
                let _ = self.load_session();
            }
            key::PATTERN_MODE => {
                let Some(name) = data.get_str() else {
                    return Err(SrError::Arg);
                };
                let previous = self.sample_generator;
                let nv = pattern_index(&shared(), self.mode, name);
                self.sample_generator = nv.map_or(PATTERN_RANDOM, |n| n as u8);
                self.reset_dsl_path(self.sample_generator);
                if self.mode == Mode::Logic {
                    if self.sample_generator == PATTERN_RANDOM {
                        if !self.channel_mode_change && previous != self.sample_generator {
                            self.logic_ch_mode = DEMO_LOGIC125X16;
                            self.logic_ch_mode_index = 0;
                            let _ = self.load_session();
                        }
                        self.channel_mode_change = false;
                    } else {
                        let _ = self.load_session();
                    }
                }
                sr_dbg!("config_set: setting pattern to {}", self.sample_generator);
            }
            key::MAX_HEIGHT => {
                if let Some(i) = data
                    .get_str()
                    .and_then(|s| MAX_HEIGHTS.iter().position(|h| *h == s))
                {
                    self.max_height = i as u8;
                }
                sr_dbg!(
                    "config_set: setting Signal Max Height to {}",
                    self.max_height
                );
            }
            key::PROBE_EN => self.ch_mut(ch)?.enabled = data.get_bool(),
            key::PROBE_VDIV => {
                // C stores 0 and divides by it when the DSO data is scaled
                let vdiv = data.get_u64();
                if vdiv == 0 {
                    return Err(SrError::Arg);
                }
                self.ch_mut(ch)?.vdiv = vdiv;
                if self.mode == Mode::Dso {
                    if let Some(pb) = self.packet_buffer.as_mut() {
                        pb.post_len = 0;
                        pb.block_read_positions[0] = 0;
                        pb.block_read_positions[1] = 0;
                        pb.block_chan_read_pos = 0;
                    }
                    self.vdiv_change = true;
                }
            }
            key::PROBE_FACTOR => self.ch_mut(ch)?.vfactor = data.get_u64(),
            key::PROBE_OFFSET => {
                let mode = self.mode;
                let c = self.ch_mut(ch)?;
                c.offset = data.get_u16();
                if mode == Mode::Dso && c.vdiv <= 200 {
                    c.hw_offset = c.offset;
                    self.offset_change = true;
                } else if c.coupling == 0 {
                    c.hw_offset = ANALOG_DC_COUL_OFFSET;
                } else {
                    c.hw_offset = ANALOG_AC_COUL_OFFSET;
                }
            }
            key::PROBE_HW_OFFSET => self.ch_mut(ch)?.hw_offset = data.get_u16(),
            key::TIMEBASE => {
                self.timebase = data.get_u64();
                if self.mode == Mode::Dso {
                    Self::timer_start(&mut self.run_time);
                    self.timebase_change = true;
                }
                sr_dbg!("Setting timebase to {}.", self.timebase);
            }
            key::PROBE_COUPLING => {
                if self.mode != Mode::Logic {
                    let mode = self.mode;
                    let c = self.ch_mut(ch)?;
                    c.coupling = data.get_byte();
                    if mode == Mode::Dso && c.vdiv <= 200 {
                        c.hw_offset = c.offset;
                    } else if c.coupling == 0 {
                        c.hw_offset = ANALOG_DC_COUL_OFFSET;
                    } else {
                        c.hw_offset = ANALOG_AC_COUL_OFFSET;
                    }
                }
            }
            key::TRIGGER_SOURCE | key::TRIGGER_SLOPE => {}
            key::TRIGGER_VALUE => self.ch_mut(ch)?.trig_value = data.get_byte(),
            key::PROBE_MAP_DEFAULT => {
                let c = self.ch_mut(ch)?;
                c.map_default = data.get_bool();
                if c.map_default {
                    c.map_unit = Some(PROBE_MAP_UNITS[0].to_owned());
                    let span = (c.vdiv * c.vfactor * 10) as f64 / 2000.0;
                    c.map_min = -span;
                    c.map_max = span;
                }
            }
            key::PROBE_MAP_UNIT => {
                self.ch_mut(ch)?.map_unit = data.get_str().map(str::to_owned);
            }
            key::PROBE_MAP_MIN => self.ch_mut(ch)?.map_min = data.get_f64(),
            key::PROBE_MAP_MAX => self.ch_mut(ch)?.map_max = data.get_f64(),
            key::NUM_BLOCKS => {
                self.num_blocks = data.get_u64() as i32;
                sr_dbg!("Setting block number to {}.", self.num_blocks);
            }
            key::CAPTURE_NUM_PROBES => self.num_probes = data.get_u64() as i32,
            key::INSTANT => self.instant = data.get_bool(),
            key::LOOP_MODE => {
                self.is_loop = data.get_bool();
                sr_info!("Set demo loop mode:{}", i32::from(self.is_loop));
            }
            key::CHANNEL_MODE => {
                let nv = i32::from(data.get_i16());
                if self.mode == Mode::Logic && self.sample_generator == PATTERN_RANDOM {
                    if let Some(i) = LOGIC_CHANNEL_MODES.iter().position(|m| m.id == nv) {
                        self.logic_ch_mode_index = i;
                        self.logic_ch_mode = nv;
                        let _ = self.load_session();
                        self.channel_mode_change = true;
                    }
                }
            }
            key::HORIZ_TRIGGERPOS => self.trigger_hrate = data.get_byte(),
            _ => {
                sr_err!("Unknown capability: {id}.");
                return Err(SrError::Na);
            }
        }
        Ok(())
    }

    fn config_list(&self, id: i32) -> SrResult<ConfValue> {
        use ConfValue::*;
        Ok(match id {
            key::DEVICE_OPTIONS => I32s(HWOPTIONS.to_vec()),
            key::DEVICE_SESSIONS => I32s(SESSIONS.to_vec()),
            key::SAMPLERATE => {
                let rates = if self.mode == Mode::Logic && self.sample_generator != PATTERN_RANDOM {
                    vec![shared().samplerates_file]
                } else {
                    SAMPLERATES[self.samplerates_min_index..=self.samplerates_max_index].to_vec()
                };
                Dict(vec![("samplerates".into(), U64s(rates))])
            }
            key::PATTERN_MODE => StrV(
                shared()
                    .patterns
                    .as_ref()
                    .map(|p| p[self.mode as usize].clone())
                    .unwrap_or_default(),
            ),
            key::MAX_HEIGHT => StrV(MAX_HEIGHTS.iter().map(|s| s.to_string()).collect()),
            key::PROBE_CONFIGS => I32s(PROBE_OPTIONS.to_vec()),
            key::PROBE_VDIV => Dict(vec![("vdivs".into(), U64s(VDIVS.to_vec()))]),
            key::PROBE_COUPLING => Dict(vec![("coupling".into(), Bytes(PROBE_COUPLING.to_vec()))]),
            key::PROBE_MAP_UNIT => StrV(PROBE_MAP_UNITS.iter().map(|s| s.to_string()).collect()),
            key::CHANNEL_MODE
                if self.mode == Mode::Logic && self.sample_generator == PATTERN_RANDOM =>
            {
                List(
                    LOGIC_CHANNEL_MODES
                        .iter()
                        .map(|m| ListItem {
                            id: m.id,
                            name: m.descr.to_owned(),
                        })
                        .collect(),
                )
            }
            _ => return Err(SrError::Arg),
        })
    }

    fn status(&self, _prg: bool) -> SrResult<Status> {
        Ok(self.mstatus)
    }

    /// hw_dev_acquisition_start()
    fn start(&mut self, _out: &mut Vec<Packet>) -> SrResult<()> {
        self.enabled_probes = 0;
        self.cur_block = 0;
        self.pending = None;
        self.restart_interval_after_sleep = false;
        sr_info!(
            "mode:{}, generator:{}",
            self.mode as i32,
            self.sample_generator
        );

        self.data_buf = vec![
            0;
            if self.mode == Mode::Logic {
                LOGIC_BUF_LEN
            } else {
                DSO_PACKET_LEN
            } as usize
        ];

        if self.sample_generator != PATTERN_RANDOM {
            if self.archive.is_some() {
                sr_err!("history archive is not closed.");
            }
            let path = self.path.clone().unwrap_or_default();
            sr_dbg!("Opening archive file {path}");
            match DslReader::open(Path::new(&path)) {
                Ok(r) => self.archive = Some(r),
                Err(e) => {
                    sr_err!("Failed to open session file '{path}': zip error: {e}");
                    return Err(SrError::Err);
                }
            }
            self.logic_stream = None;
        }

        self.enabled_probes = self.channels.iter().filter(|c| c.enabled).count() as i32;

        match self.mode {
            Mode::Logic => {
                self.post_data_len = 0;
                self.packet_len = logic_packet_len(self.samplerate);
                self.packet_time = 1.0 / 1000.0;
                if self.packet_len < LOGIC_MIN_PACKET_LEN {
                    self.packet_len = LOGIC_MIN_PACKET_LEN;
                    self.packet_time = 1.0 / ((self.samplerate / 8) / LOGIC_MIN_PACKET_LEN) as f64;
                }
                if self.samplerate >= 100_000_000 {
                    let mut ideal_len = 2u64;
                    while ideal_len < self.packet_len {
                        ideal_len *= 2;
                    }
                    self.packet_len = ideal_len;
                    self.packet_time = (self.samplerate / 8) as f64;
                    let packet_num = (self.packet_time / self.packet_len as f64).ceil() as u64;
                    self.packet_time = 1.0 / packet_num as f64;
                    self.logic_sel_probe_num = 0;
                }
                if self.sample_generator == PATTERN_RANDOM {
                    self.logci_cur_packet_num = 1;
                    self.logic_post_buf =
                        vec![0; (self.enabled_probes.max(0) as u64 * self.packet_len) as usize];
                    self.logic_mem_limit =
                        (self.total_samples / 8) * self.enabled_probes as u64 >= LOGIC_MEMORY_LIMIT;
                    self.init_random_data();
                    Self::timer_start(&mut self.run_time);
                    self.source = Source::LogicRandom;
                } else {
                    self.source = Source::LogicFile;
                }
            }
            Mode::Dso => {
                self.vdiv_change = true;
                if self.instant {
                    self.post_data_len = 0;
                    let total_time = self.timebase as f64 / 1e9 * 10.0;
                    let post_data_per_sec = (DSO_PACKET_LEN as f64 / total_time) as u64;
                    self.packet_len = 2;
                    let packet_num = post_data_per_sec / self.packet_len;
                    self.packet_time = 1.0 / packet_num as f64;
                } else {
                    self.packet_time = DSO_PACKET_TIME;
                }
                self.load_data = true;
                Self::timer_start(&mut self.run_time);
                self.source = Source::Dso;
            }
            Mode::Analog => {
                self.load_data = true;
                self.packet_len = self.samplerate * ANALOG_PROBE_NUM / ANALOG_PACKET_NUM_PER_SEC;
                if self.packet_len < ANALOG_MIN_PACKET_LEN {
                    self.packet_len = ANALOG_MIN_PACKET_LEN;
                    self.packet_time = 1.0 / ((self.samplerate * ANALOG_PROBE_NUM) / 2) as f64;
                } else {
                    if self.packet_len % 2 != 0 {
                        self.packet_len += 1;
                    }
                    self.packet_time = 1.0 / ANALOG_PACKET_NUM_PER_SEC as f64;
                }
                if self.sample_generator == PATTERN_RANDOM {
                    self.init_analog_random_data();
                }
                self.analog_read_pos = 0;
                self.analog_post_buf_len = 0;
                self.source = Source::Analog;
            }
        }
        Ok(())
    }

    fn poll(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step {
        let step = match self.source {
            Source::None => Step::Remove,
            Source::LogicRandom => self.poll_logic_random(abort, out),
            Source::LogicFile => self.poll_logic_file(abort, out),
            Source::Dso => self.poll_dso(abort, out),
            Source::Analog => self.poll_analog(out),
        };
        if step == Step::Remove {
            self.source = Source::None;
        }
        step
    }

    /// hw_dev_acquisition_stop()
    fn stop(&mut self, out: &mut Vec<Packet>) -> SrResult<()> {
        if self.mode != Mode::Logic {
            out.push(Packet::end(pkt_status::OK));
            self.close_archive();
        }
        Ok(())
    }
}

/// LOGIC_PACKET_LEN(n): `ceil((n / 8) / 1000.0 / 8) * 8` bytes per channel.
fn logic_packet_len(samplerate: u64) -> u64 {
    (((samplerate / 8) as f64 / 1000.0 / 8.0).ceil() * 8.0) as u64
}
