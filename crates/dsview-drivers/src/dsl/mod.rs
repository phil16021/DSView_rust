//! DreamSourceLab USB devices (libsigrok4DSL/hardware/DSL): what the DSLogic
//! and DSCope drivers share. The vendor protocol, the FX2 firmware and FPGA
//! uploads, the `DSL_setting` block that arms a capture, the security check
//! and the trigger masks.
//!
//! * [`dslogic`]: the DSLogic driver (`dslogic.c` + the logic parts of `dsl.c`)
//! * [`tables`]: profiles, channel modes and lists, generated from `dsl.h`

pub mod dslogic;
pub mod tables;

use std::time::Duration;

use dsview_core::{MAX_TRIGGER_PROBES, Mode, TRIGGER_PROBES, TRIGGER_STAGES, Trigger};
pub use dsview_usb::Speed;
use dsview_usb::{Setup, UsbDevice, UsbError, VENDOR_IN, VENDOR_OUT};

use crate::sys::{self, sr_err, sr_info};

pub const DS_VENDOR_ID: u16 = 0x2A0E;
pub const USB_INTERFACE: u8 = 0;
pub const USB_CONFIGURATION: u8 = 1;
pub const NUM_TRIGGER_STAGES: usize = 16;
pub const NUM_SIMUL_TRANSFERS: u32 = 64;
pub const MAX_EMPTY_POLL: u32 = 16;
pub const DSL_REQUIRED_VERSION_MAJOR: u8 = 2;
pub const DSL_HDL_VERSION: u8 = 0x0E;

/// `CAPS_FEATURE_*`
pub mod feature {
    pub const VTH: u64 = 1 << 0;
    pub const BUF: u64 = 1 << 1;
    pub const PREOFF: u64 = 1 << 2;
    pub const SEEP: u64 = 1 << 3;
    pub const ZERO: u64 = 1 << 4;
    pub const HMCAD1511: u64 = 1 << 5;
    pub const USB30: u64 = 1 << 6;
    pub const POGOPIN: u64 = 1 << 7;
    pub const ADF4360: u64 = 1 << 8;
    pub const M20: u64 = 1 << 9;
    pub const FLASH: u64 = 1 << 10;
    pub const LA_CH32: u64 = 1 << 11;
    pub const AUTO_VGAIN: u64 = 1 << 12;
    pub const MAX25_VTH: u64 = 1 << 13;
    pub const SECURITY: u64 = 1 << 14;
    pub const SELF_ZERO: u64 = 1 << 15;
}

/// `CAPS_MODE_*`
pub const CAPS_MODE_LOGIC: u64 = 1 << 0;
pub const CAPS_MODE_ANALOG: u64 = 1 << 1;
pub const CAPS_MODE_DSO: u64 = 1 << 2;

/// Vendor requests.
pub const CMD_CTL_WR: u8 = 0xb0;
pub const CMD_CTL_RD_PRE: u8 = 0xb1;
pub const CMD_CTL_RD: u8 = 0xb2;

/// `ctl_header.dest`
pub mod dest {
    pub const FW_VERSION: u8 = 0;
    pub const HW_STATUS: u8 = 2;
    pub const PROG_B: u8 = 3;
    pub const SYS: u8 = 4;
    pub const LED: u8 = 5;
    pub const INTRDY: u8 = 6;
    pub const WORDWIDE: u8 = 7;
    pub const START: u8 = 8;
    pub const STOP: u8 = 9;
    pub const BULK_WR: u8 = 10;
    pub const REG: u8 = 11;
    pub const NVM: u8 = 12;
    pub const I2C_DSO: u8 = 13;
    pub const I2C_REG: u8 = 14;
    pub const I2C_STATUS: u8 = 15;
}

