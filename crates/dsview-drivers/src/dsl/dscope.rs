//! The DSCope driver: libsigrok4DSL/hardware/DSL/dscope.c on the shared
//! [`Dsl`] device. Oscilloscope (DSO) and data acquisition (DAQ) modes, the
//! calibration stored in the device's EEPROM and the auto calibration loop;
//! also the Hermelin MSO-E8 (DSO_E8-1), which adds a logic mode.
//!
//! dso_zero()'s function statics are per device here ([`ZeroCal`]); C shares
//! them between all DSCopes. dso_tune() is not ported: nothing ever sets
//! `devc->tune` (DEVIATIONS.md).

use std::sync::Arc;

use dsview_core::{Channel, ConfValue, ListItem, Mode, Packet, SrError, SrResult, dev_status, key};
use dsview_usb::UsbBackend;

use super::device::{
    Candidate, DC_COUPLING, Dsl, Family, GND_COUPLING, Probed, State, TEST_INTERNAL, TEST_NONE,
    last_error,
};
use super::tables::CHANNEL_MODES;
use super::*;
use crate::sys::{sr_dbg, sr_err, sr_info};

/// The DSCope is the shared DSL device in its DSCope flavour.
pub type DsCope = Dsl;

/// `enum DSCOPE_OPERATION_MODE`
pub const DS_OP_NORMAL: i16 = 0;
pub const DS_OP_INTEST: i16 = 1;
/// Bandwidth limit
pub const BW_FULL: i16 = 0;
pub const BW_20M: i16 = 1;

const ZERO_BASE_ADDR: u8 = 0x40;
const ZERO_BIG_ADDR: u16 = 0x20;
/// `sizeof(struct cmd_zero_info)`, `sizeof(struct cmd_vga_info)`
const ZERO_INFO_SIZE: usize = 30;
const VGA_INFO_SIZE: usize = 17;

const HWOPTIONS: [i32; 2] = [key::OPERATION_MODE, key::BANDWIDTH_LIMIT];
const SESSIONS_DSO: [i32; 9] = [
    key::OPERATION_MODE,
    key::BANDWIDTH_LIMIT,
    key::TIMEBASE,
    key::TRIGGER_SLOPE,
    key::TRIGGER_SOURCE,
    key::TRIGGER_CHANNEL,
    key::HORIZ_TRIGGERPOS,
    key::TRIGGER_HOLDOFF,
    key::TRIGGER_MARGIN,
];
const SESSIONS_DAQ: [i32; 11] = [
    key::SAMPLERATE,
    key::LIMIT_SAMPLES,
    key::OPERATION_MODE,
    key::BANDWIDTH_LIMIT,
    key::TIMEBASE,
    key::TRIGGER_SLOPE,
    key::TRIGGER_SOURCE,
    key::TRIGGER_CHANNEL,
    key::HORIZ_TRIGGERPOS,
    key::TRIGGER_HOLDOFF,
    key::TRIGGER_MARGIN,
];
const HWOPTIONS_LOGIC: [i32; 1] = [key::VTH];
const SESSIONS_LOGIC: [i32; 3] = [key::SAMPLERATE, key::LIMIT_SAMPLES, key::VTH];

fn opmode_list() -> Vec<ListItem> {
    vec![
        ListItem {
            id: i32::from(DS_OP_NORMAL),
            name: "Normal".into(),
        },
        ListItem {
            id: i32::from(DS_OP_INTEST),
            name: "Internal Test".into(),
        },
    ]
}

fn bandwidth_list() -> Vec<ListItem> {
    vec![
        ListItem {
            id: i32::from(BW_FULL),
            name: "Full Bandwidth".into(),
        },
        ListItem {
            id: i32::from(BW_20M),
            name: "20MHz".into(),
        },
    ]
}

/// The auto calibration's progress that dso_zero() keeps in statics.
#[derive(Clone, Copy, Debug, Default)]
pub struct ZeroCal {
    warm_done: bool,
    trans_fix_done: bool,
    mid_zero_done: bool,
    margin: [f64; 2],
    vdiv_back: [u64; 2],
}

/// DSCope devices on the bus (see [`super::device::scan`]).
pub fn scan(backend: &dyn UsbBackend) -> (Vec<Candidate>, bool) {
    super::device::scan(Family::DsCope, backend)
}

/// Probes a DSCope found by [`scan`].
pub fn probe(backend: Arc<dyn UsbBackend>, c: &Candidate) -> Probed {
    super::device::probe(Family::DsCope, backend, c)
}

/// `x << n` on an `int` as x86 computes it (the count masked to 5 bits),
/// then widened to `uint64_t` (sign-extended) as C adds it to a command.
fn int_shl(x: i32, n: u32) -> u64 {
    x.wrapping_shl(n) as i64 as u64
}

/// `ceil` for positive, `floor` for other margins (C's rounding away from 0).
fn round_out(m: f64) -> f64 {
    if m > 0.0 { m.ceil() } else { m.floor() }
}

impl Dsl {
    /// A DSCope (DSCope_dev_new()).
    pub fn dscope(
        profile: &'static Profile,
        backend: Arc<dyn UsbBackend>,
        bus: u8,
        address: u8,
    ) -> Dsl {
        Dsl::new(Family::DsCope, profile, backend, bus, address)
    }

    /// DSCope_dev_new()'s values that differ from the shared defaults.
    pub(super) fn dscope_defaults(&mut self) {
        self.op_mode = DS_OP_NORMAL;
        self.stream = false;
        self.bw_limit = BW_FULL;
    }

    fn vga_of(&self, ch: usize) -> &[Vga] {
        let v = self.vga.get(ch).and_then(Option::as_deref).unwrap_or(&[]);
        // C walks the table up to its id-0 terminator
        let n = v.iter().take_while(|e| e.id != 0).count();
        &v[..n]
    }

    /// get_default_preoff()
    fn get_default_preoff(&self, ch: usize) -> u16 {
        let c = &self.channels[ch];
        match vga_default(self.profile.caps.vga_id, c.vdiv) {
            Some(d) if c.index == 1 => d.preoff_comp,
            Some(d) => d.preoff,
            None => 0,
        }
    }

    /// dso_vga()
    fn dso_vga(&self, ch: usize) -> u64 {
        let vdiv = self.channels[ch].vdiv;
        self.vga_of(ch)
            .iter()
            .find(|v| v.key == vdiv)
            .map_or(0, |v| v.vgain)
    }

    /// dso_preoff()
    fn dso_preoff(&self, ch: usize) -> u64 {
        let vdiv = self.channels[ch].vdiv;
        self.vga_of(ch)
            .iter()
            .find(|v| v.key == vdiv)
            .map_or(0, |v| u64::from(v.preoff))
    }

    /// dso_offset(): the offset word of the PROBE_OFFSET command.
    fn dso_offset(&self, ch: usize) -> u64 {
        let c = &self.channels[ch];
        // `1 << (bits - 1)` on an int: channels added in the E8's logic mode
        // have bits 0, where x86 masks the shift count (1 << 31)
        let offset_mid = f64::from(1i32.wrapping_shl(u32::from(c.bits).wrapping_sub(1)));
        let offset_max = f64::from(1i32.wrapping_shl(u32::from(c.bits))) - 1.0;
        let offset = u64::from(if self.zero { c.zero_offset } else { c.offset });
        let comb = (10f64).powf(24.0 * f64::from(c.comb_comp) / 20.0 / 4096.0) - 1.0;
        let comb_off = if self.feature(feature::HMCAD1511) {
            0.57 / comb
        } else {
            2.0 / comb
        };
        let comb_compensate = if c.comb_comp != 0 && self.en_ch_num() == 1 {
            (offset as f64 - offset_mid) / comb_off
        } else {
            0.0
        };
        let preoff = self.dso_preoff(ch);
        if self.feature(feature::PREOFF) {
            let trans_coarse = i32::from((c.vpos_trans & 0xFF00) >> 8);
            let trans_fine = i32::from(c.vpos_trans & 0x00FF);
            let voltage = (offset_mid - offset as f64) / offset_max
                * c.vdiv as f64
                * DS_CONF_DSO_VDIVS as f64;
            let (coarse, fine);
            if c.vdiv < 500 {
                coarse = c_i32(
                    (-voltage * f64::from(DSCOPE_TRANS_CMULTI) / f64::from(trans_coarse) + 0.5)
                        .floor(),
                );
                // integer arithmetic in C
                let back = coarse.wrapping_mul(trans_coarse) / DSCOPE_TRANS_CMULTI;
                fine = c_i32(
                    ((voltage + f64::from(back)) * 1000.0 / f64::from(trans_fine) + 0.5).floor(),
                );
            } else {
                coarse = c_i32((-voltage / f64::from(trans_coarse) + 0.5).floor());
                let back = coarse.wrapping_mul(trans_coarse);
                fine = c_i32(
                    ((voltage + f64::from(back)) * DSCOPE_TRANS_FMULTI / f64::from(trans_fine)
                        + 0.5)
                        .floor(),
                );
            }
            (offset << 32)
                .wrapping_add(
                    (i64::from(coarse) as u64)
                        .wrapping_add(DSCOPE_CONSTANT_BIAS as u64)
                        .wrapping_add(preoff >> 10)
                        << 16,
                )
                .wrapping_add(i64::from(fine) as u64)
                .wrapping_add(preoff & 0x03ff)
        } else {
            let pwm_off =
                c_u64((offset as f64 + comb_compensate) / offset_max * f64::from(c.vpos_trans));
            (offset << 32).wrapping_add(pwm_off).wrapping_add(preoff)
        }
    }

