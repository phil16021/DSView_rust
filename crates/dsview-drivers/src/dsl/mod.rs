//! DreamSourceLab USB devices (libsigrok4DSL/hardware/DSL): what the DSLogic
//! and DSCope drivers share. The vendor protocol, the FX2 firmware and FPGA
//! uploads, the `DSL_setting` block that arms a capture, the security check
//! and the trigger masks.
//!
//! * [`device`]: the device both drivers share (`dsl.c`): open, arm,
//!   transfers, the data path of every mode, stop
//! * [`dslogic`]: the DSLogic driver (`dslogic.c`)
//! * [`dscope`]: the DSCope driver (`dscope.c`, also the Hermelin MSO-E8)
//! * [`tables`]: profiles, channel modes and lists, generated from `dsl.h`

pub mod device;
pub mod dscope;
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
    pub const DSO_EN0: u8 = 16;
    pub const DSO_DC0: u8 = 17;
    pub const DSO_ATT0: u8 = 18;
    pub const DSO_EN1: u8 = 19;
    pub const DSO_DC1: u8 = 20;
    pub const DSO_ATT1: u8 = 21;
    pub const AWG_WR: u8 = 22;
    pub const I2C_PROBE: u8 = 23;
    pub const I2C_EXT: u8 = 24;
}

// DSO channel enable bits (write only)
pub const BM_CH_CH0: u8 = 1 << 7;
pub const BM_CH_COM: u8 = 1 << 6;
pub const BM_CH_CH1: u8 = 1 << 5;

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
pub const CTR1_ADDR: u8 = 0x71;
pub const CTR0_ADDR: u8 = 0x70;
pub const COMB_ADDR: u8 = 0x68;
pub const EI2C_ADDR: u8 = 0x60;
pub const ADCC_ADDR: u8 = 0x48;
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
pub const BM_EEWP: u8 = 1 << 0;
pub const BM_BW20M_SET: u8 = 1 << 5;
pub const BM_BW20M_CLR: u8 = 1 << 6;

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

// ------------------------------------------------------------------ DSO

/// `DSO_PKTID`: the id of a DSO status block.
pub const DSO_PKTID: u16 = 0xa500;
/// NVM pages per channel on devices with a flash (`DSO_ZERO_PAGE`).
pub const DSO_ZERO_PAGE: u16 = 8;
pub const MAX_ACC_VARIANCE: f64 = 0.0005;
pub const DSCOPE_CONSTANT_BIAS: i32 = 160;
pub const DSCOPE_TRANS_CMULTI: i32 = 10;
pub const DSCOPE_TRANS_FMULTI: f64 = 100.0;
pub const CALI_VGAIN_RANGE: u16 = 600;
/// `DS_CONF_DSO_VDIVS`: vertical divisions on screen.
pub const DS_CONF_DSO_VDIVS: u64 = 10;
/// `MAX_EMPTY_POLL_E8`
pub const MAX_EMPTY_POLL_E8: u32 = 2;
/// DSO_E8-1 (Hermelin MSO-E8): its VGA table id, zero code and logic depth.
pub const DSL_VGA_ID_E8: u8 = 0xE8;
pub const DSL_E8_ZERO_CODE: u16 = 128;
pub const DSL_E8_LOGIC_DEPTH: u64 = 8 * 1024 * 1024;

/// `dsl_wr_dso()`: one 64-bit DSO command, little endian.
pub fn wr_dso(usb: &mut dyn UsbDevice, cmd: u64) -> CmdResult<()> {
    ctl_wr(usb, dest::I2C_DSO, 0, &cmd.to_le_bytes()).inspect_err(|_| {
        sr_err!("Sent DSL_CTL_I2C_DSO command failed.");
    })
}

/// `dsl_wr_nvm()`
pub fn wr_nvm(usb: &mut dyn UsbDevice, data: &[u8], addr: u16) -> CmdResult<()> {
    ctl_wr(usb, dest::NVM, addr, data).inspect_err(|_| {
        sr_err!("Sent DSL_CTL_NVM write command failed.");
    })
}

/// `struct DSL_adc_config`: `cnt` bytes to register `dest`, after `delay` ms.
#[derive(Clone, Copy, Debug)]
pub struct AdcConfig {
    pub dest: u8,
    pub cnt: u8,
    pub delay: u8,
    pub bytes: [u8; 4],
}

