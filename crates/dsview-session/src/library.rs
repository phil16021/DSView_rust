//! `lib_main.c`: the device list, the active device, hotplug, the collect
//! thread. Function names follow the `ds_*` API they implement.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use dsview_core::{Packet, SrError, SrResult, dev_status, dev_type};
use dsview_drivers::dsl::device::{self, Family, Probed};
use dsview_drivers::sys;
use dsview_drivers::{Device, Step, demo, file};
use dsview_usb::{Hotplug, UsbBackend};

use crate::event;

/// `ds_device_handle`: names a device for as long as it is in the list.
pub type Handle = u64;

/// A device shared with the host's mirror of it.
pub type SharedDevice = Arc<Mutex<Box<dyn Device>>>;

/// What a device is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Demo,
    File(String),
    Usb {
        family: Family,
        bus: u8,
        address: u8,
    },
}

/// One device in the list (`struct sr_dev_inst` as lib_main sees it).
#[derive(Clone)]
pub struct Entry {
    pub handle: Handle,
    pub dev: SharedDevice,
    pub kind: Kind,
    pub name: String,
    pub actived_times: i32,
}

impl Entry {
    pub fn dev_type(&self) -> i32 {
        match self.kind {
            Kind::Demo => dev_type::DEMO,
            Kind::File(_) => dev_type::FILELOG,
            Kind::Usb { .. } => dev_type::USB,
        }
    }

    /// `get_device_unique_id()`
    pub fn unique_id(&self) -> String {
        match self.kind {
            Kind::Usb { bus, address, .. } => format!("USB {bus}.{address}"),
            _ => String::new(),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Box<dyn Device>> {
        lock(&self.dev)
    }
}

fn lock(d: &SharedDevice) -> MutexGuard<'_, Box<dyn Device>> {
    d.lock().unwrap_or_else(|e| e.into_inner())
}

/// The library's user: the events and data, and its copies of the devices.
pub trait Host: Send + Sync {
    /// `ds_set_event_callback`
    fn event(&self, ev: i32);
    /// `ds_set_datafeed_callback`, with the device the packets come from.
    fn packets(&self, handle: Handle, packets: &[Packet]);
    /// Before the library uses a device the host may have changed (its
    /// channels): copy them in.
    fn sync_in(&self, _handle: Handle) {}
    /// After the library changed a device: copy it out. `full`: after open,
    /// close and the start of a capture (mode, status, the channel list may
    /// have changed); otherwise during a capture (channel values only).
    fn sync_out(&self, _handle: Handle, _full: bool) {}
    /// Right before a capture starts (the trigger set up in the host).
    fn before_start(&self, _handle: Handle) {}
    /// A device left the list for good.
    fn removed(&self, _handle: Handle) {}
}

/// `CHECK_DEV_RECONNECT_TIMES`: a device that comes back within 5 hotplug
/// ticks (100 ms each) is the same device.
const RECONNECT_TICKS: u32 = 5;
const HOTPLUG_TICK: Duration = Duration::from_millis(100);

#[derive(Default)]
struct State {
    devices: Vec<Entry>,
    /// The active device; it may have left the list (detached while active,
    /// `is_delay_destory_actived_device`).
    active: Option<Entry>,
    delay_destroy_active: bool,
    next_handle: Handle,
    // hotplug
    attach_flag: bool,
    detach_flag: bool,
    waiting_reconnect: bool,
    reconnect_ticks: u32,
    /// Where the last departure was (bus, address).
    detached: Option<(u8, u8)>,
    /// The active device reconnected and is to be opened again
    /// (`DEV_TRANS_OPEN`).
    reopen: bool,
    stop_by_detached: bool,
    reloading: bool,
}

struct Collect {
    thread: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

struct Inner {
    host: Arc<dyn Host>,
    usb: Arc<dyn UsbBackend>,
    state: Mutex<State>,
    collect: Mutex<Option<Collect>>,
    collecting: AtomicBool,
    last_error: AtomicI32,
    exit: AtomicBool,
    callback_threads: AtomicUsize,
    hotplug: Mutex<Option<JoinHandle<()>>>,
}

/// The library (`struct sr_lib_context`).
#[derive(Clone)]
pub struct Library {
    inner: Arc<Inner>,
}

impl Library {
    /// `ds_lib_init()`: the demo devices, then every USB device, then the
    /// hotplug thread.
    pub fn init(host: Arc<dyn Host>, usb: Arc<dyn UsbBackend>) -> Library {
        sr_info!("Init libsigrok4DSL.");
        let lib = Library {
            inner: Arc::new(Inner {
                host,
                usb,
                state: Mutex::new(State {
                    next_handle: 1,
                    ..State::default()
                }),
                collect: Mutex::new(None),
                collecting: AtomicBool::new(false),
                last_error: AtomicI32::new(0),
                exit: AtomicBool::new(false),
                callback_threads: AtomicUsize::new(0),
                hotplug: Mutex::new(None),
            }),
        };
        {
            let mut st = lib.state();
            for d in demo::scan() {
                lib.push(&mut st, Box::new(d), Kind::Demo);
            }
        }
        sr_info!("Scan all connected hardware device.");
        lib.process_attach_event(false);
        lib.start_hotplug();
        lib
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, st: &mut State, dev: Box<dyn Device>, kind: Kind) -> Handle {
        let handle = st.next_handle;
        st.next_handle += 1;
        let name = dev.info().model;
        st.devices.push(Entry {
            handle,
            dev: Arc::new(Mutex::new(dev)),
            kind,
            name,
            actived_times: 0,
        });
        handle
    }