    /// e8_set_attenuator(): DSO_E8-1's input attenuator follows the probe
    /// factor (channel 0 only); nothing on other devices.
    fn e8_set_attenuator(&mut self, ch: usize) -> SrResult<()> {
        if !self.is_e8() || self.channels[ch].index != 0 {
            return Ok(());
        }
        let f = self.channels[ch].vfactor;
        let code = if f >= 100 {
            2
        } else if f >= 10 {
            1
        } else {
            0
        };
        self.cmd(|u| ctl_wr(u, dest::DSO_ATT0, 0, &[code]))
    }

    /// dso_cmd_gen()
    fn dso_cmd_gen(&self, ch: Option<usize>, id: i32) -> u64 {
        const CH_BIT: u32 = 7;
        let c = ch.map(|i| &self.channels[i]);
        let en = self.en_ch_num();
        let mut cmd: u64 = 0;
        match id {
            key::PROBE_EN | key::PROBE_COUPLING => {
                let c = c.expect("channel command without a channel");
                if self.mode == Mode::Analog || en == 2 {
                    cmd += 0x0E00;
                } else if en == 1 {
                    if (c.index == 0 && c.enabled) || (c.index == 1 && !c.enabled) {
                        cmd += 0x1600;
                    } else if (c.index == 1 && c.enabled) || (c.index == 0 && !c.enabled) {
                        cmd += 0x1A00;
                    }
                } else {
                    return 0;
                }
                cmd = cmd.wrapping_add(int_shl(i32::from(c.index), CH_BIT));
                if self.zero || c.coupling == DC_COUPLING {
                    cmd = cmd.wrapping_add(0x100);
                } else if c.coupling == GND_COUPLING {
                    cmd &= 0xFFFF_FDFF;
                }
            }
            key::PROBE_VDIV | key::TIMEBASE => {
                let i = ch.expect("channel command without a channel");
                let c = &self.channels[i];
                cmd += 0x8;
                cmd = cmd.wrapping_add(int_shl(i32::from(c.index), CH_BIT));
                let mut vgain = self.dso_vga(i);
                if c.comb_comp != 0 && en == 1 && !self.is_e8() {
                    vgain = vgain.wrapping_add((i64::from(c.comb_comp) as u64) << 8);
                }
                cmd = cmd.wrapping_add(vgain);
            }
            key::PROBE_OFFSET => {
                let i = ch.expect("channel command without a channel");
                cmd += 0x10;
                cmd = cmd.wrapping_add(int_shl(i32::from(self.channels[i].index), CH_BIT));
                cmd = cmd.wrapping_add(self.dso_offset(i) << 8);
            }
            key::SAMPLERATE => {
                cmd += 0x18;
                cmd += u64::from(self.dso_divider() << 8);
            }
            key::HORIZ_TRIGGERPOS => {
                cmd += 0x20;
                cmd += u64::from(self.trigger_hpos) << 8;
            }
            key::TRIGGER_SLOPE => {
                cmd += 0x28;
                cmd += u64::from(self.trigger_slope) << 8;
            }
            key::TRIGGER_SOURCE => {
                cmd += 0x30;
                if !self.zero {
                    cmd += u64::from(self.trigger_source) << 8;
                }
            }
            key::TRIGGER_VALUE => {
                cmd += 0x38;
                for p in &self.channels {
                    cmd = cmd.wrapping_add(int_shl(
                        i32::from(p.trig_value),
                        8 * (u32::from(p.index) + 1),
                    ));
                }
            }
            key::TRIGGER_MARGIN => {
                cmd += 0x40;
                cmd += u64::from(self.trigger_margin) << 8;
            }
            key::TRIGGER_HOLDOFF => {
                cmd += 0x58;
                cmd = cmd.wrapping_add(self.trigger_holdoff << 8);
            }
            key::DSO_SYNC => cmd = 0xa5a5_a500,
            _ => cmd = 0xFFFF_FFFF,
        }
        cmd
    }

    /// dsl_wr_dso(dso_cmd_gen())
    fn wr_dso_cmd(&mut self, ch: Option<usize>, id: i32) -> SrResult<()> {
        let cmd = self.dso_cmd_gen(ch, id);
        self.cmd(|u| wr_dso(u, cmd))
    }

    /// One byte to a DSO channel's enable register.
    fn wr_ch_en(&mut self, dest: u8, value: u8) -> SrResult<()> {
        self.cmd(|u| ctl_wr(u, dest, 0, &[value]))
    }

    fn channel_index(&self, index: u16) -> Option<usize> {
        self.channels.iter().position(|c| c.index == index)
    }

    /// dso_init()
    fn dso_init(&mut self) -> SrResult<()> {
        for i in 0..self.channels.len() {
            let idx = self.channels[i].index;
            if self.wr_dso_cmd(Some(i), key::PROBE_COUPLING).is_err() {
                sr_err!("DSO set coupling of channel {idx} command failed!");
                return Err(SrError::Err);
            }
            if self.wr_dso_cmd(Some(i), key::PROBE_VDIV).is_err() {
                sr_err!("Set VDIV of channel {idx} command failed!");
                return Err(SrError::Err);
            }
            if self.wr_dso_cmd(Some(i), key::PROBE_OFFSET).is_err() {
                sr_err!("Set OFFSET of channel {idx} command failed!");
                return Err(SrError::Err);
            }
        }
        for (id, what) in [
            (key::SAMPLERATE, "Set Sample Rate"),
            (key::HORIZ_TRIGGERPOS, "Set Horiz Trigger Position"),
            (key::TRIGGER_HOLDOFF, "Set Trigger Holdoff Time"),
            (key::TRIGGER_SLOPE, "Set Trigger Slope"),
            (key::TRIGGER_SOURCE, "Set Trigger Source"),
            (key::TRIGGER_VALUE, "Set Trigger Value"),
            (key::TRIGGER_MARGIN, "Set Trigger Margin"),
        ] {
            if self.wr_dso_cmd(None, id).is_err() {
                sr_err!("{what} command failed!");
                return Err(SrError::Err);
            }
        }
        Ok(())
    }

    /// Where a channel's calibration block (`second`: its VGA block) lives
    /// in the NVM, and the address byte it starts with.
    fn zero_addr(&self, index: u16, second: bool) -> (u16, usize) {
        let base =
            usize::from(ZERO_BASE_ADDR) + usize::from(index) * (ZERO_INFO_SIZE + VGA_INFO_SIZE);
        // C keeps the address bytes in uint8_t fields
        let zero_addr = base as u8;
        let vga_addr = zero_addr.wrapping_add(ZERO_INFO_SIZE as u8);
        let (addr, flash_page) = if second {
            (vga_addr, 1)
        } else {
            (zero_addr, 0)
        };
        let real = if self.feature(feature::SEEP) {
            u16::from(addr)
        } else if self.feature(feature::FLASH) {
            index.wrapping_mul(DSO_ZERO_PAGE).wrapping_add(flash_page)
        } else {
            (ZERO_BIG_ADDR << 8) + u16::from(addr)
        };
        // the read-back check: the u8 address, plus the size for the VGA block
        let expect = usize::from(zero_addr) + if second { ZERO_INFO_SIZE } else { 0 };
        (real, expect)
    }

    /// The COMB look-up table written from a channel's comb_diff values;
    /// the result of the last register write.
    fn write_comb_table(&mut self, ch: usize) -> SrResult<()> {
        let c = &self.channels[ch];
        let (top, bom, idx) = (c.comb_diff_top, c.comb_diff_bom, c.index);
        let slope = f64::from(i32::from(bom) - i32::from(top)) / (2.0 * 255.0);
        let reg = COMB_ADDR.wrapping_add((idx * 2) as u8);
        let mut ret = Ok(());
        for i in 0..256i32 {
            let _ = self.cmd(|u| wr_reg(u, reg, i as u8));
            let value = c_i32(f64::from(i) + f64::from(i) * slope + f64::from(top) * 0.5 + 0.5)
                .clamp(0, 255);
            ret = self.cmd(|u| wr_reg(u, reg.wrapping_add(1), value as u8));
        }
        ret
    }

