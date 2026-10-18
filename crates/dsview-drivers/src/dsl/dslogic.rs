//! The DSLogic driver: libsigrok4DSL/hardware/DSL/dslogic.c and the logic
//! parts of dsl.c, logic mode. (The original DSLogic's DSO and DAQ modes
//! share their code with the DSCope and come with it, Phase 3e.)
//!
//! Acquisition follows the C driver's libusb event handling: one header
//! transfer, then `n` data transfers on EP6 that are resubmitted while data
//! flows; each [`Device::poll`] is one `receive_data()` run, which also polls
//! the progress and overflow status when transfers stay quiet. Stopping is
//! C's two-step `dev_acquisition_stop()`: FORCE_RDY, the data still in the
//! device drains, SR_DF_END; at session end STOP and a status read. Unlike
//! C, every wait has a deadline and the transfers are cancelled when the
//! device does not drain.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dsview_core::{
    Body, Channel, ConfValue, LA_CROSS_DATA, ListItem, Mode, Packet, SrError, SrResult, Status,
    Trigger, TriggerPos, dev_status, dev_type, key, pkt_status,
};
use dsview_usb::{InCompletion, Info, Open, UsbBackend, UsbDevice};

use super::tables::{self, CHANNEL_MODES, DSCOPE_PROFILES, DSLOGIC_PROFILES};
use super::*;
use crate::sys::{self, sr_dbg, sr_err, sr_info};
use crate::{Device, DeviceInfo, Step};

const MAX_HEIGHTS: [&str; 5] = ["1X", "2X", "3X", "4X", "5X"];
const PROBE_MAP_UNITS: [&str; 7] = ["V", "A", "℃", "℉", "g", "m", "m/s"];
const PROBE_OPTIONS: [i32; 6] = [
    key::PROBE_COUPLING,
    key::PROBE_VDIV,
    key::PROBE_MAP_DEFAULT,
    key::PROBE_MAP_UNIT,
    key::PROBE_MAP_MIN,
    key::PROBE_MAP_MAX,
];

/// `enum DSLOGIC_OPERATION_MODE`
pub const LO_OP_BUFFER: i16 = 0;
pub const LO_OP_STREAM: i16 = 1;
pub const LO_OP_INTEST: i16 = 2;
/// `SR_TEST_*`
const TEST_NONE: u8 = 0;
const TEST_INTERNAL: u8 = 1;
const TEST_EXTERNAL: u8 = 2;
const TEST_LOOPBACK: u8 = 3;
/// `SR_BUF_*`
const BUF_STOP: i16 = 0;
const BUF_UPLOAD: i16 = 1;
const TH_3V3: i16 = 0;
const TH_5V0: i16 = 1;
const FILTER_NONE: i16 = 0;
const FILTER_1T: i16 = 1;

/// libsigrok's `ds_set_last_error` codes the GUI shows.
pub mod last_error {
    pub const DEVICE_CLOSED: i32 = 7;
    pub const FIRMWARE_NOT_EXIST: i32 = 10;
    pub const DEVICE_IS_EXCLUSIVE: i32 = 11;
    pub const DEVICE_FIRMWARE_VERSION_LOW: i32 = 12;
    pub const DEVICE_USB_IO_ERROR: i32 = 13;
}

/// `devc->status`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Error,
    Init,
    Start,
    Data,
    Stop,
    Finish,
    Abort,
}

/// A transfer on EP6, in submission order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Xfer {
    Header,
    Data,
}

/// A device found by [`scan`], before it is probed.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub info: Info,
    pub profile: &'static Profile,
}

/// What [`probe`] made of a candidate.
pub enum Probed {
    /// Runs the DSL firmware: a device.
    Device(Box<DsLogic>),
    /// Firmware uploaded: it re-enumerates and is found again.
    FirmwareUploaded,
    /// Does not answer (yet).
    NotReady,
}

/// scan() up to the profile match: DreamSourceLab devices at high or super
/// speed with a DSLogic profile. The flag is "a device was too slow"
/// (DS_EV_DEVICE_SPEED_NOT_MATCH).
pub fn scan(backend: &dyn UsbBackend) -> (Vec<Candidate>, bool) {
    sr_info!("Scan DSLogic device...");
    let Ok(list) = backend.list() else {
        sr_info!("scan(): Failed to list USB devices.");
        return (Vec::new(), false);
    };
    let mut out = Vec::new();
    let mut slow = false;
    for info in list {
        if info.vid != DS_VENDOR_ID {
            continue;
        }
        if info.speed != Speed::High && info.speed != Speed::Super {
            sr_info!(
                "scan(): The idVendor is right, but the usb speed is too low, speed type:{}",
                info.speed as i32
            );
            slow = true;
            continue;
        }
        let prof = DSLOGIC_PROFILES
            .iter()
            .find(|p| p.vid == info.vid && p.pid == info.pid && p.speed == info.speed);
        let Some(profile) = prof else {
            let dscope = DSCOPE_PROFILES
                .iter()
                .any(|p| p.vid == info.vid && p.pid == info.pid && p.speed == info.speed);
            if !dscope {
                sr_info!(
                    "scan(): The profile is not matched, idVendor:{:02X}, idProduct:{:02X}",
                    info.vid,
                    info.pid
                );
            }
            continue;
        };
        out.push(Candidate { info, profile });
    }
    (out, slow)
}

/// dsl_check_conf_profile() and the rest of scan() for one new device: a
/// device when it runs the DSL firmware, else the firmware is uploaded.
/// The strings come from the OS's device list (C reads them over USB).
pub fn probe(backend: Arc<dyn UsbBackend>, c: &Candidate) -> Probed {
    let (manufacturer, product) = if c.info.vid == 0x2A0E && c.info.pid == 0x00E8 {
        ("Hermelin Labs", "Hermelin MSO-E8")
    } else {
        ("DreamSourceLab", "USB-based DSL Instrument v2")
    };
    let has_fw = c
        .info
        .manufacturer
        .as_deref()
        .is_some_and(|m| m.starts_with(manufacturer))
        && c.info
            .product
            .as_deref()
            .is_some_and(|p| p.starts_with(product));
    if has_fw {
        sr_info!(
            "Found a DSLogic device,name:\"{}\", bus {}.{}, pid:{:02x}",
            c.profile.model,
            c.info.bus,
            c.info.address,
            c.profile.pid
        );
        return Probed::Device(Box::new(DsLogic::new(
            c.profile,
            backend,
            c.info.bus,
            c.info.address,
        )));
    }
    let fw = sys::res_dir().join(c.profile.firmware.unwrap_or_default());
    sr_info!(
        "Install firmware bin file, device:\"{}\", file:\"{}\"",
        c.profile.model,
        fw.display()
    );
    match backend.open(
        c.info.bus,
        c.info.address,
        Open::Configure(USB_CONFIGURATION),
    ) {
        Ok(mut usb) => {
            if ezusb_upload(usb.as_mut(), &fw).is_err() {
                sr_err!("Firmware upload failed for device {}.", c.profile.model);
            }
        }
        Err(e) => {
            sr_err!("Failed to open device: {e}.");
            return Probed::NotReady;
        }
    }
    sr_info!(
        "Waitting for device reconnect, name:\"{}\"",
        c.profile.model
    );
    Probed::FirmwareUploaded
}

/// `struct DSL_context` (logic parts) and the device instance.
pub struct DsLogic {
    profile: &'static Profile,
    backend: Arc<dyn UsbBackend>,
    bus: u8,
    address: u8,
    usb: Option<Box<dyn UsbDevice>>,
    mode: Mode,
    status: i32,
    channels: Vec<Channel>,
    generation: u64,
    last_error: i32,
    trigger: Trigger,