    /// `ds_lib_exit()`
    pub fn exit(&self) {
        sr_info!("Uninit libsigrok4DSL.");
        let _ = self.release_actived_device();
        self.inner.exit.store(true, Ordering::Relaxed);
        if let Some(t) = self
            .inner
            .hotplug
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = t.join();
        }
        let mut st = self.state();
        if st.delay_destroy_active
            && let Some(e) = st.active.take()
        {
            self.destroy(&e);
            st.delay_destroy_active = false;
        }
        for e in std::mem::take(&mut st.devices) {
            self.destroy(&e);
        }
        st.active = None;
        drop(st);
        while self.inner.callback_threads.load(Ordering::Relaxed) > 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // ------------------------------------------------------------ list

    /// `ds_get_device_list()`: the devices in list order.
    pub fn devices(&self) -> Vec<Entry> {
        self.state().devices.clone()
    }

    /// The active device, even if it left the list.
    pub fn active(&self) -> Option<Entry> {
        self.state().active.clone()
    }

    /// A device in the list, or the active one.
    pub fn entry(&self, handle: Handle) -> Option<Entry> {
        let st = self.state();
        st.devices
            .iter()
            .chain(st.active.iter())
            .find(|e| e.handle == handle)
            .cloned()
    }

    /// `ds_get_actived_device_index()`
    pub fn active_index(&self) -> i32 {
        let st = self.state();
        let Some(a) = &st.active else { return -1 };
        st.devices
            .iter()
            .position(|e| e.handle == a.handle)
            .map_or(-1, |i| i as i32)
    }

    /// `ds_get_last_error()`
    pub fn last_error(&self) -> i32 {
        self.inner.last_error.load(Ordering::Relaxed)
    }

    pub fn set_last_error(&self, e: i32) {
        self.inner.last_error.store(e, Ordering::Relaxed);
    }

    // ------------------------------------------------- device instances

    /// `open_device_instance()`
    fn open(&self, e: &Entry) -> SrResult<()> {
        sr_info!("To open device, name:\"{}\"", e.name);
        self.inner.host.sync_in(e.handle);
        let r = {
            let mut d = e.lock();
            if let Kind::Usb { bus, address, .. } = e.kind {
                d.set_location(bus, address);
            }
            let r = d.open();
            if r.is_err() {
                let err = d.last_error();
                if err != 0 {
                    self.set_last_error(err);
                }
            }
            r
        };
        self.inner.host.sync_out(e.handle, true);
        r
    }

    /// `close_device_instance()`
    fn close(&self, e: &Entry) {
        self.inner.host.sync_in(e.handle);
        let _ = e.lock().close();
        self.inner.host.sync_out(e.handle, true);
    }