    /// dso_load_eep(): the calibration stored in the device. C reads the
    /// 30-byte block into a struct the parse overruns by two bytes (the
    /// last two combined fine gains); those read as 0 here (F24).
    fn dso_load_eep(&mut self, ch: usize, fpga_done: bool) -> bool {
        let idx = self.channels[ch].index;
        let (real, expect) = self.zero_addr(idx, false);
        let Ok(zi) = self.cmd(|u| rd_nvm(u, real, ZERO_INFO_SIZE as u8)) else {
            return false;
        };
        if usize::from(zi[0]) != expect {
            return false;
        }
        let p = |k: usize| zi.get(1 + k).copied().unwrap_or(0);
        let n = self.vga_of(ch).len();
        if let Some(vga) = self.vga.get_mut(ch).and_then(Option::as_mut) {
            for (i, v) in vga.iter_mut().take(n).enumerate() {
                v.preoff = (u16::from(p(2 * i + 1)) << 8) + u16::from(p(2 * i));
            }
        }
        if n != 0 {
            let i = n;
            let c = &mut self.channels[ch];
            c.comb_diff_top = p(2 * i) as i8;
            c.comb_diff_bom = p(2 * i + 1) as i8;
            c.vpos_trans = u16::from(p(2 * i + 2)) + (u16::from(p(2 * i + 3)) << 8);
            c.comb_comp = p(2 * i + 4) as i8;
            c.digi_fgain = u16::from(p(2 * i + 5)) + (u16::from(p(2 * i + 6)) << 8);
            for k in 0..4 {
                c.cali_fgain[k] = adc_code2fgain(p(2 * i + 7 + k));
                c.cali_comb_fgain[k] = adc_code2fgain(p(2 * i + 11 + k));
            }
            if !fpga_done {
                let _ = self.write_comb_table(ch);
            }
        }

        let (real, expect) = self.zero_addr(idx, true);
        let Ok(vi) = self.cmd(|u| rd_nvm(u, real, VGA_INFO_SIZE as u8)) else {
            return false;
        };
        if usize::from(vi[0]) != expect {
            return false;
        }
        if let Some(vga) = self.vga.get_mut(ch).and_then(Option::as_mut) {
            for (i, v) in vga.iter_mut().take(n).enumerate() {
                let g = u16::from_le_bytes([vi[1 + 2 * i], vi[2 + 2 * i]]);
                v.vgain = u64::from(g) << 8;
            }
        }
        true
    }

    /// SR_CONF_ZERO_SET: stores the calibration in the device (the blocks
    /// C sends; the two bytes C's struct cannot hold are not sent there
    /// either, F24).
    #[allow(unused_assignments)]
    fn dso_zero_set(&mut self) -> SrResult<()> {
        let mut ret = Ok(());
        for ch in 0..self.channels.len() {
            let n = self.vga_of(ch).len();
            if n == 0 {
                continue;
            }
            let c = self.channels[ch].clone();
            let (_, zero_base) = self.zero_addr(c.index, false);
            let mut zi = [0u8; ZERO_INFO_SIZE];
            zi[0] = zero_base as u8;
            let mut put = |k: usize, b: u8| {
                if let Some(slot) = zi.get_mut(1 + k) {
                    *slot = b;
                }
            };
            let vga = self.vga_of(ch).to_vec();
            for (i, v) in vga.iter().enumerate() {
                put(2 * i, v.preoff as u8);
                put(2 * i + 1, (v.preoff >> 8) as u8);
            }
            let i = n;
            put(2 * i, c.comb_diff_top as u8);
            put(2 * i + 1, c.comb_diff_bom as u8);
            put(2 * i + 2, c.vpos_trans as u8);
            put(2 * i + 3, (c.vpos_trans >> 8) as u8);
            put(2 * i + 4, c.comb_comp as u8);
            put(2 * i + 5, c.digi_fgain as u8);
            put(2 * i + 6, (c.digi_fgain >> 8) as u8);
            for k in 0..4 {
                put(2 * i + 7 + k, adc_fgain2code(c.cali_fgain[k]));
                put(2 * i + 11 + k, adc_fgain2code(c.cali_comb_fgain[k]));
            }
            let mut vi = [0u8; VGA_INFO_SIZE];
            vi[0] = zi[0].wrapping_add(ZERO_INFO_SIZE as u8);
            for (i, v) in vga.iter().enumerate().take(8) {
                let g = (v.vgain >> 8) as u16;
                vi[1 + 2 * i..3 + 2 * i].copy_from_slice(&g.to_le_bytes());
            }
            ret = self.cmd(|u| wr_reg(u, CTR0_ADDR, BM_EEWP));
            if ret.is_ok() {
                let (real, _) = self.zero_addr(c.index, false);
                ret = self.cmd(|u| wr_nvm(u, &zi, real));
            }
            if ret.is_ok() {
                let (real, _) = self.zero_addr(c.index, true);
                ret = self.cmd(|u| wr_nvm(u, &vi, real));
            }
            ret = self.cmd(|u| wr_reg(u, CTR0_ADDR, BM_NONE));
            if !self.feature(feature::HMCAD1511) {
                ret = self.write_comb_table(ch);
            }
        }
        ret
    }

    /// dsl_probe_cali_fgain()
    fn probe_cali_fgain(&mut self, ch: usize, mean: f64, comb: bool, reset: bool) {
        const UPGAIN: f64 = 1.0077;
        const DNGAIN: f64 = 0.9923;
        const IGNORE_RATIO: f64 = 2.0;
        const RATIO: f64 = 2.0;
        let m = self.mstatus;
        let c = &mut self.channels[ch];
        let gains = if comb {
            &mut c.cali_comb_fgain
        } else {
            &mut c.cali_fgain
        };
        if reset {
            *gains = [1.0; 4];
            return;
        }
        // which accumulated mean corrects which gain (see dsl.c)
        let src: [u32; 4] = match (comb, c.index == 0) {
            (true, true) => [
                m.ch0_acc_mean,
                m.ch0_acc_mean_p1,
                m.ch1_acc_mean_p2,
                m.ch1_acc_mean_p3,
            ],
            (true, false) => [
                m.ch1_acc_mean_p1,
                m.ch1_acc_mean,
                m.ch0_acc_mean_p3,
                m.ch0_acc_mean_p2,
            ],
            (false, true) => [
                m.ch0_acc_mean,
                m.ch0_acc_mean_p2,
                m.ch0_acc_mean_p1,
                m.ch0_acc_mean_p3,
            ],
            (false, false) => [
                m.ch1_acc_mean,
                m.ch1_acc_mean_p2,
                m.ch1_acc_mean_p1,
                m.ch1_acc_mean_p3,
            ],
        };
        for (g, s) in gains.iter_mut().zip(src) {
            let drift = (f64::from(s) / mean - 1.0) / RATIO;
            if drift.abs() > MAX_ACC_VARIANCE / IGNORE_RATIO {
                *g /= 1.0 + drift;
            }
        }
        for g in gains.iter_mut() {
            // max(min(g, UP), DN) with C's macros
            let lo = if *g < UPGAIN { *g } else { UPGAIN };
            *g = if lo > DNGAIN { lo } else { DNGAIN };
        }
    }

    /// dsl_probe_fgain_inrange()
    fn probe_fgain_inrange(&self, ch: usize, comb: bool, skew: &[f64; 8]) -> bool {
        const UPGAIN: f64 = 1.0077;
        const DNGAIN: f64 = 0.9923;
        let c = &self.channels[ch];
        let (gains, order) = match (comb, c.index == 0) {
            (true, true) => (&c.cali_comb_fgain, [0, 1, 6, 7]),
            (true, false) => (&c.cali_comb_fgain, [5, 4, 3, 2]),
            (false, true) => (&c.cali_fgain, [0, 2, 1, 3]),
            (false, false) => (&c.cali_fgain, [4, 6, 5, 7]),
        };
        gains
            .iter()
            .zip(order)
            .any(|(&g, k)| skew[k].abs() > MAX_ACC_VARIANCE && g > DNGAIN && g < UPGAIN)
    }

    /// dsl_config_fpga_fgain()
    fn config_fpga_fgain(&mut self) {
        for i in 0..self.channels.len() {
            let (idx, g) = (self.channels[i].index, self.channels[i].digi_fgain);
            let base = match idx {
                0 => ADCC_ADDR + 3,
                1 => ADCC_ADDR + 5,
                _ => continue,
            };
            let _ = self.cmd(|u| wr_reg(u, base, g as u8));
            let _ = self.cmd(|u| wr_reg(u, base + 1, (g >> 8) as u8));
        }
    }

    /// dsl_skew_fpga_fgain()
    fn skew_fpga_fgain(&mut self, comb: bool, skew: &[f64; 8]) {
        let bits = |f: &dyn Fn(f64) -> bool| {
            (0..8).fold(0u8, |acc, i| acc.wrapping_add(u8::from(f(skew[i])) << i))
        };
        let up = bits(&|s| -s > 1.6 * MAX_ACC_VARIANCE);
        let dn = bits(&|s| s > 1.6 * MAX_ACC_VARIANCE);
        for (index, mut v, regs) in [(0u16, up, [3u8, 4]), (1, dn, [5, 6])] {
            if let Some(i) = self.channel_index(index) {
                let g = &mut self.channels[i].digi_fgain;
                if comb {
                    *g |= (*g & 0xFF00).wrapping_add(u16::from(v));
                    v = *g as u8;
                } else {
                    *g |= (u16::from(v) << 8).wrapping_add(*g & 0x00FF);
                    v = (*g >> 8) as u8;
                }
            }
            let reg = ADCC_ADDR + if comb { regs[0] } else { regs[1] };
            let _ = self.cmd(|u| wr_reg(u, reg, v));
        }
    }

