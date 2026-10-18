//! The DSLogic driver: libsigrok4DSL/hardware/DSL/dslogic.c, logic mode, on
//! the shared [`Dsl`] device. (dslogic.c also has DSO and DAQ branches; no
//! DSLogic profile offers those modes, so they are unreachable and not
//! ported: DEVIATIONS.md.)

use std::path::PathBuf;
use std::sync::Arc;

use dsview_core::{ConfValue, ListItem, Mode, Packet, SrError, SrResult, dev_status, key};
use dsview_usb::UsbBackend;

pub use super::device::{Candidate, Probed, last_error};
use super::device::{
    Dsl, FILTER_1T, FILTER_NONE, Family, State, TEST_INTERNAL, TEST_NONE, TH_3V3, TH_5V0,
};
use super::tables::{self, CHANNEL_MODES};
use super::*;
use crate::sys::{self, sr_dbg, sr_err, sr_info};

/// The DSLogic is the shared DSL device in its DSLogic flavour.
pub type DsLogic = Dsl;

const MAX_HEIGHTS: [&str; 5] = ["1X", "2X", "3X", "4X", "5X"];

/// `enum DSLOGIC_OPERATION_MODE`
pub const LO_OP_BUFFER: i16 = 0;
pub const LO_OP_STREAM: i16 = 1;
pub const LO_OP_INTEST: i16 = 2;
/// `SR_BUF_*`
const BUF_STOP: i16 = 0;
const BUF_UPLOAD: i16 = 1;

/// DSLogic devices on the bus (see [`super::device::scan`]).
pub fn scan(backend: &dyn UsbBackend) -> (Vec<Candidate>, bool) {
    super::device::scan(Family::DsLogic, backend)
}

/// Probes a DSLogic found by [`scan`].
pub fn probe(backend: Arc<dyn UsbBackend>, c: &Candidate) -> Probed {
    super::device::probe(Family::DsLogic, backend, c)
}

impl Dsl {
    /// A DSLogic (DSLogic_dev_new()).
    pub fn dslogic(
        profile: &'static Profile,
        backend: Arc<dyn UsbBackend>,
        bus: u8,
        address: u8,
    ) -> Dsl {
        Dsl::new(Family::DsLogic, profile, backend, bus, address)
    }

    /// DSLogic_dev_new()'s values that differ from DSCope_dev_new()'s.
    pub(super) fn dslogic_defaults(&mut self) {
        self.op_mode = LO_OP_STREAM;
        self.stream = true;
        self.buf_options = BUF_UPLOAD;
        self.vth = 1.0;
        self.ext_samplerate = 1_000_000;
    }

    /// dslogic.c dev_open()
    pub(super) fn dslogic_open(&mut self) -> SrResult<()> {
        self.dsl_dev_open()?;
        let code = self.vth_code();
        if wr_reg(self.usb()?, VTH_ADDR, code).is_err() {
            sr_err!("dev_open: Failed to call dsl_wr_reg()!");
            self.last_error = last_error::DEVICE_USB_IO_ERROR;
            return Err(SrError::Err);
        }
        if self.feature(feature::ADF4360) {
            config_adc(self.usb()?, &adc::CLK_INIT_500M);
        }
        Ok(())
    }

    /// The VTH register value for `vth`.
    fn vth_code(&self) -> u8 {
        if self.feature(feature::MAX25_VTH) {
            c_u8(self.vth / 3.3 * (1.0 / 2.0) * 255.0)
        } else {
            c_u8(self.vth / 3.3 * (1.5 / 2.5) * 255.0)
        }
    }

    /// dslogic.c receive_data() after the events: the progress poll while
    /// waiting for the trigger and the overflow poll in stream mode.
    pub(super) fn dslogic_receive_tail(&mut self, out: &mut Vec<Packet>) {
        if self.trf_completed {
            self.empty_poll_count = 0;
        } else {
            self.empty_poll_count += 1;
        }
        if self.empty_poll_count > MAX_EMPTY_POLL && self.state == State::Start {
            self.read_progress();
        }
        if self.stream && self.empty_poll_count > MAX_EMPTY_POLL {
            if let Some(usb) = self.usb.as_deref_mut() {
                match ctl_rd(usb, dest::HW_STATUS, 0, 1) {
                    Ok(v) => self.overflow = v[0] & BM_SYS_OVERFLOW != 0,
                    Err(_) => sr_err!("Failed to get hardware infos."),
                }
            }
            if self.overflow {
                out.push(Packet::ok(dsview_core::Body::Overflow));
            }
            self.empty_poll_count = 0;
        }
    }

    /// dslogic.c config_get() (after dsl_config_get())
    pub(super) fn dslogic_config_get(
        &mut self,
        id: i32,
        _ch: Option<usize>,
    ) -> SrResult<Option<ConfValue>> {
        use ConfValue::*;
        let m = channel_mode(self.ch_mode);
        let caps = &self.profile.caps;
        let v = match id {
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

    /// dslogic.c config_set() (after dsl_config_set())
    pub(super) fn dslogic_config_set(
        &mut self,
        id: i32,
        data: &ConfValue,
        ch: Option<usize>,
    ) -> SrResult<()> {
        fn chan(s: &mut Dsl, ch: Option<usize>) -> SrResult<&mut dsview_core::Channel> {
            // C dereferences the channel unchecked (NULL crashes)
            ch.and_then(|i| s.channels.get_mut(i)).ok_or(SrError::Arg)
        }
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
                    // the product is integer arithmetic in C
                    let p = (u64::from(self.trigger_hrate) * self.en_ch_num())
                        .wrapping_mul(self.limit_samples);
                    self.trigger_hpos = c_u32(p as f64 / 200.0);
                } else {
                    let p = u64::from(data.get_byte()).wrapping_mul(self.limit_samples);
                    self.trigger_hpos = c_u32(p as f64 / 100.0);
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
                    // no DSLogic profile offers DSO or DAQ mode
                    sr_err!("config_set: the DSLogic has no mode {}.", data.get_i16());
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
                    let path: PathBuf = sys::res_dir().join(bit);
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

    /// dslogic.c config_list() (after dsl_config_list())
    pub(super) fn dslogic_config_list(&self, id: i32) -> SrResult<ConfValue> {
        use ConfValue::*;
        let caps = &self.profile.caps;
        let list = ConfValue::List;
        Ok(match id {
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
            key::OPERATION_MODE => list(tables::opmode_list()),
            key::BUFFER_OPTIONS => list(tables::bufoption_list()),
            key::CHANNEL_MODE => list(
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
            key::THRESHOLD => list(tables::threshold_list()),
            key::FILTER => list(tables::filter_list()),
            key::MAX_HEIGHT => StrV(MAX_HEIGHTS.iter().map(|s| s.to_string()).collect()),
            _ => return Err(SrError::Na),
        })
    }

    /// dslogic.c dev_acquisition_start()
    pub(super) fn dslogic_start(&mut self, out: &mut Vec<Packet>) -> SrResult<()> {
        if self.status != dev_status::ACTIVE {
            self.last_error = last_error::DEVICE_CLOSED;
            return Err(SrError::DeviceClosed);
        }
        if self.mode != Mode::Logic {
            sr_err!("dev_acquisition_start: the DSLogic captures in logic mode only.");
            return Err(SrError::Err);
        }
        self.reset_acquisition();
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
        out.push(Dsl::header_packet());
        Ok(())
    }
}