    cur_samplerate: u64,
    limit_samples: u64,
    actual_samples: u64,
    actual_bytes: u64,
    clock_type: bool,
    clock_edge: bool,
    rle_mode: bool,
    rle_support: bool,
    instant: bool,
    op_mode: i16,
    stream: bool,
    test_mode: u8,
    buf_options: i16,
    ch_mode: u16,
    samplerates_min_index: usize,
    samplerates_max_index: usize,
    th_level: i16,
    vth: f64,
    filter: i16,
    timebase: u64,
    max_height: u8,
    trigger_slope: u8,
    trigger_source: u8,
    trigger_hrate: u8,
    trigger_hpos: u32,
    trigger_holdoff: u64,
    trigger_margin: u8,
    zero: bool,
    zero_comb_fgain: bool,
    zero_comb: bool,
    roll: bool,
    unit_pitch: u16,
    is_loop: bool,
    ext_samplerate: u64,

    num_samples: u64,
    num_bytes: u64,
    submitted_transfers: u32,
    empty_poll_count: u32,
    state: State,
    trf_completed: bool,
    mstatus_valid: bool,
    mstatus: Status,
    abort: bool,
    overflow: bool,
    transfers: VecDeque<Xfer>,
    buffer_size: usize,
    /// When the current wait for a completion began (µs, sys clock).
    wait_since: Option<i64>,
}

/// A wait slice: the device lock is held at most this long per poll.
const WAIT_SLICE: Duration = Duration::from_millis(10);
/// How long a stopped capture may take to drain before its transfers are
/// cancelled (C waits forever).
const DRAIN_GRACE_US: i64 = 3_000_000;

impl DsLogic {
    /// DSLogic_dev_new() plus the instance scan() makes.
    pub fn new(
        profile: &'static Profile,
        backend: Arc<dyn UsbBackend>,
        bus: u8,
        address: u8,
    ) -> DsLogic {
        let ch_mode = profile.caps.default_channelmode;
        let mut d = DsLogic {
            profile,
            backend,
            bus,
            address,
            usb: None,
            mode: channel_mode(ch_mode).mode,
            status: dev_status::INITIALIZING,
            channels: Vec::new(),
            generation: 0,
            last_error: 0,
            trigger: Trigger::default(),
            cur_samplerate: profile.caps.default_samplerate,
            limit_samples: profile.caps.default_samplelimit,
            actual_samples: 0,
            actual_bytes: 0,
            clock_type: false,
            clock_edge: false,
            rle_mode: false,
            rle_support: false,
            instant: false,
            op_mode: LO_OP_STREAM,
            stream: true,
            test_mode: TEST_NONE,
            buf_options: BUF_UPLOAD,
            ch_mode,
            samplerates_min_index: 0,
            samplerates_max_index: 0,
            th_level: TH_3V3,
            vth: 1.0,
            filter: FILTER_NONE,
            timebase: 10000,
            max_height: 0,
            trigger_slope: 0,
            trigger_source: 0,
            trigger_hrate: 0,
            trigger_hpos: 0,
            trigger_holdoff: 0,
            trigger_margin: 8,
            zero: false,
            zero_comb_fgain: false,
            zero_comb: false,
            roll: false,
            unit_pitch: 0,
            is_loop: false,
            ext_samplerate: 1_000_000,
            num_samples: 0,
            num_bytes: 0,
            submitted_transfers: 0,
            empty_poll_count: 0,
            state: State::Finish,
            trf_completed: false,
            mstatus_valid: false,
            mstatus: Status::default(),
            abort: false,
            overflow: false,
            transfers: VecDeque::new(),
            buffer_size: 0,
            wait_since: None,
        };
        d.stream = d.op_mode == LO_OP_STREAM;
        d.adjust_samplerate();
        d.setup_probes(channel_mode(d.ch_mode).num);
        d.status = dev_status::INACTIVE;
        d
    }

    /// Uses an already opened device (replays and tools): the state after
    /// a successful open, without its commands.
    pub fn attach(&mut self, usb: Box<dyn UsbDevice>) {
        self.usb = Some(usb);
        self.status = dev_status::ACTIVE;
    }

    fn feature(&self, f: u64) -> bool {
        self.profile.caps.feature_caps & f != 0
    }

    fn usb(&mut self) -> SrResult<&mut (dyn UsbDevice + 'static)> {
        self.usb.as_deref_mut().ok_or(SrError::Err)
    }