    /// The four dsl_config_adc_fgain() calls with each channel's gains.
    fn config_adc_fgains(&mut self, p0: usize, p1: usize, comb: bool) {
        let g = |c: &Channel| {
            if comb {
                c.cali_comb_fgain
            } else {
                c.cali_fgain
            }
        };
        let (a, b) = (g(&self.channels[p0]), g(&self.channels[p1]));
        let _ = self.cmd(|u| {
            config_adc_fgain(u, 0, a[0], a[1]);
            config_adc_fgain(u, 1, a[2], a[3]);
            config_adc_fgain(u, 2, b[0], b[1]);
            config_adc_fgain(u, 3, b[2], b[3]);
            Ok(())
        });
    }

    /// The 8 skews of the four accumulators of each channel against `mean`
    /// and the largest of them.
    fn acc_skews(&self, mean0: f64, mean1: f64) -> ([f64; 8], f64) {
        let m = &self.mstatus;
        let v = [
            (m.ch0_acc_mean, mean0),
            (m.ch0_acc_mean_p1, mean0),
            (m.ch0_acc_mean_p2, mean0),
            (m.ch0_acc_mean_p3, mean0),
            (m.ch1_acc_mean, mean1),
            (m.ch1_acc_mean_p1, mean1),
            (m.ch1_acc_mean_p2, mean1),
            (m.ch1_acc_mean_p3, mean1),
        ];
        let skew: [f64; 8] = std::array::from_fn(|i| f64::from(v[i].0) / v[i].1 - 1.0);
        let mut max = skew[0].abs();
        for s in &skew[1..] {
            // C's max(): ((a) > (b) ? (a) : (b))
            max = if max > s.abs() { max } else { s.abs() };
        }
        (skew, max)
    }

    /// dso_zero(sdi, TRUE): forgets the calibration's progress (a device
    /// that calibrates itself ends the calibration instead, as in C).
    pub(super) fn dso_zero_reset(&mut self) {
        if self.feature(feature::SELF_ZERO) {
            let _ = self.dso_zero();
        } else {
            self.zcal = ZeroCal::default();
        }
    }