// HW_STATUS bits
pub const BM_GPIF_DONE: u8 = 1 << 7;
pub const BM_FPGA_DONE: u8 = 1 << 6;
pub const BM_FPGA_INIT_B: u8 = 1 << 5;
pub const BM_SYS_OVERFLOW: u8 = 1 << 4;
pub const BM_SYS_CLR: u8 = 1 << 3;
pub const BM_LED_RED: u8 = 1 << 1;
pub const BM_LED_GREEN: u8 = 1 << 0;
pub const BM_WR_PROG_B: u8 = 1 << 2;
pub const BM_WR_INTRDY: u8 = 1 << 7;
pub const BM_WR_WORDWIDE: u8 = 1 << 0;

// registers
pub const VTH_ADDR: u8 = 0x78;
pub const SEC_DATA_ADDR: u8 = 0x75;
pub const SEC_CTRL_ADDR: u8 = 0x73;
pub const CTR0_ADDR: u8 = 0x70;
pub const HW_STATUS_ADDR: u8 = 0x05;
pub const HDL_VERSION_ADDR: u8 = 0x04;
pub const BM_SECU_READY: u8 = 1 << 3;
pub const BM_SECU_PASS: u8 = 1 << 4;
pub const SECU_STEPS: usize = 8;
pub const SECU_START: u16 = 0x0513;
pub const SECU_CHECK: u16 = 0x0219;
pub const SECU_EEP_ADDR: u16 = 0x3C00;
pub const SECU_TRY_CNT: i32 = 8;

// CTR0 commands
pub const BM_NONE: u8 = 0;
pub const BM_FORCE_RDY: u8 = 1 << 1;
pub const BM_FORCE_STOP: u8 = 1 << 2;
pub const BM_SCOPE_SET: u8 = 1 << 3;
pub const BM_SCOPE_CLR: u8 = 1 << 4;

// DSL_setting.mode bits
pub const TRIG_EN_BIT: u32 = 0;
pub const CLK_TYPE_BIT: u32 = 1;
pub const CLK_EDGE_BIT: u32 = 2;
pub const RLE_MODE_BIT: u32 = 3;
pub const DSO_MODE_BIT: u32 = 4;
pub const HALF_MODE_BIT: u32 = 5;
pub const QUAR_MODE_BIT: u32 = 6;
pub const ANALOG_MODE_BIT: u32 = 7;
pub const FILTER_BIT: u32 = 8;
pub const INSTANT_BIT: u32 = 9;
pub const SLOW_ACQ_BIT: u32 = 10;
pub const STRIG_MODE_BIT: u32 = 11;
pub const STREAM_MODE_BIT: u32 = 12;
pub const LPB_TEST_BIT: u32 = 13;
pub const EXT_TEST_BIT: u32 = 14;
pub const INT_TEST_BIT: u32 = 15;

pub const TRIG_CHECKID: u32 = 0x5555_5555;
pub const DSLOGIC_ATOMIC_SAMPLES: u64 = 64;
pub const DSLOGIC_ATOMIC_SIZE: u64 = 8;
pub const DSLOGIC_ATOMIC_MASK: u32 = 0xFFFF << 6;
pub const SAMPLES_ALIGN: u64 = 1023;
pub const DS_MAX_TRIG_PERCENT: u64 = 90;

/// `struct DSL_caps`
#[derive(Debug)]
pub struct Caps {
    pub mode_caps: u64,
    pub feature_caps: u64,
    pub channels: u64,
    pub total_ch_num: u64,
    pub hw_depth: u64,
    pub dso_depth: u64,
    pub intest_channel: u16,
    /// Empty where C has NULL.
    pub vdivs: &'static [u64],
    pub samplerates: &'static [u64],
    pub vga_id: u8,
    pub default_channelmode: u16,
    pub default_samplerate: u64,
    pub default_samplelimit: u64,
    pub default_pwmtrans: u16,
    pub default_pwmmargin: u16,
    pub ref_min: u32,
    pub ref_max: u32,
    pub default_comb_comp: u16,
    pub half_samplerate: u64,
    pub quarter_samplerate: u64,
}

