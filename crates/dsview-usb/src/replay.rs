//! A device that plays back a usbmon capture of the C driver.
//!
//! The capture's vendor requests and bulk OUT transfers form a script the
//! driver must follow: each control OUT, control IN request and bulk OUT is
//! compared with the next script step, and control IN returns the recorded
//! answer. Bulk IN completions are handed out in recorded order, each only
//! after the driver has performed every script step that preceded it in the
//! capture. Standard requests (descriptors) are not part of the script.
//!
//! Some steps depend on timing in the C driver (status polls while waiting
//! for data): they are optional, matched when they come and skipped when the
//! driver does something else. Header offsets the C driver leaves
//! uninitialised (DEVIATIONS.md F22) are not compared. Bulk IN data usbmon
//! did not capture (it keeps about 240 KiB per transfer) reads as zeros.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::trace::{Kind, Urb, Xfer};
use crate::{InCompletion, Setup, UsbDevice, UsbError};

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Control {
        setup: Setup,
        out: Vec<u8>,
        answer: Vec<u8>,
        status: i32,
    },
    BulkOut {
        ep: u8,
        data: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
struct InEvent {
    ep: u8,
    /// Script steps that completed before this transfer did.
    after: usize,
    requested: u32,
    data: Vec<u8>,
    status: i32,
}

/// What a replay found wrong, shared with the test that drives it.
pub type Log = Arc<Mutex<Vec<String>>>;

pub struct ReplayDevice {
    script: Vec<Step>,
    /// Per step: may be skipped, or performed when the capture does not have it.
    optional: Vec<bool>,
    pos: usize,
    ins: VecDeque<InEvent>,
    /// Transfers the driver has submitted and not yet received, per endpoint.
    submitted: Vec<(u8, usize)>,
    log: Log,
    /// Destination of the driver's last CTL_RD_PRE.
    read_dest: u8,
    /// Last answer per CTL_RD_PRE destination, for polls the capture lacks.
    last_answer: Vec<(u8, Vec<u8>)>,
}

/// CTL_WR (0xb0) destinations whose header offset C never sets.
const UNSET_WR: [u8; 3] = [8, 9, 10]; // START, STOP, BULK_WR
/// CTL_RD_PRE (0xb1) destinations whose header offset C never sets.
const UNSET_RD: [u8; 2] = [0, 2]; // FW_VERSION, HW_STATUS

/// A CTL_RD_PRE the C driver issues a timing-dependent number of times: the
/// progress poll (I2C_STATUS, 4 bytes) and, once a capture has started, the
/// HW_STATUS poll.
fn is_poll(step: &Step, started: bool) -> bool {
    match step {
        Step::Control { setup, out, .. } if setup.request == 0xb1 && out.len() >= 4 => {
            (out[0] == 15 && out[3] == 4) || (out[0] == 2 && started)
        }
        _ => false,
    }
}

impl ReplayDevice {
    /// The script of device `bus`.`dev` in `urbs`.
    pub fn new(urbs: &[Urb], bus: u16, dev: u8) -> ReplayDevice {
        let mut submits: std::collections::HashMap<u64, &Urb> = Default::default();
        let mut script = Vec::new();
        let mut ins = VecDeque::new();
        for u in urbs.iter().filter(|u| u.bus == bus && u.dev == dev) {
            if u.kind == Kind::Submit {
                submits.insert(u.id, u);
                continue;
            }
            let Some(s) = submits.remove(&u.id) else {
                continue;
            };
            match u.xfer {
                Xfer::Control => {
                    let raw = s.setup.unwrap_or_default();
                    let setup = Setup {
                        request_type: raw[0],
                        request: raw[1],
                        value: u16::from_le_bytes([raw[2], raw[3]]),
                        index: u16::from_le_bytes([raw[4], raw[5]]),
                    };
                    if setup.request_type & 0x60 == 0 {
                        continue;
                    }
                    let (out, answer) = if setup.request_type & 0x80 != 0 {
                        (Vec::new(), u.data.clone())
                    } else {
                        (s.data.clone(), Vec::new())
                    };
                    script.push(Step::Control {
                        setup,
                        out,
                        answer,
                        status: u.status,
                    });
                }
                Xfer::Bulk if u.ep & 0x80 == 0 => script.push(Step::BulkOut {
                    ep: u.ep,
                    data: s.data.clone(),
                }),
                Xfer::Bulk => {
                    // usbmon captures large transfers only in part: the
                    // rest reads as zeros, at the transfer's real length
                    let mut data = u.data.clone();
                    data.resize(u.length as usize, 0);
                    ins.push_back(InEvent {
                        ep: u.ep,
                        after: script.len(),
                        requested: s.length,
                        data,
                        status: u.status,
                    })
                }
                _ => {}
            }
        }
        // a CTL_RD (0xb2) shares its CTL_RD_PRE's optionality
        let mut optional = Vec::with_capacity(script.len());
        let mut started = false;
        for s in &script {
            let opt = match s {
                Step::Control { setup, .. } if setup.request == 0xb2 => {
                    optional.last().copied().unwrap_or(false)
                }
                s => is_poll(s, started),
            };
            if let Step::Control { setup, out, .. } = s
                && setup.request == 0xb0
            {
                match out.first() {
                    Some(8) => started = true,  // START
                    Some(9) => started = false, // STOP
                    _ => {}
                }
            }
            optional.push(opt);
        }
        ReplayDevice {
            script,
            optional,
            pos: 0,
            ins,
            submitted: Vec::new(),
            log: Arc::new(Mutex::new(Vec::new())),
            read_dest: 0xff,
            last_answer: Vec::new(),
        }
    }

    pub fn log(&self) -> Log {
        Arc::clone(&self.log)
    }

    /// Script steps not performed yet (optional ones excluded).
    pub fn remaining(&self) -> usize {
        (self.pos..self.script.len())
            .filter(|&i| !self.optional[i])
            .count()
    }

    /// Bulk IN completions not delivered yet.
    pub fn remaining_in(&self) -> usize {
        self.ins.len()
    }

    fn fail(&self, msg: String) -> UsbError {
        self.log
            .lock()
            .unwrap()
            .push(format!("step {}: {msg}", self.pos));
        UsbError::Mismatch(msg)
    }

    /// Advances to the step matching `want`, skipping optional steps.
    /// `Ok(None)`: `want` is an optional poll the capture does not have here.
    fn take(&mut self, want: &Step, want_optional: bool) -> Result<Option<Step>, UsbError> {
        let mut at = self.pos;
        while at < self.script.len() {
            let s = &self.script[at];
            if same(s, want) {
                self.pos = at + 1;
                return Ok(Some(s.clone()));
            }
            if !self.optional[at] {
                break;
            }
            at += 1;
        }
        if want_optional {
            return Ok(None);
        }
        let found = self
            .script
            .get(self.pos)
            .map(describe)
            .unwrap_or("<end of capture>".into());
        let at = self
            .script
            .get(self.pos)
            .and_then(|s| first_difference(s, want));
        Err(self.fail(format!(
            "driver sent {}, capture has {found}{}",
            describe(want),
            at.map(|(i, a, b)| format!(" (first difference at byte {i}: capture {a}, driver {b})"))
                .unwrap_or_default()
        )))
    }
}

/// Equal up to the header offsets C leaves unset.
fn same(a: &Step, b: &Step) -> bool {
    match (a, b) {
        (
            Step::Control {
                setup: s1, out: o1, ..
            },
            Step::Control {
                setup: s2, out: o2, ..
            },
        ) => {
            if s1 != s2 || o1.len() != o2.len() {
                return false;
            }
            let unset = o1.len() >= 4
                && match s1.request {
                    0xb0 => UNSET_WR.contains(&o1[0]),
                    0xb1 => UNSET_RD.contains(&o1[0]),
                    _ => false,
                };
            o1.iter()
                .zip(o2)
                .enumerate()
                .all(|(i, (x, y))| x == y || (unset && (i == 1 || i == 2)))
        }
        (Step::BulkOut { ep: e1, data: d1 }, Step::BulkOut { ep: e2, data: d2 }) => {
            e1 == e2 && d1 == d2
        }
        _ => false,
    }
}

/// Where two payloads of the same kind first differ, with hex around it.
fn first_difference(a: &Step, b: &Step) -> Option<(usize, String, String)> {
    let (x, y) = match (a, b) {
        (Step::BulkOut { data: x, .. }, Step::BulkOut { data: y, .. }) => (x, y),
        (Step::Control { out: x, .. }, Step::Control { out: y, .. }) => (x, y),
        _ => return None,
    };
    let i = x.iter().zip(y.iter()).position(|(p, q)| p != q)?;
    let w = |d: &[u8]| hex(&d[i.saturating_sub(4)..(i + 12).min(d.len())]);
    Some((i, w(x), w(y)))
}

fn hex(d: &[u8]) -> String {
    let s: String = d.iter().take(24).map(|b| format!("{b:02x}")).collect();
    if d.len() > 24 { s + "…" } else { s }
}

fn describe(s: &Step) -> String {
    match s {
        Step::Control { setup, out, .. } => format!(
            "control {:#04x}/{:#04x} value={:#x} index={:#x} data={}",
            setup.request_type,
            setup.request,
            setup.value,
            setup.index,
            hex(out)
        ),
        Step::BulkOut { ep, data } => {
            format!("bulk OUT {ep:#04x} {} bytes {}", data.len(), hex(data))
        }
    }
}

fn status_err(status: i32) -> Result<(), UsbError> {
    match status {
        0 => Ok(()),
        -2 | -104 => Err(UsbError::Cancelled), // ENOENT, ECONNRESET: unlinked
        -32 => Err(UsbError::Stall),
        -19 | -108 => Err(UsbError::Disconnected),
        -110 => Err(UsbError::Timeout),
        e => Err(UsbError::Io(format!("usbmon status {e}"))),
    }
}

impl UsbDevice for ReplayDevice {
    fn control_out(&mut self, setup: Setup, data: &[u8], _t: Duration) -> Result<(), UsbError> {
        let want = Step::Control {
            setup,
            out: data.to_vec(),
            answer: Vec::new(),
            status: 0,
        };
        let started = self.script[..self.pos].iter().rev().find_map(|s| match s {
            Step::Control { setup, out, .. } if setup.request == 0xb0 => match out.first() {
                Some(8) => Some(true),
                Some(9) => Some(false),
                _ => None,
            },
            _ => None,
        });
        let optional = is_poll(&want, started.unwrap_or(false));
        if setup.request == 0xb1 {
            self.read_dest = data.first().copied().unwrap_or(0xff);
        }
        match self.take(&want, optional)? {
            Some(Step::Control { status, .. }) => status_err(status),
            _ => Ok(()),
        }
    }

    fn control_in(&mut self, setup: Setup, len: u16, _t: Duration) -> Result<Vec<u8>, UsbError> {
        let want = Step::Control {
            setup,
            out: Vec::new(),
            answer: Vec::new(),
            status: 0,
        };
        let dest = self.read_dest;
        // optional when its CTL_RD_PRE was a poll the capture did not have
        let optional = self.pos == 0
            || !matches!(&self.script[self.pos - 1], Step::Control { setup, out, .. }
                if setup.request == 0xb1 && out.first() == Some(&dest));
        match self.take(&want, optional)? {
            Some(Step::Control { answer, status, .. }) => {
                status_err(status)?;
                if answer.len() != len as usize {
                    return Err(self.fail(format!(
                        "control IN of {len} bytes, capture answered {}",
                        answer.len()
                    )));
                }
                self.last_answer.retain(|(d, _)| *d != dest);
                self.last_answer.push((dest, answer.clone()));
                Ok(answer)
            }
            _ => Ok(self
                .last_answer
                .iter()
                .find(|(d, _)| *d == dest)
                .map(|(_, v)| v.clone())
                .unwrap_or_else(|| vec![0; len as usize])),
        }
    }

    fn bulk_out(&mut self, ep: u8, data: &[u8], _t: Duration) -> Result<usize, UsbError> {
        let want = Step::BulkOut {
            ep,
            data: data.to_vec(),
        };
        self.take(&want, false).map(|_| data.len())
    }

    fn bulk_in_submit(&mut self, ep: u8, len: usize) -> Result<(), UsbError> {
        self.submitted.push((ep, len));
        Ok(())
    }

    fn bulk_in_wait(&mut self, ep: u8, timeout: Duration) -> Option<InCompletion> {
        let ready = matches!(self.ins.front(), Some(e) if e.ep == ep && e.after <= self.pos);
        let idx = self.submitted.iter().position(|(e, _)| *e == ep);
        match (ready, idx) {
            (true, Some(i)) => {
                let (_, len) = self.submitted.remove(i);
                let ev = self.ins.pop_front().unwrap();
                if ev.requested as usize != len {
                    let msg = format!(
                        "bulk IN transfer of {len} bytes, capture requested {}",
                        ev.requested
                    );
                    self.log
                        .lock()
                        .unwrap()
                        .push(format!("step {}: {msg}", self.pos));
                }
                Some(InCompletion {
                    data: ev.data,
                    status: status_err(ev.status),
                })
            }
            _ => {
                // nothing due yet: as if the wait timed out (without waiting)
                let _ = timeout;
                None
            }
        }
    }

    fn bulk_in_pending(&self, ep: u8) -> usize {
        self.submitted.iter().filter(|(e, _)| *e == ep).count()
    }

    fn bulk_in_cancel_all(&mut self, ep: u8) {
        // cancelled transfers complete with the capture's remaining data, or
        // as cancelled when it has none left
        let n = self.bulk_in_pending(ep);
        let left = self.ins.iter().filter(|e| e.ep == ep).count();
        for _ in left..n {
            self.ins.push_back(InEvent {
                ep,
                after: 0,
                requested: 0,
                data: Vec::new(),
                status: -2,
            });
        }
        for e in self.ins.iter_mut() {
            e.after = 0;
        }
        let _ = ep;
    }
}

/// Compares the vendor-protocol scripts of two captures (C and Rust driver)
/// step by step, with the replay's rules: unset header offsets ignored,
/// status polls optional on either side. Also compares the number of bulk
/// IN completions. Returns the differences found.
pub fn compare(a: &[Urb], b: &[Urb]) -> Vec<String> {
    let device = |u: &[Urb]| {
        u.iter()
            .find(|u| u.xfer == Xfer::Control && u.setup.is_some_and(|s| s[0] & 0x60 == 0x40))
            .map(|u| (u.bus, u.dev))
    };
    let (Some((ba, da)), Some((bb, db))) = (device(a), device(b)) else {
        return vec!["a capture has no vendor requests".into()];
    };
    let ra = ReplayDevice::new(a, ba, da);
    let rb = ReplayDevice::new(b, bb, db);
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < ra.script.len() || j < rb.script.len() {
        match (ra.script.get(i), rb.script.get(j)) {
            (Some(x), Some(y)) if same(x, y) => {
                i += 1;
                j += 1;
            }
            (Some(_), _) if ra.optional[i] => i += 1,
            (_, Some(_)) if j < rb.optional.len() && rb.optional[j] => j += 1,
            (x, y) => {
                let d = match (x, y) {
                    (Some(x), Some(y)) => first_difference(x, y)
                        .map(|(k, p, q)| format!(" (byte {k}: {p} vs {q})"))
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                out.push(format!(
                    "step {i}/{j}: {} vs {}{d}",
                    x.map(describe).unwrap_or("<end>".into()),
                    y.map(describe).unwrap_or("<end>".into())
                ));
                if out.len() >= 10 {
                    break;
                }
                i += 1;
                j += 1;
            }
        }
    }
    if ra.ins.len() != rb.ins.len() {
        out.push(format!(
            "bulk IN completions: {} vs {}",
            ra.ins.len(),
            rb.ins.len()
        ));
    }
    out
}