    /// dso_zero(): one step of the auto calibration, run per completed
    /// transfer while `zero` is set. (The result keeps C's last-write-wins
    /// `ret`.)
    #[allow(unused_assignments)]
    fn dso_zero(&mut self) -> SrResult<()> {
        if self.feature(feature::SELF_ZERO) {
            // the device calibrated itself: the dialog waits for ZERO to clear
            self.zero = false;
            self.zero_stage = -1;
            self.zero_pcnt = 0;
            self.zero_branch = false;
            self.zero_comb_fgain = false;
            self.zero_comb = false;
            sr_info!("dso_zero: device self-calibrates, zero loop skipped");
            return Ok(());
        }
        if !self.mstatus_valid {
            return Err(SrError::Arg);
        }
        let bits = channel_mode(self.ch_mode).unit_bits;
        let hmcad = self.feature(feature::HMCAD1511);
        let offset_top: u16 = if hmcad { 15 } else { 30 };
        let offset_bom: u16 = ((1u16 << bits) - 1).wrapping_sub(offset_top);
        let offset_mid: u16 = 1 << (bits - 1);
        let max_trans: f64 = 1023.0;
        let interval: i32 = 50;
        let margin_pass = 0.3;
        let branch_done = if self.profile.speed == Speed::Super {
            10
        } else {
            2
        };
        let limit = self.limit_samples as f64;
        let mut end_cnt = 0;
        let mut ret: SrResult<()> = Ok(());

        let (Some(p0), Some(p1)) = (self.channel_index(0), self.channel_index(1)) else {
            // C dereferences the missing channel
            return Err(SrError::Err);
        };
        for c in &self.channels {
            let i = usize::from(c.index);
            if i < 2 && self.zcal.vdiv_back[i] == 0 {
                self.zcal.vdiv_back[i] = c.vdiv;
            }
        }
        let n = self.channels.len();
        let offset_all = |s: &mut Dsl, value: u16| -> SrResult<()> {
            let mut r = Ok(());
            for i in 0..n {
                s.channels[i].zero_offset = value;
                r = s.wr_dso_cmd(Some(i), key::PROBE_OFFSET);
            }
            r
        };

        if !self.zcal.trans_fix_done && self.zero_stage == 0 {
            ret = Ok(());
            if !self.zcal.warm_done {
                if self.zero_pcnt == 0 {
                    for i in 0..n {
                        // the highest V/div: the table's last entry
                        if let Some(v) = self.vga.get(i).and_then(Option::as_ref)
                            && let Some(last) = v.iter().take_while(|e| e.key != 0).last()
                        {
                            self.channels[i].vdiv = last.key;
                        }
                        ret = self.wr_dso_cmd(Some(i), key::PROBE_VDIV);
                        self.channels[i].zero_offset = offset_mid;
                        ret = self.wr_dso_cmd(Some(i), key::PROBE_OFFSET);
                    }
                }
                if !hmcad || self.zero_pcnt == interval {
                    self.zcal.warm_done = true;
                    self.zero_pcnt = -1;
                }
            } else if !self.feature(feature::PREOFF) {
                if self.zero_pcnt == 0 {
                    ret = offset_all(self, offset_bom);
                }
                if self.zero_pcnt == interval {
                    self.zcal.margin[0] = f64::from(self.mstatus.ch0_acc_mean) / limit;
                    self.zcal.margin[1] = f64::from(self.mstatus.ch1_acc_mean) / limit;
                }
                if self.zero_pcnt == interval + 1 {
                    ret = offset_all(self, offset_top);
                }
                if self.zero_pcnt == 2 * interval {
                    let top0 = f64::from(self.mstatus.ch0_acc_mean) / limit;
                    let top1 = f64::from(self.mstatus.ch1_acc_mean) / limit;
                    self.zcal.margin[0] -= top0;
                    self.zcal.margin[1] -= top1;
                    for i in 0..n {
                        let idx = usize::from(self.channels[i].index);
                        let Some(m) = self.zcal.margin.get_mut(idx) else {
                            continue;
                        };
                        *m -= f64::from(offset_bom - offset_top);
                        if m.abs() > margin_pass {
                            *m = round_out(*m);
                            let v = f64::from(self.channels[i].vpos_trans) - *m;
                            self.channels[i].vpos_trans =
                                c_u16(if v < max_trans { v } else { max_trans });
                            // C sends channel 0's offset whichever channel moved
                            ret = self.wr_dso_cmd(Some(p0), key::PROBE_OFFSET);
                        } else {
                            *m = 0.0;
                        }
                    }
                    self.zcal.trans_fix_done =
                        self.zcal.margin[0] == 0.0 && self.zcal.margin[1] == 0.0;
                    self.zero_pcnt = if self.zcal.trans_fix_done { 0 } else { -1 };
                }
            } else {
                self.zcal.trans_fix_done = true;
            }
            if !self.zcal.trans_fix_done && ret.is_ok() {
                self.zero_pcnt += 1;
            }
        } else if !self.zcal.mid_zero_done {
            let stage = self.zero_stage.max(0) as usize;
            if self.zero_pcnt == 0 {
                for i in 0..n {
                    let key_at = |s: &Dsl, k: usize| s.vga_of(i).get(k).map_or(0, |v| v.key);
                    self.channels[i].vdiv = key_at(self, stage);
                    if self.channels[i].vdiv == 0 {
                        self.channels[i].vdiv = key_at(self, stage.wrapping_sub(1));
                        self.zcal.mid_zero_done = true;
                        break;
                    }
                    ret = self.wr_dso_cmd(Some(i), key::PROBE_VDIV);
                    self.channels[i].zero_offset = offset_mid;
                    ret = self.wr_dso_cmd(Some(i), key::PROBE_OFFSET);
                    // must after offset setting
                    let idx = usize::from(self.channels[i].index);
                    self.channels[i].vdiv = self.zcal.vdiv_back.get(idx).copied().unwrap_or(0);
                }
            }
            if self.zero_pcnt == interval {
                let mid = f64::from(offset_mid);
                self.zcal.margin[0] = mid - f64::from(self.mstatus.ch0_acc_mean) / limit;
                self.zcal.margin[1] = mid - f64::from(self.mstatus.ch1_acc_mean) / limit;
                if self.zcal.margin[0].abs() < margin_pass
                    && self.zcal.margin[1].abs() < margin_pass
                {
                    self.zero_stage += 1;
                } else if self.feature(feature::PREOFF) {
                    for i in 0..n {
                        let c = &self.channels[i];
                        let (vpos, idx) = (c.vpos_trans, usize::from(c.index));
                        let Some(v) = self
                            .vga
                            .get_mut(i)
                            .and_then(Option::as_mut)
                            .and_then(|v| v.get_mut(stage))
                        else {
                            continue;
                        };
                        let small = v.key < 500;
                        let trans_coarse = if small {
                            f64::from(i32::from(vpos >> 8) / DSCOPE_TRANS_CMULTI)
                        } else {
                            f64::from(vpos >> 8)
                        };
                        let trans_fine = if small {
                            f64::from(vpos & 0x00ff) / 1000.0
                        } else {
                            f64::from(vpos & 0x00ff) / DSCOPE_TRANS_FMULTI
                        };
                        let margin = self.zcal.margin.get(idx).copied().unwrap_or(0.0);
                        let voltage_margin = margin * v.key as f64 * 10.0 / 255.0;
                        let last = i32::from(v.preoff);
                        let coarse = c_i32((voltage_margin / trans_coarse + 0.5).floor());
                        let fine = c_i32(
                            (-(voltage_margin - f64::from(coarse) * trans_coarse) / trans_fine
                                + 0.5)
                                .floor(),
                        );
                        let coarse = (last >> 10).wrapping_add(coarse);
                        let fine = (last & 0x03ff).wrapping_add(fine);
                        v.preoff = coarse.wrapping_shl(10).wrapping_add(fine) as u16;
                    }
                } else {
                    for i in 0..n {
                        let idx = usize::from(self.channels[i].index);
                        let margin = self.zcal.margin.get(idx).copied().unwrap_or(0.0);
                        if let Some(v) = self
                            .vga
                            .get_mut(i)
                            .and_then(Option::as_mut)
                            .and_then(|v| v.get_mut(stage))
                        {
                            v.preoff = c_u16(f64::from(v.preoff) + round_out(margin));
                        }
                    }
                }
                self.zero_pcnt = 0;
            } else if !self.zcal.mid_zero_done {
                self.zero_pcnt += 1;
            }
        } else {
            ret = Ok(());
            end_cnt = 1;
            if hmcad {
                if !self.zero_comb_fgain {
                    if self.zero_pcnt == 1 {
                        self.zero_branch = true;
                        self.channels[p0].zero_offset = offset_bom;
                        ret = self.wr_dso_cmd(Some(p0), key::PROBE_OFFSET);
                        self.channels[p1].zero_offset = offset_bom;
                        ret = self.wr_dso_cmd(Some(p1), key::PROBE_OFFSET);
                        self.probe_cali_fgain(p0, 1.0, false, true);
                        self.probe_cali_fgain(p1, 1.0, false, true);
                        self.config_adc_fgains(p0, p1, false);
                    } else if self.zero_pcnt == branch_done * interval {
                        let m = self.mstatus;
                        let mean0 = f64::from(
                            m.ch0_acc_mean
                                .wrapping_add(m.ch0_acc_mean_p1)
                                .wrapping_add(m.ch0_acc_mean_p2)
                                .wrapping_add(m.ch0_acc_mean_p3),
                        ) / 4.0;
                        let mean1 = f64::from(
                            m.ch1_acc_mean
                                .wrapping_add(m.ch1_acc_mean_p1)
                                .wrapping_add(m.ch1_acc_mean_p2)
                                .wrapping_add(m.ch1_acc_mean_p3),
                        ) / 4.0;
                        let (skew, max) = self.acc_skews(mean0, mean1);
                        if max > MAX_ACC_VARIANCE
                            && (self.probe_fgain_inrange(p0, false, &skew)
                                || self.probe_fgain_inrange(p1, false, &skew))
                        {
                            self.zero_pcnt = 1;
                            self.probe_cali_fgain(p0, mean0, false, false);
                            self.probe_cali_fgain(p1, mean1, false, false);
                            self.config_adc_fgains(p0, p1, false);
                        } else if max <= MAX_ACC_VARIANCE {
                            self.zero_comb_fgain = true;
                            self.zero_pcnt = 0;
                        } else {
                            self.zero_pcnt = 0;
                            self.skew_fpga_fgain(false, &skew);
                        }
                    }
                }
                if self.zero_comb_fgain {
                    if self.zero_pcnt == 1 {
                        self.channels[p0].zero_offset = offset_bom;
                        ret = self.wr_dso_cmd(Some(p0), key::PROBE_OFFSET);
                        self.channels[p1].zero_offset = offset_bom;
                        ret = self.wr_dso_cmd(Some(p1), key::PROBE_OFFSET);
                        self.zero_comb = false;
                        self.probe_cali_fgain(p0, 1.0, true, true);
                        self.probe_cali_fgain(p1, 1.0, true, true);
                        self.config_adc_fgains(p0, p1, true);
                    } else if self.zero_pcnt == interval {
                        if !self.zero_comb {
                            self.zero_pcnt = 1;
                        }
                    } else if self.zero_pcnt == branch_done * interval {
                        let m = self.mstatus;
                        let sum = [
                            m.ch0_acc_mean,
                            m.ch0_acc_mean_p1,
                            m.ch0_acc_mean_p2,
                            m.ch0_acc_mean_p3,
                            m.ch1_acc_mean,
                            m.ch1_acc_mean_p1,
                            m.ch1_acc_mean_p2,
                            m.ch1_acc_mean_p3,
                        ]
                        .iter()
                        .fold(0u32, |a, &b| a.wrapping_add(b));
                        let mean = f64::from(sum) / 8.0;
                        let (skew, max) = self.acc_skews(mean, mean);
                        if max > MAX_ACC_VARIANCE
                            && (self.probe_fgain_inrange(p0, true, &skew)
                                || self.probe_fgain_inrange(p1, true, &skew))
                        {
                            self.zero_pcnt = 1;
                            self.probe_cali_fgain(p0, mean, true, false);
                            self.probe_cali_fgain(p1, mean, true, false);
                            self.config_adc_fgains(p0, p1, true);
                        } else if max <= MAX_ACC_VARIANCE {
                            self.zero_comb_fgain = false;
                            self.zero_branch = false;
                        } else {
                            self.zero_pcnt = 0;
                            self.skew_fpga_fgain(true, &skew);
                        }
                    }
                }
                end_cnt = branch_done * interval + 1;
            } else {
                let diff = |s: &Dsl, a: u32, b: u32| {
                    c_i8((f64::from(a) * 2.0 - f64::from(b) * 2.0) / s.limit_samples as f64)
                };
                let pc = self.zero_pcnt;
                if pc == 1 {
                    ret = self.cmd(|u| wr_reg(u, COMB_ADDR + 6, 0b1101));
                    ret = self.wr_ch_en(dest::DSO_EN1, !BM_CH_CH1);
                    self.channels[p0].zero_offset = offset_top;
                    ret = self.wr_dso_cmd(Some(p0), key::PROBE_OFFSET);
                } else if pc == interval {
                    let m = self.mstatus;
                    self.channels[p0].comb_diff_top = diff(self, m.ch0_acc_mean, m.ch1_acc_mean);
                    self.channels[p0].zero_offset = offset_bom;
                    ret = self.wr_dso_cmd(Some(p0), key::PROBE_OFFSET);
                } else if pc == 2 * interval {
                    let m = self.mstatus;
                    self.channels[p0].comb_diff_bom = diff(self, m.ch0_acc_mean, m.ch1_acc_mean);
                }
                if pc == 2 * interval + 1 {
                    ret = self.cmd(|u| wr_reg(u, COMB_ADDR + 6, 0b1110));
                    ret = self.wr_ch_en(dest::DSO_EN1, BM_CH_CH1);
                    ret = self.wr_ch_en(dest::DSO_EN0, !BM_CH_CH0);
                    self.channels[p1].zero_offset = offset_top;
                    ret = self.wr_dso_cmd(Some(p1), key::PROBE_OFFSET);
                } else if pc == 3 * interval {
                    let m = self.mstatus;
                    self.channels[p1].comb_diff_top = diff(self, m.ch1_acc_mean, m.ch0_acc_mean);
                    self.channels[p1].zero_offset = offset_bom;
                    ret = self.wr_dso_cmd(Some(p1), key::PROBE_OFFSET);
                } else if pc == 4 * interval {
                    let m = self.mstatus;
                    self.channels[p1].comb_diff_bom = diff(self, m.ch1_acc_mean, m.ch0_acc_mean);
                }
                end_cnt = 4 * interval + 1;
            }
            if ret.is_ok() {
                self.zero_pcnt += 1;
            }
            if self.zero_pcnt == end_cnt {
                let caps = &self.profile.caps;
                for i in 0..n {
                    let idx = usize::from(self.channels[i].index);
                    self.channels[i].vdiv = self.zcal.vdiv_back.get(idx).copied().unwrap_or(0);
                    ret = self.wr_dso_cmd(Some(i), key::PROBE_VDIV);
                    // vgain tuning
                    if self.feature(feature::AUTO_VGAIN) {
                        let cur = i64::from(self.channels[i].vpos_trans);
                        let def = i64::from(caps.default_pwmtrans);
                        let delta = if cur > def {
                            (cur - def) << 8
                        } else {
                            (cur - def).wrapping_shl(7) & !0xFF
                        };
                        if let Some(vga) = self.vga.get_mut(i).and_then(Option::as_mut) {
                            for (k, &vd) in caps.vdivs.iter().enumerate() {
                                if let Some(d) = vga_default(caps.vga_id, vd)
                                    && let Some(v) = vga.get_mut(k)
                                {
                                    v.vgain = d.vgain.wrapping_add(delta as u64);
                                }
                            }
                        }
                    }
                }
                ret = self.cmd(|u| wr_reg(u, COMB_ADDR + 6, 0b0011));
                ret = self.wr_ch_en(dest::DSO_EN0, BM_CH_CH0);
                ret = self.wr_ch_en(dest::DSO_EN1, BM_CH_CH1);
                self.zero = false;
                self.zcal = ZeroCal::default();
                let _ = self.dso_init();
            }
        }
        ret
    }

