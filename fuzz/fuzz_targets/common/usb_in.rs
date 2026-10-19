//! A simulated DSLogic or DSCope whose bulk IN data (the trigger header,
//! sample data, DSO frames with their status blocks) comes from the fuzzer.
//! The driver must survive any of it: no panic, no hang.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsview_drivers::dsl::device::{Family, Probed, probe, scan};
use dsview_drivers::sys::{self, Clock};
use dsview_drivers::{Device, Step};
use dsview_usb::sim::{Hub, SimBackend, SimConfig};
use dsview_usb::{InCompletion, Info, Open, Setup, UsbBackend, UsbDevice, UsbError};

/// Waits are instant.
struct Fast(AtomicI64);

impl Clock for Fast {
    fn now_us(&self) -> i64 {
        self.0.fetch_add(1000, Ordering::Relaxed)
    }
    fn sleep_us(&self, us: u64) {
        self.0.fetch_add(us as i64, Ordering::Relaxed);
    }
}

type Input = Arc<Mutex<(Vec<u8>, usize)>>;

struct Fuzzed {
    inner: Box<dyn UsbDevice>,
    input: Input,
}

impl UsbDevice for Fuzzed {
    fn control_out(&mut self, s: Setup, d: &[u8], t: Duration) -> Result<(), UsbError> {
        self.inner.control_out(s, d, t)
    }
    fn control_in(&mut self, s: Setup, len: u16, t: Duration) -> Result<Vec<u8>, UsbError> {
        self.inner.control_in(s, len, t)
    }
    fn bulk_out(&mut self, ep: u8, d: &[u8], t: Duration) -> Result<usize, UsbError> {
        self.inner.bulk_out(ep, d, t)
    }
    fn bulk_in_submit(&mut self, ep: u8, len: usize) -> Result<(), UsbError> {
        self.inner.bulk_in_submit(ep, len)
    }
    fn bulk_in_wait(&mut self, ep: u8, t: Duration) -> Option<InCompletion> {
        let mut c = self.inner.bulk_in_wait(ep, t)?;
        let mut g = self.input.lock().unwrap();
        let (data, at) = &mut *g;
        // the fuzzer's bytes over the simulated ones, while they last; a
        // zero length byte truncates the transfer
        if *at < data.len() {
            let n = c.data.len().min(data.len() - *at);
            c.data[..n].copy_from_slice(&data[*at..*at + n]);
            *at += n;
            if n > 0 && data[*at - n] == 0 && n < c.data.len() {
                c.data.truncate(n);
            }
        }
        Some(c)
    }
    fn bulk_in_pending(&self, ep: u8) -> usize {
        self.inner.bulk_in_pending(ep)
    }
    fn bulk_in_cancel_all(&mut self, ep: u8) {
        self.inner.bulk_in_cancel_all(ep)
    }
}

struct Backend {
    sim: SimBackend,
    input: Input,
}

impl UsbBackend for Backend {
    fn list(&self) -> Result<Vec<Info>, UsbError> {
        self.sim.list()
    }
    fn open(&self, bus: u8, address: u8, how: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
        let inner = self.sim.open(bus, address, how)?;
        Ok(Box::new(Fuzzed {
            inner,
            input: self.input.clone(),
        }))
    }
}

/// Byte 0 picks the profile, the rest replaces IN data.
pub fn run(family: Family, data: &[u8]) {
    use dsview_drivers::dsl::tables::{DSCOPE_PROFILES, DSLOGIC_PROFILES};
    let Some((&pick, rest)) = data.split_first() else {
        return;
    };
    sys::set_clock(Arc::new(Fast(AtomicI64::new(0))));
    let (vid, pid, speed) = match family {
        Family::DsLogic => {
            let p = &DSLOGIC_PROFILES[pick as usize % DSLOGIC_PROFILES.len()];
            (p.vid, p.pid, p.speed)
        }
        Family::DsCope => {
            let p = &DSCOPE_PROFILES[pick as usize % DSCOPE_PROFILES.len()];
            (p.vid, p.pid, p.speed)
        }
    };
    let input: Input = Arc::new(Mutex::new((rest.to_vec(), 0)));
    let hub = Arc::new(Mutex::new(Hub::new(SimConfig::new(vid, pid, speed))));
    let backend: Arc<dyn UsbBackend> = Arc::new(Backend {
        sim: SimBackend::new(hub, Arc::new(|_| {})),
        input,
    });
    let (list, _) = scan(family, backend.as_ref());
    let Some(c) = list.first() else { return };
    let Probed::Device(mut d) = probe(family, backend.clone(), c) else {
        return;
    };
    if d.open().is_err() {
        return;
    }
    // small captures: the fuzzer's bytes reach many headers and frames
    let _ = d.config_set(
        dsview_core::key::LIMIT_SAMPLES,
        &dsview_core::ConfValue::U64(4096),
        None,
    );
    let mut out = Vec::new();
    if d.start(&mut out).is_ok() {
        for _ in 0..100 {
            if d.poll(false, &mut out) == Step::Remove {
                break;
            }
            out.clear();
        }
        let _ = d.poll(true, &mut out);
        let _ = d.stop(&mut out);
    }
    let _ = d.close();
}