const fn adc(dest: u8, cnt: u8, delay: u8, bytes: [u8; 4]) -> AdcConfig {
    AdcConfig {
        dest,
        cnt,
        delay,
        bytes,
    }
}

/// The HMCAD1511 ADC set-ups of `dsl.h`.
pub mod adc {
    use super::{ADCC_ADDR as A, AdcConfig, adc};

    pub static SINGLE_CH0: [AdcConfig; 12] = [
        adc(A + 1, 3, 0, [0x03, 0x01, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x01, 0x00, 0x31]),
        adc(A + 1, 1, 0, [0x01, 0x00, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x02, 0x02, 0x3A]),
        adc(A, 4, 0, [0x00, 0x02, 0x02, 0x3B]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x42]),
        adc(A, 4, 0, [0x00, 0x34, 0x00, 0x50]),
        adc(A, 4, 0, [0x00, 0x22, 0x02, 0x11]),
        adc(A, 4, 0, [0x00, 0x7F, 0x00, 0x24]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x55]),
        adc(A, 4, 0, [0x00, 0x02, 0x00, 0x33]),
        adc(A, 4, 0, [0x00, 0x00, 0x03, 0x2B]),
    ];
    pub static SINGLE_CH3: [AdcConfig; 12] = [
        adc(A + 1, 3, 0, [0x03, 0x01, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x01, 0x00, 0x31]),
        adc(A + 1, 1, 0, [0x01, 0x00, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x10, 0x10, 0x3A]),
        adc(A, 4, 0, [0x00, 0x10, 0x10, 0x3B]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x42]),
        adc(A, 4, 0, [0x00, 0x34, 0x00, 0x50]),
        adc(A, 4, 0, [0x00, 0x22, 0x02, 0x11]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x24]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x55]),
        adc(A, 4, 0, [0x00, 0x02, 0x00, 0x33]),
        adc(A, 4, 0, [0x00, 0x00, 0x03, 0x2B]),
    ];
    pub static DUAL_CH03: [AdcConfig; 12] = [
        adc(A + 1, 3, 0, [0x03, 0x01, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x02, 0x01, 0x31]),
        adc(A + 1, 1, 0, [0x01, 0x00, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x02, 0x02, 0x3A]),
        adc(A, 4, 0, [0x00, 0x10, 0x10, 0x3B]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x42]),
        adc(A, 4, 0, [0x00, 0x34, 0x00, 0x50]),
        adc(A, 4, 0, [0x00, 0x22, 0x02, 0x11]),
        adc(A, 4, 0, [0x00, 0x11, 0x00, 0x24]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x55]),
        adc(A, 4, 0, [0x00, 0x02, 0x00, 0x33]),
        adc(A, 4, 0, [0x00, 0x33, 0x00, 0x2B]),
    ];
    pub static INIT_FIX: [AdcConfig; 10] = [
        adc(A + 1, 3, 0, [0x03, 0x01, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x02, 0x01, 0x31]),
        adc(A + 1, 1, 0, [0x01, 0x00, 0x00, 0x00]),
        adc(A, 4, 0, [0x00, 0x02, 0x02, 0x3A]),
        adc(A, 4, 0, [0x00, 0x10, 0x10, 0x3B]),
        adc(A, 4, 0, [0x00, 0x00, 0x00, 0x42]),
        adc(A, 4, 0, [0x00, 0x34, 0x00, 0x50]),
        adc(A, 4, 0, [0x00, 0x22, 0x02, 0x11]),
        adc(A, 4, 0, [0x00, 0x10, 0x00, 0x25]),
        adc(A, 4, 0, [0x00, 0x00, 0x55, 0x26]),
    ];
    pub static CLK_INIT_1G: [AdcConfig; 4] = [
        adc(A + 2, 1, 0, [0x01, 0x00, 0x00, 0x00]),
        adc(A, 4, 0, [0x01, 0x61, 0x00, 0x30]),
        adc(A, 4, 0, [0x01, 0x40, 0xF1, 0x46]),
        adc(A, 4, 10, [0x01, 0x62, 0x3D, 0x00]),
    ];
    pub static CLK_INIT_500M: [AdcConfig; 4] = [
        adc(A + 2, 1, 0, [0x01, 0x00, 0x00, 0x00]),
        adc(A, 4, 0, [0x01, 0x61, 0x00, 0x30]),
        adc(A, 4, 0, [0x01, 0x40, 0xF1, 0x46]),
        adc(A, 4, 10, [0x01, 0x62, 0x3D, 0x40]),
    ];
    pub static POWER_DOWN: [AdcConfig; 1] = [adc(A + 1, 1, 0, [0x00, 0x00, 0x00, 0x00])];
    pub static POWER_UP: [AdcConfig; 1] = [adc(A + 1, 1, 0, [0x01, 0x00, 0x00, 0x00])];
}