    /// dscope.c receive_data() after the events: the calibration step and
    /// the progress poll.
    pub(super) fn dscope_receive_tail(&mut self) {
        if self.trf_completed {
            self.empty_poll_count = 0;
        } else {
            self.empty_poll_count += 1;
        }
        if self.zero && self.trf_completed {
            let _ = self.dso_zero();
        }
        let limit = if self.is_e8() {
            MAX_EMPTY_POLL_E8
        } else {
            MAX_EMPTY_POLL
        };
        if self.empty_poll_count > limit && matches!(self.state, State::Start | State::Data) {
            self.read_progress();
        }
    }

    /// dscope.c dev_open()
    pub(super) fn dscope_open(&mut self) -> SrResult<()> {
        let fpga_done = self.dsl_dev_open()?;
        let mut zeroed = true;
        for i in 0..self.channels.len() {
            zeroed = self.dso_load_eep(i, fpga_done);
            if !zeroed {
                break;
            }
        }
        if !zeroed {
            let _ = self.dscope_config_set_active(key::ZERO, &ConfValue::Bool(true), None);
            sr_info!("Zero have not been setted!");
        }
        if !fpga_done {
            if self.feature(feature::HMCAD1511) {
                let _ = self.cmd(|u| {
                    config_adc(u, &adc::INIT_FIX);
                    config_adc(u, &adc::CLK_INIT_1G);
                    config_adc(u, &adc::POWER_DOWN);
                    Ok(())
                });
            }
            let _ = self.dso_init();
        }
        Ok(())
    }

    /// dscope.c config_get() (after dsl_config_get())
    pub(super) fn dscope_config_get(
        &mut self,
        id: i32,
        ch: Option<usize>,
    ) -> SrResult<Option<ConfValue>> {
        use ConfValue::*;
        let m = channel_mode(self.ch_mode);
        let caps = &self.profile.caps;
        let chan = || ch.filter(|&i| i < self.channels.len()).ok_or(SrError::Err);
        let default_vgain =
            |i: usize| vga_default(caps.vga_id, self.channels[i].vdiv).map(|d| d.vgain);
        let v = match id {
            key::OPERATION_MODE => I16(self.op_mode),
            key::BANDWIDTH_LIMIT => I16(self.bw_limit),
            key::VTH => F64(self.vth),
            key::CALI => Bool(self.cali),
            key::TEST => Bool(false),
            key::STREAM => Bool(self.stream),
            key::MAX_DSO_SAMPLERATE => U64(m.max_samplerate),
            key::MAX_DSO_SAMPLELIMITS => U64(caps.dso_depth),
            key::HW_DEPTH => U64(if self.is_e8() && self.mode == Mode::Logic {
                DSL_E8_LOGIC_DEPTH
            } else {
                caps.hw_depth / u64::from(m.unit_bits)
            }),
            key::PROBE_VGAIN => U64(self.dso_vga(chan()?) >> 8),
            key::PROBE_COMB_COMP_EN => Bool(self.feature(feature::HMCAD1511)),
            key::PROBE_COMB_COMP => I16(i16::from(self.channels[chan()?].comb_comp)),
            key::PROBE_VGAIN_DEFAULT => match default_vgain(chan()?) {
                Some(g) => U64(g >> 8),
                // C returns SR_OK without a value
                None => return Ok(None),
            },
            key::PROBE_VGAIN_RANGE => {
                // C reads the channel unchecked
                let g = (default_vgain(chan()?).unwrap_or(0) >> 8) & 0x0FFF;
                U16((g * 2).min(u64::from(CALI_VGAIN_RANGE)) as u16)
            }
            key::PROBE_PREOFF => {
                let i = chan()?;
                let c = &self.channels[i];
                let mut preoff = self.dso_preoff(i) as u16;
                let def = self.get_default_preoff(i);
                if self.feature(feature::PREOFF) {
                    let skew_coarse = i32::from(preoff >> 10) - i32::from(def >> 10);
                    let skew_fine = i32::from(preoff & 0x03ff) - i32::from(def & 0x03ff);
                    let (tc, tf) = trans_steps(c.vdiv, c.vpos_trans);
                    let rate =
                        (f64::from(skew_coarse) * tc - f64::from(skew_fine) * tf) / c.vdiv as f64;
                    preoff = c_u16((rate * 0.5 + 0.5) * f64::from(caps.default_pwmmargin));
                }
                U16(preoff)
            }
            key::PROBE_PREOFF_DEFAULT => U16(self.get_default_preoff(chan()?)),
            // dscope.c repeats these from dsl_config_get(): reached when
            // that failed, i.e. without a channel
            key::PROBE_MAP_DEFAULT => Bool(self.channels[chan()?].map_default),
            key::PROBE_MAP_UNIT => return Ok(self.channels[chan()?].map_unit.clone().map(Str)),
            key::PROBE_MAP_MIN => F64(self.channels[chan()?].map_min),
            key::PROBE_MAP_MAX => F64(self.channels[chan()?].map_max),
            key::PROBE_PREOFF_MARGIN => U16(caps.default_pwmmargin),
            key::VLD_CH_NUM => I16(m.vld_num as i16),
            _ => return Err(SrError::Na),
        };
        Ok(Some(v))
    }

    /// dscope.c config_set() (after dsl_config_set())
    pub(super) fn dscope_config_set(
        &mut self,
        id: i32,
        data: &ConfValue,
        ch: Option<usize>,
    ) -> SrResult<()> {
        self.dscope_config_set_active(id, data, ch)
    }

