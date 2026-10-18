//! usbmon captures in pcapng files (`dumpcap -i usbmonN`, link type 220,
//! LINUX_USB_MMAPPED): each packet is one URB event, a 64-byte usbmon header
//! followed by the captured data.

use std::path::Path;

/// URB event kind (`usbmon_packet.type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Submit,
    Complete,
    Error,
}

/// Transfer type (`usbmon_packet.xfer_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Xfer {
    Iso,
    Interrupt,
    Control,
    Bulk,
}

/// One URB submission or completion.
#[derive(Clone, Debug, PartialEq)]
pub struct Urb {
    pub id: u64,
    pub kind: Kind,
    pub xfer: Xfer,
    /// Endpoint address, 0x80 set for IN.
    pub ep: u8,
    pub dev: u8,
    pub bus: u16,
    /// The 8 setup bytes of a control submission.
    pub setup: Option<[u8; 8]>,
    pub status: i32,
    /// Requested (submit) or actual (complete) length.
    pub length: u32,
    /// Captured data (may be shorter than `length` with a snap length).
    pub data: Vec<u8>,
    /// Microseconds since the epoch.
    pub ts_us: i64,
}

impl Urb {
    pub fn is_in(&self) -> bool {
        self.ep & 0x80 != 0
    }
}

#[derive(Debug)]
pub enum TraceError {
    Io(std::io::Error),
    Format(String),
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TraceError::Io(e) => write!(f, "{e}"),
            TraceError::Format(m) => write!(f, "bad pcapng: {m}"),
        }
    }
}

impl std::error::Error for TraceError {}

impl From<std::io::Error> for TraceError {
    fn from(e: std::io::Error) -> Self {
        TraceError::Io(e)
    }
}

const LINKTYPE_USB_LINUX: u16 = 189;
const LINKTYPE_USB_LINUX_MMAPPED: u16 = 220;

fn bad(m: impl Into<String>) -> TraceError {
    TraceError::Format(m.into())
}

/// Reads every URB event of a pcapng usbmon capture, in file order.
pub fn read(path: &Path) -> Result<Vec<Urb>, TraceError> {
    parse(&std::fs::read(path)?)
}

pub fn parse(buf: &[u8]) -> Result<Vec<Urb>, TraceError> {
    let mut out = Vec::new();
    // per interface: (link type, header size)
    let mut ifaces: Vec<usize> = Vec::new();
    let mut pos = 0usize;
    let u32le = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    while pos + 12 <= buf.len() {
        let ty = u32le(buf, pos);
        let len = u32le(buf, pos + 4) as usize;
        if len < 12 || !len.is_multiple_of(4) || pos + len > buf.len() {
            return Err(bad(format!("block at {pos} has length {len}")));
        }
        let body = &buf[pos + 8..pos + len - 4];
        match ty {
            0x0A0D_0D0A => {
                if u32le(body, 0) != 0x1A2B_3C4D {
                    return Err(bad("big-endian or unknown byte order"));
                }
                ifaces.clear();
            }
            1 => {
                let link = u16::from_le_bytes([body[0], body[1]]);
                let hdr = match link {
                    LINKTYPE_USB_LINUX_MMAPPED => 64,
                    LINKTYPE_USB_LINUX => 48,
                    other => return Err(bad(format!("link type {other} is not usbmon"))),
                };
                ifaces.push(hdr);
            }
            6 => {
                let iface = u32le(body, 0) as usize;
                let cap = u32le(body, 12) as usize;
                let hdr = *ifaces
                    .get(iface)
                    .ok_or_else(|| bad("packet before its interface"))?;
                if 20 + cap > body.len() {
                    return Err(bad("packet longer than its block"));
                }
                out.push(urb(&body[20..20 + cap], hdr)?);
            }
            _ => {}
        }
        pos += len;
    }
    Ok(out)
}

/// One usbmon packet: `struct usbmon_packet` (linux/Documentation/usb/usbmon.rst).
fn urb(p: &[u8], hdr: usize) -> Result<Urb, TraceError> {
    if p.len() < hdr {
        return Err(bad("short usbmon header"));
    }
    let le32 = |at: usize| i32::from_le_bytes(p[at..at + 4].try_into().unwrap());
    let kind = match p[8] {
        b'S' => Kind::Submit,
        b'C' => Kind::Complete,
        b'E' => Kind::Error,
        k => return Err(bad(format!("URB type {k:#x}"))),
    };
    let xfer = match p[9] {
        0 => Xfer::Iso,
        1 => Xfer::Interrupt,
        2 => Xfer::Control,
        _ => Xfer::Bulk,
    };
    let setup = (p[14] == 0).then(|| p[40..48].try_into().unwrap());
    let ndesc = if hdr == 64 {
        le32(60).max(0) as usize
    } else {
        0
    };
    let data_at = hdr + ndesc * 16;
    Ok(Urb {
        id: u64::from_le_bytes(p[0..8].try_into().unwrap()),
        kind,
        xfer,
        ep: p[10],
        dev: p[11],
        bus: u16::from_le_bytes([p[12], p[13]]),
        setup,
        status: le32(28),
        length: le32(32) as u32,
        data: p.get(data_at..).map(<[u8]>::to_vec).unwrap_or_default(),
        ts_us: i64::from_le_bytes(p[16..24].try_into().unwrap()) * 1_000_000 + le32(24) as i64,
    })
}