/// `struct DSL_profile`
#[derive(Debug)]
pub struct Profile {
    pub vid: u16,
    pub pid: u16,
    pub speed: Speed,
    pub vendor: &'static str,
    pub model: &'static str,
    pub model_version: Option<&'static str>,
    /// None where C has NULL (devices without these files).
    pub firmware: Option<&'static str>,
    pub fpga_bit33: Option<&'static str>,
    pub fpga_bit50: Option<&'static str>,
    pub caps: Caps,
}

/// `struct DSL_channels`
#[derive(Debug)]
pub struct ChannelMode {
    pub id: u16,
    pub mode: Mode,
    pub ty: i32,
    pub stream: bool,
    pub num: u16,
    pub vld_num: u16,
    pub unit_bits: u8,
    pub min_samplerate: u64,
    pub max_samplerate: u64,
    pub hw_min_samplerate: u64,
    pub hw_max_samplerate: u64,
    pub pre_div: u8,
    pub descr: &'static str,
}

pub fn channel_mode(id: u16) -> &'static ChannelMode {
    &tables::CHANNEL_MODES[id as usize]
}

// ------------------------------------------------------------ commands

const CTL_TIMEOUT: Duration = Duration::from_millis(3000);

/// libsigrok's error for a failed command.
#[derive(Debug)]
pub struct CmdError(pub UsbError);

pub type CmdResult<T> = Result<T, CmdError>;

/// `command_ctl_wr()`: header `{dest, offset, size}` plus data. The fork's
/// C zeroes the offset for the address-less destinations; the ones it never
/// sets (START, STOP, BULK_WR) carry stack contents in C (F22) and 0 here.
pub fn ctl_wr(usb: &mut dyn UsbDevice, dest: u8, offset: u16, data: &[u8]) -> CmdResult<()> {
    let mut buf = Vec::with_capacity(4 + data.len());
    buf.push(dest);
    buf.extend_from_slice(&offset.to_le_bytes());
    buf.push(data.len() as u8);
    buf.extend_from_slice(data);
    let setup = Setup {
        request_type: VENDOR_OUT,
        request: CMD_CTL_WR,
        value: 0,
        index: 0,
    };
    usb.control_out(setup, &buf, CTL_TIMEOUT).map_err(|e| {
        sr_err!(
            "Unable to send CMD_CTL_WR command(dest:{dest}/offset:{offset}/size:{}): {e}.",
            data.len()
        );
        CmdError(e)
    })
}

/// `command_ctl_rd()`: CTL_RD_PRE with the header, 10 ms, CTL_RD.
pub fn ctl_rd(usb: &mut dyn UsbDevice, dest: u8, offset: u16, size: u8) -> CmdResult<Vec<u8>> {
    let header = [dest, offset as u8, (offset >> 8) as u8, size];
    let pre = Setup {
        request_type: VENDOR_OUT,
        request: CMD_CTL_RD_PRE,
        value: 0,
        index: 0,
    };
    usb.control_out(pre, &header, CTL_TIMEOUT).map_err(|e| {
        sr_err!(
            "Unable to send CMD_CTL_RD_PRE command(dest:{dest}/offset:{offset}/size:{size}): {e}."
        );
        CmdError(e)
    })?;
    sys::clock().sleep_us(10_000);
    let rd = Setup {
        request_type: VENDOR_IN,
        request: CMD_CTL_RD,
        value: 0,
        index: 0,
    };
    let mut v = usb
        .control_in(rd, u16::from(size), CTL_TIMEOUT)
        .map_err(|e| {
            sr_err!("Unable to send CMD_CTL_RD command: {e}.");
            CmdError(e)
        })?;
    v.resize(size as usize, 0);
    Ok(v)
}

/// `dsl_wr_reg()`
pub fn wr_reg(usb: &mut dyn UsbDevice, addr: u8, value: u8) -> CmdResult<()> {
    ctl_wr(usb, dest::I2C_REG, u16::from(addr), &[value]).inspect_err(|_| {
        sr_err!("Sent DSL_CTL_I2C_REG command failed.");
    })
}