    /// dscope.c config_set() on an open device: also called by the driver
    /// itself (ZERO from open and ZERO_LOAD).
    fn dscope_config_set_active(
        &mut self,
        id: i32,
        data: &ConfValue,
        ch: Option<usize>,
    ) -> SrResult<()> {
        if self.status != dev_status::ACTIVE {
            return Err(SrError::Err);
        }
        // C dereferences the channel unchecked (NULL crashes)
        let chan = || ch.filter(|&i| i < self.channels.len()).ok_or(SrError::Arg);
        let dso = self.mode == Mode::Dso;
        let mut ret: SrResult<()> = Ok(());
        match id {
            key::CLOCK_TYPE => self.clock_type = data.get_bool(),
            key::CLOCK_EDGE => self.clock_edge = data.get_bool(),
            key::LIMIT_SAMPLES => self.limit_samples = data.get_u64(),
            key::PROBE_VDIV => {
                let i = chan()?;
                self.channels[i].vdiv = data.get_u64();
                ret = self.wr_dso_cmd(Some(i), key::PROBE_VDIV);
            }
            key::PROBE_FACTOR => {
                let i = chan()?;
                self.channels[i].vfactor = data.get_u64();
                ret = self.e8_set_attenuator(i);
            }
            key::TIMEBASE => self.timebase = data.get_u64(),
            key::PROBE_COUPLING => {
                let i = chan()?;
                let mut c = data.get_byte();
                if c == GND_COUPLING {
                    c = DC_COUPLING;
                }
                self.channels[i].coupling = c;
                ret = self.wr_dso_cmd(Some(i), key::PROBE_COUPLING);
            }
            key::TRIGGER_SLOPE => {
                self.trigger_slope = data.get_byte();
                if dso {
                    ret = self.wr_dso_cmd(None, key::TRIGGER_SLOPE);
                }
            }
            key::TRIGGER_VALUE => {
                let i = chan()?;
                self.channels[i].trig_value = data.get_byte();
                if dso {
                    ret = self.wr_dso_cmd(Some(i), key::TRIGGER_VALUE);
                }
            }
            key::HORIZ_TRIGGERPOS => {
                if dso {
                    self.trigger_hrate = data.get_byte();
                    self.update_hpos();
                    ret = self.wr_dso_cmd(None, key::HORIZ_TRIGGERPOS);
                } else {
                    let p = u64::from(data.get_byte()).wrapping_mul(self.limit_samples);
                    self.trigger_hpos = c_u32(p as f64 / 100.0);
                }
            }
            key::TRIGGER_HOLDOFF => {
                self.trigger_holdoff = data.get_u64();
                if dso {
                    ret = self.wr_dso_cmd(None, key::TRIGGER_HOLDOFF);
                }
            }
            key::TRIGGER_MARGIN => {
                self.trigger_margin = data.get_byte();
                if dso {
                    ret = self.wr_dso_cmd(None, key::TRIGGER_MARGIN);
                }
            }
            key::SAMPLERATE => {
                self.cur_samplerate = data.get_u64();
                if dso {
                    ret = self.wr_dso_cmd(None, key::SAMPLERATE);
                }
            }
            key::VTH => {
                self.vth = data.get_f64();
                let code = c_u8(self.vth / 3.3 * (1.5 / 2.5) * 255.0);
                ret = self.cmd(|u| wr_reg(u, VTH_ADDR, code));
            }
            key::INSTANT => {
                self.instant = data.get_bool();
                if dso {
                    let en = self.en_ch_num();
                    let caps = &self.profile.caps;
                    self.limit_samples = if self.instant {
                        caps.hw_depth / u64::from(channel_mode(self.ch_mode).unit_bits) / en
                    } else {
                        caps.dso_depth / en
                    };
                }
            }
            key::DEVICE_MODE => ret = self.dscope_set_mode(data.get_i16()),
            key::OPERATION_MODE => {
                match data.get_i16() {
                    DS_OP_NORMAL => {
                        self.op_mode = DS_OP_NORMAL;
                        self.test_mode = TEST_NONE;
                    }
                    DS_OP_INTEST => {
                        self.op_mode = DS_OP_INTEST;
                        self.test_mode = TEST_INTERNAL;
                    }
                    _ => ret = Err(SrError::Err),
                }
                sr_dbg!("config_set: setting pattern to {}", self.op_mode);
            }
            key::BANDWIDTH_LIMIT => {
                match data.get_i16() {
                    BW_FULL => {
                        self.bw_limit = BW_FULL;
                        let _ = self.cmd(|u| wr_reg(u, CTR0_ADDR, BM_BW20M_CLR));
                    }
                    BW_20M => {
                        self.bw_limit = BW_20M;
                        let _ = self.cmd(|u| wr_reg(u, CTR0_ADDR, BM_BW20M_SET));
                    }
                    _ => ret = Err(SrError::Err),
                }
                sr_dbg!("config_set: setting bandwidth limit to {}", self.bw_limit);
            }
            key::PROBE_EN => {
                let i = chan()?;
                self.channels[i].enabled = data.get_bool();
                ret = self.dscope_probe_en(i);
            }
            key::PROBE_OFFSET => {
                let i = chan()?;
                let caps = &self.profile.caps;
                let mut o = data.get_u16();
                if u32::from(o) > caps.ref_max {
                    o = caps.ref_max as u16;
                }
                if u32::from(o) < caps.ref_min {
                    o = caps.ref_min as u16;
                }
                self.channels[i].offset = o;
                if self.state != State::Finish {
                    ret = self.wr_dso_cmd(Some(i), key::PROBE_OFFSET);
                }
            }
            key::TRIGGER_SOURCE => {
                self.trigger_source =
                    (self.trigger_source & 0xf0).wrapping_add(data.get_byte() & 0x0f);
                ret = self.wr_dso_cmd(None, key::TRIGGER_SOURCE);
            }
            key::TRIGGER_CHANNEL => {
                self.trigger_source =
                    (data.get_byte() << 4).wrapping_add(self.trigger_source & 0x0f);
                ret = self.wr_dso_cmd(None, key::TRIGGER_SOURCE);
            }
            key::ZERO => {
                self.zero = data.get_bool();
                if self.zero {
                    self.zero_stage = -1;
                    self.zero_pcnt = 0;
                    self.reset_vga_tables(false);
                } else {
                    self.dso_zero_reset();
                }
            }
            key::ZERO_DEFAULT => self.reset_vga_tables(true),
            key::CALI => self.cali = data.get_bool(),
            key::ZERO_LOAD => {
                for i in 0..self.channels.len() {
                    if !self.dso_load_eep(i, false) {
                        let _ =
                            self.dscope_config_set_active(key::ZERO, &ConfValue::Bool(true), None);
                        sr_info!("Zero have not been setted!");
                        break;
                    }
                }
            }
            key::ZERO_SET => ret = self.dso_zero_set(),
            key::VOCM => {
                let v = data.get_byte();
                ret = self.cmd(|u| wr_reg(u, COMB_ADDR + 4, v));
            }
            key::PROBE_VGAIN => {
                let i = chan()?;
                let vgain = data.get_u64() << 8;
                let vdiv = self.channels[i].vdiv;
                let n = self.vga_of(i).len();
                if let Some(vga) = self.vga[i].as_mut() {
                    for v in vga.iter_mut().take(n).filter(|v| v.key == vdiv) {
                        v.vgain = vgain;
                    }
                }
                ret = self.wr_dso_cmd(Some(i), key::PROBE_VDIV);
            }
            key::PROBE_PREOFF => {
                let i = chan()?;
                let mut preoff = data.get_u16();
                if self.feature(feature::PREOFF) {
                    let c = &self.channels[i];
                    let margin = f64::from(self.profile.caps.default_pwmmargin);
                    let voltage_off = (2.0 * f64::from(preoff) / margin - 1.0) * c.vdiv as f64;
                    let (tc, tf) = trans_steps(c.vdiv, c.vpos_trans);
                    let def = i32::from(self.get_default_preoff(i));
                    let coarse = c_i32((voltage_off / tc + 0.5).floor());
                    let fine = c_i32((-(voltage_off - f64::from(coarse) * tc) / tf + 0.5).floor());
                    let coarse = (def >> 10).wrapping_add(coarse);
                    let fine = (def & 0x03ff).wrapping_add(fine);
                    preoff = coarse.wrapping_shl(10).wrapping_add(fine) as u16;
                }
                let vdiv = self.channels[i].vdiv;
                let n = self.vga_of(i).len();
                if let Some(vga) = self.vga[i].as_mut() {
                    for v in vga.iter_mut().take(n).filter(|v| v.key == vdiv) {
                        v.preoff = preoff;
                    }
                }
                if self.state != State::Finish {
                    ret = self.wr_dso_cmd(Some(i), key::PROBE_OFFSET);
                }
            }
            key::PROBE_COMB_COMP => {
                let i = chan()?;
                self.channels[i].comb_comp = data.get_i16() as i8;
                let _ = self.wr_dso_cmd(Some(i), key::PROBE_VDIV);
                ret = self.wr_dso_cmd(Some(i), key::PROBE_OFFSET);
            }
            _ => ret = Err(SrError::Na),
        }
        ret
    }

    /// The DSO trigger position from its rate (in percent of the frame).
    fn update_hpos(&mut self) {
        let p = (u64::from(self.trigger_hrate) * self.en_ch_num()).wrapping_mul(self.limit_samples);
        self.trigger_hpos = c_u32(p as f64 / 200.0);
    }

    /// SR_CONF_ZERO (`defaults` false: offsets only) and
    /// SR_CONF_ZERO_DEFAULT (`defaults` true: all calibration values).
    fn reset_vga_tables(&mut self, defaults: bool) {
        let caps = &self.profile.caps;
        for i in 0..self.channels.len() {
            let c = &mut self.channels[i];
            c.vpos_trans = caps.default_pwmtrans;
            if defaults {
                c.comb_comp = caps.default_comb_comp as i8;
                c.digi_fgain = 0;
                c.cali_fgain = [1.0; 4];
                c.cali_comb_fgain = [1.0; 4];
            }
            let Some(vga) = self.vga[i].as_mut() else {
                continue;
            };
            for (k, &vd) in caps.vdivs.iter().enumerate() {
                if let Some(d) = vga_default(caps.vga_id, vd)
                    && let Some(v) = vga.get_mut(k)
                {
                    v.id = d.id;
                    v.key = d.key;
                    v.preoff = d.preoff;
                    if defaults {
                        v.vgain = d.vgain;
                    }
                }
            }
        }
    }

    /// SR_CONF_PROBE_EN after the flag changed.
    #[allow(unused_assignments)]
    fn dscope_probe_en(&mut self, i: usize) -> SrResult<()> {
        let (idx, enabled) = (self.channels[i].index, self.channels[i].enabled);
        let mut ret = Ok(());
        if self.mode == Mode::Dso {
            if self.state == State::Data && self.feature(feature::HMCAD1511) {
                let en = self.en_ch_num();
                let cfg: Option<&[AdcConfig]> = if en == 2 {
                    Some(&adc::DUAL_CH03)
                } else if en == 1 {
                    self.channels
                        .iter()
                        .find(|p| p.enabled && p.index <= 1)
                        .map(|p| {
                            if p.index == 0 {
                                &adc::SINGLE_CH0[..]
                            } else {
                                &adc::SINGLE_CH3[..]
                            }
                        })
                } else {
                    None
                };
                if let Some(cfg) = cfg {
                    let _ = self.cmd(|u| {
                        config_adc(u, cfg);
                        Ok(())
                    });
                }
            }
            let _ = self.wr_dso_cmd(Some(i), key::PROBE_EN);
            let (d, v) = if idx == 0 {
                (dest::DSO_EN0, if enabled { BM_CH_CH0 } else { !BM_CH_CH0 })
            } else {
                (dest::DSO_EN1, if enabled { BM_CH_CH1 } else { !BM_CH_CH1 })
            };
            ret = self.wr_ch_en(d, v);
            // dsl_en_ch_num() is never 0
            ret = self.wr_dso_cmd(None, key::SAMPLERATE);
            self.limit_samples = self.profile.caps.dso_depth / self.en_ch_num();
        } else if self.mode == Mode::Analog {
            let _ = self.wr_dso_cmd(Some(i), key::PROBE_EN);
            let (d, v) = if idx == 0 {
                (dest::DSO_EN0, BM_CH_CH0)
            } else {
                (dest::DSO_EN1, BM_CH_CH1)
            };
            ret = self.wr_ch_en(d, v);
        }
        ret
    }