    /// `destroy_device_instance()`: closed, and gone for the host too.
    fn destroy(&self, e: &Entry) {
        self.close(e);
        self.inner.host.removed(e.handle);
    }

    /// `ds_active_device()`
    pub fn activate(&self, handle: Handle) -> SrResult<()> {
        self.set_last_error(0);
        if handle == 0 {
            return Err(SrError::Arg);
        }
        sr_info!("Start activating device.");
        if self.is_collecting() {
            sr_err!("Error!the current device is collecting,can not switch to it.");
            return Err(SrError::CallStatus);
        }
        let mut st = self.state();
        let mut old: Option<Entry> = None;
        if let Some(a) = st.active.clone() {
            if st.delay_destroy_active {
                sr_info!(
                    "The current device is delayed for destruction, name:\"{}\"",
                    a.name
                );
                self.destroy(&a);
                st.active = None;
                st.delay_destroy_active = false;
            } else {
                sr_info!("Close the previous device, name:\"{}\"", a.name);
                self.close(&a);
                old = Some(a);
            }
        }
        let Some(i) = st.devices.iter().position(|e| e.handle == handle) else {
            drop(st);
            sr_info!("Activating device end.");
            sr_err!("Active device error, can't find the device.");
            return Err(SrError::CallStatus);
        };
        let e = st.devices[i].clone();
        if e.dev_type() == dev_type::USB && sys::res_dir().as_os_str().is_empty() {
            sr_err!(
                "Please call ds_set_firmware_resource_dir() to set the firmware resource path."
            );
        }
        if matches!(e.kind, Kind::File(_)) {
            sr_info!("Activating virtual device name: \"{}\".", e.name);
        } else {
            sr_info!("Activating device name: \"{}\".", e.name);
        }
        let mut ret = self.open(&e);
        if ret.is_ok() {
            st.devices[i].actived_times += 1;
            st.active = Some(st.devices[i].clone());
        } else {
            if old.is_none() {
                old = st.devices.first().cloned();
            }
            if let Some(o) = old {
                sr_err!("Open device error! Will switch to \"{}\".", o.name);
                st.active = Some(o.clone());
                ret = self.open(&o);
            }
        }
        drop(st);
        sr_info!("Activating device end.");
        ret
    }

    /// `ds_active_device_by_index()` (-1: the last one)
    pub fn activate_index(&self, index: i32) -> SrResult<()> {
        let handle = {
            let st = self.state();
            if index == -1 {
                st.devices.last().map(|e| e.handle)
            } else {
                usize::try_from(index)
                    .ok()
                    .and_then(|i| st.devices.get(i))
                    .map(|e| e.handle)
            }
        };
        match handle {
            Some(h) => self.activate(h),
            None => {
                sr_err!("ds_active_device_by_index(), index is error!");
                Err(SrError::CallStatus)
            }
        }
    }

    /// `ds_device_from_file()`
    pub fn device_from_file(&self, path: &str) -> SrResult<Handle> {
        if path.is_empty() {
            sr_err!("Error!File name is empty.");
            return Err(SrError::Arg);
        }
        let d = file::FileDevice::new(path)?;
        let mut st = self.state();
        Ok(self.push(&mut st, Box::new(d), Kind::File(path.to_owned())))
    }

    /// `ds_remove_device()`
    pub fn remove(&self, handle: Handle) -> SrResult<()> {
        if handle == 0 {
            return Err(SrError::Arg);
        }
        let active = self.active();
        if active.as_ref().is_some_and(|a| a.handle == handle) && self.is_collecting() {
            sr_err!("Device is collecting, can't remove it.");
            return Err(SrError::CallStatus);
        }
        let mut st = self.state();
        if st.delay_destroy_active
            && let Some(a) = st.active.clone()
            && a.handle == handle
        {
            self.destroy(&a);
            st.active = None;
            st.delay_destroy_active = false;
        }
        let Some(i) = st.devices.iter().position(|e| e.handle == handle) else {
            return Err(SrError::CallStatus);
        };
        let e = st.devices.remove(i);
        if st.active.as_ref().is_some_and(|a| a.handle == handle) {
            st.active = None;
        }
        self.destroy(&e);
        Ok(())
    }