/// `dsl_rd_reg()`
pub fn rd_reg(usb: &mut dyn UsbDevice, addr: u8) -> CmdResult<u8> {
    ctl_rd(usb, dest::I2C_STATUS, u16::from(addr), 1)
        .map(|v| v[0])
        .inspect_err(|_| sr_err!("Sent DSL_CTL_I2C_STATUS read command failed."))
}

/// `dsl_rd_nvm()`
pub fn rd_nvm(usb: &mut dyn UsbDevice, addr: u16, len: u8) -> CmdResult<Vec<u8>> {
    ctl_rd(usb, dest::NVM, addr, len)
        .inspect_err(|_| sr_err!("Sent DSL_CTL_NVM read command failed."))
}

/// `dsl_hdl_version()`: byte HDL_VERSION_ADDR of the I2C status block.
pub fn hdl_version(usb: &mut dyn UsbDevice) -> CmdResult<u8> {
    ctl_rd(usb, dest::I2C_STATUS, 0, HDL_VERSION_ADDR + 1)
        .map(|v| v[HDL_VERSION_ADDR as usize])
        .inspect_err(|_| sr_err!("Sent DSL_CTL_I2C_STATUS command failed."))
}

/// Status polls the C driver loops on without a bound. Here a poll gives up
/// after this many reads (seconds of waiting; real devices answer at once).
const MAX_STATUS_POLLS: u32 = 200;

/// Reads HW_STATUS until `mask` is set (C: `while(1)`).
pub(crate) fn wait_hw_status(usb: &mut dyn UsbDevice, mask: u8) -> CmdResult<u8> {
    for _ in 0..MAX_STATUS_POLLS {
        let v = ctl_rd(usb, dest::HW_STATUS, 0, 1)?[0];
        if v & mask != 0 {
            return Ok(v);
        }
    }
    sr_err!("Device does not report HW_STATUS bit {mask:#04x}.");
    Err(CmdError(UsbError::Timeout))
}

/// `dsl_fpga_config()`: loads an FPGA bitstream.
pub fn fpga_config(usb: &mut dyn UsbDevice, path: &std::path::Path) -> CmdResult<()> {
    sr_info!("Configure FPGA using \"{}\"", path.display());
    let bits = std::fs::read(path).map_err(|e| {
        sr_err!(
            "Unable to open FPGA bit file {} for reading: {e}",
            path.display()
        );
        CmdError(UsbError::NotFound)
    })?;
    if bits.is_empty() {
        return Err(CmdError(UsbError::Io("empty FPGA bit file".into())));
    }
    // PROG_B low, LEDs off, PROG_B high, wait INIT_B
    ctl_wr(usb, dest::PROG_B, 0, &[!BM_WR_PROG_B])?;
    ctl_wr(usb, dest::LED, 0, &[!BM_LED_GREEN & !BM_LED_RED])?;
    ctl_wr(usb, dest::PROG_B, 0, &[BM_WR_PROG_B])?;
    wait_hw_status(usb, BM_FPGA_INIT_B)?;
    // config command and data
    ctl_wr(usb, dest::INTRDY, 0, &[!BM_WR_INTRDY])?;
    let n = bits.len() as u64;
    ctl_wr(
        usb,
        dest::BULK_WR,
        0,
        &[n as u8, (n >> 8) as u8, (n >> 16) as u8],
    )
    .inspect_err(|_| {
        sr_err!("Configure FPGA error: send command fpga_config failed.");
    })?;
    match usb.bulk_out(2, &bits, Duration::from_millis(1000)) {
        Ok(t) if t == bits.len() => {}
        Ok(t) => {
            sr_err!(
                "Configure FPGA error: expacted transfer size {}; actually {t}.",
                bits.len()
            );
            return Err(CmdError(UsbError::Io("short FPGA transfer".into())));
        }
        Err(e) => {
            sr_err!("Unable to configure FPGA of dsl device: {e}.");
            return Err(CmdError(e));
        }
    }
    // INTRDY high, wait GPIF_DONE, INTRDY low, wait FPGA_DONE, green LED
    ctl_wr(usb, dest::INTRDY, 0, &[BM_WR_INTRDY])?;
    wait_hw_status(usb, BM_GPIF_DONE)?;
    ctl_wr(usb, dest::INTRDY, 0, &[!BM_WR_INTRDY])?;
    wait_hw_status(usb, BM_FPGA_DONE)?;
    ctl_wr(usb, dest::LED, 0, &[BM_LED_GREEN])?;
    ctl_wr(usb, dest::WORDWIDE, 0, &[BM_WR_WORDWIDE]).inspect_err(|_| {
        sr_err!("Sent DSL_CTL_WORDWIDE command failed.");
    })?;
    sr_info!("FPGA configure done: {} bytes.", bits.len());
    Ok(())
}

