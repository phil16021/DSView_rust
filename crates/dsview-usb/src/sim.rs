//! A simulated DreamSourceLab device running the DSL firmware: the vendor
//! protocol (CTL_WR / CTL_RD_PRE / CTL_RD), its registers, NVM and DSO
//! command port, the arm block on EP2 and the data on EP6 (trigger header,
//! then logic, DSO frames with their status block, or DAQ data).
//!
//! It exists to run the C and the Rust driver against the *same* device
//! (the DSCope and E8 have no hardware here): the C driver through a fake
//! libusb in the oracle, the Rust one through [`SimBackend`]. Both sides see
//! one deterministic device and the [`Hub`] records what they sent; the
//! records must match.
//!
//! Bulk IN completes in *ticks*: a tick takes every queued transfer, oldest
//! first, that the device has data for. C ticks once per
//! `libusb_handle_events*()`, Rust once per `bulk_in_wait` with a timeout;
//! transfers submitted from a completion wait for the next tick, as with a
//! real host controller.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::{InCompletion, Info, Open, Setup, Speed, UsbBackend, UsbDevice, UsbError};

/// What kind of device to simulate.
#[derive(Clone, Debug)]
pub struct SimConfig {
    pub vid: u16,
    pub pid: u16,
    pub speed: Speed,
    pub bus: u8,
    pub address: u8,
    pub manufacturer: String,
    pub product: String,
    /// Trigger header size: 1024 on USB 3 devices, else 512.
    pub header_size: usize,
    /// The FPGA runs already (no bitstream upload on open).
    pub fpga_done: bool,
    pub hdl_version: u8,
    pub fw_version: [u8; 2],
    /// NVM contents: blocks by start address. The DSL firmware addresses
    /// some NVMs in pages, so a read returns the block written at exactly
    /// that address (0xff where none was).
    pub nvm: std::collections::BTreeMap<u16, Vec<u8>>,
    /// Added to each channel's mean level (ADC codes) in the status block:
    /// 0 is a perfectly calibrated front end.
    pub bias: [f64; 2],
    /// Seed of the sample data.
    pub seed: u64,
}

impl SimConfig {
    /// A device of this USB identity with DreamSourceLab's firmware strings
    /// (Hermelin's for the MSO-E8), an empty NVM and a configured FPGA.
    pub fn new(vid: u16, pid: u16, speed: Speed) -> SimConfig {
        let e8 = vid == 0x2A0E && pid == 0x00E8;
        SimConfig {
            vid,
            pid,
            speed,
            bus: 1,
            address: 7,
            manufacturer: if e8 {
                "Hermelin Labs"
            } else {
                "DreamSourceLab"
            }
            .into(),
            product: if e8 {
                "Hermelin MSO-E8"
            } else {
                "USB-based DSL Instrument v2"
            }
            .into(),
            header_size: if speed == Speed::Super { 1024 } else { 512 },
            fpga_done: true,
            hdl_version: 0x0E,
            fw_version: [2, 0],
            nvm: Default::default(),
            bias: [0.0; 2],
            seed: 1,
        }
    }
}

const CMD_CTL_WR: u8 = 0xb0;
const CMD_CTL_RD_PRE: u8 = 0xb1;
const CMD_CTL_RD: u8 = 0xb2;

// DSL_CTL_* destinations
const D_FW_VERSION: u8 = 0;
const D_HW_STATUS: u8 = 2;
const D_START: u8 = 8;
const D_STOP: u8 = 9;
const D_BULK_WR: u8 = 10;
const D_NVM: u8 = 12;
const D_I2C_DSO: u8 = 13;
const D_I2C_REG: u8 = 14;
const D_I2C_STATUS: u8 = 15;

const CTR0_ADDR: u8 = 0x70;
const BM_FORCE_RDY: u8 = 1 << 1;
const BM_FORCE_STOP: u8 = 1 << 2;

/// The DSL_setting fields the device acts on.
#[derive(Clone, Copy, Debug, Default)]
struct Armed {
    mode: u16,
    ch_en: u32,
    dso_cnt: u32,
}

/// The device: protocol state and the data it produces.
pub struct DslSim {
    cfg: SimConfig,
    regs: [u8; 256],
    pending_rd: Option<(u8, u16, u8)>,
    bulk_expect: usize,
    fpga_done: bool,
    armed: Armed,
    running: bool,
    header_sent: bool,
    frames: u64,
    /// DSO channel offset codes (bits 40.. of the last offset command).
    dso_offset: [u8; 2],
    /// Sample divider of the last DSO samplerate command.
    divider: u32,
    /// No data for now (a capture waiting for its trigger).
    pub hold: bool,
}