    /// SR_CONF_DEVICE_MODE
    fn dscope_set_mode(&mut self, value: i16) -> SrResult<()> {
        let Some(mode) = Mode::from_i32(i32::from(value)) else {
            // C stores the value and drops every channel (num_probes 0)
            sr_err!("config_set: no device mode {value}.");
            return Err(SrError::Arg);
        };
        self.mode = mode;
        let mut ret = Ok(());
        let first = |s: &Dsl, mode: Mode| {
            CHANNEL_MODES
                .iter()
                .enumerate()
                .find(|(i, m)| m.mode == mode && s.profile.caps.channels & (1 << i) != 0)
                .map(|(_, m)| m)
        };
        let mut num = 0;
        match mode {
            Mode::Dso => {
                if self.wr_dso_cmd(None, key::DSO_SYNC).is_err() {
                    sr_dbg!("config_set: DSO configuration sync failed");
                }
                if !self.channels.is_empty() {
                    ret = self.wr_dso_cmd(Some(0), key::PROBE_VDIV);
                }
                self.op_mode = DS_OP_NORMAL;
                self.test_mode = TEST_NONE;
                self.instant = false;
                if let Some(m) = first(self, Mode::Dso) {
                    self.ch_mode = m.id;
                    num = m.num;
                    self.stream = m.stream;
                    self.cur_samplerate = m.max_samplerate / u64::from(m.num);
                }
                self.limit_samples = self
                    .profile
                    .caps
                    .dso_depth
                    .checked_div(u64::from(num))
                    .unwrap_or(0);
            }
            Mode::Analog => {
                if !self.channels.is_empty() {
                    ret = self.wr_dso_cmd(Some(0), key::PROBE_VDIV);
                }
                self.op_mode = DS_OP_NORMAL;
                self.test_mode = TEST_NONE;
                self.instant = true;
                if let Some(m) = first(self, Mode::Analog) {
                    self.ch_mode = m.id;
                    num = m.num;
                    self.stream = m.stream;
                    self.cur_samplerate = m.max_samplerate;
                }
                self.limit_samples = self.cur_samplerate;
            }
            Mode::Logic => {
                self.op_mode = DS_OP_NORMAL;
                self.test_mode = TEST_NONE;
                self.instant = false;
                if let Some(m) = first(self, Mode::Logic) {
                    self.ch_mode = m.id;
                    num = m.num;
                    self.stream = m.stream;
                }
                self.limit_samples = self.profile.caps.default_samplelimit;
                if self.vth <= 0.0 {
                    self.vth = 1.65;
                }
                let code = c_u8(self.vth / 3.3 * (1.5 / 2.5) * 255.0);
                ret = self.cmd(|u| wr_reg(u, VTH_ADDR, code));
            }
        }
        self.adjust_probes(num);
        self.adjust_samplerate();
        sr_dbg!("config_set: setting mode to {}", value);
        ret
    }

    /// dscope.c config_list() (after dsl_config_list())
    pub(super) fn dscope_config_list(&self, id: i32) -> SrResult<ConfValue> {
        use ConfValue::*;
        Ok(match id {
            key::DEVICE_OPTIONS => I32s(if self.mode == Mode::Logic {
                HWOPTIONS_LOGIC.to_vec()
            } else {
                HWOPTIONS.to_vec()
            }),
            key::DEVICE_SESSIONS => I32s(match self.mode {
                Mode::Dso => SESSIONS_DSO.to_vec(),
                Mode::Analog => SESSIONS_DAQ.to_vec(),
                Mode::Logic => SESSIONS_LOGIC.to_vec(),
            }),
            key::OPERATION_MODE => List(opmode_list()),
            key::BANDWIDTH_LIMIT => List(bandwidth_list()),
            _ => return Err(SrError::Na),
        })
    }

    /// dscope.c dev_acquisition_start()
    pub(super) fn dscope_start(&mut self, out: &mut Vec<Packet>) -> SrResult<()> {
        if self.status != dev_status::ACTIVE {
            self.last_error = last_error::DEVICE_CLOSED;
            return Err(SrError::DeviceClosed);
        }
        self.reset_acquisition();
        self.instant_tail_bytes = self.header_size() as i64;
        if self.configure_probes().is_err() {
            sr_err!("dev_acquisition_start: Failed to configure probes.");
            return Err(SrError::Err);
        }
        if ctl_wr(self.usb()?, dest::STOP, 0, &[]).is_err() {
            sr_err!("dev_acquisition_start: Stop DSCope acquisition failed!");
            return Err(SrError::Err);
        }
        sr_info!("dev_acquisition_start: Stop Previous DSCope acquisition!");

        if self.feature(feature::HMCAD1511) {
            let en = self.en_ch_num();
            let chans: Vec<(u16, bool, [f64; 4], [f64; 4])> = self
                .channels
                .iter()
                .map(|c| (c.index, c.enabled, c.cali_fgain, c.cali_comb_fgain))
                .collect();
            let _ = self.cmd(|u| {
                if en == 2 {
                    config_adc(u, &adc::DUAL_CH03);
                    for &(idx, _, g, _) in &chans {
                        let b = (idx * 2) as u8;
                        config_adc_fgain(u, b, g[0], g[1]);
                        config_adc_fgain(u, b.wrapping_add(1), g[2], g[3]);
                    }
                } else if en == 1 {
                    for &(idx, enabled, _, _) in &chans {
                        if enabled && idx == 0 {
                            config_adc(u, &adc::SINGLE_CH0);
                        } else if enabled && idx == 1 {
                            config_adc(u, &adc::SINGLE_CH3);
                        }
                    }
                    for &(idx, _, _, g) in &chans {
                        let b = (idx * 2) as u8;
                        config_adc_fgain(u, b, g[0], g[1]);
                        config_adc_fgain(u, b.wrapping_add(1), g[2], g[3]);
                    }
                }
                Ok(())
            });
            self.config_fpga_fgain();
        }
        if let Err(e) = self.fpga_arm() {
            sr_err!("dev_acquisition_start: Arm FPGA failed!");
            return Err(e);
        }
        if self.zero && self.zero_stage == -1 {
            // initialize before Auto Calibration
            if let Err(e) = self.dso_init() {
                sr_err!("dev_acquisition_start: DSO zero initialization failed!");
                return Err(e);
            }
            self.zero_stage = 0;
            self.zero_comb_fgain = false;
            self.zero_branch = false;
        }
        // settings must be updated before acquisition
        if self.mode != Mode::Logic {
            self.update_hpos();
            if self.wr_dso_cmd(None, key::HORIZ_TRIGGERPOS).is_err() {
                sr_dbg!("dev_acquisition_start: setting DSO Horiz Trigger Position failed");
            }
            for i in 0..self.channels.len() {
                let idx = self.channels[i].index;
                if self.wr_dso_cmd(Some(i), key::PROBE_VDIV).is_err() {
                    sr_err!("dev_acquisition_start: Set VDIV of channel {idx} command failed!");
                }
                if self.wr_dso_cmd(Some(i), key::PROBE_OFFSET).is_err() {
                    sr_err!("dev_acquisition_start: Set OFFSET of channel {idx} command failed!");
                }
                // DSO_E8-1 has no programmable offset: its zero code is fixed
                self.channels[i].hw_offset = if self.is_e8() {
                    DSL_E8_ZERO_CODE
                } else {
                    self.channels[i].offset
                };
                if self.e8_set_attenuator(i).is_err() {
                    sr_err!(
                        "dev_acquisition_start: Set attenuator of channel {idx} command failed!"
                    );
                }
            }
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

/// The coarse and fine offset steps (mV) of a DSCope with PREOFF at this
/// V/div: `vpos_trans` holds both, the coarse one in its high byte.
fn trans_steps(vdiv: u64, vpos_trans: u16) -> (f64, f64) {
    if vdiv < 500 {
        // integer division in C
        (
            f64::from(i32::from(vpos_trans >> 8) / DSCOPE_TRANS_CMULTI),
            f64::from(vpos_trans & 0x00ff) / 1000.0,
        )
    } else {
        (
            f64::from(vpos_trans >> 8),
            f64::from(vpos_trans & 0x00ff) / DSCOPE_TRANS_FMULTI,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fgain_codes_round_trip() {
        // 0 and 127 both mean gain 1.0, which encodes as 127
        for code in 1..=127u8 {
            let g = adc_code2fgain(code);
            assert_eq!(adc_fgain2code(g), code, "code {code}");
        }
        assert_eq!(adc_fgain2code(adc_code2fgain(0)), 127);
    }

    #[test]
    fn int_shift_wraps_like_x86() {
        assert_eq!(int_shl(0x80, 24), 0xFFFF_FFFF_8000_0000);
        assert_eq!(int_shl(1, 32), 1);
        assert_eq!(int_shl(5, 8), 0x500);
    }
}
