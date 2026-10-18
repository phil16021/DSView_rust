//! USB access for the DSView drivers (docs/migration/PLAN.md, Phase 3c).
//!
//! * [`UsbBackend`] lists and opens devices, [`UsbDevice`] is an opened one.
//! * [`nusb_backend`]: the real devices, through the `nusb` crate.
//! * [`replay`]: a device that plays back a usbmon capture of the C driver
//!   and checks every command the driver sends against it.
//! * [`trace`]: usbmon captures (pcapng) as URB events.
//!
//! Every call has a timeout; bulk IN is a queue of submitted transfers that
//! complete in order, which can be cancelled as a whole.

pub mod nusb_backend;
pub mod replay;
pub mod trace;

use std::time::Duration;

/// The SETUP packet of a control transfer; bit 7 of `request_type` is the
/// direction (set for IN).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
}

/// Vendor request to the device (bmRequestType 0x40 / 0xC0).
pub const VENDOR_OUT: u8 = 0x40;
pub const VENDOR_IN: u8 = 0xC0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsbError {
    Timeout,
    Stall,
    Disconnected,
    Cancelled,
    /// Claimed by another program or driver.
    Busy,
    NotFound,
    Access,
    Io(String),
    /// Replay: the driver sent something the capture does not have.
    Mismatch(String),
}

impl std::fmt::Display for UsbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsbError::Timeout => write!(f, "timeout"),
            UsbError::Stall => write!(f, "stall"),
            UsbError::Disconnected => write!(f, "device disconnected"),
            UsbError::Cancelled => write!(f, "cancelled"),
            UsbError::Busy => write!(f, "device busy"),
            UsbError::NotFound => write!(f, "device not found"),
            UsbError::Access => write!(f, "access denied"),
            UsbError::Io(m) => write!(f, "{m}"),
            UsbError::Mismatch(m) => write!(f, "replay mismatch: {m}"),
        }
    }
}

impl std::error::Error for UsbError {}

/// `enum libusb_speed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Unknown = 0,
    Low = 1,
    Full = 2,
    High = 3,
    Super = 4,
    SuperPlus = 5,
}

/// A device found by [`UsbBackend::list`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub bus: u8,
    pub address: u8,
    pub vid: u16,
    pub pid: u16,
    pub speed: Speed,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

/// How to open a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Open {
    /// Claim this interface (the normal case).
    Claim(u8),
    /// Select this configuration and claim nothing (FX2 firmware upload).
    Configure(u8),
}

/// A completed bulk IN transfer.
#[derive(Clone, Debug, PartialEq)]
pub struct InCompletion {
    pub data: Vec<u8>,
    pub status: Result<(), UsbError>,
}

/// An opened device.
pub trait UsbDevice: Send {
    fn control_out(&mut self, setup: Setup, data: &[u8], timeout: Duration)
    -> Result<(), UsbError>;
    fn control_in(
        &mut self,
        setup: Setup,
        len: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, UsbError>;
    fn bulk_out(&mut self, ep: u8, data: &[u8], timeout: Duration) -> Result<usize, UsbError>;

    /// Queues an IN transfer of `len` bytes on `ep` (a multiple of the
    /// endpoint's packet size); transfers complete in submission order.
    fn bulk_in_submit(&mut self, ep: u8, len: usize) -> Result<(), UsbError>;
    /// The oldest transfer once it completes, or `None` after `timeout`.
    fn bulk_in_wait(&mut self, ep: u8, timeout: Duration) -> Option<InCompletion>;
    fn bulk_in_pending(&self, ep: u8) -> usize;
    /// Cancels every queued transfer; they still complete (as cancelled).
    fn bulk_in_cancel_all(&mut self, ep: u8);
}

pub trait UsbBackend: Send + Sync {
    fn list(&self) -> Result<Vec<Info>, UsbError>;
    fn open(&self, bus: u8, address: u8, how: Open) -> Result<Box<dyn UsbDevice>, UsbError>;
}
