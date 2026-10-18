//! The capture session of DSView's GUI without the GUI: `pv/sigsession.cpp`
//! (device selection, signals, capture lifecycle, data feed, decoder
//! scheduling) with the parts of `DeviceAgent`, `SamplingBar` (sample rate
//! and depth selection) and `TriggerDock` (simple trigger) that drive it.
//!
//! A [`Session`] owns the acquisition [`Library`] and is its [`Host`]: the
//! packets of a capture go into the logic, DSO and analog stores, and at
//! its end the decoder stacks run over the data. All methods may be called
//! from any thread; [`Session::wait`] blocks until the state satisfies a
//! condition (the GUI's message loop).

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use dsview_core::{
    Body, Channel, ConfValue, Mode, Packet, SrResult, Trigger, channel_type, key, pkt_status,
};
use dsview_store::analog::{AnalogChannel, AnalogStore};
use dsview_store::dso::{DsoChannel, DsoStore};
use dsview_store::logic::LogicStore;
use dsview_usb::UsbBackend;

use crate::decode::DecoderStack;
use crate::event;
use crate::library::{Entry, Handle, Host, Kind, Library};

/// `SAMPLES_ALIGN`
const SAMPLES_ALIGN: u64 = 1023;
/// `SamplingBar::LogicMaxSWDepth64`
const LOGIC_MAX_SW_DEPTH: u64 = 16 * 1024 * 1024 * 1024;
/// `SamplingBar::AnalogMaxSWDepth`
const ANALOG_MAX_SW_DEPTH: u64 = 100_000_000;
const SEC: f64 = 1e9;
/// `TriggerDock::MinTrigPosition`, `DS_MAX_TRIG_PERCENT`
const MIN_TRIG_POS: i32 = 1;
const MAX_TRIG_PERCENT: i32 = 90;
/// `DS_CONF_DSO_HDIVS`
pub const DSO_HDIVS: u64 = 10;
/// `DS_CONF_DSO_VDIVS`
pub const DSO_VDIVS: u64 = 10;

/// `SigSession::SESSION_ERROR_STATUS`
pub mod error {
    pub const NONE: i32 = 0;
    pub const HW: i32 = 1;
    pub const MALLOC: i32 = 2;
    pub const TEST_TIMEOUT: i32 = 3;
    pub const PKT_DATA: i32 = 4;
    pub const DATA_OVERFLOW: i32 = 5;
}

/// `LogicSignal::LogicSetRegions`
pub mod trig {
    pub const NONE: i32 = 0;
    pub const RISING: i32 = 1;
    pub const HIGH: i32 = 2;
    pub const FALLING: i32 = 3;
    pub const LOW: i32 = 4;
    pub const EDGE: i32 = 5;
}

/// What changed in a session, for frontends (the messages `SigSession`
/// broadcasts to its listeners). Data and decode progress are sent at most
/// every [`EVENT_INTERVAL`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// Another device is active (or the active one was reopened).
    DeviceChanged,
    /// Devices came or went.
    DeviceListChanged,
    ModeChanged,
    CaptureStarted,
    /// New samples in the capture buffer.
    DataAppended,
    /// The capture ended (a repeat capture between two rounds as well).
    CaptureEnded,
    /// Decoders made progress or finished one stack.
    DecodeProgress,
    /// The decode queue is empty.
    DecodeDone,
    /// `SigSession::get_error()` became non-zero.
    Error(i32),
}

/// Minimum time between two `DataAppended`/`DecodeProgress` events (30 Hz).
pub const EVENT_INTERVAL: Duration = Duration::from_millis(33);

/// `DEVICE_COLLECT_MODE`: one capture, captures one after another, or
/// one endless capture into a ring buffer (stream mode only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CollectMode {
    #[default]
    Single = 0,
    Repeat = 1,
    Loop = 2,
}

/// A trace of the current mode (`view::Signal`): logic and analog signals
/// exist for enabled channels only, DSO signals for every DSO channel.
#[derive(Clone, Debug)]
pub struct Signal {
    pub index: u16,
    pub ty: i32,
    pub name: String,
    /// Logic signals: the simple trigger (`trig::*`).
    pub trig: i32,
    pub view_index: i32,
    /// The user's colour (`Trace::set_colour`), none for the default.
    pub colour: Option<[u8; 3]>,
}

/// A trace of the view: a signal, or decoder trace `i`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TraceRef {
    Signal { index: u16, ty: i32 },
    Decoder(usize),
}

/// A decoder trace (`view::DecodeTrace`): a named stack.
pub struct DecodeTrace {
    pub name: Mutex<String>,
    pub view_index: Mutex<i32>,
    pub stack: DecoderStack,
}

impl DecodeTrace {
    pub fn new(name: String, stack: DecoderStack) -> DecodeTrace {
        DecodeTrace {
            name: Mutex::new(name),
            view_index: Mutex::new(-1),
            stack,
        }
    }

    pub fn name(&self) -> String {
        lock(&self.name).clone()
    }
}

/// What the session knows about the active device (`DeviceAgent`).
#[derive(Clone, Debug)]
pub struct DeviceDesc {
    pub handle: Handle,
    pub name: String,
    pub driver: &'static str,
    pub kind: Kind,
}

impl DeviceDesc {
    pub fn is_hardware(&self) -> bool {
        matches!(self.kind, Kind::Usb { .. })
    }
    pub fn is_demo(&self) -> bool {
        self.kind == Kind::Demo
    }
    pub fn is_file(&self) -> bool {
        matches!(self.kind, Kind::File(_))
    }
}

fn driver_name(kind: &Kind) -> &'static str {
    use dsview_drivers::dsl::device::Family;
    match kind {
        Kind::Demo => "virtual-demo",
        Kind::File(_) => "virtual-session",
        Kind::Usb {
            family: Family::DsLogic,
            ..
        } => "DSLogic",
        Kind::Usb { .. } => "DSCope",
    }
}

/// `SamplingBar`'s two selectors: the rate list and the duration list.
#[derive(Default, Clone)]
struct Selectors {
    rates: Vec<u64>,
    rate: usize,
    /// (duration in ns, labelled RLE)
    durations: Vec<(f64, bool)>,
    duration: usize,
}

impl Selectors {
    fn rate(&self) -> u64 {
        self.rates.get(self.rate).copied().unwrap_or(0)
    }
    fn duration(&self) -> (f64, bool) {
        self.durations
            .get(self.duration)
            .copied()
            .unwrap_or((0.0, false))
    }
}

/// What a capture needs from the device, read when it starts.
#[derive(Default, Clone)]
struct CaptureSetup {
    mode: i32,
    limit: u64,
    logic: Vec<u16>,
    dso: Vec<DsoChannel>,
    analog: Vec<AnalogChannel>,
    dso_total: usize,
    dso_enabled: usize,
    is_file: bool,
}

/// The session's state; see the accessors on [`Session`].
#[derive(Default)]
pub struct State {
    pub device: Option<DeviceDesc>,
    pub signals: Vec<Signal>,
    /// Trigger dock: position slider and its upper bound.
    pub trig_pos: i32,
    trig_pos_max: i32,
    pub working: bool,
    pub instant: bool,
    pub triged: bool,
    /// `_device_status == ST_RUNNING`
    pub running: bool,
    pub saving: bool,
    pub error: i32,
    stream_mode: bool,
    capture_times: u64,
    /// Increments when a capture starts / ends (`START_COLLECT_WORK`,
    /// `END_COLLECT_WORK`).
    pub started_seq: u64,
    /// The capture whose data was last offered for saving
    /// (`_confirm_store_time_id`).
    confirm_store_seq: u64,
    /// The profile a hardware device's settings came from
    /// (`_title_ext_string`).
    pub profile_file: Option<PathBuf>,
    /// `swapBackBufferAlways`: a repeat capture ending after the stop is
    /// shown too.
    pub swap_back_buffer_always: bool,
    pub ended_seq: u64,
    pub cur_snap_samplerate: u64,
    pub cur_samplelimits: u64,
    /// `_view_data->_trig_pos`
    pub trigger_pos: u64,
    /// `_show_trig_cursor` of a logic view: a trigger arrived while one was
    /// set, or from a virtual device.
    pub trig_cursor: bool,
    /// Capture start, ms since the epoch.
    pub session_time_ms: i64,
    capture_work_time: u64,
    setup: CaptureSetup,
    dso_packets: u64,
    /// DSO: the last frame was triggered, on channel `trig_ch`
    /// (`_trigger_flag`, `_trigger_ch`).
    pub trig_flag: bool,
    pub trig_ch: u8,
    /// Increments with each DSO frame (`_dso_data_seq`).
    pub dso_seq: u64,
    /// The device status of the last DSO frame (`_dso_status`), if valid.
    pub dso_status: Option<dsview_core::Status>,
    sel: Selectors,
    pub decoders: Vec<Arc<DecodeTrace>>,
    decode_queue: VecDeque<Arc<DecodeTrace>>,
    pub decoding: bool,
    pub collect_mode: CollectMode,
    /// Seconds between two captures in repeat mode (`_repeat_intvl`).
    pub repeat_interval: f64,
    /// Captures since the start (`_capture_times`), for repeat mode.
    pub captures: u64,
    pub cursors: Vec<u64>,
    pub region: Option<(u64, u64)>,
    /// The trigger dock's advanced and serial settings.
    pub adv_trigger: crate::trigger::AdvTrigger,
}