// ------------------------------------------------------------- security

/// `dsl_secuReset()`
fn secu_reset(usb: &mut dyn UsbDevice) -> CmdResult<()> {
    wr_reg(usb, SEC_CTRL_ADDR, 0)?;
    wr_reg(usb, SEC_CTRL_ADDR + 1, 0)?;
    sys::clock().sleep_us(10_000);
    wr_reg(usb, SEC_CTRL_ADDR, 1)?;
    wr_reg(usb, SEC_CTRL_ADDR + 1, 0)
}

/// `dsl_secuWrite()`
fn secu_write(usb: &mut dyn UsbDevice, cmd: u16, din: u16) -> CmdResult<()> {
    wr_reg(usb, SEC_DATA_ADDR, din as u8)?;
    wr_reg(usb, SEC_DATA_ADDR + 1, (din >> 8) as u8)?;
    wr_reg(usb, SEC_CTRL_ADDR, cmd as u8)?;
    wr_reg(usb, SEC_CTRL_ADDR + 1, (cmd >> 8) as u8)
}

fn secu_ctrl(usb: &mut dyn UsbDevice, bit: u8) -> bool {
    rd_reg(usb, SEC_CTRL_ADDR).is_ok_and(|v| v & bit != 0)
}

/// `dsl_secuRead()`
fn secu_read(usb: &mut dyn UsbDevice) -> u16 {
    let Ok(hi) = rd_reg(usb, SEC_DATA_ADDR + 1) else {
        return 0;
    };
    let Ok(lo) = rd_reg(usb, SEC_DATA_ADDR) else {
        return 0;
    };
    (u16::from(hi) << 8) | u16::from(lo)
}

/// `dsl_secuCheck()`: answers the FPGA's challenge with the EEPROM words.
/// Command failures are ignored as in C; only the protocol decides.
pub fn secu_check(usb: &mut dyn UsbDevice, encryption: &[u16; SECU_STEPS]) -> bool {
    let mut try_cnt = SECU_TRY_CNT;
    let _ = secu_reset(usb);
    if secu_ctrl(usb, BM_SECU_PASS) {
        return false;
    }
    let _ = secu_write(usb, SECU_START, 0);
    let mut steps = SECU_STEPS;
    while steps > 0 {
        steps -= 1;
        if secu_ctrl(usb, BM_SECU_PASS) {
            return false;
        }
        while !secu_ctrl(usb, BM_SECU_READY) {
            if try_cnt == 0 {
                sr_err!("Get security ready failed.");
                return false;
            }
            try_cnt -= 1;
        }
        if secu_read(usb) != 0 {
            return false;
        }
        let _ = secu_write(usb, SECU_CHECK, encryption[steps]);
    }
    true
}

// -------------------------------------------------------------- trigger

/// Which `ds_trigger_get_*` to compute.
#[derive(Clone, Copy)]
pub enum TrigField {
    Mask0,
    Mask1,
    Value0,
    Value1,
    Edge0,
    Edge1,
}

