//! Real devices through `nusb` (no libusb).

use std::collections::HashMap;
use std::time::Duration;

use nusb::MaybeFuture;
use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, In, Recipient, TransferError};

use crate::{InCompletion, Info, Open, Setup, Speed, UsbBackend, UsbDevice, UsbError};

pub struct NusbBackend;

fn err(e: nusb::Error) -> UsbError {
    match e.kind() {
        nusb::ErrorKind::Disconnected => UsbError::Disconnected,
        nusb::ErrorKind::Busy => UsbError::Busy,
        nusb::ErrorKind::PermissionDenied => UsbError::Access,
        nusb::ErrorKind::NotFound => UsbError::NotFound,
        _ => UsbError::Io(e.to_string()),
    }
}

/// Control transfers report a timeout as a cancellation.
fn ctrl_err(e: TransferError) -> UsbError {
    match e {
        TransferError::Cancelled => UsbError::Timeout,
        other => xfer_err(other),
    }
}

fn xfer_err(e: TransferError) -> UsbError {
    match e {
        TransferError::Cancelled => UsbError::Cancelled,
        TransferError::Stall => UsbError::Stall,
        TransferError::Disconnected => UsbError::Disconnected,
        other => UsbError::Io(other.to_string()),
    }
}

fn speed(s: Option<nusb::Speed>) -> Speed {
    match s {
        Some(nusb::Speed::Low) => Speed::Low,
        Some(nusb::Speed::Full) => Speed::Full,
        Some(nusb::Speed::High) => Speed::High,
        Some(nusb::Speed::Super) => Speed::Super,
        Some(nusb::Speed::SuperPlus) => Speed::SuperPlus,
        _ => Speed::Unknown,
    }
}

fn info_of(d: &nusb::DeviceInfo) -> Info {
    Info {
        bus: bus_of(d),
        address: d.device_address(),
        vid: d.vendor_id(),
        pid: d.product_id(),
        speed: speed(d.speed()),
        manufacturer: d.manufacturer_string().map(str::to_owned),
        product: d.product_string().map(str::to_owned),
    }
}

/// The next item of a stream, blocking the thread.
fn next_blocking<S: futures_core::Stream + Unpin>(s: &mut S) -> Option<S::Item> {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    loop {
        match Pin::new(&mut *s).poll_next(&mut cx) {
            Poll::Ready(x) => return x,
            Poll::Pending => std::thread::park(),
        }
    }
}

impl UsbBackend for NusbBackend {
    fn list(&self) -> Result<Vec<Info>, UsbError> {
        let devs = nusb::list_devices().wait().map_err(err)?;
        Ok(devs.map(|d| info_of(&d)).collect())
    }