    fn samplerates(&self) -> &'static [u64] {
        // DSO_E8-1 in logic mode has its own list; no DSLogic profile does
        self.profile.caps.samplerates
    }

    /// dsl_adjust_samplerate()
    fn adjust_samplerate(&mut self) {
        let rates = self.samplerates();
        let m = channel_mode(self.ch_mode);
        let mut i = rates
            .iter()
            .position(|&r| r > m.max_samplerate)
            .unwrap_or(rates.len());
        self.samplerates_max_index = i.wrapping_sub(1);
        i = rates
            .iter()
            .position(|&r| r >= m.min_samplerate)
            .unwrap_or(rates.len());
        self.samplerates_min_index = i;
        if let Some(&max) = rates.get(self.samplerates_max_index)
            && self.cur_samplerate > max
        {
            self.cur_samplerate = max;
        }
        if let Some(&min) = rates.get(self.samplerates_min_index)
            && self.cur_samplerate < min
        {
            self.cur_samplerate = min;
        }
    }

    /// dsl_setup_probes() (after sr_dev_probes_free(), when there were any)
    fn setup_probes(&mut self, num: u16) {
        self.channels.clear();
        let ty = channel_mode(self.ch_mode).ty;
        for j in 0..num {
            self.channels
                .push(Channel::new(j, ty, true, &j.to_string()));
        }
        self.generation += 1;
        self.probe_init();
    }

    /// dsl_probe_init()
    fn probe_init(&mut self) {
        let caps = &self.profile.caps;
        let bits = channel_mode(self.ch_mode).unit_bits;
        for p in &mut self.channels {
            p.bits = bits;
            p.vdiv = 1000;
            if !caps.vdivs.is_empty() {
                let mut best = 0;
                let mut best_err = u64::MAX;
                for &v in caps.vdivs {
                    let e = v.abs_diff(1000);
                    if e < best_err {
                        best_err = e;
                        best = v;
                    }
                }
                if best != 0 {
                    p.vdiv = best;
                }
            }
            p.vfactor = 1;
            p.cali_fgain = [1.0; 4];
            p.cali_comb_fgain = [1.0; 4];
            p.offset = 1 << (bits - 1);
            p.coupling = 0;
            p.trig_value = 1 << (bits - 1);
            p.vpos_trans = caps.default_pwmtrans;
            p.comb_comp = caps.default_comb_comp as i8;
            p.digi_fgain = 0;
            p.map_default = true;
            p.map_unit = Some(PROBE_MAP_UNITS[0].to_owned());
            let span = (p.vdiv * p.vfactor * 10) as f64 / 2000.0;
            p.map_min = -span;
            p.map_max = span;
        }
    }

    /// dsl_adjust_probes(): grows or shrinks the list in place.
    fn adjust_probes(&mut self, num: u16) {
        let ty = channel_mode(self.ch_mode).ty;
        let mut j = self.channels.len() as u16;
        while j < num {
            self.channels
                .push(Channel::new(j, ty, true, &j.to_string()));
            j += 1;
        }
        self.channels.truncate(num as usize);
        for p in &mut self.channels {
            p.enabled = true;
            p.ty = ty;
        }
    }

    /// dsl_en_ch_num(): enabled channels, at least 1.
    fn en_ch_num(&self) -> u64 {
        let n = self.channels.iter().filter(|c| c.enabled).count() as u64;
        n + u64::from(n == 0)
    }

    /// dsl_channel_depth()
    fn channel_depth(&self) -> u64 {
        (self.profile.caps.hw_depth / self.en_ch_num()) & !SAMPLES_ALIGN
    }

    fn super_speed(&self) -> bool {
        self.profile.speed == Speed::Super
    }

    /// to_bytes_per_ms() (logic mode)
    fn bytes_per_ms(&self) -> u32 {
        (self.cur_samplerate as f64 / 1000.0 * self.en_ch_num() as f64 / 8.0).ceil() as u32
    }

    /// dsl_header_size()
    fn header_size(&self) -> usize {
        if self.feature(feature::USB30) {
            1024
        } else {
            512
        }
    }

    /// get_buffer_size() (logic mode)
    fn get_buffer_size(&self) -> usize {
        let single: u32 = if self.super_speed() { 10 } else { 20 };
        let s = if self.stream {
            (single * self.bytes_per_ms()) as usize
        } else {
            1024 * 1024
        };
        if self.super_speed() {
            (s + 1023) & !1023
        } else {
            (s + 511) & !511
        }
    }

    /// get_number_of_transfers() (non-Windows)
    fn number_of_transfers(&self) -> u32 {
        let total: u32 = if self.super_speed() { 40 } else { 100 };
        let n = if self.stream {
            (total as f32 * 1.0f32 * self.bytes_per_ms() as f32 / self.get_buffer_size() as f32)
                .ceil() as u32
        } else {
            1
        };
        n.min(NUM_SIMUL_TRANSFERS)
    }

    /// dsl_get_timeout(), in ms
    fn get_timeout(&self) -> u32 {
        let total = self.get_buffer_size() as u64 * u64::from(self.number_of_transfers());
        let mut t = (total / u64::from(self.bytes_per_ms().max(1))) as u32;
        if self.stream {
            t += t / 4;
        } else {
            t = 20;
        }
        t.max(1)
    }

    /// dsl_dev_open() + dslogic dev_open()
    fn dev_open(&mut self) -> SrResult<()> {
        // hw_dev_open()
        if self.status == dev_status::ACTIVE {
            return Ok(());
        }
        if self.status == dev_status::INITIALIZING {
            sr_info!("The device instance is still boosting.");
        }
        sr_info!(
            "Open usb device instance, bus {}.{}",
            self.bus,
            self.address
        );
        let usb = match self
            .backend
            .open(self.bus, self.address, Open::Claim(USB_INTERFACE))
        {
            Ok(u) => u,
            Err(e) => {
                sr_err!("dev_open: Failed to open device: {e}");
                self.last_error = last_error::DEVICE_IS_EXCLUSIVE;
                return Err(SrError::Err);
            }
        };
        self.usb = Some(usb);
        let vi = match ctl_rd(self.usb()?, dest::FW_VERSION, 0, 2) {
            Ok(v) => v,
            Err(_) => {
                sr_err!("hw_dev_open: Failed to get firmware version.");
                self.last_error = last_error::DEVICE_USB_IO_ERROR;
                return Err(SrError::Err);
            }
        };
        if vi[0] != DSL_REQUIRED_VERSION_MAJOR {
            sr_err!(
                "Expected firmware version {}.{}, got {}.{}.",
                DSL_REQUIRED_VERSION_MAJOR,
                0,
                vi[0],
                vi[1]
            );
            self.last_error = last_error::DEVICE_FIRMWARE_VERSION_LOW;
            self.status = dev_status::INCOMPATIBLE;
        } else {
            self.status = dev_status::ACTIVE;
        }
        sr_info!(
            "Opened device on {}.{}, interface {}, firmware {}.{}.",
            self.bus,
            self.address,
            USB_INTERFACE,
            vi[0],
            vi[1]
        );

        let hw_info = match ctl_rd(self.usb()?, dest::HW_STATUS, 0, 1) {
            Ok(v) => v[0],
            Err(_) => {
                sr_err!("dsl_dev_open: Failed to get hardware information.");
                self.last_error = last_error::DEVICE_USB_IO_ERROR;
                return Err(SrError::Err);
            }
        };
        let fpga_done = hw_info & BM_FPGA_DONE != 0;
        if self.status == dev_status::ACTIVE {
            if !fpga_done {
                let bit = match self.th_level {
                    TH_3V3 => self.profile.fpga_bit33,
                    TH_5V0 => self.profile.fpga_bit50,
                    _ => return Err(SrError::Err),
                }
                .unwrap_or_default();
                let path: PathBuf = sys::res_dir().join(bit);
                if fpga_config(self.usb()?, &path).is_err() {
                    sr_err!("dsl_dev_open: Configure FPGA failed!");
                    self.last_error = last_error::DEVICE_USB_IO_ERROR;
                    return Err(SrError::Err);
                }
            } else {
                let _ = wr_reg(self.usb()?, CTR0_ADDR, BM_NONE);
                match hdl_version(self.usb()?) {
                    Err(_) => {
                        sr_err!("dsl_dev_open: Failed to get FPGA bin version!");
                        self.last_error = last_error::DEVICE_USB_IO_ERROR;
                        self.status = dev_status::INACTIVE;
                        return Err(SrError::Err);
                    }
                    Ok(v) if v != DSL_HDL_VERSION => {
                        sr_err!(
                            "dsl_dev_open: HDL verison incompatible! device:{v:02X}, target:{DSL_HDL_VERSION:02X}"
                        );
                        self.last_error = last_error::DEVICE_FIRMWARE_VERSION_LOW;
                        self.status = dev_status::INCOMPATIBLE;
                        return Err(SrError::Err);
                    }
                    Ok(_) => {}
                }
            }
        }

        // security check
        let _ = wr_reg(self.usb()?, CTR0_ADDR, BM_NONE);
        let raw = match rd_nvm(self.usb()?, SECU_EEP_ADDR, (SECU_STEPS * 2) as u8) {
            Ok(v) => v,
            Err(_) => {
                sr_err!("dsl_dev_open: Read EEPROM content failed!");
                self.last_error = last_error::DEVICE_USB_IO_ERROR;
                return Err(SrError::Err);
            }
        };
        if self.feature(feature::SECURITY) {
            let mut enc = [0u16; SECU_STEPS];
            for (i, w) in enc.iter_mut().enumerate() {
                *w = u16::from_le_bytes([raw[2 * i], raw[2 * i + 1]]);
            }
            if secu_check(self.usb()?, &enc) {
                sr_err!("Security check pass!");
            } else {
                sr_err!("Security check failed!");
            }
        }

        // dslogic dev_open(): threshold
        let code = self.vth_code();
        if wr_reg(self.usb()?, VTH_ADDR, code).is_err() {
            sr_err!("dev_open: Failed to call dsl_wr_reg()!");
            self.last_error = last_error::DEVICE_USB_IO_ERROR;
            return Err(SrError::Err);
        }
        if self.feature(feature::ADF4360) {
            sr_err!("ADC clock setup (DSLogic U3Pro) arrives with the DSCope port.");
        }
        Ok(())
    }

    /// The VTH register value for `vth`.
    fn vth_code(&self) -> u8 {
        if self.feature(feature::MAX25_VTH) {
            (self.vth / 3.3 * (1.0 / 2.0) * 255.0) as u8
        } else {
            (self.vth / 3.3 * (1.5 / 2.5) * 255.0) as u8
        }
    }

    /// dsl_configure_probes(): only its check survives (the masks it builds
    /// are unused): at most 16 stages per channel trigger string.
    fn configure_probes(&self) -> SrResult<()> {
        for p in self.channels.iter().filter(|p| p.enabled) {
            if let Some(t) = &p.trigger
                && t.len() > NUM_TRIGGER_STAGES
            {
                return Err(SrError::Err);
            }
        }
        Ok(())
    }

    /// dsl_fpga_arm() (logic mode)
    fn fpga_arm(&mut self) -> SrResult<()> {
        let t = self.trigger;
        let ch_num = self.en_ch_num();
        let m = channel_mode(self.ch_mode);
        let caps = &self.profile.caps;
        let mut s = Setting::default();
        let mut x = SettingExt32::default();

        let bit = |cond: bool, b: u32| u32::from(cond) << b;
        let mode = u32::from(t.trigger_en) << TRIG_EN_BIT
            | bit(self.clock_type, CLK_TYPE_BIT)
            | bit(self.clock_edge, CLK_EDGE_BIT)
            | bit(self.rle_mode, RLE_MODE_BIT)
            | bit(self.mode == Mode::Dso, DSO_MODE_BIT)
            | bit(self.cur_samplerate == caps.half_samplerate, HALF_MODE_BIT)
            | bit(
                self.cur_samplerate == caps.quarter_samplerate,
                QUAR_MODE_BIT,
            )
            | bit(self.mode == Mode::Analog || self.is_loop, ANALOG_MODE_BIT)
            | bit(self.filter == FILTER_1T, FILTER_BIT)
            | bit(self.instant, INSTANT_BIT)
            | bit(self.bytes_per_ms() < 1024, SLOW_ACQ_BIT)
            | bit(
                t.trigger_mode == dsview_core::SERIAL_TRIGGER,
                STRIG_MODE_BIT,
            )
            | bit(self.stream, STREAM_MODE_BIT)
            | bit(self.test_mode == TEST_LOOPBACK, LPB_TEST_BIT)
            | bit(self.test_mode == TEST_EXTERNAL, EXT_TEST_BIT)
            | bit(self.test_mode == TEST_INTERNAL, INT_TEST_BIT);
        s.mode = mode as u16;

        // sample rate divider
        let mut div = (m.hw_max_samplerate as f64 / self.cur_samplerate as f64).ceil() as u32;
        self.unit_pitch = (m.hw_min_samplerate as f64 / self.cur_samplerate as f64).ceil() as u16;
        let pre = u32::from(m.pre_div);
        s.div_h = ((if div >= pre {
            pre - 1
        } else {
            div.wrapping_sub(1)
        }) << 8) as u16;
        div = (div as f64 / f64::from(pre)).ceil() as u32;
        s.div_l = (div & 0xffff) as u16;
        s.div_h = s.div_h.wrapping_add((div >> 16) as u16);

        // capture counter (64-sample units)
        let cnt = self.actual_samples >> 4;
        s.cnt_l = (cnt & 0xffff) as u16;
        s.cnt_h = (cnt >> 16) as u16;
        s.dso_cnt_l = (self.actual_samples & 0xffff) as u16;
        s.dso_cnt_h = (self.actual_samples >> 16) as u16;

        // trigger position, aligned to the parallel bits
        let mut tpos = ((f64::from(t.trigger_pos) / 100.0 * self.limit_samples as f64) as u32)
            .max(DSLOGIC_ATOMIC_SAMPLES as u32) as u64;
        let pct = if self.stream { 10 } else { DS_MAX_TRIG_PERCENT };
        tpos = tpos.min(self.channel_depth() * pct / 100);
        let tpos = tpos as u32;
        s.tpos_l = (tpos & DSLOGIC_ATOMIC_MASK) as u16;
        s.tpos_h = (tpos >> 16) as u16;

        s.trig_glb = (((ch_num & 0x1f) << 8) + u64::from(t.trigger_stages & 0x00ff)) as u16;

        for p in &self.channels {
            if p.index < 16 {
                s.ch_en_l = s.ch_en_l.wrapping_add(u16::from(p.enabled) << p.index);
            } else {
                s.ch_en_h = s
                    .ch_en_h
                    .wrapping_add(u16::from(p.enabled) << (p.index - 16));
            }
        }
        s.fgain = self.channels.first().map_or(0, |p| p.digi_fgain);

        let mode = s.mode;
        fill_trigger(&mut s, &mut x, &t, mode, caps.feature_caps);

        let super_speed = self.super_speed();
        let ch32 = self.feature(feature::LA_CH32);
        let usb = self.usb()?;
        let fail = |what: &str| {
            sr_err!("{what}");
            SrError::Err
        };
        if !super_speed {
            ctl_wr(usb, dest::WORDWIDE, 0, &[BM_WR_WORDWIDE])
                .map_err(|_| fail("Sent DSL_CTL_WORDWIDE command failed."))?;
        }
        let arm = (SETTING_SIZE / 2) as u32;
        ctl_wr(
            usb,
            dest::BULK_WR,
            0,
            &[arm as u8, (arm >> 8) as u8, (arm >> 16) as u8],
        )
        .map_err(|_| fail("Sent bulk write command of arm FPGA failed."))?;
        // check sys_clr dessert (C polls without a bound)
        wait_hw_status(usb, BM_SYS_CLR).map_err(|_| SrError::Err)?;

        let bytes = s.to_bytes();
        match usb.bulk_out(2, &bytes, Duration::from_millis(1000)) {
            Ok(n) if n == bytes.len() => {}
            Ok(n) => {
                return Err(fail(&format!(
                    "Arm FPGA error: expacted transfer size {}; actually {n}",
                    bytes.len()
                )));
            }
            Err(e) => return Err(fail(&format!("Unable to arm FPGA of dsl device: {e}."))),
        }
        if ch32 {
            let bytes = x.to_bytes();
            match usb.bulk_out(2, &bytes, Duration::from_millis(1000)) {
                Ok(n) if n == bytes.len() => {}
                _ => return Err(fail("Unable to arm FPGA(setting_ext32) of dsl device.")),
            }
        }
        ctl_wr(usb, dest::INTRDY, 0, &[BM_WR_INTRDY]).map_err(|_| SrError::Err)?;
        let st = ctl_rd(usb, dest::HW_STATUS, 0, 1).map_err(|_| SrError::Err)?[0];
        if st & BM_GPIF_DONE != 0 {
            sr_info!("Arm FPGA done");
            Ok(())
        } else {
            Err(SrError::Err)
        }
    }

    // ------------------------------------------------------- transfers

    /// dsl_start_transfers()
    fn start_transfers(&mut self) -> SrResult<()> {
        let n = self.number_of_transfers();
        self.buffer_size = self.get_buffer_size();
        let header = self.header_size();
        let size = self.buffer_size;
        let usb = self.usb()?;
        if let Err(e) = usb.bulk_in_submit(0x86, header) {
            sr_err!("dsl_start_transfers: Failed to submit trigger_pos transfer: {e}.");
            self.state = State::Error;
            return Err(SrError::Err);
        }
        self.transfers.push_back(Xfer::Header);
        self.submitted_transfers += 1;
        for _ in 0..n {
            let usb = self.usb()?;
            if let Err(e) = usb.bulk_in_submit(0x86, size) {
                sr_err!("dsl_start_transfers: Failed to submit transfer: {e}.");
                self.state = State::Error;
                self.abort = true;
                return Err(SrError::Err);
            }
            self.transfers.push_back(Xfer::Data);
            self.submitted_transfers += 1;
        }
        Ok(())
    }

    /// finish_acquisition()
    fn finish(&mut self, out: &mut Vec<Packet>) {
        sr_info!("finish_acquisition: send SR_DF_END packet");
        out.push(Packet::end(pkt_status::OK));
        self.state = State::Finish;
    }

    /// free_transfer()
    fn free_transfer(&mut self, force: bool, out: &mut Vec<Packet>) {
        if !self.is_loop || self.state != State::Data || force {
            self.submitted_transfers -= 1;
        }
        if self.submitted_transfers == 0 {
            sr_info!("Call finish_acquisition()");
            self.finish(out);
        }
    }

    /// receive_header()
    fn receive_header(&mut self, c: InCompletion, out: &mut Vec<Packet>) {
        let d = &c.data;
        let le32 = |at: usize| {
            d.get(at..at + 4)
                .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        let pos = TriggerPos {
            check_id: le32(0),
            real_pos: le32(4),
            ram_saddr: le32(8),
            remain_cnt_l: le32(12),
            remain_cnt_h: le32(16),
            status: le32(20),
        };
        if self.state != State::Abort {
            self.state = State::Error;
        }
        if !self.abort && c.status.is_ok() && pos.check_id == TRIG_CHECKID {
            sr_info!("receive_trigger_pos(): received {} bytes.", d.len());
            let remain = (u64::from(pos.remain_cnt_h) << 32) + u64::from(pos.remain_cnt_l);
            if d.len() == self.header_size()
                && (self.mode != Mode::Logic || self.stream || remain < self.limit_samples)
            {
                if self.mode == Mode::Logic && (!self.stream || self.state == State::Abort) {
                    let en = self.en_ch_num();
                    self.actual_samples = (self.limit_samples - remain) & !SAMPLES_ALIGN;
                    self.actual_bytes =
                        self.actual_samples / DSLOGIC_ATOMIC_SAMPLES * en * DSLOGIC_ATOMIC_SIZE;
                    self.actual_samples = self.actual_bytes / en * 8;
                }
                out.push(Packet::ok(Body::Trigger(pos)));
                self.state = State::Data;
            }
        } else if !self.abort {
            sr_err!(
                "receive_header: trigger packet data error. received {} bytes (expected {}), check_id 0x{:08X}",
                d.len(),
                self.header_size(),
                pos.check_id
            );
            out.push(Packet {
                status: pkt_status::DATA_ERROR,
                body: Body::Trigger(pos),
            });
        }
        self.free_transfer(true, out);
    }

    /// receive_transfer() (logic mode)
    fn receive_transfer(&mut self, c: InCompletion, out: &mut Vec<Packet>) {
        if self.state == State::Start {
            self.state = State::Data;
        }
        if self.abort {
            self.state = State::Stop;
        }
        match &c.status {
            Ok(()) | Err(dsview_usb::UsbError::Timeout) => {}
            Err(e) => {
                sr_err!(
                    "receive_transfer: transfer failed: {e} after {} bytes",
                    c.data.len()
                );
                self.state = State::Error;
            }
        }
        let len = c.data.len() as u64;
        if self.state == State::Data && len != 0 {
            let cur_samples = len * 8 / self.en_ch_num();
            let mut length = len;
            if (self.limit_samples != 0 && (self.num_bytes < self.actual_bytes || self.is_loop))
                || self.mode != Mode::Logic
            {
                if !self.is_loop {
                    length = length.min(self.actual_bytes - self.num_bytes);
                }
                out.push(Packet::ok(Body::Logic {
                    format: LA_CROSS_DATA,
                    index: 0,
                    order: 0,
                    unitsize: 0,
                    data_error: 0,
                    error_pattern: 0,
                    data: c.data[..length as usize].to_vec(),
                }));
            }
            self.num_samples += cur_samples;
            self.num_bytes += length;
            if self.mode == Mode::Logic
                && self.limit_samples != 0
                && !self.is_loop
                && self.num_bytes >= self.actual_bytes
            {
                self.state = State::Stop;
            }
        }
        if self.state == State::Data {
            // resubmit_transfer()
            let size = self.buffer_size;
            let ok = self
                .usb
                .as_deref_mut()
                .map(|u| u.bulk_in_submit(0x86, size));
            match ok {
                Some(Ok(())) => self.transfers.push_back(Xfer::Data),
                _ => {
                    sr_err!("resubmit_transfer: submit failed");
                    self.free_transfer(false, out);
                }
            }
        } else {
            self.free_transfer(false, out);
        }
        self.trf_completed = true;
    }

    /// Handles every completion that is ready within `wait`.
    fn handle_events(&mut self, wait: Duration, out: &mut Vec<Packet>) -> bool {
        let mut any = false;
        let mut wait = wait;
        while let Some(kind) = self.transfers.front().copied() {
            let Some(usb) = self.usb.as_deref_mut() else {
                break;
            };
            let Some(c) = usb.bulk_in_wait(0x86, wait) else {
                break;
            };
            self.transfers.pop_front();
            any = true;
            wait = Duration::ZERO;
            match kind {
                Xfer::Header => self.receive_header(c, out),
                Xfer::Data => self.receive_transfer(c, out),
            }
        }
        any
    }

    /// The bookkeeping part of receive_data(), after the events: the
    /// progress and overflow polls on quiet transfers.
    fn receive_data_tail(&mut self, out: &mut Vec<Packet>) -> Step {
        if self.trf_completed {
            self.empty_poll_count = 0;
        } else {
            self.empty_poll_count += 1;
        }
        if self.empty_poll_count > MAX_EMPTY_POLL && self.state == State::Start {
            self.mstatus.captured_cnt0 = 0;
            if let Some(usb) = self.usb.as_deref_mut() {
                match ctl_rd(usb, dest::I2C_STATUS, 0, 4) {
                    Ok(v) => {
                        self.mstatus.trig_hit = v[0];
                        self.mstatus.captured_cnt3 = v[1];
                        self.mstatus.captured_cnt2 = v[2];
                        self.mstatus.captured_cnt1 = v[3];
                    }
                    Err(_) => sr_err!("Failed to get progress infos."),
                }
            }
            self.empty_poll_count = 0;
        }
        if self.stream && self.empty_poll_count > MAX_EMPTY_POLL {
            if let Some(usb) = self.usb.as_deref_mut() {
                match ctl_rd(usb, dest::HW_STATUS, 0, 1) {
                    Ok(v) => self.overflow = v[0] & BM_SYS_OVERFLOW != 0,
                    Err(_) => sr_err!("Failed to get hardware infos."),
                }
            }
            if self.overflow {
                out.push(Packet::ok(Body::Overflow));
            }
            self.empty_poll_count = 0;
        }
        self.trf_completed = false;
        if self.state == State::Finish {
            Step::Remove
        } else {
            Step::Continue
        }
    }

    /// One receive_data() run: waits up to the C poll timeout (in slices, so
    /// the device lock is released in between) for a completion.
    fn receive_data(&mut self, out: &mut Vec<Packet>) -> Option<Step> {
        let clock = sys::clock();
        let now = clock.now_us();
        let since = *self.wait_since.get_or_insert(now);
        let got = self.handle_events(WAIT_SLICE, out);
        let timed_out = clock.now_us() - since >= i64::from(self.get_timeout()) * 1000;
        if !got && !timed_out {
            return None; // still inside C's g_poll() wait
        }
        self.wait_since = None;
        Some(self.receive_data_tail(out))
    }

    /// dsl_dev_acquisition_stop()
    fn acquisition_stop(&mut self) {
        if !self.abort {
            self.abort = true;
            if let Some(usb) = self.usb.as_deref_mut() {
                let _ = wr_reg(usb, CTR0_ADDR, BM_FORCE_RDY);
            }
            sr_info!("Send command:\"bmFORCE_RDY\"");
        } else if self.state == State::Finish
            && let Some(usb) = self.usb.as_deref_mut()
        {
            match ctl_wr(usb, dest::STOP, 0, &[]) {
                Ok(()) => sr_info!("dsl_dev_acquisition_stop: Sent acquisition stop command!"),
                Err(_) => {
                    sr_err!("dsl_dev_acquisition_stop: Sent acquisition stop command failed!")
                }
            }
            let _ = rd_reg(usb, HW_STATUS_ADDR);
        }
    }

    /// After a stop request: C keeps running receive_data() until the
    /// device has drained. Here that has a deadline, then the transfers are
    /// cancelled.
    fn drain(&mut self, out: &mut Vec<Packet>) {
        let clock = sys::clock();
        let deadline = clock.now_us() + DRAIN_GRACE_US;
        let mut cancelled = false;
        while self.state != State::Finish && !self.transfers.is_empty() {
            if !cancelled && clock.now_us() > deadline {
                sr_err!("The device did not drain after stop; cancelling its transfers.");
                if let Some(usb) = self.usb.as_deref_mut() {
                    usb.bulk_in_cancel_all(0x86);
                }
                cancelled = true;
            }
            if let Some(Step::Remove) = self.receive_data(out) {
                return;
            }
            if self.usb.is_none() {
                break;
            }
        }
        if self.state != State::Finish {
            self.finish(out);
        }
    }

    fn list_items(items: Vec<ListItem>) -> ConfValue {
        ConfValue::List(items)
    }
}