/// `ds_trigger_get_mask0()` … `ds_trigger_get_edge1()`: probes `lsc..=msc`
/// of `stage` as a bit mask (msc first), replicated for half/quarter rate.
pub fn trigger_bits(
    t: &Trigger,
    field: TrigField,
    stage: usize,
    msc: usize,
    lsc: usize,
    qutr: bool,
    half: bool,
) -> u16 {
    debug_assert!(stage <= TRIGGER_STAGES && lsc <= msc && msc < MAX_TRIGGER_PROBES);
    let (probes, test): (&[u8; MAX_TRIGGER_PROBES], fn(u8) -> bool) = match field {
        TrigField::Mask0 => (&t.trigger0[stage], |c| c == b'X' || c == b'C'),
        TrigField::Mask1 => (&t.trigger1[stage], |c| c == b'X' || c == b'C'),
        TrigField::Value0 => (&t.trigger0[stage], |c| c == b'1' || c == b'R'),
        TrigField::Value1 => (&t.trigger1[stage], |c| c == b'1' || c == b'R'),
        TrigField::Edge0 => (&t.trigger0[stage], |c| c == b'R' || c == b'F' || c == b'C'),
        TrigField::Edge1 => (&t.trigger1[stage], |c| c == b'R' || c == b'F' || c == b'C'),
    };
    let mut v: u16 = 0;
    for i in (lsc..=msc).rev() {
        v = (v << 1).wrapping_add(u16::from(test(probes[i])));
    }
    let qutr_mask: u16 = 0xffff >> (TRIGGER_PROBES - TRIGGER_PROBES / 4);
    let half_mask: u16 = 0xffff >> (TRIGGER_PROBES - TRIGGER_PROBES / 2);
    let p = TRIGGER_PROBES as u32;
    if qutr {
        let m = v & qutr_mask;
        (m << (p / 4 * 3))
            .wrapping_add(m << (p / 4 * 2))
            .wrapping_add(m << (p / 4))
            .wrapping_add(m)
    } else if half {
        let m = v & half_mask;
        (m << (p / 2)).wrapping_add(m)
    } else {
        v
    }
}

// ------------------------------------------------------------ settings

/// `struct DSL_setting` (372 bytes, little-endian on the wire).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Setting {
    pub mode: u16,
    pub div_l: u16,
    pub div_h: u16,
    pub cnt_l: u16,
    pub cnt_h: u16,
    pub tpos_l: u16,
    pub tpos_h: u16,
    pub trig_glb: u16,
    pub dso_cnt_l: u16,
    pub dso_cnt_h: u16,
    pub ch_en_l: u16,
    pub ch_en_h: u16,
    pub fgain: u16,
    pub trig_mask0: [u16; NUM_TRIGGER_STAGES],
    pub trig_mask1: [u16; NUM_TRIGGER_STAGES],
    pub trig_value0: [u16; NUM_TRIGGER_STAGES],
    pub trig_value1: [u16; NUM_TRIGGER_STAGES],
    pub trig_edge0: [u16; NUM_TRIGGER_STAGES],
    pub trig_edge1: [u16; NUM_TRIGGER_STAGES],
    pub trig_logic0: [u16; NUM_TRIGGER_STAGES],
    pub trig_logic1: [u16; NUM_TRIGGER_STAGES],
    pub trig_count: [u32; NUM_TRIGGER_STAGES],
}

pub const SETTING_SIZE: usize = 372;
pub const SETTING_EXT32_SIZE: usize = 204;