    /// `ds_release_actived_device()`: stops a capture and closes the active
    /// device (it stays the active one).
    pub fn release_actived_device(&self) -> SrResult<()> {
        let Some(a) = self.active() else {
            return Err(SrError::CallStatus);
        };
        if self.is_collecting() {
            let _ = self.stop_collect();
        }
        sr_info!("Release current actived device. name:\"{}\"", a.name);
        self.close(&a);
        Ok(())
    }

    /// `ds_close_all_device()`: closes the USB devices.
    pub fn close_all(&self) {
        sr_info!("Close all device.");
        let st = self.state();
        for e in st.devices.iter().filter(|e| e.dev_type() == dev_type::USB) {
            self.close(e);
        }
    }

    /// `ds_reload_device_list()`
    pub fn reload(&self) {
        sr_info!("Reload device list.");
        {
            let mut st = self.state();
            st.reloading = true;
            st.delay_destroy_active = false;
            st.detached = None;
            st.detach_flag = false;
        }
        self.process_attach_event(false);
        self.state().reloading = false;
    }

    // ------------------------------------------------------------- scan

    /// The USB devices of one driver not yet in the list, probed (the
    /// drivers' scan()). Returns them with their location.
    fn scan_usb(&self, family: Family) -> Vec<(Box<dyn Device>, Kind)> {
        let (cands, slow) = device::scan(family, self.inner.usb.as_ref());
        let mut out = Vec::new();
        let mut num = 0;
        for c in cands {
            let (bus, address) = (c.info.bus, c.info.address);
            let exists = {
                let st = self.state();
                !st.reloading
                    && st.devices.iter().any(|e| {
                        matches!(e.kind, Kind::Usb { bus: b, address: a, .. } if b == bus && a == address)
                    })
            };
            if exists {
                continue;
            }
            match device::probe(family, self.inner.usb.clone(), &c) {
                Probed::Device(d) => {
                    out.push((
                        d as Box<dyn Device>,
                        Kind::Usb {
                            family,
                            bus,
                            address,
                        },
                    ));
                    num += 1;
                }
                Probed::FirmwareUploaded | Probed::NotReady => {}
            }
        }
        sr_info!(
            "Found new {} device count: {num}",
            match family {
                Family::DsLogic => "DSLogic",
                Family::DsCope => "DSCope",
            }
        );
        if slow {
            self.inner.host.event(event::DEVICE_SPEED_NOT_MATCH);
        }
        out
    }

    /// `process_attach_event()`: every hardware driver scans; new devices
    /// join the list.
    fn process_attach_event(&self, is_event: bool) {
        if is_event {
            sr_info!("Process device attach event.");
        }
        let mut num = 0;
        for family in [Family::DsLogic, Family::DsCope] {
            for (dev, kind) in self.scan_usb(family) {
                let mut st = self.state();
                let dup = st.devices.iter().any(|e| e.kind == kind);
                if !dup {
                    let mut dev = dev;
                    dev.set_status(dev_status::INACTIVE);
                    self.push(&mut st, dev, kind);
                    num += 1;
                }
            }
        }
        if num > 0 && is_event {
            self.post_event_async(event::NEW_DEVICE_ATTACH);
        }
    }

    // ---------------------------------------------------------- hotplug

    fn start_hotplug(&self) {
        let Some(rx) = self.inner.usb.watch() else {
            return;
        };
        let lib = self.clone();
        let t = std::thread::Builder::new()
            .name("hotplug_proc".into())
            .spawn(move || {
                sr_info!("Hotplug thread start!");
                while !lib.inner.exit.load(Ordering::Relaxed) {
                    // sr_hotplug_wait_timout() + xsleep(100): events arrive
                    // within the tick
                    let deadline = std::time::Instant::now() + HOTPLUG_TICK;
                    loop {
                        let left = deadline.saturating_duration_since(std::time::Instant::now());
                        match rx.recv_timeout(left) {
                            Ok(ev) => lib.hotplug_event(ev),
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => {
                                std::thread::sleep(left);
                                break;
                            }
                        }
                    }
                    lib.hotplug_tick();
                }
                sr_info!("Hotplug thread end!");
            });
        if let Ok(t) = t {
            *self.inner.hotplug.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
        }
    }