/// The captured data (`SessionData`): one store per signal type.
pub struct Data {
    pub logic: Mutex<LogicStore>,
    pub dso: Mutex<DsoStore>,
    pub analog: Mutex<AnalogStore>,
}

impl Data {
    fn new() -> Data {
        Data {
            logic: Mutex::new(LogicStore::new()),
            dso: Mutex::new(DsoStore::new()),
            analog: Mutex::new(AnalogStore::new()),
        }
    }

    /// `SessionData::clear()`
    fn clear(&self) {
        lock(&self.logic).clear();
        lock(&self.dso).clear();
        lock(&self.analog).clear();
    }
}

struct Inner {
    lib: Library,
    state: Mutex<State>,
    changed: Condvar,
    subscribers: Mutex<Vec<std::sync::mpsc::Sender<SessionEvent>>>,
    /// When `DataAppended`/`DecodeProgress` were last sent.
    last_sent: Mutex<(Option<Instant>, Option<Instant>)>,
    /// Two buffers (`_data_list`): repeat mode captures into one while the
    /// other is shown and decoded.
    data: [Data; 2],
    view: AtomicUsize,
    capture: AtomicUsize,
    /// `set_capture_end_flag`: false while a live decode follows a capture.
    capture_ended: AtomicBool,
    trigger: Mutex<Trigger>,
    data_dir: PathBuf,
    profile_dir: Option<PathBuf>,
    version: String,
}

/// Forwards the library's callbacks to the session.
struct HostShim(Weak<Inner>);