/// `dsl_config_adc()`: register writes whose results C ignores.
pub fn config_adc(usb: &mut dyn UsbDevice, config: &[AdcConfig]) {
    for c in config {
        if c.delay > 0 {
            sys::clock().sleep_us(u64::from(c.delay) * 1000);
        }
        for &b in &c.bytes[..c.cnt as usize] {
            let _ = wr_reg(usb, c.dest, b);
        }
    }
}

/// `dsl_adc_code2fgain()`
pub fn adc_code2fgain(code: u8) -> f64 {
    let xcode = if code & 0x40 != 0 {
        -f64::from(!code & 0x3F)
    } else {
        f64::from(code & 0x3F)
    };
    1.0 + xcode / f64::from(1 << 13)
}

/// `dsl_adc_fgain2code()`
pub fn adc_fgain2code(gain: f64) -> u8 {
    let xratio = c_i32((gain - 1.0) * f64::from(1 << 13));
    if xratio > 63 {
        63
    } else if xratio > 0 {
        xratio as u8
    } else if xratio < -63 {
        64
    } else {
        !(xratio.wrapping_neg() as u8) & 0x7F
    }
}

/// `dsl_config_adc_fgain()`: fine gains of one ADC branch.
pub fn config_adc_fgain(usb: &mut dyn UsbDevice, branch: u8, gain0: f64, gain1: f64) {
    let _ = wr_reg(usb, ADCC_ADDR, 0x00);
    let _ = wr_reg(usb, ADCC_ADDR, adc_fgain2code(gain0));
    let _ = wr_reg(usb, ADCC_ADDR, adc_fgain2code(gain1));
    let _ = wr_reg(usb, ADCC_ADDR, 0x34u8.wrapping_add(branch));
}

/// `struct DSL_vga`: gain and offset calibration of one V/div step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vga {
    pub id: u8,
    pub key: u64,
    pub vgain: u64,
    pub preoff: u16,
    pub preoff_comp: u16,
}

const fn vga(id: u8, key: u64, vgain: u64, preoff: u16, preoff_comp: u16) -> Vga {
    Vga {
        id,
        key,
        vgain,
        preoff,
        preoff_comp,
    }
}

