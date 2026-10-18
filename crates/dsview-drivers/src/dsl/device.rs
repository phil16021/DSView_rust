//! The device the DSLogic and DSCope drivers share: libsigrok4DSL's
//! `struct DSL_context` and `dsl.c`. Open, arm, the transfers and the data
//! path of every mode (logic, DSO, DAQ), the progress polls, stop. What
//! differs per driver (`dslogic.c`, `dscope.c`) lives in [`super::dslogic`]
//! and [`super::dscope`]; [`Dsl`] dispatches on its [`Family`].
//!
//! Acquisition follows the C driver's libusb event handling: one header
//! transfer, then `n` data transfers on EP6 that are resubmitted while data
//! flows; each [`Device::poll`] is one `receive_data()` run, which also polls
//! the progress status when transfers stay quiet. Stopping is C's two-step
//! `dev_acquisition_stop()`: FORCE_RDY, the data still in the device drains,
//! SR_DF_END; at session end STOP and a status read. Unlike C, every wait has
//! a deadline and the transfers are cancelled when the device does not drain.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dsview_core::{
    Body, Channel, ConfValue, LA_CROSS_DATA, Mode, Packet, SrError, SrResult, Status, Trigger,
    TriggerPos, dev_status, dev_type, key, pkt_status,
};
use dsview_usb::{InCompletion, Info, Open, UsbBackend, UsbDevice};

use super::dscope::ZeroCal;
use super::tables::{self, DSCOPE_PROFILES, DSLOGIC_PROFILES};
use super::*;
use crate::sys::{self, sr_dbg, sr_err, sr_info};
use crate::{Device, DeviceInfo, Step};

pub(super) const PROBE_MAP_UNITS: [&str; 7] = ["V", "A", "℃", "℉", "g", "m", "m/s"];
const PROBE_OPTIONS: [i32; 6] = [
    key::PROBE_COUPLING,
    key::PROBE_VDIV,
    key::PROBE_MAP_DEFAULT,
    key::PROBE_MAP_UNIT,
    key::PROBE_MAP_MIN,
    key::PROBE_MAP_MAX,
];

/// `SR_TEST_*`
pub(super) const TEST_NONE: u8 = 0;
pub(super) const TEST_INTERNAL: u8 = 1;
pub(super) const TEST_EXTERNAL: u8 = 2;
pub(super) const TEST_LOOPBACK: u8 = 3;
pub(super) const TH_3V3: i16 = 0;
pub(super) const TH_5V0: i16 = 1;
pub(super) const FILTER_NONE: i16 = 0;
pub(super) const FILTER_1T: i16 = 1;
/// `SR_*_COUPLING`
pub(super) const DC_COUPLING: u8 = 0;
pub(super) const AC_COUPLING: u8 = 1;
pub(super) const GND_COUPLING: u8 = 2;
/// `SR_MQ_VOLTAGE`, `SR_UNIT_VOLT`, `SR_MQFLAG_AC`
const MQ_VOLTAGE: i32 = 10000;
const UNIT_VOLT: i32 = 10000;
const MQFLAG_AC: u64 = 1;

/// libsigrok's `ds_set_last_error` codes the GUI shows.
pub mod last_error {
    pub const DEVICE_CLOSED: i32 = 7;
    pub const FIRMWARE_NOT_EXIST: i32 = 10;
    pub const DEVICE_IS_EXCLUSIVE: i32 = 11;
    pub const DEVICE_FIRMWARE_VERSION_LOW: i32 = 12;
    pub const DEVICE_USB_IO_ERROR: i32 = 13;
}

/// Which driver a device belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    DsLogic,
    DsCope,
}

impl Family {
    fn profiles(self) -> &'static [Profile] {
        match self {
            Family::DsLogic => &DSLOGIC_PROFILES,
            Family::DsCope => &DSCOPE_PROFILES,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Family::DsLogic => "DSLogic",
            Family::DsCope => "DSCope",
        }
    }
}

/// `devc->status`
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
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
pub(super) enum Xfer {
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
    Device(Box<Dsl>),
    /// Firmware uploaded: it re-enumerates and is found again.
    FirmwareUploaded,
    /// Does not answer (yet).
    NotReady,
}