impl Host for HostShim {
    fn event(&self, ev: i32) {
        if let Some(i) = self.0.upgrade() {
            Session(i).on_event(ev);
        }
    }
    fn packets(&self, _h: Handle, packets: &[Packet]) {
        if let Some(i) = self.0.upgrade() {
            let s = Session(i);
            for p in packets {
                s.data_feed_in(p);
            }
        }
    }
    fn before_start(&self, h: Handle) {
        if let Some(i) = self.0.upgrade() {
            let t = *lock(&i.trigger);
            if let Some(e) = i.lib.entry(h) {
                e.lock().set_trigger(&t);
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where DSView's data files are (`GetAppDataDir()`): `demo/`, `res/`.
#[derive(Clone, Debug)]
pub struct Paths {
    pub data_dir: PathBuf,
    /// Where hardware settings are kept between runs (`GetProfileDir()`);
    /// none: not kept.
    pub profile_dir: Option<PathBuf>,
    /// The application version written into saved sessions and profiles.
    pub version: String,
}

impl Paths {
    /// The Qt GUI's profile directory (`QStandardPaths::AppDataLocation`):
    /// `$XDG_DATA_HOME/DreamSourceLab/DSView`, else `~/.local/share/...`.
    pub fn default_profile_dir() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
        Some(base.join("DreamSourceLab/DSView"))
    }
}

/// The capture session; cheap to clone.
#[derive(Clone)]
pub struct Session(Arc<Inner>);

impl Session {
    /// `ds_lib_init()` and the session: the device list is scanned, no
    /// device is selected yet (see [`set_default_device`](Self::set_default_device)).
    pub fn new(paths: Paths, usb: Arc<dyn UsbBackend>) -> Session {
        dsview_drivers::sys::set_user_dir(paths.data_dir.clone());
        if dsview_drivers::sys::res_dir().as_os_str().is_empty() {
            dsview_drivers::sys::set_res_dir(paths.data_dir.join("res"));
        }
        let inner = Arc::new_cyclic(|w: &Weak<Inner>| Inner {
            lib: Library::init(Arc::new(HostShim(w.clone())), usb),
            state: Mutex::new(State {
                trig_pos: MIN_TRIG_POS,
                trig_pos_max: MAX_TRIG_PERCENT,
                repeat_interval: 1.0,
                ..State::default()
            }),
            changed: Condvar::new(),
            subscribers: Mutex::new(Vec::new()),
            last_sent: Mutex::new((None, None)),
            data: [Data::new(), Data::new()],
            view: AtomicUsize::new(0),
            capture: AtomicUsize::new(0),
            capture_ended: AtomicBool::new(true),
            trigger: Mutex::new(Trigger::default()),
            data_dir: paths.data_dir,
            profile_dir: paths.profile_dir,
            version: paths.version,
        });
        Session(inner)
    }

    /// `ds_lib_exit()`: stops a capture and closes the devices.
    pub fn exit(&self) {
        if self.state().working {
            self.stop_capture();
        }
        self.save_config();
        self.clear_all_decoders();
        self.0.lib.exit();
    }

    pub fn library(&self) -> &Library {
        &self.0.lib
    }

    /// The data shown (`_view_data`): what readouts, decoders and saving use.
    pub fn data(&self) -> &Data {
        &self.0.data[self.0.view.load(Ordering::Acquire)]
    }

    /// The buffer a capture writes (`_capture_data`).
    fn capture_data(&self) -> &Data {
        &self.0.data[self.0.capture.load(Ordering::Acquire)]
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.0.data_dir
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.0.state)
    }

    fn notify(&self) {
        self.0.changed.notify_all();
    }

    /// Events from now on; the receiver may be dropped at any time.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<SessionEvent> {
        let (tx, rx) = std::sync::mpsc::channel();
        lock(&self.0.subscribers).push(tx);
        rx
    }

    fn emit(&self, ev: SessionEvent) {
        let throttle = match ev {
            SessionEvent::DataAppended => Some(0),
            SessionEvent::DecodeProgress => Some(1),
            _ => None,
        };
        if let Some(k) = throttle {
            let now = Instant::now();
            let mut last = lock(&self.0.last_sent);
            let slot = if k == 0 { &mut last.0 } else { &mut last.1 };
            if slot.is_some_and(|t| now.duration_since(t) < EVENT_INTERVAL) {
                return;
            }
            *slot = Some(now);
        }
        lock(&self.0.subscribers).retain(|tx| tx.send(ev.clone()).is_ok());
    }

    /// Waits until `cond` holds or `timeout` passes; true if it holds.
    pub fn wait(&self, timeout: Duration, cond: impl Fn(&State) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut st = self.state();
        loop {
            if cond(&st) {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            // wake regularly: some changes (store contents) do not notify
            st = self
                .0
                .changed
                .wait_timeout(st, (deadline - now).min(Duration::from_millis(100)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    // ------------------------------------------------------------ device access

    fn active(&self) -> Option<Entry> {
        self.0.lib.active()
    }

    /// `sr_config_get` on the active device: `None` when it fails or gives
    /// no value.
    pub fn config(&self, key: i32, ch: Option<usize>) -> Option<ConfValue> {
        let e = self.active()?;
        let mut d = e.lock();
        d.config_get(key, ch).ok().flatten()
    }

    pub fn config_u64(&self, key: i32) -> Option<u64> {
        self.config(key, None).map(|v| v.get_u64())
    }

    pub fn config_bool(&self, key: i32) -> Option<bool> {
        self.config(key, None).map(|v| v.get_bool())
    }

    pub fn config_f64(&self, key: i32) -> Option<f64> {
        self.config(key, None).map(|v| v.get_f64())
    }

    pub fn set_config(&self, key: i32, v: ConfValue, ch: Option<usize>) -> bool {
        let Some(e) = self.active() else { return false };
        e.lock().config_set(key, &v, ch).is_ok()
    }

    pub fn config_list(&self, key: i32) -> Option<ConfValue> {
        let e = self.active()?;
        let d = e.lock();
        d.config_list(key).ok()
    }

    /// The active device's channels.
    pub fn channels(&self) -> Vec<Channel> {
        self.active()
            .map(|e| e.lock().channels().to_vec())
            .unwrap_or_default()
    }

    pub fn modes(&self) -> Vec<Mode> {
        self.active()
            .map(|e| e.lock().mode_list())
            .unwrap_or_default()
    }

    /// `DeviceAgent::get_work_mode()` (logic without a device).
    pub fn mode(&self) -> i32 {
        self.active().map_or(0, |e| e.lock().info().mode as i32)
    }

    /// `get_sample_rate()`: the external clock rate when the clock is external.
    pub fn sample_rate(&self) -> u64 {
        let key = if self.config_bool(key::CLOCK_TYPE).unwrap_or(false) {
            key::EXT_SAMPLERATE
        } else {
            key::SAMPLERATE
        };
        self.config_u64(key).unwrap_or(0)
    }

    /// `get_sample_limit()`
    pub fn sample_limit(&self) -> u64 {
        self.config_u64(key::LIMIT_SAMPLES).unwrap_or(0)
    }

    /// The rates of `SR_CONF_SAMPLERATE`'s list.
    pub fn sample_rates(&self) -> Vec<u64> {
        match self.config_list(key::SAMPLERATE) {
            Some(ConfValue::Dict(d)) => d
                .into_iter()
                .find(|(k, _)| k == "samplerates")
                .and_then(|(_, v)| match v {
                    ConfValue::U64s(r) => Some(r),
                    _ => None,
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub fn have_enabled_channel(&self) -> bool {
        self.channels().iter().any(|c| c.enabled)
    }

    /// `ds_enable_device_channel_index`
    pub fn enable_channel(&self, index: u16, enabled: bool) -> bool {
        let Some(e) = self.active() else { return false };
        let mut d = e.lock();
        match d.channels_mut().iter_mut().find(|c| c.index == index) {
            Some(c) => {
                c.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Changes channel `index` of type `ty` in place (the GUI writes
    /// `sdi->channels` directly); false if there is none.
    pub fn update_channel(&self, index: u16, ty: i32, f: impl FnOnce(&mut Channel)) -> bool {
        let Some(e) = self.active() else { return false };
        let mut d = e.lock();
        match d
            .channels_mut()
            .iter_mut()
            .find(|c| c.index == index && c.ty == ty)
        {
            Some(c) => {
                f(c);
                true
            }
            None => false,
        }
    }

    /// `ds_set_device_channel_name`
    pub fn set_channel_name(&self, index: u16, name: &str) -> bool {
        let Some(e) = self.active() else { return false };
        let mut d = e.lock();
        match d.channels_mut().iter_mut().find(|c| c.index == index) {
            Some(c) => {
                c.name = name.to_owned();
                true
            }
            None => false,
        }
    }

    // ------------------------------------------------------------ devices

    /// `ds_get_device_list()` and the active index.
    pub fn devices(&self) -> (Vec<Entry>, i32) {
        (self.0.lib.devices(), self.0.lib.active_index())
    }

    /// `SigSession::set_default_device()`: the last device of the list.
    pub fn set_default_device(&self) -> bool {
        let list = self.0.lib.devices();
        match list.last() {
            Some(e) => self.set_device(e.handle),
            None => false,
        }
    }

    /// `SigSession::set_device()` with MainWindow's reaction to the change
    /// (`DSV_MSG_CURRENT_DEVICE_CHANGED`): a demo in logic mode loads the
    /// decoders of its pattern file, a capture file loads its session and
    /// replays.
    pub fn set_device(&self, handle: Handle) -> bool {
        // the device selector saves the settings of the device it leaves
        self.save_config();
        self.clear_all_decoders();
        if self.0.lib.activate(handle).is_err() {
            sr_err!("Switch device error!");
            return false;
        }
        let desc = self.active().map(|e| DeviceDesc {
            handle: e.handle,
            name: e.name.clone(),
            driver: driver_name(&e.kind),
            kind: e.kind.clone(),
        });
        {
            let mut st = self.state();
            st.device = desc.clone();
            st.stream_mode = false;
            st.cursors.clear();
            st.region = None;
        }
        self.clear_data();
        self.init_signals();
        self.sync_snap_settings();
        self.device_updated();

        let desc = desc.expect("activated");
        self.load_device_config();
        if desc.is_file() {
            crate::store::load_file_session(self);
        } else if desc.is_demo() && self.mode() == Mode::Logic as i32 {
            self.load_demo_decoders();
        }
        self.emit(SessionEvent::DeviceChanged);
        let err = self.0.lib.last_error();
        // SR_ERR_DEVICE_FIRMWARE_VERSION_LOW .. SR_ERR_DEVICE_NO_DRIVER
        !(1000..1100).contains(&err)
    }

    /// The hardware profile of the active device and mode
    /// (`gen_config_file_path(true)`): `<profile dir>/<driver><mode>.ses.dsc`,
    /// the file Qt DSView uses too.
    fn profile_path(&self) -> Option<PathBuf> {
        let d = self.state().device.clone()?;
        if !d.is_hardware() {
            return None;
        }
        let dir = self.0.profile_dir.as_ref()?;
        Some(dir.join(format!("{}{}.ses.dsc", d.driver, self.mode())))
    }

    /// `gen_config_file_path(false)`: the profile of DSView versions that
    /// kept one per language, `<driver><mode>.ses<language>.dsc`. The
    /// language is Qt's setting, unknown here: the first such file by name.
    fn legacy_profile_path(&self) -> Option<PathBuf> {
        let d = self.state().device.clone()?;
        let dir = self.0.profile_dir.as_ref()?;
        let prefix = format!("{}{}.ses", d.driver, self.mode());
        let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_prefix(&prefix))
                    .and_then(|n| n.strip_suffix(".dsc"))
                    .is_some_and(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
            })
            .collect();
        found.sort();
        found.into_iter().next()
    }

    /// `MainWindow::load_config_from_file`: a `.dsc` profile chosen by the
    /// user (File > Load); the decoders are replaced by its own.
    pub fn load_profile(&self, path: &std::path::Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|_| "Warning: Couldn't open device profile to load!".to_string())?;
        let doc: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        while !self.state().decoders.is_empty() {
            self.remove_decoder(0);
        }
        if crate::store::load_config_from_json(self, &doc) {
            let hardware = self
                .state()
                .device
                .as_ref()
                .is_some_and(DeviceDesc::is_hardware);
            if hardware {
                self.state().profile_file = Some(path.to_owned());
            }
            Ok(())
        } else {
            Err("The profile does not suit this device or mode.".into())
        }
    }

    /// `MainWindow::save_config_to_file` (File > Store).
    pub fn store_profile(&self, path: &std::path::Path) -> Result<(), String> {
        let text = crate::store::qt_json(&crate::store::session_json(self, &self.0.version));
        std::fs::write(path, text)
            .map_err(|_| "Warning: Couldn't open profile to write!".to_string())
    }

    /// `FileBar::genDefaultSessionFile`: the shipped default profile of the
    /// device and mode (File > Default).
    pub fn default_profile_path(&self) -> Option<PathBuf> {
        let d = self.state().device.clone()?;
        Some(
            self.0
                .data_dir
                .join("res")
                .join(format!("{}{}.def.dsc", d.driver, self.mode())),
        )
    }

    /// `MainWindow::save_config()`: a hardware device's settings go to its
    /// profile.
    pub fn save_config(&self) {
        let Some(path) = self.profile_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = crate::store::qt_json(&crate::store::session_json(self, &self.0.version));
        sr_info!("Store session to file: \"{}\"", path.display());
        if let Err(e) = std::fs::write(&path, text) {
            sr_err!("Warning: Couldn't open profile to write! {e}");
        }
    }

    /// `MainWindow::load_device_config()`:
    /// - hardware: the user's profile for the device and mode, else the one
    ///   of an older DSView kept per language, else the one shipped in
    ///   `res/` (`<driver><mode>.def.dsc`);
    /// - a demo: the profile shipped for its mode (`res/virtual-demo<mode>.dsc`,
    ///   only DSO has one).
    fn load_device_config(&self) {
        self.state().profile_file = None;
        let Some(d) = self.state().device.clone() else {
            return;
        };
        let res = dsview_drivers::sys::res_dir();
        let file = if d.is_hardware() {
            match self
                .profile_path()
                .filter(|p| p.is_file())
                .or_else(|| self.legacy_profile_path())
            {
                Some(p) => p,
                None => res.join(format!("{}{}.def.dsc", d.driver, self.mode())),
            }
        } else if d.is_demo() {
            res.join(format!("{}{}.dsc", d.driver, self.mode()))
        } else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&file) else {
            return;
        };
        match serde_json::from_str(&text) {
            Ok(doc) => {
                if crate::store::load_config_from_json(self, &doc) && d.is_hardware() {
                    self.state().profile_file = Some(file);
                }
            }
            Err(e) => sr_err!("Profile {}: {e}", file.display()),
        }
    }

    /// `load_demo_decoder_config(pattern)` for a demo's current pattern.
    fn load_demo_decoders(&self) {
        let pattern = self
            .config(key::PATTERN_MODE, None)
            .and_then(|v| v.get_str().map(str::to_owned))
            .unwrap_or_default();
        if pattern == "random" || pattern.is_empty() {
            return;
        }
        let file = self
            .0
            .data_dir
            .join("demo/logic")
            .join(format!("{pattern}.demo"));
        crate::store::load_demo_decoders(self, &file);
    }

    /// A demo's pattern changed (`DSV_MSG_DEMO_OPERATION_MODE_CHNAGED`): new
    /// signals and selectors; a protocol pattern brings its decoders.
    pub fn set_demo_pattern(&self, pattern: &str) -> bool {
        if !self.set_config(key::PATTERN_MODE, ConfValue::Str(pattern.to_owned()), None) {
            return false;
        }
        self.clear_all_decoders();
        self.clear_data();
        self.init_signals();
        self.sync_snap_settings();
        self.device_updated();
        if pattern != "random" && self.mode() == Mode::Logic as i32 {
            self.state().collect_mode = CollectMode::Single;
            self.load_demo_decoders();
        }
        true
    }

    /// `SigSession::switch_work_mode()` with MainWindow's demo reaction.
    pub fn switch_work_mode(&self, mode: i32) -> bool {
        if self.mode() == mode {
            return false;
        }
        let was_logic = self.mode() == Mode::Logic as i32;
        self.save_config();
        self.set_config(key::DEVICE_MODE, ConfValue::I16(mode as i16), None);
        if was_logic {
            self.clear_all_decoders();
        }
        self.state().stream_mode = false;
        self.clear_data();
        self.init_signals();
        self.sync_snap_settings();
        self.device_updated();
        self.load_device_config();
        let demo = self
            .state()
            .device
            .as_ref()
            .is_some_and(DeviceDesc::is_demo);
        if demo && mode == Mode::Logic as i32 {
            self.load_demo_decoders();
        }
        self.emit(SessionEvent::ModeChanged);
        true
    }

    /// `init_signals()`: new signals for the device's channels.
    pub fn init_signals(&self) {
        self.state().signals.clear();
        self.reload_signals();
    }

    /// `reload()`: like [`init_signals`](Self::init_signals), but signals that
    /// stay keep their name, trigger and position.
    pub fn reload_signals(&self) {
        let mode = self.mode();
        let channels = self.channels();
        let mut st = self.state();
        let old = std::mem::take(&mut st.signals);
        let want = match mode {
            1 => channel_type::DSO,
            2 => channel_type::ANALOG,
            _ => channel_type::LOGIC,
        };
        for c in channels.iter().filter(|c| c.ty == want) {
            if !c.enabled && c.ty != channel_type::DSO {
                continue;
            }
            let prev = old.iter().find(|s| s.index == c.index && s.ty == c.ty);
            st.signals.push(match prev {
                Some(p) => p.clone(),
                None => Signal {
                    index: c.index,
                    ty: c.ty,
                    name: c.name.clone(),
                    trig: trig::NONE,
                    view_index: -1,
                    colour: None,
                },
            });
        }
        drop(st);
        self.notify();
    }

    /// Renames signal `index` (`Header` rename: the trace and the channel).
    pub fn set_signal_name(&self, index: u16, ty: i32, name: &str) {
        if let Some(sig) = self
            .state()
            .signals
            .iter_mut()
            .find(|x| x.index == index && x.ty == ty)
        {
            sig.name = name.to_owned();
        }
        self.set_channel_name(index, name);
        self.notify();
    }

    /// Sets signal `index`'s colour (none: the default).
    pub fn set_signal_colour(&self, index: u16, ty: i32, colour: Option<[u8; 3]>) {
        if let Some(sig) = self
            .state()
            .signals
            .iter_mut()
            .find(|x| x.index == index && x.ty == ty)
        {
            sig.colour = colour;
        }
        self.notify();
    }

    /// The header's drag reorder: new view indexes for signals and decoder
    /// traces, in the order given (`Header::mouseReleaseEvent`).
    pub fn set_view_order(&self, order: &[TraceRef]) {
        let mut st = self.state();
        for (vi, r) in order.iter().enumerate() {
            match *r {
                TraceRef::Signal { index, ty } => {
                    if let Some(sig) = st
                        .signals
                        .iter_mut()
                        .find(|x| x.index == index && x.ty == ty)
                    {
                        sig.view_index = vi as i32;
                    }
                }
                TraceRef::Decoder(i) => {
                    if let Some(t) = st.decoders.get(i) {
                        *lock(&t.view_index) = vi as i32;
                    }
                }
            }
        }
        drop(st);
        self.notify();
    }

    /// The simple trigger of one logic signal (header trigger squares).
    pub fn set_signal_trigger(&self, index: u16, t: i32) {
        if let Some(sig) = self
            .state()
            .signals
            .iter_mut()
            .find(|x| x.index == index && x.ty == channel_type::LOGIC)
        {
            sig.trig = if (trig::RISING..=trig::EDGE).contains(&t) {
                t
            } else {
                trig::NONE
            };
        }
        self.notify();
    }

    /// `get_ch_num(type)`: enabled signals of a type.
    pub fn ch_num(&self, ty: i32) -> usize {
        let channels = self.channels();
        self.state()
            .signals
            .iter()
            .filter(|s| s.ty == ty)
            .filter(|s| {
                channels
                    .iter()
                    .any(|c| c.index == s.index && c.ty == s.ty && c.enabled)
            })
            .count()
    }

    /// `set_cur_snap_samplerate` / `set_cur_samplelimits` from the device.
    fn sync_snap_settings(&self) {
        let rate = self.sample_rate();
        let limit = self.sample_limit();
        let mut st = self.state();
        st.cur_snap_samplerate = rate;
        st.cur_samplelimits = limit;
    }

    /// `TriggerDock::device_updated()` and the sampling bar's lists.
    pub fn device_updated(&self) {
        self.update_trigger_range();
        self.update_sample_rate_list();
    }

    /// `TriggerDock::device_updated()`: the position slider's range.
    pub fn update_trigger_range(&self) {
        if self.mode() != Mode::Logic as i32 {
            return;
        }
        let Some(depth) = self.config_u64(key::HW_DEPTH) else {
            return;
        };
        let stream = self.config_bool(key::STREAM).unwrap_or(false);
        let limit = self.sample_limit();
        let max = if stream {
            1
        } else if depth >= limit {
            MAX_TRIG_PERCENT
        } else {
            (depth as f64 * MAX_TRIG_PERCENT as f64 / limit as f64).ceil() as i32
        };
        let virt = self
            .state()
            .device
            .as_ref()
            .is_some_and(|d| d.is_demo() || d.is_file());
        let mut st = self.state();
        st.trig_pos_max = max.max(MIN_TRIG_POS);
        st.trig_pos = st.trig_pos.clamp(MIN_TRIG_POS, st.trig_pos_max);
        // virtual devices and streams have the simple trigger only
        if virt || stream {
            st.adv_trigger.advanced = false;
        }
    }

    /// The trigger position slider (`set_session(["triggerPos"])`): clamped
    /// to its range.
    pub fn set_trigger_position(&self, pos: i32) {
        let mut st = self.state();
        st.trig_pos = pos.clamp(MIN_TRIG_POS, st.trig_pos_max);
    }

    /// `View::set_trig_cursor_posistion`: where the trigger cursor shows.
    pub fn set_trigger_cursor(&self, index: u64) {
        self.state().trigger_pos = index;
        self.notify();
    }

    /// `LogicSignal::set_trig` on every logic signal: `trig` on `channel`,
    /// none elsewhere. False if `channel` has no signal.
    pub fn set_simple_trigger(&self, channel: i32, t: i32) -> bool {
        let mut st = self.state();
        let mut found = t == trig::NONE;
        for s in st
            .signals
            .iter_mut()
            .filter(|s| s.ty == channel_type::LOGIC)
        {
            let target = s.index as i32 == channel;
            found |= target;
            s.trig = if target && (trig::RISING..=trig::EDGE).contains(&t) {
                t
            } else {
                trig::NONE
            };
        }
        found
    }

    // ------------------------------------------------------------ sampling bar

    /// `update_sample_rate_list()`: rebuilds both selectors from the device.
    pub fn update_sample_rate_list(&self) {
        let rates = self.sample_rates();
        let device_rate = self.sample_rate();
        let mut sel = self.state().sel.clone();
        sel.rates = rates;
        sel.rate = 0;
        // update_sample_rate_selector_value
        if device_rate != sel.rate() {
            for i in (0..sel.rates.len()).rev() {
                if device_rate >= sel.rates[i] {
                    sel.rate = i;
                    break;
                }
            }
        }
        self.state().sel = sel;
        self.update_sample_count_selector();
    }

    /// `update_sample_count_selector()`: the duration list from the depth.
    fn update_sample_count_selector(&self) {
        let mode = self.mode();
        let stream = self.config_bool(key::STREAM).unwrap_or(false);
        let hw_depth = self.config_u64(key::HW_DEPTH).unwrap_or(0);
        let sw_depth = if mode == Mode::Logic as i32 {
            LOGIC_MAX_SW_DEPTH
        } else {
            ANALOG_MAX_SW_DEPTH
        };
        let (mut rle_support, mut rle_depth) = (false, 0);
        let (mut max_tb, mut min_tb) = (0u64, 10u64);
        if mode == Mode::Logic as i32 {
            rle_support = self.config_bool(key::RLE_SUPPORT).unwrap_or(false);
            if rle_support {
                rle_depth = (hw_depth * 1024).min(sw_depth);
            }
        } else if mode == Mode::Dso as i32 {
            max_tb = self.config_u64(key::MAX_TIMEBASE).unwrap_or(0);
            min_tb = self.config_u64(key::MIN_TIMEBASE).unwrap_or(10);
        }
        let e8 = self
            .state()
            .device
            .as_ref()
            .is_some_and(|d| d.is_hardware() && d.name == "MSO-E8");

        let mut sel = self.state().sel.clone();
        let pre_duration = if sel.durations.is_empty() {
            SEC
        } else {
            sel.duration().0
        };
        let rate = sel.rate() as f64;
        let hw_duration = hw_depth as f64 / (rate * (1.0 / SEC));
        let mut duration = if mode == Mode::Dso as i32 {
            max_tb as f64
        } else if stream {
            sw_depth as f64 / (rate * (1.0 / SEC))
        } else if rle_support {
            rle_depth as f64 / (rate * (1.0 / SEC))
        } else {
            let mut d = hw_duration;
            if e8 {
                let mut unit = 1e6;
                while unit > 1.0 && d < unit {
                    unit /= 1000.0;
                }
                let mut rounded = (d / unit).round() * unit;
                if rounded > hw_duration {
                    rounded = (d / unit).floor() * unit;
                }
                if rounded > 0.0 {
                    d = rounded;
                }
            }
            d
        };

        const DAY: f64 = 86_400e9;
        const HOUR: f64 = 3_600e9;
        const MIN: f64 = 60e9;
        sel.durations.clear();
        // a zero or invalid start would loop forever; C asserts it is > 0
        if duration > 0.0 && duration.is_finite() {
            loop {
                let rle = mode != Mode::Dso as i32 && !stream && duration > hw_duration;
                sel.durations.push((duration, rle));
                let unit = if duration >= DAY {
                    DAY
                } else if duration >= HOUR {
                    HOUR
                } else if duration >= MIN {
                    MIN
                } else {
                    1.0
                };
                let l = 10f64.powf((duration / unit).log10().floor());
                duration = if duration > 5.0 * l * unit {
                    5.0 * l * unit
                } else if duration > 2.0 * l * unit {
                    2.0 * l * unit
                } else if duration > l * unit {
                    l * unit
                } else if l > 1.0 {
                    duration * 0.5
                } else if unit == DAY {
                    20.0 * HOUR
                } else if unit == HOUR {
                    50.0 * MIN
                } else if unit == MIN {
                    50e9
                } else {
                    duration * 0.5
                };
                let not_last = if mode == Mode::Dso as i32 {
                    duration >= min_tb as f64
                } else if mode == Mode::Analog as i32 {
                    duration >= 200e6 && duration / SEC * rate >= 1024.0
                } else {
                    duration / SEC * rate >= 1024.0
                };
                if !not_last || sel.durations.len() > 10_000 {
                    break;
                }
            }
        }

        let n = sel.durations.len();
        sel.duration = 0;
        if n > 0 {
            if pre_duration > sel.durations[0].0 {
                sel.duration = 0;
            } else if pre_duration < sel.durations[n - 1].0 {
                sel.duration = n - 1;
            } else if let Some(i) = sel.durations.iter().position(|d| pre_duration >= d.0) {
                sel.duration = i;
            }
        }
        self.state().sel = sel;
        self.update_sample_count_selector_value();
        // on_samplecount_sel
        if mode == Mode::Dso as i32 {
            self.commit_hori_res();
            if !self.have_view_data() {
                self.sync_snap_settings();
            }
        }
    }

    /// `update_sample_count_selector_value()`: selects the device's depth.
    fn update_sample_count_selector_value(&self) {
        let duration = if self.mode() == Mode::Dso as i32 {
            match self.config_u64(key::TIMEBASE) {
                Some(v) => v as f64,
                None => return,
            }
        } else {
            let Some(v) = self.config_u64(key::LIMIT_SAMPLES) else {
                return;
            };
            v as f64 / self.sample_rate() as f64 * SEC
        };
        let mut st = self.state();
        if duration != st.sel.duration().0
            && let Some(i) = st.sel.durations.iter().position(|d| duration >= d.0)
        {
            st.sel.duration = i;
        }
    }

    /// `commit_hori_res()` (DSO): rate and timebase from the selected
    /// duration per division.
    fn commit_hori_res(&self) {
        let hori_res = self.state().sel.duration().0;
        let limit = self.sample_limit();
        let Some(max_rate) = self.config_u64(key::MAX_DSO_SAMPLERATE) else {
            sr_err!("ERROR: config_get SR_CONF_MAX_DSO_SAMPLERATE failed.");
            return;
        };
        let ch = self.ch_num(channel_type::DSO).max(1) as u64;
        let rate = ((limit.wrapping_mul(1_000_000_000) as f64 / (hori_res * DSO_HDIVS as f64))
            as u64)
            .min(max_rate / ch);
        // set_sample_rate: the largest listed rate not above it, else the first
        {
            let mut st = self.state();
            let n = st.sel.rates.len();
            if let Some(i) = (0..n).rev().find(|&i| rate >= st.sel.rates[i]) {
                st.sel.rate = i;
            } else if n > 0 {
                st.sel.rate = 0;
            }
        }
        self.commit_settings();
        self.set_config(key::TIMEBASE, ConfValue::U64(hori_res as u64), None);
    }

    /// The sampling bar's rate list and the selected entry.
    pub fn rate_selector(&self) -> (Vec<u64>, usize) {
        let st = self.state();
        (st.sel.rates.clone(), st.sel.rate)
    }

    /// The sampling bar's duration list (ns, labelled RLE) and the
    /// selected entry; in DSO mode the time per division.
    pub fn duration_selector(&self) -> (Vec<(f64, bool)>, usize) {
        let st = self.state();
        (st.sel.durations.clone(), st.sel.duration)
    }

    /// Selects a sample rate (`on_samplerate_sel`): the duration list
    /// follows; the device gets it with `commit_settings` (at the latest
    /// when a capture starts).
    pub fn select_rate(&self, i: usize) {
        {
            let mut st = self.state();
            if i >= st.sel.rates.len() {
                return;
            }
            st.sel.rate = i;
        }
        if self.mode() != Mode::Dso as i32 {
            self.update_sample_count_selector();
        }
        self.notify();
    }

    /// Selects a duration (`on_samplecount_sel`); in DSO mode the timebase
    /// and rate go to the device at once (`commit_hori_res`).
    pub fn select_duration(&self, i: usize) {
        {
            let mut st = self.state();
            if i >= st.sel.durations.len() {
                return;
            }
            st.sel.duration = i;
        }
        if self.mode() == Mode::Dso as i32 {
            self.commit_hori_res();
            if !self.have_view_data() {
                self.sync_snap_settings();
            }
        }
        self.notify();
    }

    /// `SamplingBar::commit_settings()`: pushes the selected rate and depth
    /// (rounded up to 1024 samples) to the device.
    pub fn commit_settings(&self) {
        if self.config_bool(key::TEST).unwrap_or(false) {
            let device_rate = self.sample_rate();
            {
                let mut st = self.state();
                if device_rate != st.sel.rate()
                    && let Some(i) = (0..st.sel.rates.len())
                        .rev()
                        .find(|&i| device_rate >= st.sel.rates[i])
                {
                    st.sel.rate = i;
                }
            }
            self.update_sample_count_selector_value();
            return;
        }
        let (rate, (duration, rle)) = {
            let st = self.state();
            (st.sel.rate(), st.sel.duration())
        };
        self.state().capture_work_time = duration as u64;
        if self.active().is_none() {
            return;
        }
        if rate != self.sample_rate() {
            self.set_config(key::SAMPLERATE, ConfValue::U64(rate), None);
        }
        if self.mode() != Mode::Dso as i32 {
            let count =
                ((duration / SEC * rate as f64).ceil() as u64 + SAMPLES_ALIGN) & !SAMPLES_ALIGN;
            if count != self.sample_limit() {
                self.set_config(key::LIMIT_SAMPLES, ConfValue::U64(count), None);
            }
            self.set_config(key::RLE, ConfValue::Bool(rle), None);
        }
    }

    // ------------------------------------------------------------ capture

    pub fn is_working(&self) -> bool {
        let st = self.state();
        st.working || st.running
    }

    /// `is_realtime_refresh()`: the view follows the data while capturing
    /// (loop mode, or a stream that is shown as it arrives).
    pub fn is_realtime_refresh(&self) -> bool {
        let st = self.state();
        st.working
            && (st.collect_mode == CollectMode::Loop
                || (st.stream_mode
                    && (st.collect_mode == CollectMode::Single
                        || self.0.view.load(Ordering::Acquire)
                            == self.0.capture.load(Ordering::Acquire))))
    }

    pub fn is_decoding(&self) -> bool {
        self.state().decoding
    }

    /// `have_hardware_data()`: a hardware device's capture is shown.
    pub fn have_hardware_data(&self) -> bool {
        self.state()
            .device
            .as_ref()
            .is_some_and(DeviceDesc::is_hardware)
            && self.have_view_data()
    }

    /// `is_first_store_confirm()`: true once per capture, for the first
    /// "Save captured data?".
    pub fn is_first_store_confirm(&self) -> bool {
        let mut st = self.state();
        if st.started_seq != st.confirm_store_seq {
            st.confirm_store_seq = st.started_seq;
            return true;
        }
        false
    }

    /// `clear_store_confirm_flag()`: the capture was saved.
    pub fn clear_store_confirm_flag(&self) {
        let mut st = self.state();
        st.confirm_store_seq = st.started_seq;
    }

    /// `have_view_data()`: the current mode's store has samples.
    pub fn have_view_data(&self) -> bool {
        match self.mode() {
            1 => lock(&self.data().dso).sample_count() > 0,
            2 => lock(&self.data().analog).sample_count() > 0,
            _ => lock(&self.data().logic).sample_count() > 0,
        }
    }

    /// Clears both buffers and makes the shown one the capture buffer.
    fn clear_data(&self) {
        self.0.data[0].clear();
        self.0.data[1].clear();
        let v = self.0.view.load(Ordering::Acquire);
        self.0.capture.store(v, Ordering::Release);
        let mut st = self.state();
        st.trigger_pos = 0;
        st.trig_cursor = false;
        drop(st);
    }

    /// `set_collect_mode()`; also the interval between repeated captures.
    pub fn set_collect_mode(&self, mode: CollectMode, repeat_interval: f64) {
        let mut st = self.state();
        st.collect_mode = mode;
        st.repeat_interval = repeat_interval.max(0.0);
    }

    /// `TriggerDock::try_commit_trigger()`: the dock's advanced or serial
    /// trigger, else the channels' simple ones.
    fn commit_trigger(&self) {
        let mut t = Trigger::default();
        let ch_num = self
            .config(key::TOTAL_CH_NUM, None)
            .map_or(16, |v| v.get_i16() as usize)
            .clamp(1, dsview_core::MAX_TRIGGER_PROBES);
        let st = self.state();
        if st.setup.mode == Mode::Logic as i32
            && !st.instant
            && !st.adv_trigger.commit(&mut t, st.trig_pos as u16, ch_num)
        {
            let stage = dsview_core::TRIGGER_STAGES;
            for s in st.signals.iter().filter(|s| s.ty == channel_type::LOGIC) {
                let c = match s.trig {
                    trig::RISING => b'R',
                    trig::HIGH => b'1',
                    trig::FALLING => b'F',
                    trig::LOW => b'0',
                    trig::EDGE => b'C',
                    _ => b'X',
                };
                if c != b'X' {
                    t.trigger_en = 1;
                }
                if let Some(p) = t.trigger0[stage].get_mut(s.index as usize) {
                    *p = c;
                    t.trigger1[stage][s.index as usize] = b'X';
                }
            }
        }
        drop(st);
        *lock(&self.0.trigger) = t;
    }

    /// `SamplingBar::action_run_stop` / `action_instant_stop` and
    /// `SigSession::start_capture`. False if the capture did not start.
    pub fn start_capture(&self, instant: bool) -> bool {
        if self.is_working() {
            return false;
        }
        if self.active().is_none() {
            return false;
        }
        self.commit_settings();
        if self.state().signals.is_empty() {
            sr_info!("ERROR: channel list is empty, unable to capture data.");
            return false;
        }
        if self.0.lib.is_collecting() {
            sr_err!("Error!Device is running.");
            return false;
        }
        self.clear_all_decode_tasks();
        self.clear_decode_results();
        self.clear_data();
        let mode = self.mode();
        let desc = self.state().device.clone();
        let is_file = desc.as_ref().is_some_and(DeviceDesc::is_file);
        {
            let mut st = self.state();
            st.stream_mode = false;
            st.capture_times = 0;
            st.dso_packets = 0;
            st.dso_status = None;
            st.session_time_ms = now_ms();
        }
        self.sync_snap_settings();
        if mode == Mode::Logic as i32 {
            let hardware_stream = self.config_bool(key::STREAM).unwrap_or(false);
            let stream = match &desc {
                Some(d) if d.is_hardware() => hardware_stream,
                Some(d) if d.is_demo() || d.is_file() => true,
                _ => false,
            };
            let pattern = self
                .config(key::PATTERN_MODE, None)
                .and_then(|v| v.get_str().map(str::to_owned))
                .unwrap_or_default();
            let lp = {
                let mut st = self.state();
                st.stream_mode = stream;
                if st.collect_mode == CollectMode::Repeat
                    && desc.as_ref().is_some_and(DeviceDesc::is_hardware)
                    && hardware_stream
                {
                    st.repeat_interval = 0.1;
                }
                // loop mode needs a stream; the demo loops its random pattern only
                if st.collect_mode == CollectMode::Loop
                    && (!stream
                        || (desc.as_ref().is_some_and(DeviceDesc::is_demo) && pattern != "random"))
                {
                    st.collect_mode = CollectMode::Single;
                }
                st.collect_mode == CollectMode::Loop && stream
            };
            if desc
                .as_ref()
                .is_some_and(|d| d.is_hardware() || d.is_demo())
            {
                self.set_config(key::LOOP_MODE, ConfValue::Bool(lp), None);
            }
        }
        if mode == Mode::Dso as i32 && desc.as_ref().is_some_and(DeviceDesc::is_hardware) {
            let rmin = self
                .config(key::REF_MIN, None)
                .map(|v| v.get_u32())
                .unwrap_or(0);
            let rmax = self
                .config(key::REF_MAX, None)
                .map(|v| v.get_u32())
                .unwrap_or(0);
            lock(&self.data().dso).set_ref_range(rmax, rmin);
        }
        self.state().instant = is_file || instant;
        self.exec_capture()
    }

    /// `exec_capture()` + `capture_init()`
    fn exec_capture(&self) -> bool {
        if self.0.lib.is_collecting() {
            return false;
        }
        if !self.have_enabled_channel() {
            sr_err!("No channels enabled!");
            return false;
        }
        let instant = self.state().instant;
        self.set_config(key::INSTANT, ConfValue::Bool(instant), None);
        self.sync_snap_settings();
        let channels = self.channels();
        let mode = self.mode();
        let setup = CaptureSetup {
            mode,
            limit: self.sample_limit(),
            logic: channels
                .iter()
                .filter(|c| c.ty == channel_type::LOGIC && c.enabled)
                .map(|c| c.index)
                .collect(),
            dso: channels
                .iter()
                .filter(|c| c.ty == channel_type::DSO)
                .map(|c| DsoChannel {
                    index: c.index,
                    enabled: c.enabled,
                })
                .collect(),
            analog: channels
                .iter()
                .filter(|c| c.ty == channel_type::ANALOG)
                .map(|c| AnalogChannel {
                    index: c.index,
                    enabled: c.enabled,
                })
                .collect(),
            dso_total: channels
                .iter()
                .filter(|c| c.ty == channel_type::DSO)
                .count(),
            dso_enabled: channels
                .iter()
                .filter(|c| c.ty == channel_type::DSO && c.enabled)
                .count(),
            is_file: self
                .state()
                .device
                .as_ref()
                .is_some_and(DeviceDesc::is_file),
        };
        let (swap, live) = {
            let mut st = self.state();
            st.capture_times += 1;
            st.captures = st.capture_times;
            st.triged = false;
            st.trig_flag = false;
            st.trig_ch = 0;
            if st.collect_mode != CollectMode::Repeat {
                st.trig_cursor = false;
            }
            st.error = crate::session::error::NONE;
            st.setup = setup;
            let logic = mode == Mode::Logic as i32;
            // repeat mode captures into the other buffer while one is shown
            let swap = st.collect_mode == CollectMode::Repeat
                && logic
                && (!st.stream_mode || st.capture_times > 1);
            // a stream shown as it arrives is decoded as it arrives
            let live = logic
                && st.stream_mode
                && match st.collect_mode {
                    CollectMode::Single => true,
                    CollectMode::Repeat => st.capture_times == 1,
                    CollectMode::Loop => false,
                };
            (swap, live)
        };
        if live {
            self.clear_all_decode_tasks();
            self.clear_decode_results();
        }
        self.0.capture_ended.store(!live, Ordering::Release);
        if swap {
            let other = 1 - self.0.view.load(Ordering::Acquire);
            self.0.capture.store(other, Ordering::Release);
            self.sync_snap_settings();
        }
        self.commit_trigger();
        self.capture_data().clear();
        if self.0.lib.start_collect(true).is_err() {
            sr_err!("Start collect error!");
            return false;
        }
        {
            let mut st = self.state();
            st.working = true;
            st.started_seq += 1;
        }
        if live {
            self.start_all_decoders();
        }
        self.notify();
        self.emit(SessionEvent::CaptureStarted);
        true
    }

    /// `SigSession::stop_capture()`: stops and waits for the capture thread.
    pub fn stop_capture(&self) -> bool {
        if !self.state().working {
            return false;
        }
        let wait_upload = self.mode() == Mode::Logic as i32
            && self.config_bool(key::WAIT_UPLOAD).unwrap_or(false);
        if wait_upload {
            sr_info!("Data is uploading from device data buffer, waiting for stop.");
            return false;
        }
        {
            let mut st = self.state();
            st.working = false;
            st.instant = false;
        }
        if self.0.lib.is_collecting() {
            let _ = self.0.lib.stop_collect();
        }
        self.notify();
        true
    }

    /// `get_capture_status()`: (triggered, progress percent), when the
    /// device reports a status.
    pub fn capture_status(&self) -> Option<(bool, i32)> {
        let limits = self.state().cur_samplelimits;
        let status = {
            let e = self.active()?;
            let d = e.lock();
            d.status(true).ok()?
        };
        let triggered = status.trig_hit & 1 != 0;
        let mut cnt = u64::from(status.captured_cnt0)
            + (u64::from(status.captured_cnt1) << 8)
            + (u64::from(status.captured_cnt2) << 16)
            + (u64::from(status.captured_cnt3) << 24)
            + (u64::from(status.trig_hit >> 2) << 32);
        let mode = self.mode();
        if mode == Mode::Dso as i32 {
            let n = self.state().signals.len() as u64;
            cnt = cnt * n / (self.ch_num(channel_type::DSO).max(1) as u64);
        }
        let limits_f = limits.max(1) as f64;
        let mut progress = if triggered {
            (limits.wrapping_sub(cnt) as f64 * 100.0 / limits_f) as i32
        } else {
            (cnt as f64 * 100.0 / limits_f) as i32
        };
        if progress == 100 && mode == Mode::Logic as i32 && !self.have_view_data() {
            progress = 0;
        }
        Some((triggered, progress))
    }

    /// `get_device_status(status, false)`
    fn device_status(&self) -> Option<dsview_core::Status> {
        let e = self.active()?;
        let d = e.lock();
        d.status(false).ok()
    }

    fn on_event(&self, ev: i32) {
        match ev {
            event::DEVICE_RUNNING => self.state().running = true,
            event::DEVICE_STOPPED => self.state().running = false,
            event::COLLECT_TASK_END
            | event::COLLECT_TASK_END_BY_ERROR
            | event::COLLECT_TASK_END_BY_DETACHED => {
                let mut st = self.state();
                st.running = false;
                if st.collect_mode == CollectMode::Repeat
                    && st.working
                    && ev == event::COLLECT_TASK_END
                {
                    // DSV_MSG_TRIG_NEXT_COLLECT: the next capture after the interval
                    let wait = Duration::from_secs_f64(st.repeat_interval);
                    drop(st);
                    let me = self.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(wait);
                        let again = {
                            let st = me.state();
                            st.working && st.collect_mode == CollectMode::Repeat
                        };
                        if again && !me.exec_capture() {
                            me.state().working = false;
                            me.state().ended_seq += 1;
                            me.notify();
                        }
                    });
                } else {
                    st.working = false;
                    st.instant = false;
                    st.ended_seq += 1;
                }
            }
            event::CURRENT_DEVICE_DETACH if self.state().working => {
                let me = self.clone();
                // not on the hotplug thread that reports it
                std::thread::spawn(move || me.stop_capture());
            }
            _ => {}
        }
        self.notify();
        match ev {
            event::COLLECT_TASK_END
            | event::COLLECT_TASK_END_BY_ERROR
            | event::COLLECT_TASK_END_BY_DETACHED => self.emit(SessionEvent::CaptureEnded),
            event::NEW_DEVICE_ATTACH
            | event::CURRENT_DEVICE_DETACH
            | event::INACTIVE_DEVICE_DETACH => self.emit(SessionEvent::DeviceListChanged),
            _ => {}
        }
    }

    /// `SigSession::data_feed_in()`
    fn data_feed_in(&self, p: &Packet) {
        let err_before = self.state().error;
        if !matches!(p.body, Body::End) && p.status != pkt_status::OK {
            sr_err!("Unexpected packet status: {}", p.status);
            self.state().error = error::PKT_DATA;
            if err_before != error::PKT_DATA {
                self.emit(SessionEvent::Error(error::PKT_DATA));
            }
            return;
        }
        let setup = self.state().setup.clone();
        match &p.body {
            Body::Header { .. } => {}
            Body::Trigger(t) => {
                let trigger_en = lock(&self.0.trigger).trigger_en != 0;
                let mut st = self.state();
                if setup.mode != Mode::Dso as i32 {
                    if t.status & 1 != 0 {
                        st.trigger_pos = u64::from(t.real_pos);
                        let hardware = st.device.as_ref().is_some_and(DeviceDesc::is_hardware);
                        if trigger_en || !hardware {
                            st.trig_cursor = true;
                        }
                    }
                } else if setup.dso_enabled > 0 {
                    st.trigger_pos =
                        u64::from(t.real_pos) * setup.dso_total as u64 / setup.dso_enabled as u64;
                }
            }
            Body::Logic { data, .. } => {
                let mut store = lock(&self.capture_data().logic);
                if store.memory_failed() {
                    return;
                }
                if !data.is_empty() {
                    let mut st = self.state();
                    if !st.triged {
                        st.triged = true;
                    }
                }
                if store.last_ended() {
                    store.set_loop(self.state().collect_mode == CollectMode::Loop);
                    // constant leaves stay while a live decode may still read them
                    let decoding_live = self.state().decoding
                        && self.0.view.load(Ordering::Acquire)
                            == self.0.capture.load(Ordering::Acquire);
                    store.first_payload(data, setup.limit, &setup.logic, !decoding_live);
                } else {
                    store.append_payload(data);
                }
                if store.memory_failed() {
                    self.state().error = error::MALLOC;
                }
            }
            Body::Dso {
                num_samples,
                data,
                samplerate_tog,
                trig_flag,
                trig_ch,
                ..
            } => {
                let mut store = lock(&self.capture_data().dso);
                if store.memory_failed() {
                    return;
                }
                let n = (*num_samples).max(0) as u64;
                if !self.state().instant
                    && let Some(status) = self.device_status()
                {
                    self.state().dso_status = Some(status);
                }
                let instant = {
                    let mut st = self.state();
                    st.dso_packets += 1;
                    if !st.triged && n > 0 {
                        st.triged = true;
                        st.session_time_ms = now_ms();
                    }
                    st.instant
                };
                if store.last_ended() {
                    store.first_payload(
                        data,
                        n,
                        setup.limit,
                        &setup.dso,
                        instant,
                        setup.is_file,
                        *samplerate_tog,
                    );
                } else {
                    store.append_payload(data, n, *samplerate_tog);
                }
                {
                    let mut st = self.state();
                    st.dso_seq += 1;
                    st.trig_flag = *trig_flag;
                    st.trig_ch = *trig_ch;
                }
                let first = self.state().dso_packets == 1;
                if n != 0 && (!instant || first) {
                    let rate = self.sample_rate();
                    self.state().cur_snap_samplerate = rate;
                }
                if store.memory_failed() {
                    self.state().error = error::MALLOC;
                }
            }
            Body::Analog {
                num_samples,
                unit_bits,
                unit_pitch,
                data,
                ..
            } => {
                let mut store = lock(&self.capture_data().analog);
                if store.memory_failed() {
                    return;
                }
                let n = (*num_samples).max(0) as u64;
                if store.last_ended() {
                    store.first_payload(
                        data,
                        n,
                        *unit_bits,
                        *unit_pitch,
                        setup.limit,
                        &setup.analog,
                    );
                } else {
                    store.append_payload(data, n, *unit_pitch);
                }
                if store.memory_failed() {
                    self.state().error = error::MALLOC;
                }
            }
            Body::Overflow => {
                let mut st = self.state();
                if st.error == error::NONE {
                    st.error = error::DATA_OVERFLOW;
                }
            }
            Body::End => {
                let d = self.capture_data();
                lock(&d.logic).capture_ended();
                lock(&d.dso).capture_ended();
                lock(&d.analog).capture_ended();
                if p.status != pkt_status::OK {
                    self.state().error = error::PKT_DATA;
                } else if setup.mode == Mode::Dso as i32 && self.state().instant {
                    if let Some(status) = self.device_status() {
                        self.state().dso_status = Some(status);
                    }
                } else if setup.mode == Mode::Logic as i32 {
                    self.rev_end_packet();
                }
            }
        }
        let err = self.state().error;
        if matches!(err, error::MALLOC | error::DATA_OVERFLOW) {
            // MainWindow::on_session_error stops the capture
            self.0.lib.request_stop();
        }
        match &p.body {
            Body::Logic { .. } | Body::Dso { .. } | Body::Analog { .. } => {
                self.emit(SessionEvent::DataAppended)
            }
            _ => {}
        }
        if err != error::NONE && err != err_before {
            self.emit(SessionEvent::Error(err));
        }
    }

    // ------------------------------------------------------------ decoders

    /// Appends a decoder trace (`SigSession::add_decoder`); returns its index.
    pub fn add_decoder(&self, trace: DecodeTrace) -> usize {
        let mut st = self.state();
        st.decoders.push(Arc::new(trace));
        st.decoders.len() - 1
    }

    /// Moves the last decoder trace to position `index` (an edited stack
    /// takes the place of the one it replaces).
    pub fn move_last_decoder_to(&self, index: usize) {
        let mut st = self.state();
        if let Some(t) = st.decoders.pop() {
            let i = index.min(st.decoders.len());
            st.decoders.insert(i, t);
        }
    }

    /// `remove_decoder`: stops its decoding and drops it.
    pub fn remove_decoder(&self, index: usize) -> bool {
        let t = {
            let mut st = self.state();
            if index >= st.decoders.len() {
                return false;
            }
            let t = st.decoders.remove(index);
            st.decode_queue.retain(|q| !Arc::ptr_eq(q, &t));
            t
        };
        t.stack.request_stop();
        self.notify();
        true
    }

    fn clear_all_decoders(&self) {
        let list = {
            let mut st = self.state();
            st.decode_queue.clear();
            std::mem::take(&mut st.decoders)
        };
        for t in list {
            t.stack.request_stop();
        }
        self.wait(Duration::from_secs(30), |st| !st.decoding);
    }

    fn clear_all_decode_tasks(&self) {
        let list = {
            let mut st = self.state();
            st.decode_queue.clear();
            st.decoders.clone()
        };
        for t in &list {
            t.stack.request_stop();
        }
        self.wait(Duration::from_secs(30), |st| !st.decoding);
    }

    fn clear_decode_results(&self) {
        for t in self.state().decoders.clone() {
            t.stack.clear();
        }
    }

    /// `DSV_MSG_REV_END_PACKET`: a logic capture ended; in repeat mode it
    /// becomes the shown data. The decoders run over it.
    fn rev_end_packet(&self) {
        let (mode, stream, times, working) = {
            let st = self.state();
            (
                st.collect_mode,
                st.stream_mode,
                st.capture_times,
                st.working,
            )
        };
        // in a single stream capture the decoders run already (live)
        let swap_always = self.state().swap_back_buffer_always;
        let (mut add, mut swap) = match mode {
            CollectMode::Single => (!stream, false),
            CollectMode::Loop => (true, false),
            CollectMode::Repeat => (!stream || times > 1, !stream || times > 1),
        };
        self.0.capture_ended.store(true, Ordering::Release);
        // a repeat capture that ends after the stop is dropped
        // (swapBackBufferAlways off)
        if mode == CollectMode::Repeat && !swap_always && !working && times > 1 {
            add = false;
            swap = false;
            self.capture_data().clear();
        }
        if add {
            self.clear_all_decode_tasks();
            self.clear_decode_results();
        }
        if swap {
            let c = self.0.capture.load(Ordering::Acquire);
            if self.0.view.load(Ordering::Acquire) != c {
                self.data().clear();
            }
            self.0.view.store(c, Ordering::Release);
        }
        if add {
            self.start_all_decoders();
        }
    }

    fn start_all_decoders(&self) {
        let list = self.state().decoders.clone();
        for t in list {
            self.add_decode_task(t);
        }
    }

    /// `start_decode(index)`: decodes one trace again.
    pub fn start_decode(&self, index: usize) -> bool {
        if !self.have_view_data() || self.is_working() {
            return false;
        }
        let Some(t) = self.state().decoders.get(index).cloned() else {
            return false;
        };
        {
            let mut st = self.state();
            st.decode_queue.retain(|q| !Arc::ptr_eq(q, &t));
        }
        t.stack.clear();
        self.add_decode_task(t);
        true
    }

    /// `add_decode_task`: queues a trace; one thread works the queue.
    fn add_decode_task(&self, t: Arc<DecodeTrace>) {
        let spawn = {
            let mut st = self.state();
            st.decode_queue.push_back(t);
            let spawn = !st.decoding;
            st.decoding = true;
            spawn
        };
        self.notify();
        if spawn {
            let me = self.clone();
            std::thread::Builder::new()
                .name("decode_task".into())
                .spawn(move || me.decode_task_proc())
                .expect("decode thread");
        }
    }

    fn decode_task_proc(&self) {
        loop {
            let (t, rate, last, signals) = {
                let mut st = self.state();
                let Some(t) = st.decode_queue.pop_front() else {
                    st.decoding = false;
                    break;
                };
                let signals: Vec<i32> = st
                    .signals
                    .iter()
                    .filter(|s| s.ty == channel_type::LOGIC)
                    .map(|s| s.index as i32)
                    .collect();
                (
                    t,
                    st.cur_snap_samplerate,
                    st.cur_samplelimits.wrapping_sub(1),
                    signals,
                )
            };
            t.stack.decode(
                &self.data().logic,
                &|i| signals.contains(&i),
                rate as f64,
                last,
                &self.0.capture_ended,
            );
            if self.0.capture_ended.load(Ordering::Acquire) {
                // the leaves kept for a live decode can go now
                lock(&self.data().logic).decode_end();
            }
            self.notify();
            self.emit(SessionEvent::DecodeProgress);
        }
        self.notify();
        self.emit(SessionEvent::DecodeDone);
    }

    /// Marks the session as saving (or done).
    pub fn set_saving(&self, saving: bool) {
        self.state().saving = saving;
        self.notify();
    }

    /// The simple-trigger state per logic signal and the dock's position,
    /// for saving.
    /// The trigger position slider's range (`MinTrigPosition`, the maximum
    /// `device_updated` sets).
    pub fn trigger_range(&self) -> (i32, i32) {
        (MIN_TRIG_POS, self.state().trig_pos_max)
    }

    pub fn trigger_position(&self) -> i32 {
        self.state().trig_pos
    }

    /// Loads a capture file as a device (`SigSession::set_file`).
    pub fn set_file(&self, path: &str) -> SrResult<()> {
        self.0.lib.device_from_file(path)?;
        if self.set_default_device() {
            Ok(())
        } else {
            Err(dsview_core::SrError::Err)
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