/// One USB request seen by the device, as text (the transcript).
pub type Log = Vec<String>;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl DslSim {
    pub fn new(cfg: SimConfig) -> DslSim {
        let fpga_done = cfg.fpga_done;
        let mut regs = [0u8; 256];
        regs[4] = cfg.hdl_version;
        // security block: ready and passed
        regs[0x73] = (1 << 3) | (1 << 4);
        DslSim {
            cfg,
            regs,
            pending_rd: None,
            bulk_expect: 0,
            fpga_done,
            armed: Armed::default(),
            running: false,
            header_sent: false,
            frames: 0,
            dso_offset: [0x80; 2],
            divider: 1,
            hold: false,
        }
    }

    pub fn config(&self) -> &SimConfig {
        &self.cfg
    }

    /// The NVM as the drivers left it.
    pub fn nvm(&self) -> &std::collections::BTreeMap<u16, Vec<u8>> {
        &self.cfg.nvm
    }

    fn hw_status(&self) -> u8 {
        // GPIF done, INIT_B, SYS_CLR; FPGA done once configured
        let mut v = (1 << 7) | (1 << 5) | (1 << 3);
        if self.fpga_done {
            v |= 1 << 6;
        }
        v
    }

    fn ctl_wr(&mut self, dest: u8, offset: u16, data: &[u8]) {
        match dest {
            D_START => {
                self.running = true;
                self.header_sent = false;
                self.frames = 0;
            }
            D_STOP => self.running = false,
            D_BULK_WR => {
                let n = data
                    .iter()
                    .take(3)
                    .enumerate()
                    .fold(0usize, |a, (i, &b)| a | usize::from(b) << (8 * i));
                self.bulk_expect = n;
            }
            D_NVM => {
                self.cfg.nvm.insert(offset, data.to_vec());
            }
            D_I2C_REG => {
                let v = data.first().copied().unwrap_or(0);
                self.regs[usize::from(offset as u8)] = v;
                if offset as u8 == CTR0_ADDR && v & (BM_FORCE_RDY | BM_FORCE_STOP) != 0 {
                    // the capture ends; queued data still drains
                }
            }
            D_I2C_DSO => {
                let mut b = [0u8; 8];
                b[..data.len().min(8)].copy_from_slice(&data[..data.len().min(8)]);
                let cmd = u64::from_le_bytes(b);
                let ch = ((cmd >> 7) & 1) as usize;
                match cmd & 0x7f {
                    0x10 => self.dso_offset[ch] = (cmd >> 40) as u8,
                    0x18 => self.divider = ((cmd >> 8) & 0x00ff_ffff) as u32,
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn ctl_rd(&mut self, dest: u8, offset: u16, size: u8) -> Vec<u8> {
        let n = usize::from(size);
        match dest {
            D_FW_VERSION => {
                let mut v = self.cfg.fw_version.to_vec();
                v.resize(n, 0);
                v
            }
            D_HW_STATUS => vec![self.hw_status(); n.max(1)][..n].to_vec(),
            D_NVM => {
                let block = self.cfg.nvm.get(&offset).map_or(&[][..], Vec::as_slice);
                (0..n)
                    .map(|i| block.get(i).copied().unwrap_or(0xff))
                    .collect()
            }
            D_I2C_STATUS => {
                let mut st = self.regs;
                st[4] = self.cfg.hdl_version;
                st[5] = self.hw_status();
                (0..n)
                    .map(|i| st.get(usize::from(offset) + i).copied().unwrap_or(0))
                    .collect()
            }
            _ => vec![0; n],
        }
    }

    fn control_out(&mut self, s: Setup, data: &[u8]) -> Result<String, UsbError> {
        match s.request {
            CMD_CTL_WR if data.len() >= 4 => {
                let dest = data[0];
                let offset = u16::from_le_bytes([data[1], data[2]]);
                let body = &data[4..];
                self.ctl_wr(dest, offset, body);
                Ok(format!("wr {dest} @{offset} {}", hex(body)))
            }
            CMD_CTL_RD_PRE if data.len() >= 4 => {
                let offset = u16::from_le_bytes([data[1], data[2]]);
                self.pending_rd = Some((data[0], offset, data[3]));
                Ok(format!("rd {} @{offset} #{}", data[0], data[3]))
            }
            // FX2 firmware upload (vendor request 0xa0)
            0xa0 => Ok(format!("fx2 {:04x} #{}", s.value, data.len())),
            r => Ok(format!(
                "ctl_out {:02x}/{r:02x} {:04x} {:04x} {}",
                s.request_type,
                s.value,
                s.index,
                hex(data)
            )),
        }
    }

    fn control_in(&mut self, s: Setup, len: u16) -> Result<(Vec<u8>, String), UsbError> {
        if s.request == CMD_CTL_RD {
            let Some((dest, offset, size)) = self.pending_rd.take() else {
                return Err(UsbError::Stall);
            };
            let mut v = self.ctl_rd(dest, offset, size);
            v.truncate(usize::from(len));
            let log = format!("<- {}", hex(&v));
            return Ok((v, log));
        }
        Err(UsbError::Stall)
    }

    fn bulk_out(&mut self, ep: u8, data: &[u8]) -> String {
        if data.len() == 372 {
            let w16 = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
            // DSL_setting: mode at 6, dso_cnt at 32/34, ch_en at 38/40
            self.armed = Armed {
                mode: w16(6),
                ch_en: u32::from(w16(38)) | u32::from(w16(40)) << 16,
                dso_cnt: u32::from(w16(32)) | u32::from(w16(34)) << 16,
            };
        } else if self.bulk_expect != 0 && data.len() == self.bulk_expect {
            // an FPGA bitstream
            self.fpga_done = true;
        }
        format!("bulk {ep} #{} {:016x}", data.len(), fnv(data))
    }

    /// Whether a transfer of `len` bytes would complete now.
    fn can_fill(&self, _len: usize) -> bool {
        self.running && !self.hold
    }

    /// The data for the next IN transfer of `len` bytes.
    fn fill(&mut self, len: usize) -> Vec<u8> {
        if !self.header_sent {
            self.header_sent = true;
            let mut v = vec![0u8; len];
            let words = [0x5555_5555u32, 0, 0, 0, 0, 0];
            for (i, w) in words.iter().enumerate() {
                if 4 * i + 4 <= len {
                    v[4 * i..4 * i + 4].copy_from_slice(&w.to_le_bytes());
                }
            }
            return v;
        }
        self.frames += 1;
        let dso = self.armed.mode & (1 << 4) != 0;
        let mut v = self.pattern(len);
        if dso && len > self.cfg.header_size {
            let at = len - self.cfg.header_size;
            self.dso_status(&mut v[at..]);
        }
        v
    }

    /// Deterministic sample bytes, different per frame.
    fn pattern(&self, len: usize) -> Vec<u8> {
        let mut x = self.cfg.seed ^ self.frames.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    /// The status block behind a DSO frame (see dsl.c get_measure()).
    fn dso_status(&self, s: &mut [u8]) {
        let en = self.armed.ch_en.count_ones().max(1);
        // the driver's limit_samples: dso_cnt = limit / (2 channels / en)
        let limit = u64::from(self.armed.dso_cnt) * u64::from(2 / en.min(2));
        let put16 =
            |s: &mut [u8], at: usize, v: u16| s[at..at + 2].copy_from_slice(&v.to_le_bytes());
        let put32 =
            |s: &mut [u8], at: usize, v: u32| s[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let n = s.len().min(184);
        s[..n].fill(0);
        put16(s, 0, 0xa500);
        // vlen: half the frame's sample bytes (2 channel slots)
        let vlen = (u64::from(self.armed.dso_cnt) * u64::from(en) / 2).max(1) as u32;
        put32(s, 4, (vlen & 0x0fff_ffff) | 0x4000_0000);
        put32(s, 8, self.divider & 0x00ff_ffff);
        for ch in 0..2 {
            let level = f64::from(self.dso_offset[ch]) + self.cfg.bias[ch];
            let quarter = (level * limit as f64 / 4.0).round().max(0.0) as u32;
            let base = if ch == 0 { 104 } else { 168 };
            for k in 0..4 {
                put32(s, base + 4 * k, quarter);
            }
            s[if ch == 0 { 66 } else { 130 }] = self.dso_offset[ch].wrapping_add(20);
            s[if ch == 0 { 67 } else { 131 }] = self.dso_offset[ch].wrapping_sub(20);
            s[if ch == 0 { 102 } else { 166 }] = self.dso_offset[ch];
        }
    }
}

/// FNV-1a over a bulk payload (the transcript records it, not the bytes).
fn fnv(d: &[u8]) -> u64 {
    d.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

/// A completed IN transfer: who submitted it and what it got.
#[derive(Debug)]
pub struct Done {
    pub id: u64,
    pub data: Vec<u8>,
    pub status: Result<(), UsbError>,
}

/// The device plus its IN transfer queue and the transcript of everything
/// the host did.
pub struct Hub {
    pub dev: DslSim,
    queue: VecDeque<(u64, usize)>,
    ready: VecDeque<Done>,
    pub log: Log,
    open: bool,
    /// Plugged in (an unplugged device fails every request).
    present: bool,
    /// Called after each tick with the number of transfers it completed.
    pub on_tick: Option<Box<dyn FnMut(usize) + Send>>,
}

impl Hub {
    pub fn new(cfg: SimConfig) -> Hub {
        Hub {
            dev: DslSim::new(cfg),
            queue: VecDeque::new(),
            ready: VecDeque::new(),
            log: Vec::new(),
            open: false,
            present: true,
            on_tick: None,
        }
    }

    pub fn present(&self) -> bool {
        self.present
    }

    /// Unplugs (`false`) or plugs the device in again, then at `address`;
    /// a plug-in powers it up anew (its capture state is gone).
    pub fn set_present(&mut self, present: bool, address: Option<u8>) {
        self.present = present;
        if let Some(a) = address {
            self.dev.cfg.address = a;
        }
        if present {
            self.dev.running = false;
            self.dev.hold = false;
        }
    }

    pub fn info(&self) -> Info {
        let c = self.dev.config();
        Info {
            bus: c.bus,
            address: c.address,
            vid: c.vid,
            pid: c.pid,
            speed: c.speed,
            manufacturer: Some(c.manufacturer.clone()),
            product: Some(c.product.clone()),
        }
    }

    /// A handle was opened (C also opens one to read the strings: not
    /// part of the transcript).
    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.queue.clear();
        self.ready.clear();
    }

    pub fn control_out(&mut self, s: Setup, data: &[u8]) -> Result<(), UsbError> {
        if !self.present {
            return Err(UsbError::Disconnected);
        }
        let line = self.dev.control_out(s, data)?;
        self.log.push(line);
        Ok(())
    }

    pub fn control_in(&mut self, s: Setup, len: u16) -> Result<Vec<u8>, UsbError> {
        if !self.present {
            return Err(UsbError::Disconnected);
        }
        let (v, line) = self.dev.control_in(s, len)?;
        self.log.push(line);
        Ok(v)
    }

    pub fn bulk_out(&mut self, ep: u8, data: &[u8]) -> usize {
        if !self.present {
            return 0;
        }
        let line = self.dev.bulk_out(ep, data);
        self.log.push(line);
        data.len()
    }

    pub fn submit(&mut self, id: u64, len: usize) {
        self.queue.push_back((id, len));
    }

    /// Whether the next tick completes anything.
    pub fn can_complete(&self) -> bool {
        self.queue
            .front()
            .is_some_and(|&(_, len)| !self.present || self.dev.can_fill(len))
    }

    /// A tick: completes the queued transfers the device has data for.
    pub fn tick(&mut self) {
        let mut n = 0;
        if !self.present {
            // gone: every queued transfer fails
            for (id, _) in self.queue.drain(..) {
                self.ready.push_back(Done {
                    id,
                    data: Vec::new(),
                    status: Err(UsbError::Disconnected),
                });
                n += 1;
            }
        }
        while let Some(&(id, len)) = self.queue.front() {
            if !self.dev.can_fill(len) {
                break;
            }
            self.queue.pop_front();
            let data = self.dev.fill(len);
            self.log.push(format!("in #{len} {:016x}", fnv(&data)));
            self.ready.push_back(Done {
                id,
                data,
                status: Ok(()),
            });
            n += 1;
        }
        if let Some(f) = self.on_tick.as_mut() {
            f(n);
        }
    }

    /// The next transfer the last tick completed.
    pub fn take_ready(&mut self) -> Option<Done> {
        self.ready.pop_front()
    }

    /// Cancels one queued transfer (`None`: all of them); they complete as
    /// cancelled at the next take.
    pub fn cancel(&mut self, id: Option<u64>) {
        let (gone, keep): (Vec<_>, Vec<_>) = self
            .queue
            .drain(..)
            .partition(|&(i, _)| id.is_none_or(|x| x == i));
        self.queue = keep.into();
        for (i, _) in gone {
            self.log.push("cancel".into());
            self.ready.push_back(Done {
                id: i,
                data: Vec::new(),
                status: Err(UsbError::Cancelled),
            });
        }
    }

    pub fn pending(&self) -> usize {
        self.queue.len() + self.ready.len()
    }
}

/// Shared handle on a [`Hub`].
pub type SharedHub = Arc<Mutex<Hub>>;

/// What an idle wait does with the time (the oracle's virtual clock).
pub type Idle = Arc<dyn Fn(Duration) + Send + Sync>;

/// The simulated device as a [`UsbBackend`] for the Rust drivers.
pub struct SimBackend {
    pub hub: SharedHub,
    pub idle: Idle,
    /// Hotplug watchers (`watch`).
    pub watchers: Mutex<Vec<std::sync::mpsc::Sender<crate::Hotplug>>>,
}

impl SimBackend {
    pub fn new(hub: SharedHub, idle: Idle) -> SimBackend {
        SimBackend {
            hub,
            idle,
            watchers: Mutex::new(Vec::new()),
        }
    }

    /// Unplugs the device, or plugs it in (at `address`), and tells the
    /// watchers.
    pub fn plug(&self, present: bool, address: Option<u8>) {
        let ev = {
            let mut h = self.hub.lock().unwrap();
            let before = h.info();
            h.set_present(present, address);
            if present {
                crate::Hotplug::Arrived(h.info())
            } else {
                crate::Hotplug::Left {
                    bus: before.bus,
                    address: before.address,
                }
            }
        };
        self.watchers
            .lock()
            .unwrap()
            .retain(|w| w.send(ev.clone()).is_ok());
    }
}

impl UsbBackend for SimBackend {
    fn list(&self) -> Result<Vec<Info>, UsbError> {
        let h = self.hub.lock().unwrap();
        Ok(if h.present() {
            vec![h.info()]
        } else {
            Vec::new()
        })
    }
    fn watch(&self) -> Option<std::sync::mpsc::Receiver<crate::Hotplug>> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.watchers.lock().unwrap().push(tx);
        Some(rx)
    }
    fn open(&self, bus: u8, address: u8, _how: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
        let mut h = self.hub.lock().unwrap();
        let i = h.info();
        if !h.present() || (i.bus, i.address) != (bus, address) {
            return Err(UsbError::NotFound);
        }
        h.open();
        Ok(Box::new(SimDevice {
            hub: self.hub.clone(),
            idle: self.idle.clone(),
            next_id: 0,
        }))
    }
}

/// An opened simulated device.
pub struct SimDevice {
    hub: SharedHub,
    idle: Idle,
    next_id: u64,
}

impl Drop for SimDevice {
    fn drop(&mut self) {
        self.hub.lock().unwrap().close();
    }
}

impl UsbDevice for SimDevice {
    fn control_out(&mut self, s: Setup, data: &[u8], _t: Duration) -> Result<(), UsbError> {
        self.hub.lock().unwrap().control_out(s, data)
    }
    fn control_in(&mut self, s: Setup, len: u16, _t: Duration) -> Result<Vec<u8>, UsbError> {
        self.hub.lock().unwrap().control_in(s, len)
    }
    fn bulk_out(&mut self, ep: u8, data: &[u8], _t: Duration) -> Result<usize, UsbError> {
        Ok(self.hub.lock().unwrap().bulk_out(ep, data))
    }
    fn bulk_in_submit(&mut self, _ep: u8, len: usize) -> Result<(), UsbError> {
        self.next_id += 1;
        self.hub.lock().unwrap().submit(self.next_id, len);
        Ok(())
    }
    fn bulk_in_wait(&mut self, _ep: u8, timeout: Duration) -> Option<InCompletion> {
        let mut h = self.hub.lock().unwrap();
        if !timeout.is_zero() {
            h.tick();
        }
        match h.take_ready() {
            Some(d) => Some(InCompletion {
                data: d.data,
                status: d.status,
            }),
            None => {
                drop(h);
                if !timeout.is_zero() {
                    (self.idle)(timeout);
                }
                None
            }
        }
    }
    fn bulk_in_pending(&self, _ep: u8) -> usize {
        self.hub.lock().unwrap().pending()
    }
    fn bulk_in_cancel_all(&mut self, _ep: u8) {
        self.hub.lock().unwrap().cancel(None);
    }
}