/// `vga_defaults[]` (without the terminator).
pub static VGA_DEFAULTS: [Vga; 41] = [
    vga(1, 10, 0x162400, (32 << 10) + 558, (32 << 10) + 558),
    vga(1, 20, 0x14C000, (32 << 10) + 558, (32 << 10) + 558),
    vga(1, 50, 0x12E800, (32 << 10) + 558, (32 << 10) + 558),
    vga(1, 100, 0x118000, (32 << 10) + 558, (32 << 10) + 558),
    vga(1, 200, 0x102400, (32 << 10) + 558, (32 << 10) + 558),
    vga(1, 500, 0x2E800, (32 << 10) + 558, (32 << 10) + 558),
    vga(1, 1000, 0x18000, (32 << 10) + 558, (32 << 10) + 558),
    vga(1, 2000, 0x02400, (32 << 10) + 558, (32 << 10) + 558),
    vga(2, 10, 0x1DA800, 45, 1024 - 920 - 45),
    vga(2, 20, 0x1A7200, 45, 1024 - 920 - 45),
    vga(2, 50, 0x164200, 45, 1024 - 920 - 45),
    vga(2, 100, 0x131800, 45, 1024 - 920 - 45),
    vga(2, 200, 0xBD000, 45, 1024 - 920 - 45),
    vga(2, 500, 0x7AD00, 45, 1024 - 920 - 45),
    vga(2, 1000, 0x48800, 45, 1024 - 920 - 45),
    vga(2, 2000, 0x12000, 45, 1024 - 920 - 45),
    vga(3, 10, 0x1C5C00, 45, 1024 - 920 - 45),
    vga(3, 20, 0x19EB00, 45, 1024 - 920 - 45),
    vga(3, 50, 0x16AE00, 45, 1024 - 920 - 45),
    vga(3, 100, 0x143D00, 45, 1024 - 920 - 45),
    vga(3, 200, 0xB1000, 45, 1024 - 920 - 45),
    vga(3, 500, 0x7F000, 45, 1024 - 920 - 45),
    vga(3, 1000, 0x57200, 45, 1024 - 920 - 45),
    vga(3, 2000, 0x2DD00, 45, 1024 - 920 - 45),
    vga(4, 10, 0x1C6C00, 60, 1024 - 900 - 60),
    vga(4, 20, 0x19E000, 60, 1024 - 900 - 60),
    vga(4, 50, 0x16A800, 60, 1024 - 900 - 60),
    vga(4, 100, 0x142800, 60, 1024 - 900 - 60),
    vga(4, 200, 0xC7F00, 60, 1024 - 900 - 60),
    vga(4, 500, 0x94000, 60, 1024 - 900 - 60),
    vga(4, 1000, 0x6CF00, 60, 1024 - 900 - 60),
    vga(4, 2000, 0x44F00, 60, 1024 - 900 - 60),
    vga(5, 10, 0x1C3400, 60, 1024 - 900 - 60),
    vga(5, 20, 0x19BD00, 60, 1024 - 900 - 60),
    vga(5, 50, 0x167400, 60, 1024 - 900 - 60),
    vga(5, 100, 0x13F300, 60, 1024 - 900 - 60),
    vga(5, 200, 0xC4F00, 60, 1024 - 900 - 60),
    vga(5, 500, 0x91B00, 60, 1024 - 900 - 60),
    vga(5, 1000, 0x69D00, 60, 1024 - 900 - 60),
    vga(5, 2000, 0x41D00, 60, 1024 - 900 - 60),
    // DSO_E8-1: its only range. vdivs_dso_e8[] says 218, so no lookup by
    // V/div finds this entry (C has the same mismatch, F24).
    vga(DSL_VGA_ID_E8, 182, 0, 0, 0),
];

/// The `vga_defaults[]` entry for this VGA id and V/div.
pub fn vga_default(id: u8, key: u64) -> Option<&'static Vga> {
    VGA_DEFAULTS.iter().find(|v| v.id == id && v.key == key)
}

// ------------------------------------------- C conversions (x86-64, gcc)

/// `(int)x`: truncation; out of range and NaN give INT_MIN (cvttsd2si).
pub fn c_i32(x: f64) -> i32 {
    if x.is_nan() || x >= 2147483648.0 || x <= -2147483649.0 {
        i32::MIN
    } else {
        x as i32
    }
}

/// `(int64_t)x`, same rule.
pub fn c_i64(x: f64) -> i64 {
    const TWO63: f64 = (1u64 << 63) as f64;
    if x.is_nan() || x >= TWO63 || x < -TWO63 {
        i64::MIN
    } else {
        x as i64
    }
}

/// `(uint64_t)x` as gcc compiles it: below 2^63 through int64, above by
/// subtracting 2^63 first.
pub fn c_u64(x: f64) -> u64 {
    const TWO63: f64 = (1u64 << 63) as f64;
    if x < TWO63 || x.is_nan() {
        c_i64(x) as u64
    } else {
        (c_i64(x - TWO63) as u64) ^ (1 << 63)
    }
}

/// `(uint32_t)x`: through int64.
pub fn c_u32(x: f64) -> u32 {
    c_i64(x) as u32
}

/// `(uint16_t)x` / `(uint8_t)x` / `(int8_t)x`: through int.
pub fn c_u16(x: f64) -> u16 {
    c_i32(x) as u16
}
pub fn c_u8(x: f64) -> u8 {
    c_i32(x) as u8
}
pub fn c_i8(x: f64) -> i8 {
    c_i32(x) as i8
}