    /// nusb's hotplug stream on a thread of its own. A departure names the
    /// device only by its id, so the thread remembers where each id was.
    fn watch(&self) -> Option<std::sync::mpsc::Receiver<crate::Hotplug>> {
        let mut watch = nusb::watch_devices().ok()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut known: HashMap<nusb::DeviceId, (u8, u8)> = nusb::list_devices()
            .wait()
            .map(|l| {
                l.map(|d| (d.id(), (bus_of(&d), d.device_address())))
                    .collect()
            })
            .unwrap_or_default();
        std::thread::Builder::new()
            .name("usb-hotplug".into())
            .spawn(move || {
                while let Some(ev) = next_blocking(&mut watch) {
                    let msg = match ev {
                        nusb::hotplug::HotplugEvent::Connected(d) => {
                            known.insert(d.id(), (bus_of(&d), d.device_address()));
                            crate::Hotplug::Arrived(info_of(&d))
                        }
                        nusb::hotplug::HotplugEvent::Disconnected(id) => {
                            let Some((bus, address)) = known.remove(&id) else {
                                continue;
                            };
                            crate::Hotplug::Left { bus, address }
                        }
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                }
            })
            .ok()?;
        Some(rx)
    }

    fn open(&self, bus: u8, address: u8, how: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
        let info = nusb::list_devices()
            .wait()
            .map_err(err)?
            .find(|d| bus_of(d) == bus && d.device_address() == address)
            .ok_or(UsbError::NotFound)?;
        let device = info.open().wait().map_err(err)?;
        let interface = match how {
            Open::Claim(n) => Some(device.claim_interface(n).wait().map_err(err)?),
            Open::Configure(c) => {
                #[cfg(target_os = "linux")]
                {
                    let _ = device.detach_kernel_driver(0);
                }
                device.set_configuration(c).wait().map_err(err)?;
                None
            }
        };
        Ok(Box::new(NusbDevice {
            device,
            interface,
            endpoints: HashMap::new(),
        }))
    }
}

#[cfg(target_os = "linux")]
fn bus_of(d: &nusb::DeviceInfo) -> u8 {
    d.busnum()
}

#[cfg(not(target_os = "linux"))]
fn bus_of(d: &nusb::DeviceInfo) -> u8 {
    d.bus_id().parse().unwrap_or(0)
}

pub struct NusbDevice {
    // control transfers without an interface (not on Windows)
    #[cfg_attr(windows, allow(dead_code))]
    device: nusb::Device,
    interface: Option<nusb::Interface>,
    endpoints: HashMap<u8, nusb::Endpoint<Bulk, In>>,
}

fn control_type(rt: u8) -> ControlType {
    match rt & 0x60 {
        0x00 => ControlType::Standard,
        0x20 => ControlType::Class,
        _ => ControlType::Vendor,
    }
}

fn recipient(rt: u8) -> Recipient {
    match rt & 0x1f {
        1 => Recipient::Interface,
        2 => Recipient::Endpoint,
        3 => Recipient::Other,
        _ => Recipient::Device,
    }
}

impl NusbDevice {
    fn endpoint(&mut self, ep: u8) -> Result<&mut nusb::Endpoint<Bulk, In>, UsbError> {
        if !self.endpoints.contains_key(&ep) {
            let iface = self.interface.as_ref().ok_or(UsbError::NotFound)?;
            let e = iface.endpoint::<Bulk, In>(ep).map_err(err)?;
            self.endpoints.insert(ep, e);
        }
        Ok(self.endpoints.get_mut(&ep).unwrap())
    }
}

impl UsbDevice for NusbDevice {
    fn control_out(&mut self, s: Setup, data: &[u8], timeout: Duration) -> Result<(), UsbError> {
        let req = ControlOut {
            control_type: control_type(s.request_type),
            recipient: recipient(s.request_type),
            request: s.request,
            value: s.value,
            index: s.index,
            data,
        };
        match &self.interface {
            Some(i) => i.control_out(req, timeout).wait().map_err(ctrl_err),
            #[cfg(not(windows))]
            None => self
                .device
                .control_out(req, timeout)
                .wait()
                .map_err(ctrl_err),
            // WinUSB sends control transfers through a claimed interface only
            #[cfg(windows)]
            None => Err(UsbError::Io(
                "control transfer without a claimed interface".into(),
            )),
        }
    }

    fn control_in(&mut self, s: Setup, len: u16, timeout: Duration) -> Result<Vec<u8>, UsbError> {
        let req = ControlIn {
            control_type: control_type(s.request_type),
            recipient: recipient(s.request_type),
            request: s.request,
            value: s.value,
            index: s.index,
            length: len,
        };
        match &self.interface {
            Some(i) => i.control_in(req, timeout).wait().map_err(ctrl_err),
            #[cfg(not(windows))]
            None => self
                .device
                .control_in(req, timeout)
                .wait()
                .map_err(ctrl_err),
            // WinUSB sends control transfers through a claimed interface only
            #[cfg(windows)]
            None => Err(UsbError::Io(
                "control transfer without a claimed interface".into(),
            )),
        }
    }

    fn bulk_out(&mut self, ep: u8, data: &[u8], timeout: Duration) -> Result<usize, UsbError> {
        let iface = self.interface.as_ref().ok_or(UsbError::NotFound)?;
        let mut e = iface
            .endpoint::<Bulk, nusb::transfer::Out>(ep)
            .map_err(err)?;
        let mut buf = e.allocate(data.len());
        buf.extend_from_slice(data);
        let c = e.transfer_blocking(buf, timeout);
        match c.status {
            Ok(()) => Ok(c.actual_len),
            Err(TransferError::Cancelled) => Err(UsbError::Timeout),
            Err(other) => Err(xfer_err(other)),
        }
    }

    fn bulk_in_submit(&mut self, ep: u8, len: usize) -> Result<(), UsbError> {
        let e = self.endpoint(ep)?;
        let buf = e.allocate(len);
        e.submit(buf);
        Ok(())
    }

    fn bulk_in_wait(&mut self, ep: u8, timeout: Duration) -> Option<InCompletion> {
        let e = self.endpoint(ep).ok()?;
        if e.pending() == 0 {
            std::thread::sleep(timeout);
            return None;
        }
        let c = e.wait_next_complete(timeout)?;
        let mut data = c.buffer.into_vec();
        data.truncate(c.actual_len);
        Some(InCompletion {
            data,
            status: c.status.map_err(xfer_err),
        })
    }

    fn bulk_in_pending(&self, ep: u8) -> usize {
        self.endpoints.get(&ep).map_or(0, |e| e.pending())
    }

    fn bulk_in_cancel_all(&mut self, ep: u8) {
        if let Some(e) = self.endpoints.get_mut(&ep) {
            e.cancel_all();
        }
    }
}