    /// `hotplug_event_listen_callback()`
    pub fn hotplug_event(&self, ev: Hotplug) {
        match ev {
            Hotplug::Arrived(info) => {
                sr_info!(
                    "----------One device attached, bus {}.{}",
                    info.bus,
                    info.address
                );
                let mut done = false;
                let mut st = self.state();
                if st.waiting_reconnect {
                    match st.detached.take() {
                        None => sr_err!(
                            "The detached device handle is null, but the status is waitting for reconnect."
                        ),
                        Some(old) => {
                            if self.update_device_location(&mut st, old, (info.bus, info.address)) {
                                done = true;
                                sr_info!(
                                    "----------One device loose contact, but it reconnect success."
                                );
                            } else {
                                sr_err!(
                                    "Update reconnected device handle error! can't find the old."
                                );
                            }
                        }
                    }
                }
                if !done {
                    st.attach_flag = true;
                }
                st.waiting_reconnect = false;
            }
            Hotplug::Left { bus, address } => {
                sr_info!("----------One device detached, bus {bus}.{address}");
                let active_here = self.active().is_some_and(|a| {
                    matches!(a.kind, Kind::Usb { bus: b, address: ad, .. } if b == bus && ad == address)
                });
                if active_here && self.is_collecting() {
                    sr_info!("The collecting device is detached, will stop the collect thread.");
                    self.state().stop_by_detached = true;
                    let _ = self.release_actived_device();
                }
                let mut st = self.state();
                if st.detached.is_some() {
                    sr_err!("One detached device haven't processed complete.");
                }
                st.waiting_reconnect = true;
                st.reconnect_ticks = 0;
                st.detached = Some((bus, address));
            }
        }
    }

    /// `update_device_handle()`: the device that left at `old` is back at
    /// `new`. Its handle stays the same here (C changes it).
    fn update_device_location(&self, st: &mut State, old: (u8, u8), new: (u8, u8)) -> bool {
        let Some(i) = st.devices.iter().position(
            |e| matches!(e.kind, Kind::Usb { bus, address, .. } if (bus, address) == old),
        ) else {
            return false;
        };
        let is_active = st
            .active
            .as_ref()
            .is_some_and(|a| a.handle == st.devices[i].handle);
        if is_active {
            sr_info!("To release the old device's resource.");
            self.close(&st.devices[i]);
        }
        if let Kind::Usb { family, .. } = st.devices[i].kind {
            st.devices[i].kind = Kind::Usb {
                family,
                bus: new.0,
                address: new.1,
            };
        }
        st.devices[i].lock().set_location(new.0, new.1);
        if is_active {
            st.active = Some(st.devices[i].clone());
            st.reopen = true;
        }
        true
    }

    /// One pass of `usb_hotplug_process_proc()` after its wait.
    fn hotplug_tick(&self) {
        let (attach, detach) = {
            let mut st = self.state();
            let r = (st.attach_flag, st.detach_flag);
            st.attach_flag = false;
            st.detach_flag = false;
            r
        };
        if attach {
            self.process_attach_event(true);
        }
        if detach {
            self.process_detach_event();
        }
        let reopen = {
            let mut st = self.state();
            if st.waiting_reconnect {
                st.reconnect_ticks += 1;
                if st.reconnect_ticks == RECONNECT_TICKS {
                    // lost contact for good: a detach
                    st.detach_flag = true;
                    st.waiting_reconnect = false;
                }
            }
            std::mem::take(&mut st.reopen)
                .then(|| st.active.clone())
                .flatten()
        };
        if let Some(a) = reopen {
            sr_info!("To reopen the current device.");
            let _ = self.open(&a);
        }
    }