/// scan() up to the profile match: DreamSourceLab devices at high or super
/// speed with a profile of `family`. The flag is "a device was too slow"
/// (DS_EV_DEVICE_SPEED_NOT_MATCH).
pub fn scan(family: Family, backend: &dyn UsbBackend) -> (Vec<Candidate>, bool) {
    sr_info!("Scan {} device...", family.name());
    let Ok(list) = backend.list() else {
        sr_info!("scan(): Failed to list USB devices.");
        return (Vec::new(), false);
    };
    let other = match family {
        Family::DsLogic => Family::DsCope,
        Family::DsCope => Family::DsLogic,
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
        let matches =
            |p: &&Profile| p.vid == info.vid && p.pid == info.pid && p.speed == info.speed;
        let Some(profile) = family.profiles().iter().find(matches) else {
            if !other.profiles().iter().any(|p| matches(&p)) {
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
pub fn probe(family: Family, backend: Arc<dyn UsbBackend>, c: &Candidate) -> Probed {
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
            "Found a {} device,name:\"{}\", bus {}.{}, pid:{:02x}",
            family.name(),
            c.profile.model,
            c.info.bus,
            c.info.address,
            c.profile.pid
        );
        return Probed::Device(Box::new(Dsl::new(
            family,
            c.profile,
            backend,
            c.info.bus,
            c.info.address,
        )));
    }
    let Some(firmware) = c.profile.firmware else {
        // C builds the path from a NULL name and crashes (F24)
        sr_err!(
            "Device {} has no firmware file and does not run its firmware.",
            c.profile.model
        );
        return Probed::NotReady;
    };
    let fw = sys::res_dir().join(firmware);
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

/// `struct DSL_context` and the device instance.
pub struct Dsl {
    pub(super) family: Family,
    pub(super) profile: &'static Profile,
    pub(super) backend: Arc<dyn UsbBackend>,
    pub(super) bus: u8,
    pub(super) address: u8,
    pub(super) usb: Option<Box<dyn UsbDevice>>,
    pub(super) mode: Mode,
    pub(super) status: i32,
    pub(super) channels: Vec<Channel>,
    /// `sr_channel.vga_ptr` of each channel: the per-V/div calibration
    /// (None where C has NULL: channels added by dsl_adjust_probes()).
    pub(super) vga: Vec<Option<Vec<Vga>>>,
    pub(super) generation: u64,
    pub(super) last_error: i32,
    pub(super) trigger: Trigger,

    pub(super) cur_samplerate: u64,
    pub(super) limit_samples: u64,
    pub(super) actual_samples: u64,
    pub(super) actual_bytes: u64,
    pub(super) clock_type: bool,
    pub(super) clock_edge: bool,
    pub(super) rle_mode: bool,
    pub(super) rle_support: bool,
    pub(super) instant: bool,
    pub(super) op_mode: i16,
    pub(super) stream: bool,
    pub(super) test_mode: u8,
    pub(super) buf_options: i16,
    pub(super) ch_mode: u16,
    pub(super) samplerates_min_index: usize,
    pub(super) samplerates_max_index: usize,
    pub(super) th_level: i16,
    pub(super) vth: f64,
    pub(super) filter: i16,
    pub(super) timebase: u64,
    pub(super) max_height: u8,
    pub(super) trigger_slope: u8,
    pub(super) trigger_source: u8,
    pub(super) trigger_hrate: u8,
    pub(super) trigger_hpos: u32,
    pub(super) trigger_holdoff: u64,
    pub(super) trigger_margin: u8,
    pub(super) zero: bool,
    pub(super) zero_stage: i32,
    pub(super) zero_pcnt: i32,
    pub(super) zero_branch: bool,
    pub(super) zero_comb_fgain: bool,
    pub(super) zero_comb: bool,
    /// dso_zero()'s function statics (C shares them between devices).
    pub(super) zcal: ZeroCal,
    pub(super) cali: bool,
    pub(super) bw_limit: i16,
    pub(super) roll: bool,
    pub(super) unit_pitch: u16,
    pub(super) is_loop: bool,
    pub(super) ext_samplerate: u64,
    pub(super) instant_tail_bytes: i64,

    pub(super) num_samples: u64,
    pub(super) num_bytes: u64,
    pub(super) submitted_transfers: u32,
    pub(super) empty_poll_count: u32,
    pub(super) state: State,
    pub(super) trf_completed: bool,
    pub(super) mstatus_valid: bool,
    pub(super) mstatus: Status,
    pub(super) abort: bool,
    pub(super) overflow: bool,
    pub(super) transfers: VecDeque<Xfer>,
    pub(super) buffer_size: usize,
    /// When the current wait for a completion began (µs, sys clock).
    pub(super) wait_since: Option<i64>,
}

/// A wait slice: the device lock is held at most this long per poll.
const WAIT_SLICE: Duration = Duration::from_millis(10);
/// How long a stopped capture may take to drain before its transfers are
/// cancelled (C waits forever).
const DRAIN_GRACE_US: i64 = 3_000_000;

impl Dsl {
    /// DSLogic_dev_new() / DSCope_dev_new() plus the instance scan() makes.
    pub fn new(
        family: Family,
        profile: &'static Profile,
        backend: Arc<dyn UsbBackend>,
        bus: u8,
        address: u8,
    ) -> Dsl {
        let ch_mode = profile.caps.default_channelmode;
        let mut d = Dsl {
            family,
            profile,
            backend,
            bus,
            address,
            usb: None,
            mode: channel_mode(ch_mode).mode,
            status: dev_status::INITIALIZING,
            channels: Vec::new(),
            vga: Vec::new(),
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
            op_mode: 0,
            stream: false,
            test_mode: TEST_NONE,
            buf_options: 0,
            ch_mode,
            samplerates_min_index: 0,
            samplerates_max_index: 0,
            th_level: TH_3V3,
            vth: 0.0,
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
            zero_stage: 0,
            zero_pcnt: 0,
            zero_branch: false,
            zero_comb_fgain: false,
            zero_comb: false,
            zcal: ZeroCal::default(),
            cali: false,
            bw_limit: 0,
            roll: false,
            unit_pitch: 0,
            is_loop: false,
            ext_samplerate: 0,
            instant_tail_bytes: 0,
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
        match family {
            Family::DsLogic => d.dslogic_defaults(),
            Family::DsCope => d.dscope_defaults(),
        }
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

    pub fn family(&self) -> Family {
        self.family
    }

    pub(super) fn feature(&self, f: u64) -> bool {
        self.profile.caps.feature_caps & f != 0
    }

    pub(super) fn is_e8(&self) -> bool {
        self.profile.caps.vga_id == DSL_VGA_ID_E8
    }

    pub(super) fn usb(&mut self) -> SrResult<&mut (dyn UsbDevice + 'static)> {
        self.usb.as_deref_mut().ok_or(SrError::Err)
    }

    /// Runs `f` on the open device; C ignores the result of most commands
    /// (and has no "no device" case: the handle is always set while open).
    pub(super) fn cmd<T>(
        &mut self,
        f: impl FnOnce(&mut dyn UsbDevice) -> CmdResult<T>,
    ) -> SrResult<T> {
        let usb = self.usb()?;
        f(usb).map_err(|_| SrError::Err)
    }

    /// dsl_samplerates(): the profile's list, or DSO_E8-1's logic list.
    pub(super) fn samplerates(&self) -> &'static [u64] {
        if self.is_e8() && channel_mode(self.ch_mode).mode == Mode::Logic {
            &tables::SAMPLERATES_LOGIC_E8
        } else {
            self.profile.caps.samplerates
        }
    }

    /// dsl_adjust_samplerate()
    pub(super) fn adjust_samplerate(&mut self) {
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
    pub(super) fn setup_probes(&mut self, num: u16) {
        self.channels.clear();
        self.vga.clear();
        let ty = channel_mode(self.ch_mode).ty;
        for j in 0..num {
            self.channels
                .push(Channel::new(j, ty, true, &j.to_string()));
            self.vga.push(None);
        }
        self.generation += 1;
        self.probe_init();
    }

    /// dsl_probe_init()
    pub(super) fn probe_init(&mut self) {
        let caps = &self.profile.caps;
        let bits = channel_mode(self.ch_mode).unit_bits;
        for (p, vga) in self.channels.iter_mut().zip(self.vga.iter_mut()) {
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
            p.coupling = DC_COUPLING;
            p.trig_value = 1 << (bits - 1);
            p.vpos_trans = caps.default_pwmtrans;
            p.comb_comp = caps.default_comb_comp as i8;
            p.digi_fgain = 0;
            p.map_default = true;
            p.map_unit = Some(PROBE_MAP_UNITS[0].to_owned());
            let span = (p.vdiv * p.vfactor * DS_CONF_DSO_VDIVS) as f64 / 2000.0;
            p.map_min = -span;
            p.map_max = span;
            if !caps.vdivs.is_empty() && vga.is_none() {
                *vga = Some(
                    caps.vdivs
                        .iter()
                        .map(|&key| {
                            let mut v = Vga {
                                id: caps.vga_id,
                                key,
                                ..Vga::default()
                            };
                            // the last match wins, as in C's loop
                            if let Some(d) = VGA_DEFAULTS
                                .iter()
                                .rfind(|d| d.id == caps.vga_id && d.key == key)
                            {
                                v.vgain = d.vgain;
                                v.preoff = d.preoff;
                            }
                            v
                        })
                        .collect(),
                );
            }
        }
    }

    /// dsl_adjust_probes(): grows or shrinks the list in place.
    pub(super) fn adjust_probes(&mut self, num: u16) {
        let ty = channel_mode(self.ch_mode).ty;
        let mut j = self.channels.len() as u16;
        while j < num {
            self.channels
                .push(Channel::new(j, ty, true, &j.to_string()));
            self.vga.push(None);
            j += 1;
        }
        self.channels.truncate(num as usize);
        self.vga.truncate(num as usize);
        for p in &mut self.channels {
            p.enabled = true;
            p.ty = ty;
        }
    }

    /// dsl_en_ch_num(): enabled channels, at least 1.
    pub(super) fn en_ch_num(&self) -> u64 {
        let n = self.channels.iter().filter(|c| c.enabled).count() as u64;
        n + u64::from(n == 0)
    }

    /// dsl_channel_depth()
    pub(super) fn channel_depth(&self) -> u64 {
        (self.profile.caps.hw_depth / self.en_ch_num()) & !SAMPLES_ALIGN
    }

    fn super_speed(&self) -> bool {
        self.profile.speed == Speed::Super
    }

    /// to_bytes_per_ms()
    pub(super) fn bytes_per_ms(&self) -> u32 {
        let en = self.en_ch_num() as f64;
        if self.mode == Mode::Logic {
            (self.cur_samplerate as f64 / 1000.0 * en / 8.0).ceil() as u32
        } else if self.cur_samplerate > 100_000_000 {
            c_u32(100_000_000.0 / 1000.0 * en)
        } else {
            let m = channel_mode(self.ch_mode);
            c_u32((self.cur_samplerate.max(m.hw_min_samplerate) as f64 / 1000.0 * en).ceil())
        }
    }

    /// dsl_header_size()
    pub(super) fn header_size(&self) -> usize {
        if self.feature(feature::USB30) {
            1024
        } else {
            512
        }
    }

    /// get_buffer_size()
    pub(super) fn get_buffer_size(&self) -> usize {
        let single: u32 = if self.super_speed() { 10 } else { 20 };
        let s = if self.mode == Mode::Dso {
            if self.instant {
                self.profile.caps.dso_depth as usize
            } else {
                (self.actual_samples * self.en_ch_num()) as usize + self.header_size()
            }
        } else if self.stream {
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
    pub(super) fn number_of_transfers(&self) -> u32 {
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
    pub(super) fn get_timeout(&self) -> u32 {
        let total = self.get_buffer_size() as u64 * u64::from(self.number_of_transfers());
        let mut t = (total / u64::from(self.bytes_per_ms().max(1))) as u32;
        if self.stream {
            t += t / 4;
        } else {
            t = 20;
        }
        t.max(1)
    }

    /// dsl_dev_open(): opens the device, checks firmware and FPGA, the
    /// security check. Returns `fpga_done` (the FPGA was configured already).
    pub(super) fn dsl_dev_open(&mut self) -> SrResult<bool> {
        // hw_dev_open(): an open device is used as it is (C then claims
        // the interface again and redoes the checks below)
        if self.status == dev_status::ACTIVE {
            if self.usb.is_none() {
                return Err(SrError::Err);
            }
            return self.dsl_dev_open_checks();
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
        self.dsl_dev_open_checks()
    }

    /// dsl_dev_open() after hw_dev_open(): FPGA and security.
    fn dsl_dev_open_checks(&mut self) -> SrResult<bool> {
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
                };
                let Some(bit) = bit else {
                    // C builds the path from a NULL name and crashes (F24)
                    sr_err!("dsl_dev_open: the device has no FPGA bitstream.");
                    self.last_error = last_error::FIRMWARE_NOT_EXIST;
                    return Err(SrError::Err);
                };
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
        Ok(fpga_done)
    }

    /// dsl_dev_close()
    pub(super) fn dsl_dev_close(&mut self) -> SrResult<()> {
        if self.usb.is_none() {
            sr_dbg!("dsl_dev_close(), libusb_device_handle is null.");
            return Err(SrError::Err);
        }
        sr_info!(
            "{}: Closing device on {}.{} interface {}.",
            self.family.name(),
            self.bus,
            self.address,
            USB_INTERFACE
        );
        self.usb = None;
        self.status = dev_status::INACTIVE;
        Ok(())
    }

    /// dsl_configure_probes(): only its check survives (the masks it builds
    /// are unused): at most 16 stages per channel trigger string.
    pub(super) fn configure_probes(&self) -> SrResult<()> {
        for p in self.channels.iter().filter(|p| p.enabled) {
            if let Some(t) = &p.trigger
                && t.len() > NUM_TRIGGER_STAGES
            {
                return Err(SrError::Err);
            }
        }
        Ok(())
    }

    /// dsl_fpga_arm()
    pub(super) fn fpga_arm(&mut self) -> SrResult<()> {
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
        let cur = self.cur_samplerate as f64;
        let mut div = match self.mode {
            Mode::Dso => c_u32((m.max_samplerate as f64 / cur / ch_num as f64).ceil()),
            Mode::Analog => c_u32(
                (m.hw_max_samplerate as f64 / self.cur_samplerate.max(m.hw_min_samplerate) as f64)
                    .ceil(),
            ),
            Mode::Logic => c_u32((m.hw_max_samplerate as f64 / cur).ceil()),
        };
        self.unit_pitch = c_u16((m.hw_min_samplerate as f64 / cur).ceil());
        let pre = u32::from(m.pre_div);
        s.div_h = ((if div >= pre {
            pre - 1
        } else {
            div.wrapping_sub(1)
        }) << 8) as u16;
        div = c_u32((f64::from(div) / f64::from(pre)).ceil());
        s.div_l = (div & 0xffff) as u16;
        s.div_h = s.div_h.wrapping_add((div >> 16) as u16);

        // capture counter (64-sample units)
        let per = u64::from(m.num) / ch_num;
        let cnt = if self.mode == Mode::Dso {
            self.actual_samples.checked_div(per).unwrap_or(0)
        } else {
            self.actual_samples
        } >> 4;
        s.cnt_l = (cnt & 0xffff) as u16;
        s.cnt_h = (cnt >> 16) as u16;
        let dso_cnt = if self.mode == Mode::Dso {
            self.limit_samples.checked_div(per).unwrap_or(0)
        } else {
            self.actual_samples
        };
        s.dso_cnt_l = (dso_cnt & 0xffff) as u16;
        s.dso_cnt_h = (dso_cnt >> 16) as u16;

        // trigger position, aligned to the parallel bits
        let mut tpos = c_u32(f64::from(t.trigger_pos) / 100.0 * self.limit_samples as f64)
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
    pub(super) fn start_transfers(&mut self) -> SrResult<()> {
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

    /// The start of an acquisition (dev_acquisition_start()'s first part,
    /// the same in both drivers).
    pub(super) fn reset_acquisition(&mut self) {
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
    }

    /// The header packet std_session_send_df_header() sends.
    pub(super) fn header_packet() -> Packet {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Packet::ok(Body::Header {
            feed_version: 1,
            start_sec: now.as_secs() as i64,
            start_usec: i64::from(now.subsec_micros()),
        })
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

    /// The DSO sample divider the device should report.
    pub(super) fn dso_divider(&self) -> u32 {
        if self.zero {
            1
        } else {
            let m = channel_mode(self.ch_mode);
            c_u32(
                (m.max_samplerate as f64 / self.cur_samplerate as f64 / self.en_ch_num() as f64)
                    .ceil(),
            )
        }
    }

    /// get_measure(): the status block behind a DSO frame, at `offset`
    /// 16-bit words. The field reads keep C's index arithmetic (each one
    /// counts in units of its own width).
    fn get_measure(&mut self, buf: &[u8], offset: u64) {
        let o = offset as usize;
        let b8 = |at: usize| buf.get(at).copied().unwrap_or(0);
        let w16 = |idx: usize| u16::from_le_bytes([b8(2 * idx), b8(2 * idx + 1)]);
        let w32 = |idx: usize| u32::from_le_bytes(std::array::from_fn(|k| b8(4 * idx + k)));
        let w64 = |idx: usize| u64::from_le_bytes(std::array::from_fn(|k| b8(8 * idx + k)));
        let m = &mut self.mstatus;
        m.pkt_id = w16(o);
        let v = w32(o / 2 + 1);
        m.vlen = v & 0x0fff_ffff;
        m.stream_mode = i32::from(v & 0x8000_0000 != 0);
        m.measure_valid = (v & 0x4000_0000) as i32;
        let v = w32(o / 2 + 2);
        m.sample_divider = v & 0x00ff_ffff;
        m.sample_divider_tog = i32::from(v & 0x8000_0000 != 0);
        m.trig_flag = i32::from(v & 0x4000_0000 != 0);
        m.trig_ch = (b8(o * 2 + 11) & 0x38) >> 3;
        m.trig_offset = b8(o * 2 + 11) & 0x07;

        m.ch0_max = b8(o * 2 + 66);
        m.ch0_min = b8(o * 2 + 67);
        m.ch0_cyc_cnt = w32(o / 2 + 17);
        m.ch0_cyc_tlen = w32(o / 2 + 18);
        m.ch0_cyc_plen = w32(o / 2 + 19);
        m.ch0_cyc_llen = w32(o / 2 + 20);
        m.ch0_level_valid = i32::from(w32(o / 2 + 21) & 0x8000 != 0);
        m.ch0_plevel = i32::from(w32(o / 2 + 21) & 0x4000 != 0);
        m.ch0_high_level = b8(o * 2 + 86);
        m.ch0_low_level = b8(o * 2 + 87);
        m.ch0_cyc_rlen = w32(o / 2 + 22);
        m.ch0_cyc_flen = w32(o / 2 + 23);
        m.ch0_acc_square = w64(o / 4 + 12) & 0x0000_FFFF_FFFF_FFFF;
        m.ch0_acc_mean = w32(o / 2 + 26);
        m.ch0_acc_mean_p1 = w32(o / 2 + 27);
        m.ch0_acc_mean_p2 = w32(o / 2 + 28);
        m.ch0_acc_mean_p3 = w32(o / 2 + 29);

        m.ch1_max = b8(o * 2 + 130);
        m.ch1_min = b8(o * 2 + 131);
        m.ch1_cyc_cnt = w32(o / 2 + 33);
        m.ch1_cyc_tlen = w32(o / 2 + 34);
        m.ch1_cyc_plen = w32(o / 2 + 35);
        m.ch1_cyc_llen = w32(o / 2 + 36);
        m.ch1_level_valid = i32::from(w32(o / 2 + 37) & 0x8000 != 0);
        m.ch1_plevel = i32::from(w32(o / 2 + 37) & 0x4000 != 0);
        m.ch1_high_level = b8(o * 2 + 150);
        m.ch1_low_level = b8(o * 2 + 151);
        m.ch1_cyc_rlen = w32(o / 2 + 38);
        m.ch1_cyc_flen = w32(o / 2 + 39);
        m.ch1_acc_square = w64(o / 4 + 20) & 0x0000_FFFF_FFFF_FFFF;
        m.ch1_acc_mean = w32(o / 2 + 42);
        m.ch1_acc_mean_p1 = w32(o / 2 + 43);
        m.ch1_acc_mean_p2 = w32(o / 2 + 44);
        m.ch1_acc_mean_p3 = w32(o / 2 + 45);

        if !self.zero_branch {
            m.ch0_acc_mean = m
                .ch0_acc_mean
                .wrapping_add(m.ch0_acc_mean_p1)
                .wrapping_add(m.ch0_acc_mean_p2)
                .wrapping_add(m.ch0_acc_mean_p3);
            m.ch1_acc_mean = m
                .ch1_acc_mean
                .wrapping_add(m.ch1_acc_mean_p1)
                .wrapping_add(m.ch1_acc_mean_p2)
                .wrapping_add(m.ch1_acc_mean_p3);
            if self.channels.iter().filter(|c| c.enabled).count() <= 1 {
                let sq = m.ch0_acc_square.wrapping_add(m.ch1_acc_square);
                m.ch0_acc_square = sq;
                m.ch1_acc_square = sq;
                let mean = u64::from(m.ch0_acc_mean) + u64::from(m.ch1_acc_mean);
                m.ch0_acc_mean = mean as u32;
                m.ch1_acc_mean = mean as u32;
            }
        }

        let divider = self.dso_divider();
        let m = &mut self.mstatus;
        self.mstatus_valid = if self.instant {
            m.pkt_id == DSO_PKTID
        } else {
            m.pkt_id == DSO_PKTID && m.sample_divider == divider && m.vlen != 0
        };
        if self.mstatus_valid {
            for p in &mut self.channels {
                p.hw_offset = u16::from(b8(o * 2 + (51 + 32 * p.index as usize) * 2));
            }
        }
    }

    /// receive_transfer()
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
            let m = channel_mode(self.ch_mode);
            let en = self.en_ch_num();
            let mut cur_samples = 0;
            // C's `logic.length`, only set in logic mode (F25)
            let mut length = 0;
            let mut packet: Option<Packet> = None;
            match self.mode {
                Mode::Logic => {
                    cur_samples = len * 8 / en;
                    length = len;
                }
                Mode::Dso => {
                    if !self.instant {
                        let offset = self.actual_samples / (u64::from(m.num) / en);
                        self.get_measure(&c.data, offset);
                    } else {
                        self.mstatus.vlen = (self.get_buffer_size() / usize::from(m.num)) as u32;
                        self.mstatus.trig_offset = 0;
                        self.mstatus.sample_divider_tog = 0;
                        self.mstatus_valid = true;
                    }
                    if self.mstatus_valid {
                        self.roll = self.mstatus.stream_mode != 0;
                        cur_samples = (u64::from(m.num) * u64::from(self.mstatus.vlen) / en)
                            .min(self.limit_samples);
                        let start = if self.zero {
                            0
                        } else {
                            2 * usize::from(self.mstatus.trig_offset)
                        };
                        // the readers take num_samples per enabled channel
                        // from the pointer; C hands out its transfer buffer,
                        // here the bytes past the transfer are zeros
                        let n = cur_samples as usize * en as usize;
                        let mut data = c.data.get(start..).unwrap_or_default().to_vec();
                        data.resize(n, 0);
                        packet = Some(Packet::ok(Body::Dso {
                            num_samples: cur_samples as i32,
                            mq: MQ_VOLTAGE,
                            unit: UNIT_VOLT,
                            mqflags: MQFLAG_AC,
                            samplerate_tog: self.mstatus.sample_divider_tog != 0,
                            trig_flag: self.mstatus.trig_flag != 0,
                            trig_ch: self.mstatus.trig_ch,
                            data,
                        }));
                    } else {
                        sr_dbg!(
                            "receive_transfer: invalid DSO packet dropped: {len} bytes, pkt_id 0x{:04X}",
                            self.mstatus.pkt_id
                        );
                    }
                }
                Mode::Analog => {
                    let unit = u64::from(m.unit_bits).div_ceil(8) * self.channels.len() as u64;
                    cur_samples = len.checked_div(unit).unwrap_or(0);
                    packet = Some(Packet::ok(Body::Analog {
                        num_samples: cur_samples as i32,
                        unit_bits: m.unit_bits,
                        unit_pitch: self.unit_pitch,
                        mq: MQ_VOLTAGE,
                        unit: UNIT_VOLT,
                        mqflags: MQFLAG_AC,
                        data: c.data.clone(),
                    }));
                }
            }
            if (self.limit_samples != 0 && (self.num_bytes < self.actual_bytes || self.is_loop))
                || self.mode != Mode::Logic
            {
                if !self.is_loop {
                    length = length.min(self.actual_bytes.wrapping_sub(self.num_bytes));
                }
                if self.mode == Mode::Logic {
                    packet = Some(Packet::ok(Body::Logic {
                        format: LA_CROSS_DATA,
                        index: 0,
                        order: 0,
                        unitsize: 0,
                        data_error: 0,
                        error_pattern: 0,
                        data: c.data[..length as usize].to_vec(),
                    }));
                }
                if let Some(p) = packet {
                    out.push(p);
                }
            }
            self.num_samples += cur_samples;
            self.num_bytes = self.num_bytes.wrapping_add(length);
            if self.mode == Mode::Logic
                && self.limit_samples != 0
                && !self.is_loop
                && self.num_bytes >= self.actual_bytes
            {
                self.state = State::Stop;
            } else if self.mode == Mode::Dso
                && self.instant
                && self.limit_samples != 0
                && self.num_samples >= self.actual_samples
            {
                let over = ((self.num_samples - self.actual_samples) * en) as i64;
                if over >= self.instant_tail_bytes {
                    let offset = (len as i64 - over) / 2;
                    self.get_measure(&c.data, offset as u64);
                    self.state = State::Stop;
                }
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
    pub(super) fn handle_events(&mut self, wait: Duration, out: &mut Vec<Packet>) -> bool {
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

    /// The I2C_STATUS progress read both drivers do on quiet transfers.
    pub(super) fn read_progress(&mut self) {
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

    /// The bookkeeping part of receive_data(), after the events.
    fn receive_data_tail(&mut self, out: &mut Vec<Packet>) -> Step {
        match self.family {
            Family::DsLogic => self.dslogic_receive_tail(out),
            Family::DsCope => self.dscope_receive_tail(),
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
    pub(super) fn acquisition_stop(&mut self) {
        if !self.abort {
            self.abort = true;
            if let Some(usb) = self.usb.as_deref_mut() {
                let _ = wr_reg(usb, CTR0_ADDR, BM_FORCE_RDY);
            }
            sr_info!("Send command:\"bmFORCE_RDY\"");
        } else if self.state == State::Finish {
            let adc_off = self.feature(feature::HMCAD1511);
            let Some(usb) = self.usb.as_deref_mut() else {
                return;
            };
            match ctl_wr(usb, dest::STOP, 0, &[]) {
                Ok(()) => sr_info!("dsl_dev_acquisition_stop: Sent acquisition stop command!"),
                Err(_) => {
                    sr_err!("dsl_dev_acquisition_stop: Sent acquisition stop command failed!")
                }
            }
            let _ = rd_reg(usb, HW_STATUS_ADDR);
            if adc_off {
                config_adc(usb, &adc::POWER_DOWN);
            }
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

    /// dsl_config_get(): the keys both drivers answer the same way. `None`
    /// when the key is not one of them.
    fn dsl_config_get(&self, id: i32, ch: Option<usize>) -> Option<SrResult<Option<ConfValue>>> {
        use ConfValue::*;
        let m = channel_mode(self.ch_mode);
        let caps = &self.profile.caps;
        let c = ch.and_then(|i| self.channels.get(i));
        let chan = || c.ok_or(SrError::Err);
        let v = (|| -> SrResult<Option<ConfValue>> {
            Ok(Some(match id {
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
                key::MAX_TIMEBASE => U64(10_000_000_000u64.min(
                    1_000_000_000 * caps.dso_depth / u64::from(m.num) / m.min_samplerate / 10,
                )),
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
                _ => return Err(SrError::Na),
            }))
        })();
        match v {
            Err(SrError::Na) => None,
            v => Some(v),
        }
    }

    /// dsl_config_set(): `None` when the key is not one of its keys.
    fn dsl_config_set(
        &mut self,
        id: i32,
        data: &ConfValue,
        ch: Option<usize>,
    ) -> Option<SrResult<()>> {
        fn chan(channels: &mut [Channel], ch: Option<usize>) -> SrResult<&mut Channel> {
            // C dereferences the channel unchecked (NULL crashes)
            ch.and_then(|i| channels.get_mut(i)).ok_or(SrError::Arg)
        }
        let span = |c: &Channel| (c.vdiv * c.vfactor * DS_CONF_DSO_VDIVS) as f64 / 2000.0;
        let channels = &mut self.channels;
        let zero_comb = &mut self.zero_comb;
        let r = (|| {
            match id {
                key::ZERO_COMB => {
                    *zero_comb = data.get_bool();
                }
                key::PROBE_MAP_DEFAULT => {
                    let c = chan(channels, ch)?;
                    c.map_default = data.get_bool();
                    if c.map_default {
                        c.map_unit = Some(PROBE_MAP_UNITS[0].to_owned());
                        c.map_min = -span(c);
                        c.map_max = span(c);
                    }
                }
                key::PROBE_MAP_UNIT => {
                    let c = chan(channels, ch)?;
                    c.map_unit = if c.map_default {
                        Some(PROBE_MAP_UNITS[0].to_owned())
                    } else {
                        data.get_str().map(str::to_owned)
                    };
                }
                key::PROBE_MAP_MIN => {
                    let c = chan(channels, ch)?;
                    c.map_min = if c.map_default {
                        -span(c)
                    } else {
                        data.get_f64()
                    };
                }
                key::PROBE_MAP_MAX => {
                    let c = chan(channels, ch)?;
                    c.map_max = if c.map_default {
                        span(c)
                    } else {
                        data.get_f64()
                    };
                }
                _ => return Err(SrError::Na),
            }
            Ok(())
        })();
        match r {
            Err(SrError::Na) => None,
            r => Some(r),
        }
    }

    /// dsl_config_list(): `None` when the key is not one of its keys.
    fn dsl_config_list(&self, id: i32) -> Option<ConfValue> {
        use ConfValue::*;
        let caps = &self.profile.caps;
        Some(match id {
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
            key::PROBE_COUPLING => Dict(vec![(
                "coupling".into(),
                Bytes(vec![DC_COUPLING, AC_COUPLING]),
            )]),
            key::PROBE_MAP_UNIT => StrV(PROBE_MAP_UNITS.iter().map(|s| s.to_string()).collect()),
            _ => return None,
        })
    }
}

impl Device for Dsl {
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
        // the handle stays open after a failure, as in C; close() releases it
        match self.family {
            Family::DsLogic => self.dslogic_open(),
            Family::DsCope => self.dscope_open(),
        }
    }

    fn close(&mut self) -> SrResult<()> {
        if self.family == Family::DsCope {
            self.dso_zero_reset();
        }
        self.dsl_dev_close()
    }

    fn config_get(&mut self, id: i32, ch: Option<usize>) -> SrResult<Option<ConfValue>> {
        // C falls through to the driver's own keys on any dsl_config_get()
        // failure, so a common key without its channel ends as SR_ERR_NA
        if let Some(Ok(v)) = self.dsl_config_get(id, ch) {
            return Ok(v);
        }
        match self.family {
            Family::DsLogic => self.dslogic_config_get(id, ch),
            Family::DsCope => self.dscope_config_get(id, ch),
        }
    }

    fn config_set(&mut self, id: i32, data: &ConfValue, ch: Option<usize>) -> SrResult<()> {
        if self.status != dev_status::ACTIVE {
            sr_err!("config_set: Device is not opened.");
            return Err(SrError::Err);
        }
        if let Some(r) = self.dsl_config_set(id, data, ch) {
            return r;
        }
        match self.family {
            Family::DsLogic => self.dslogic_config_set(id, data, ch),
            Family::DsCope => self.dscope_config_set(id, data, ch),
        }
    }

    fn config_list(&self, id: i32) -> SrResult<ConfValue> {
        if let Some(v) = self.dsl_config_list(id) {
            return Ok(v);
        }
        match self.family {
            Family::DsLogic => self.dslogic_config_list(id),
            Family::DsCope => self.dscope_config_list(id),
        }
    }

    /// dsl_dev_status_get()
    fn status(&self, prg: bool) -> SrResult<Status> {
        if prg || self.mstatus_valid {
            Ok(self.mstatus)
        } else {
            Err(SrError::Err)
        }
    }

    fn start(&mut self, out: &mut Vec<Packet>) -> SrResult<()> {
        match self.family {
            Family::DsLogic => self.dslogic_start(out),
            Family::DsCope => self.dscope_start(out),
        }
    }

    fn poll(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step {
        if self.transfers.is_empty() && self.state == State::Finish {
            return Step::Remove;
        }
        if !abort {
            return self.receive_data(out).unwrap_or(Step::Continue);
        }
        // the session's stop request: C (real USB fds, no dummy-source
        // callback) calls dev_acquisition_stop() right after the receive_data()
        // that saw it, then receive_data() until the device drained
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