impl Device for DsLogic {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor: self.profile.vendor.to_owned(),
            model: self.profile.model.to_owned(),
            version: self.profile.model_version.map(str::to_owned),
            dev_type: dev_type::USB,
            status: self.status,
            mode: self.mode,
            path: None,
        }
    }

    fn set_status(&mut self, status: i32) {
        self.status = status;
    }

    fn mode_list(&self) -> Vec<Mode> {
        [
            (CAPS_MODE_LOGIC, Mode::Logic),
            (CAPS_MODE_ANALOG, Mode::Analog),
            (CAPS_MODE_DSO, Mode::Dso),
        ]
        .into_iter()
        .filter(|&(c, _)| self.profile.caps.mode_caps & c != 0)
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

    fn open(&mut self) -> SrResult<()> {
        let r = self.dev_open();
        if r.is_err() && self.status == dev_status::ACTIVE {
            // the handle stays open as in C; close() releases it
        }
        r
    }

    /// dsl_dev_close()
    fn close(&mut self) -> SrResult<()> {
        if self.usb.is_none() {
            sr_dbg!("dsl_dev_close(), libusb_device_handle is null.");
            return Err(SrError::Err);
        }
        sr_info!(
            "DSLogic: Closing device on {}.{} interface {}.",
            self.bus,
            self.address,
            USB_INTERFACE
        );
        self.usb = None;
        self.status = dev_status::INACTIVE;
        Ok(())
    }

    fn config_get(&mut self, id: i32, ch: Option<usize>) -> SrResult<Option<ConfValue>> {
        use ConfValue::*;
        let m = channel_mode(self.ch_mode);
        let caps = &self.profile.caps;
        let c = ch.and_then(|i| self.channels.get(i));
        let chan = || c.ok_or(SrError::Err);
        let v = match id {
            // dsl_config_get()
            key::CONN => {
                if self.address == 255 {
                    return Err(SrError::Err);
                }
                Str(format!("{}.{}", self.bus, self.address))
            }
            key::USB_SPEED => I32(self.profile.speed as i32),
            key::USB30_SUPPORT => Bool(caps.feature_caps & feature::USB30 != 0),
            key::LIMIT_SAMPLES => U64(self.limit_samples),
            key::SAMPLERATE => U64(self.cur_samplerate),
            key::RLE_SUPPORT => Bool(self.test_mode == TEST_NONE && self.rle_support),
            key::CLOCK_TYPE => Bool(self.clock_type),
            key::CLOCK_EDGE => Bool(self.clock_edge),
            key::INSTANT => Bool(self.instant),
            key::PROBE_VDIV => U64(chan()?.vdiv),
            key::PROBE_FACTOR => U64(chan()?.vfactor),
            key::PROBE_OFFSET => U16(chan()?.offset),
            key::PROBE_HW_OFFSET => U16(chan()?.hw_offset),
            key::TIMEBASE => U64(self.timebase),
            key::MAX_TIMEBASE => U64(10_000_000_000u64
                .min(1_000_000_000 * caps.dso_depth / u64::from(m.num) / m.min_samplerate / 10)),
            key::MIN_TIMEBASE => U64(1_000_000_000 / m.hw_max_samplerate),
            key::PROBE_COUPLING => Byte(chan()?.coupling),
            key::PROBE_EN => Bool(chan()?.enabled),
            key::TRIGGER_SLOPE => Byte(self.trigger_slope),
            key::TRIGGER_SOURCE => Byte(self.trigger_source & 0x0f),
            key::TRIGGER_CHANNEL => Byte(self.trigger_source >> 4),
            key::TRIGGER_VALUE => Byte(chan()?.trig_value),
            key::HORIZ_TRIGGERPOS => Byte(if self.mode == Mode::Dso {
                self.trigger_hrate
            } else {
                self.trigger_hpos as u8
            }),
            key::TRIGGER_HOLDOFF => U64(self.trigger_holdoff),
            key::TRIGGER_MARGIN => Byte(self.trigger_margin),
            key::HAVE_ZERO => Bool(caps.feature_caps & feature::ZERO != 0),
            key::ZERO => Bool(self.mode == Mode::Dso && self.zero),
            key::ZERO_COMB_FGAIN => Bool(self.zero_comb_fgain),
            key::ROLL => Bool(self.roll),
            key::UNIT_BITS => Byte(m.unit_bits),
            key::REF_MIN => U32(caps.ref_min),
            key::REF_MAX => U32(caps.ref_max),
            key::PROBE_MAP_DEFAULT => Bool(chan()?.map_default),
            key::PROBE_MAP_UNIT => return Ok(chan()?.map_unit.clone().map(Str)),
            key::PROBE_MAP_MIN => F64(chan()?.map_min),
            key::PROBE_MAP_MAX => F64(chan()?.map_max),
            key::ACTUAL_SAMPLES => U64(self.actual_samples),
            key::BANDWIDTH => Bool(caps.feature_caps & feature::M20 != 0),
            key::LA_CH32 => Bool(caps.feature_caps & feature::LA_CH32 != 0),
            // dslogic.c config_get()
            key::OPERATION_MODE => I16(self.op_mode),
            key::FILTER => I16(self.filter),
            key::RLE => Bool(self.rle_mode),
            key::TEST => Bool(self.test_mode != TEST_NONE),
            key::WAIT_UPLOAD => {
                if self.buf_options == BUF_UPLOAD && self.state == State::Start {
                    self.state = State::Abort;
                    if let Some(usb) = self.usb.as_deref_mut() {
                        let _ = wr_reg(usb, CTR0_ADDR, BM_FORCE_STOP);
                    }
                    Bool(true)
                } else {
                    Bool(false)
                }
            }
            key::BUFFER_OPTIONS => I16(self.buf_options),
            key::CHANNEL_MODE => I16(self.ch_mode as i16),
            key::MAX_HEIGHT => Str(MAX_HEIGHTS[self.max_height as usize].to_owned()),
            key::MAX_HEIGHT_VALUE => Byte(self.max_height),
            key::THRESHOLD => I16(self.th_level),
            key::VTH => F64(self.vth),
            key::STREAM => Bool(self.stream),
            key::MAX_DSO_SAMPLERATE => U64(m.max_samplerate),
            key::MAX_DSO_SAMPLELIMITS => U64(caps.dso_depth),
            key::HW_DEPTH => U64(self.channel_depth()),
            key::VLD_CH_NUM => I16(m.vld_num as i16),
            key::TOTAL_CH_NUM => I16(caps.total_ch_num as i16),
            key::EXT_SAMPLERATE => U64(self.ext_samplerate),
            _ => return Err(SrError::Na),
        };
        Ok(Some(v))
    }

    fn config_set(&mut self, id: i32, data: &ConfValue, ch: Option<usize>) -> SrResult<()> {
        if self.status != dev_status::ACTIVE {
            sr_err!("config_set: Device is not opened.");
            return Err(SrError::Err);
        }
        fn chan(s: &mut DsLogic, ch: Option<usize>) -> SrResult<&mut Channel> {
            // C dereferences the channel unchecked (NULL crashes)
            ch.and_then(|i| s.channels.get_mut(i)).ok_or(SrError::Arg)
        }
        // dsl_config_set()
        match id {
            key::ZERO_COMB => {
                self.zero_comb = data.get_bool();
                return Ok(());
            }
            key::PROBE_MAP_DEFAULT => {
                let c = chan(self, ch)?;
                c.map_default = data.get_bool();
                if c.map_default {
                    c.map_unit = Some(PROBE_MAP_UNITS[0].to_owned());
                    let span = (c.vdiv * c.vfactor * 10) as f64 / 2000.0;
                    c.map_min = -span;
                    c.map_max = span;
                }
                return Ok(());
            }
            key::PROBE_MAP_UNIT => {
                let c = chan(self, ch)?;
                c.map_unit = if c.map_default {
                    Some(PROBE_MAP_UNITS[0].to_owned())
                } else {
                    data.get_str().map(str::to_owned)
                };
                return Ok(());
            }
            key::PROBE_MAP_MIN => {
                let c = chan(self, ch)?;
                c.map_min = if c.map_default {
                    -((c.vdiv * c.vfactor * 10) as f64 / 2000.0)
                } else {
                    data.get_f64()
                };
                return Ok(());
            }
            key::PROBE_MAP_MAX => {
                let c = chan(self, ch)?;
                c.map_max = if c.map_default {
                    (c.vdiv * c.vfactor * 10) as f64 / 2000.0
                } else {
                    data.get_f64()
                };
                return Ok(());
            }
            _ => {}
        }
        // dslogic.c config_set()
        let mut ret = Ok(());
        match id {
            key::CLOCK_TYPE => self.clock_type = data.get_bool(),
            key::RLE_SUPPORT => self.rle_support = data.get_bool(),
            key::CLOCK_EDGE => self.clock_edge = data.get_bool(),
            key::LIMIT_SAMPLES => self.limit_samples = data.get_u64(),
            key::PROBE_VDIV => chan(self, ch)?.vdiv = data.get_u64(),
            key::PROBE_FACTOR => chan(self, ch)?.vfactor = data.get_u64(),
            key::TIMEBASE => self.timebase = data.get_u64(),
            key::PROBE_COUPLING => {
                let c = chan(self, ch)?;
                c.coupling = data.get_byte();
                if c.coupling == 2 {
                    c.coupling = 0;
                }
            }
            key::TRIGGER_SLOPE => self.trigger_slope = data.get_byte(),
            key::TRIGGER_VALUE => chan(self, ch)?.trig_value = data.get_byte(),
            key::HORIZ_TRIGGERPOS => {
                if self.mode == Mode::Dso {
                    self.trigger_hrate = data.get_byte();
                    self.trigger_hpos = (f64::from(self.trigger_hrate)
                        * self.en_ch_num() as f64
                        * self.limit_samples as f64
                        / 200.0) as u32;
                } else {
                    self.trigger_hpos =
                        (f64::from(data.get_byte()) * self.limit_samples as f64 / 100.0) as u32;
                }
            }
            key::TRIGGER_HOLDOFF => self.trigger_holdoff = data.get_u64(),
            key::TRIGGER_MARGIN => self.trigger_margin = data.get_byte(),
            key::SAMPLERATE => {
                if self.test_mode == TEST_NONE {
                    self.cur_samplerate = data.get_u64();
                }
            }
            key::FILTER => {
                let nv = data.get_i16();
                if nv == FILTER_NONE || nv == FILTER_1T {
                    self.filter = nv;
                } else {
                    ret = Err(SrError::Err);
                }
            }
            key::RLE => self.rle_mode = data.get_bool(),
            key::INSTANT => {
                if self.mode == Mode::Dso {
                    self.instant = data.get_bool();
                }
            }
            key::DEVICE_MODE => {
                let mode = Mode::from_i32(i32::from(data.get_i16()));
                if mode != Some(Mode::Logic) {
                    sr_err!("DSLogic DSO and DAQ modes arrive with the DSCope port (Phase 3e).");
                    return Err(SrError::Na);
                }
                self.mode = Mode::Logic;
                if let Some(usb) = self.usb.as_deref_mut() {
                    let _ = wr_reg(usb, CTR0_ADDR, BM_SCOPE_CLR);
                }
                let mut num = 0;
                if let Some(m) = CHANNEL_MODES.iter().enumerate().find(|(i, m)| {
                    m.mode == Mode::Logic && self.profile.caps.channels & (1 << i) != 0
                }) {
                    self.ch_mode = m.1.id;
                    num = m.1.num;
                    self.stream = m.1.stream;
                    self.adjust_samplerate();
                }
                self.setup_probes(num);
            }
            key::OPERATION_MODE => {
                let nv = data.get_i16();
                if self.mode == Mode::Logic && self.op_mode != nv {
                    let first_mode = |stream: bool| {
                        CHANNEL_MODES
                            .iter()
                            .enumerate()
                            .find(|(i, m)| {
                                m.mode == Mode::Logic
                                    && m.stream == stream
                                    && self.profile.caps.channels & (1 << i) != 0
                            })
                            .map(|(_, m)| m.id)
                    };
                    match nv {
                        LO_OP_BUFFER | LO_OP_STREAM => {
                            self.op_mode = nv;
                            self.test_mode = TEST_NONE;
                            self.stream = nv == LO_OP_STREAM;
                            if let Some(id) = first_mode(self.stream) {
                                self.ch_mode = id;
                            }
                        }
                        LO_OP_INTEST => {
                            self.op_mode = LO_OP_INTEST;
                            self.test_mode = TEST_INTERNAL;
                            self.ch_mode = self.profile.caps.intest_channel;
                            self.stream = !self.feature(feature::BUF);
                        }
                        _ => ret = Err(SrError::Err),
                    }
                    self.adjust_probes(channel_mode(self.ch_mode).num);
                    self.adjust_samplerate();
                    if self.op_mode == LO_OP_INTEST {
                        let m = channel_mode(self.ch_mode);
                        self.cur_samplerate = if self.stream {
                            m.max_samplerate / 10
                        } else {
                            100_000_000
                        };
                        self.limit_samples = if self.stream {
                            self.cur_samplerate * 3
                        } else {
                            self.profile.caps.hw_depth / self.en_ch_num()
                        };
                    }
                }
                sr_dbg!("config_set: setting pattern to {}", self.op_mode);
            }
            key::BUFFER_OPTIONS => {
                let nv = data.get_i16();
                if self.mode == Mode::Logic && (nv == BUF_STOP || nv == BUF_UPLOAD) {
                    self.buf_options = nv;
                }
            }
            key::CHANNEL_MODE => {
                let nv = data.get_i16();
                if self.mode == Mode::Logic {
                    if let Some((_, m)) = CHANNEL_MODES.iter().enumerate().find(|(i, m)| {
                        self.profile.caps.channels & (1 << i) != 0
                            && i32::from(m.id) == i32::from(nv)
                    }) {
                        self.ch_mode = m.id;
                    }
                    self.adjust_probes(channel_mode(self.ch_mode).num);
                    self.adjust_samplerate();
                }
                sr_info!("config_set: setting channel mode to {}", self.ch_mode);
            }
            key::THRESHOLD => {
                let nv = data.get_i16();
                if self.mode == Mode::Logic && nv != self.th_level {
                    self.th_level = match nv {
                        TH_3V3 | TH_5V0 => nv,
                        _ => return Err(SrError::Err),
                    };
                    let bit = if self.th_level == TH_3V3 {
                        self.profile.fpga_bit33
                    } else {
                        self.profile.fpga_bit50
                    }
                    .unwrap_or_default();
                    let path = sys::res_dir().join(bit);
                    if fpga_config(self.usb()?, &path).is_err() {
                        sr_err!("Configure FPGA failed!");
                        ret = Err(SrError::Err);
                    }
                }
            }
            key::VTH => {
                self.vth = data.get_f64();
                let code = self.vth_code();
                ret = wr_reg(self.usb()?, VTH_ADDR, code).map_err(|_| SrError::Err);
            }
            key::MAX_HEIGHT => {
                if let Some(i) = data
                    .get_str()
                    .and_then(|s| MAX_HEIGHTS.iter().position(|h| *h == s))
                {
                    self.max_height = i as u8;
                }
            }
            key::PROBE_EN => chan(self, ch)?.enabled = data.get_bool(),
            key::PROBE_OFFSET => chan(self, ch)?.offset = data.get_u16(),
            key::TRIGGER_SOURCE => self.trigger_source = data.get_byte(),
            key::TRIGGER_CHANNEL => {
                self.trigger_source =
                    (data.get_byte() << 4).wrapping_add(self.trigger_source & 0x0f)
            }
            key::STREAM => self.stream = data.get_bool(),
            key::LOOP_MODE => {
                self.is_loop = data.get_bool();
                sr_info!("Set device loop mode:{}", i32::from(self.is_loop));
            }
            key::EXT_SAMPLERATE => {
                self.ext_samplerate = data.get_u64();
                sr_info!("Set device ext samplerate: {}", self.ext_samplerate);
            }
            _ => ret = Err(SrError::Na),
        }
        ret
    }

    fn config_list(&self, id: i32) -> SrResult<ConfValue> {
        use ConfValue::*;
        let caps = &self.profile.caps;
        Ok(match id {
            // dsl_config_list()
            key::SAMPLERATE => {
                let mut v = Vec::new();
                if self.clock_type {
                    v.push(self.ext_samplerate);
                }
                let rates = self.samplerates();
                if let Some(r) = rates.get(self.samplerates_min_index..=self.samplerates_max_index)
                {
                    v.extend_from_slice(r);
                }
                Dict(vec![("samplerates".into(), U64s(v))])
            }
            key::PROBE_CONFIGS => I32s(PROBE_OPTIONS.to_vec()),
            // C dereferences a NULL vdivs list for profiles without one
            key::PROBE_VDIV => Dict(vec![("vdivs".into(), U64s(caps.vdivs.to_vec()))]),
            key::PROBE_COUPLING => Dict(vec![("coupling".into(), Bytes(vec![0, 1]))]),
            key::PROBE_MAP_UNIT => StrV(PROBE_MAP_UNITS.iter().map(|s| s.to_string()).collect()),
            // dslogic.c config_list()
            key::DEVICE_OPTIONS => I32s(if self.feature(feature::VTH) {
                tables::HWOPTIONS_PRO.to_vec()
            } else {
                tables::HWOPTIONS.to_vec()
            }),
            key::DEVICE_SESSIONS => I32s(if self.feature(feature::VTH) {
                tables::SESSIONS_PRO.to_vec()
            } else {
                tables::SESSIONS.to_vec()
            }),
            key::OPERATION_MODE => Self::list_items(tables::opmode_list()),
            key::BUFFER_OPTIONS => Self::list_items(tables::bufoption_list()),
            key::CHANNEL_MODE => Self::list_items(
                CHANNEL_MODES
                    .iter()
                    .enumerate()
                    .filter(|(i, m)| m.stream == self.stream && caps.channels & (1 << i) != 0)
                    .filter(|(_, m)| self.test_mode == TEST_NONE || caps.intest_channel == m.id)
                    .map(|(_, m)| ListItem {
                        id: i32::from(m.id),
                        name: m.descr.to_owned(),
                    })
                    .collect(),
            ),
            key::THRESHOLD => Self::list_items(tables::threshold_list()),
            key::FILTER => Self::list_items(tables::filter_list()),
            key::MAX_HEIGHT => StrV(MAX_HEIGHTS.iter().map(|s| s.to_string()).collect()),
            _ => return Err(SrError::Na),
        })
    }

    /// dsl_dev_status_get()
    fn status(&self, prg: bool) -> SrResult<Status> {
        if prg || self.mstatus_valid {
            Ok(self.mstatus)
        } else {
            Err(SrError::Err)
        }
    }

    /// dev_acquisition_start()
    fn start(&mut self, out: &mut Vec<Packet>) -> SrResult<()> {
        if self.status != dev_status::ACTIVE {
            self.last_error = last_error::DEVICE_CLOSED;
            return Err(SrError::DeviceClosed);
        }
        if self.mode != Mode::Logic {
            sr_err!("DSLogic DSO and DAQ modes arrive with the DSCope port (Phase 3e).");
            return Err(SrError::Err);
        }
        self.num_samples = 0;
        self.num_bytes = 0;
        self.empty_poll_count = 0;
        self.state = State::Init;
        self.transfers.clear();
        self.submitted_transfers = 0;
        self.actual_samples = (self.limit_samples + SAMPLES_ALIGN) & !SAMPLES_ALIGN;
        self.actual_bytes =
            self.actual_samples / DSLOGIC_ATOMIC_SAMPLES * self.en_ch_num() * DSLOGIC_ATOMIC_SIZE;
        self.abort = false;
        self.mstatus_valid = false;
        self.mstatus.captured_cnt0 = 0;
        self.mstatus.captured_cnt1 = 0;
        self.mstatus.captured_cnt2 = 0;
        self.mstatus.captured_cnt3 = 0;
        self.mstatus.trig_hit = 0;
        self.overflow = false;
        self.wait_since = None;
        self.trf_completed = false;

        if self.configure_probes().is_err() {
            sr_err!("dev_acquisition_start: Failed to configure probes.");
            return Err(SrError::Err);
        }
        if ctl_wr(self.usb()?, dest::STOP, 0, &[]).is_err() {
            sr_err!("dev_acquisition_start: Stop DSLogic acquisition failed!");
            return Err(SrError::Err);
        }
        sr_info!("dev_acquisition_start: Stop Previous DSLogic acquisition!");
        if let Err(e) = self.fpga_arm() {
            sr_err!("dev_acquisition_start: Arm FPGA failed!");
            return Err(e);
        }
        if let Err(e) = self.start_transfers() {
            sr_err!("dev_acquisition_start: Could not submit usb transfer");
            return Err(e);
        }
        if ctl_wr(self.usb()?, dest::START, 0, &[]).is_err() {
            self.state = State::Error;
            self.abort = true;
            return Err(SrError::Err);
        }
        self.state = State::Start;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        out.push(Packet::ok(Body::Header {
            feed_version: 1,
            start_sec: now.as_secs() as i64,
            start_usec: i64::from(now.subsec_micros()),
        }));
        Ok(())
    }

    fn poll(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step {
        if self.transfers.is_empty() && self.state == State::Finish {
            return Step::Remove;
        }
        if !abort {
            return self.receive_data(out).unwrap_or(Step::Continue);
        }
        // the session's stop request: C runs this callback, then
        // dev_acquisition_stop(), then receive_data() until the device drained
        let _ = self.handle_events(Duration::ZERO, out);
        let _ = self.receive_data_tail(out);
        if self.state != State::Finish {
            self.acquisition_stop();
            self.drain(out);
        }
        Step::Remove
    }

    fn stop(&mut self, _out: &mut Vec<Packet>) -> SrResult<()> {
        self.acquisition_stop();
        Ok(())
    }

    fn set_trigger(&mut self, t: &Trigger) {
        self.trigger = *t;
    }

    fn set_location(&mut self, bus: u8, address: u8) {
        self.bus = bus;
        self.address = address;
    }

    fn last_error(&self) -> i32 {
        self.last_error
    }
}