impl Setting {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(SETTING_SIZE);
        let w16 = |b: &mut Vec<u8>, v: u16| b.extend_from_slice(&v.to_le_bytes());
        b.extend_from_slice(&0xf5a5_f5a5u32.to_le_bytes());
        for (header, values) in [
            (0x0001u16, &[self.mode][..]),
            (0x0102, &[self.div_l, self.div_h][..]),
            (0x0302, &[self.cnt_l, self.cnt_h][..]),
            (0x0502, &[self.tpos_l, self.tpos_h][..]),
            (0x0701, &[self.trig_glb][..]),
            (0x0802, &[self.dso_cnt_l, self.dso_cnt_h][..]),
            (0x0a02, &[self.ch_en_l, self.ch_en_h][..]),
            (0x0c01, &[self.fgain][..]),
        ] {
            w16(&mut b, header);
            for &v in values {
                w16(&mut b, v);
            }
        }
        w16(&mut b, 0x40a0);
        for arr in [
            &self.trig_mask0,
            &self.trig_mask1,
            &self.trig_value0,
            &self.trig_value1,
            &self.trig_edge0,
            &self.trig_edge1,
            &self.trig_logic0,
            &self.trig_logic1,
        ] {
            for &v in arr {
                w16(&mut b, v);
            }
        }
        for &v in &self.trig_count {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&0xfa5a_fa5au32.to_le_bytes());
        debug_assert_eq!(b.len(), SETTING_SIZE);
        b
    }
}

/// `struct DSL_setting_ext32` (204 bytes): trigger stages of probes 16-31.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingExt32 {
    pub trig_mask0: [u16; NUM_TRIGGER_STAGES],
    pub trig_mask1: [u16; NUM_TRIGGER_STAGES],
    pub trig_value0: [u16; NUM_TRIGGER_STAGES],
    pub trig_value1: [u16; NUM_TRIGGER_STAGES],
    pub trig_edge0: [u16; NUM_TRIGGER_STAGES],
    pub trig_edge1: [u16; NUM_TRIGGER_STAGES],
}