    /// `process_detach_event()`
    fn process_detach_event(&self) {
        sr_info!("Process device detach event.");
        let mut st = self.state();
        let Some((bus, address)) = st.detached.take() else {
            sr_err!("The detached device handle is null.");
            return;
        };
        let Some(i) = st.devices.iter().position(
            |e| matches!(e.kind, Kind::Usb { bus: b, address: a, .. } if (b, a) == (bus, address)),
        ) else {
            return;
        };
        let e = st.devices.remove(i);
        let ev = if st.active.as_ref().is_some_and(|a| a.handle == e.handle) {
            sr_info!("The current device will be delayed for destruction.");
            st.delay_destroy_active = true;
            event::CURRENT_DEVICE_DETACH
        } else {
            self.destroy(&e);
            event::INACTIVE_DEVICE_DETACH
        };
        drop(st);
        self.post_event_async(ev);
    }

    // ----------------------------------------------------------- events

    /// `post_event_async()`: the callback on a thread of its own.
    fn post_event_async(&self, ev: i32) {
        self.inner.callback_threads.fetch_add(1, Ordering::Relaxed);
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new()
            .name("callback_thread".into())
            .spawn(move || {
                inner.host.event(ev);
                inner.callback_threads.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            self.inner.callback_threads.fetch_sub(1, Ordering::Relaxed);
        }
    }

    // ---------------------------------------------------------- capture

    /// `ds_is_collecting()`
    pub fn is_collecting(&self) -> bool {
        self.inner.collecting.load(Ordering::Acquire)
    }

    /// `ds_start_collect()`. `has_datafeed`: the host has a data callback.
    pub fn start_collect(&self, has_datafeed: bool) -> SrResult<()> {
        self.set_last_error(0);
        sr_info!("Start collect.");
        if self.is_collecting() {
            sr_err!("Error,it's collecting!");
            return Err(SrError::CallStatus);
        }
        let Some(a) = self.active() else {
            sr_err!("Please set a current device first.");
            return Err(SrError::CallStatus);
        };
        self.inner.host.sync_in(a.handle);
        let (status, any_enabled) = {
            let d = a.lock();
            (d.info().status, d.channels().iter().any(|c| c.enabled))
        };
        if status == dev_status::INITIALIZING {
            sr_err!("Error!The device is initializing.");
            return Err(SrError::CallStatus);
        }
        if !any_enabled {
            sr_err!("There have no useable channel, unable to collect.");
            return Err(SrError::CallStatus);
        }
        if !has_datafeed {
            sr_err!(
                "Error! Data forwarding callback is not set, see \"ds_set_datafeed_callback()\"."
            );
            return Err(SrError::CallStatus);
        }
        if status != dev_status::ACTIVE
            && let Err(e) = self.open(&a)
        {
            sr_err!("Open device error!");
            return Err(e);
        }
        // the stop flag is in place before the thread runs: a stop requested
        // from its first packets (sr_session_stop) must not be lost
        let stop = Arc::new(AtomicBool::new(false));
        let mut collect = self.inner.collect.lock().unwrap_or_else(|e| e.into_inner());
        *collect = Some(Collect {
            thread: None,
            stop: stop.clone(),
        });
        self.inner.collecting.store(true, Ordering::Release);
        let lib = self.clone();
        let t = std::thread::Builder::new()
            .name("collect_proc".into())
            .spawn(move || lib.collect_run(a, stop));
        match t {
            Ok(t) => {
                if let Some(c) = collect.as_mut() {
                    c.thread = Some(t);
                }
                Ok(())
            }
            Err(_) => {
                *collect = None;
                self.inner.collecting.store(false, Ordering::Release);
                Err(SrError::Err)
            }
        }
    }

    /// One callback of the session's dummy source (`rs_receive()` through
    /// `dsv_drv_poll()`): polls until the driver returns. True while the
    /// source stays.
    fn poll_source(&self, e: &Entry, abort: bool, stop: &AtomicBool) -> bool {
        let host = &self.inner.host;
        host.sync_in(e.handle);
        let mut out = Vec::new();
        let more = loop {
            let step = e.lock().poll(abort, &mut out);
            host.sync_out(e.handle, false);
            host.packets(e.handle, &out);
            out.clear();
            match step {
                Step::Continue => break true,
                Step::Remove => break false,
                Step::SleepThenPoll(us) => sys::clock().sleep_us(us),
                Step::SleepThenReturn(us) => {
                    sys::clock().sleep_us(us);
                    break true;
                }
            }
        };
        // A USB driver's C original polls real fds: the session acts on a
        // stop request after every callback, also the one that ended the
        // capture (then dev_acquisition_stop() runs twice).
        if !more && !abort && e.dev_type() == dev_type::USB && stop.load(Ordering::Acquire) {
            self.acquisition_stop(e);
        }
        more
    }

    /// `current_device_acquisition_stop()`
    fn acquisition_stop(&self, e: &Entry) {
        let host = &self.inner.host;
        host.sync_in(e.handle);
        let mut out = Vec::new();
        let _ = e.lock().stop(&mut out);
        host.sync_out(e.handle, false);
        host.packets(e.handle, &out);
    }

    /// `collect_run_proc()` with `sr_session_run()` (dummy source).
    fn collect_run(&self, e: Entry, stop: Arc<AtomicBool>) {
        let host = self.inner.host.clone();
        host.event(event::COLLECT_TASK_START);
        sr_info!("Collect thread start.");
        let mut error = false;
        host.before_start(e.handle);
        host.sync_in(e.handle);
        let mut out = Vec::new();
        let started = e.lock().start(&mut out);
        host.sync_out(e.handle, true);
        host.packets(e.handle, &out);
        match started {
            Err(r) => {
                let err = e.lock().last_error();
                if err != 0 {
                    self.set_last_error(err);
                }
                sr_err!(
                    "Failed to start acquisition of device in running session: {}",
                    r as i32
                );
                error = true;
            }
            Ok(()) => {
                host.event(event::DEVICE_RUNNING);
                sr_info!("Running...");
                loop {
                    if stop.load(Ordering::Acquire) {
                        let _ = self.poll_source(&e, true, &stop);
                        break;
                    }
                    if !self.poll_source(&e, false, &stop) {
                        break;
                    }
                }
                self.acquisition_stop(&e);
                host.event(event::DEVICE_STOPPED);
            }
        }
        sr_info!("Collect thread end.");
        self.inner.collecting.store(false, Ordering::Release);
        let mut detached = std::mem::take(&mut self.state().stop_by_detached);
        // the transfers can fail and end the capture before the hotplug
        // thread hears of the unplug (C has the same race): a capture that
        // ended on its own while its device is off the bus ended by detach
        if !detached
            && !error
            && !stop.load(Ordering::Acquire)
            && let Kind::Usb { bus, address, .. } = e.kind
        {
            let present = self
                .inner
                .usb
                .list()
                .map(|l| l.iter().any(|i| i.bus == bus && i.address == address));
            if present == Ok(false) {
                sr_info!("The collecting device is gone: the capture ended by detach.");
                detached = true;
            }
        }
        if error {
            host.event(event::COLLECT_TASK_END_BY_ERROR);
        } else if detached {
            self.post_event_async(event::COLLECT_TASK_END_BY_DETACHED);
        } else {
            host.event(event::COLLECT_TASK_END);
        }
    }

    /// `sr_session_stop()`: asks the capture to stop, without waiting (also
    /// from the capture's own callbacks). A request before the capture loop
    /// runs is acted on at its first pass.
    pub fn request_stop(&self) {
        if let Some(c) = self
            .inner
            .collect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            c.stop.store(true, Ordering::Release);
        }
    }

    /// `ds_stop_collect()`: asks the capture to stop and waits for it.
    pub fn stop_collect(&self) -> SrResult<()> {
        sr_info!("Stop collect.");
        if !self.is_collecting() {
            sr_err!("It's not collecting now.");
            return Err(SrError::CallStatus);
        }
        let thread = {
            let mut c = self.inner.collect.lock().unwrap_or_else(|e| e.into_inner());
            c.as_mut().and_then(|c| {
                c.stop.store(true, Ordering::Release);
                c.thread.take()
            })
        };
        if let Some(t) = thread
            && t.thread().id() != std::thread::current().id()
        {
            let _ = t.join();
        }
        Ok(())
    }
}