impl SettingExt32 {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(SETTING_EXT32_SIZE);
        b.extend_from_slice(&0xf5a5_f5a5u32.to_le_bytes());
        b.extend_from_slice(&0x6060u16.to_le_bytes());
        for arr in [
            &self.trig_mask0,
            &self.trig_mask1,
            &self.trig_value0,
            &self.trig_value1,
            &self.trig_edge0,
            &self.trig_edge1,
        ] {
            for &v in arr {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b.extend_from_slice(&0xffffu16.to_le_bytes());
        b.extend_from_slice(&0xfa5a_fa5au32.to_le_bytes());
        debug_assert_eq!(b.len(), SETTING_EXT32_SIZE);
        b
    }
}

/// Fills the trigger part of `s`/`x` from `t` (dsl_fpga_arm()'s trigger
/// section). `mode` is the setting's mode word, `caps` the feature caps.
pub fn fill_trigger(s: &mut Setting, x: &mut SettingExt32, t: &Trigger, mode: u16, caps: u64) {
    use TrigField::*;
    let adf = caps & feature::ADF4360 != 0;
    let quar_bit = mode & (1 << QUAR_MODE_BIT) != 0;
    let half_bit = mode & (1 << HALF_MODE_BIT) != 0;
    let rates = || (!adf && quar_bit, (!adf && half_bit) || (adf && quar_bit));
    let p = TRIGGER_PROBES;
    let mut stage_fill = |i: usize, stage: usize, qutr: bool, half: bool| {
        s.trig_mask0[i] = trigger_bits(t, Mask0, stage, p - 1, 0, qutr, half);
        s.trig_mask1[i] = trigger_bits(t, Mask1, stage, p - 1, 0, qutr, half);
        s.trig_value0[i] = trigger_bits(t, Value0, stage, p - 1, 0, qutr, half);
        s.trig_value1[i] = trigger_bits(t, Value1, stage, p - 1, 0, qutr, half);
        s.trig_edge0[i] = trigger_bits(t, Edge0, stage, p - 1, 0, qutr, half);
        s.trig_edge1[i] = trigger_bits(t, Edge1, stage, p - 1, 0, qutr, half);
        x.trig_mask0[i] = trigger_bits(t, Mask0, stage, 2 * p - 1, p, qutr, half);
        x.trig_mask1[i] = trigger_bits(t, Mask1, stage, 2 * p - 1, p, qutr, half);
        x.trig_value0[i] = trigger_bits(t, Value0, stage, 2 * p - 1, p, qutr, half);
        x.trig_value1[i] = trigger_bits(t, Value1, stage, 2 * p - 1, p, qutr, half);
        x.trig_edge0[i] = trigger_bits(t, Edge0, stage, 2 * p - 1, p, qutr, half);
        x.trig_edge1[i] = trigger_bits(t, Edge1, stage, 2 * p - 1, p, qutr, half);
        s.trig_logic0[i] =
            (u16::from(t.trigger_logic[stage]) << 1) + u16::from(t.trigger0_inv[stage]);
        s.trig_logic1[i] =
            (u16::from(t.trigger_logic[stage]) << 1) + u16::from(t.trigger1_inv[stage]);
        s.trig_count[i] = t.trigger0_count[stage];
    };
    if t.trigger_mode == dsview_core::SIMPLE_TRIGGER {
        let (qutr, half) = rates();
        stage_fill(0, TRIGGER_STAGES, qutr, half);
        for i in 1..NUM_TRIGGER_STAGES {
            s.trig_mask0[i] = 0xffff;
            s.trig_mask1[i] = 0xffff;
            s.trig_value0[i] = 0;
            s.trig_value1[i] = 0;
            s.trig_edge0[i] = 0;
            s.trig_edge1[i] = 0;
            x.trig_mask0[i] = 0xffff;
            x.trig_mask1[i] = 0xffff;
            x.trig_value0[i] = 0;
            x.trig_value1[i] = 0;
            x.trig_edge0[i] = 0;
            x.trig_edge1[i] = 0;
            s.trig_logic0[i] = 2;
            s.trig_logic1[i] = 2;
            s.trig_count[i] = 0;
        }
    } else {
        for i in 0..NUM_TRIGGER_STAGES {
            let (qutr, half) =
                if mode & (1 << STRIG_MODE_BIT) != 0 && i == dsview_core::S_TRIGGER_DATA_STAGE {
                    (false, false)
                } else {
                    rates()
                };
            stage_fill(i, i, qutr, half);
        }
    }
}

// ------------------------------------------------------- FX2 firmware

/// `ezusb_upload_firmware()` on a device opened with `Open::Configure(1)`:
/// CPU reset on, the image in 4 KiB pieces to RAM from address 0, CPU reset off.
pub fn ezusb_upload(usb: &mut dyn UsbDevice, path: &std::path::Path) -> Result<(), UsbError> {
    let image = std::fs::read(path).map_err(|e| {
        sr_err!(
            "Unable to open firmware file {} for reading: {e}",
            path.display()
        );
        UsbError::NotFound
    })?;
    let reset = |usb: &mut dyn UsbDevice, on: bool| {
        sr_info!(
            "setting CPU reset mode {}...",
            if on { "on" } else { "off" }
        );
        let setup = Setup {
            request_type: VENDOR_OUT,
            request: 0xa0,
            value: 0xe600,
            index: 0,
        };
        usb.control_out(setup, &[u8::from(on)], CTL_TIMEOUT)
            .inspect_err(|e| sr_err!("Unable to send control request: {e}."))
    };
    sr_info!("Uploading firmware at {}", path.display());
    reset(usb, true)?;
    for (i, chunk) in image.chunks(4096).enumerate() {
        let setup = Setup {
            request_type: VENDOR_OUT,
            request: 0xa0,
            value: (i * 4096) as u16,
            index: 0,
        };
        usb.control_out(setup, chunk, CTL_TIMEOUT)
            .inspect_err(|e| sr_err!("Unable to send firmware to device: {e}."))?;
        sr_info!("Uploaded {} bytes", chunk.len());
    }
    sr_info!("Firmware upload done");
    reset(usb, false)
}
