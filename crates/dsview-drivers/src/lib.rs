//! DSView device drivers in Rust (docs/migration/PLAN.md, Phase 3).
//!
//! * [`demo`]: the "virtual-demo" driver (`libsigrok4DSL/hardware/demo`)
//! * [`file`]: the "virtual-session" driver playing `.dsl` files (`session_driver.c`)
//!
//! A driver hands out [`Device`]s. The C seam (`libsigrok4DSL/hardware/rust/
//! rs_driver.c`) wraps each in an `sr_dev_driver`, so DSView keeps calling
//! the same libsigrok API; later the Rust session drives them directly.
//!
//! Acquisition follows libsigrok's session loop: [`Device::poll`] is one call
//! of the C driver's source callback. Waits are returned as [`Step`]s
//! instead of slept in place, so the caller can sleep without holding the
//! device lock, and packets come back as owned values, so the caller can
//! forward them (and the GUI can call back into the device) unlocked.

pub mod demo;
pub mod file;
pub mod sys;

use dsview_core::{Channel, ConfValue, Mode, Packet, SrResult, Status};

/// What the session loop does after a [`Device::poll`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// This callback is done; call again on the next loop iteration.
    Continue,
    /// Sleep this many microseconds (`g_usleep`), then poll again within the
    /// same callback.
    SleepThenPoll(u64),
    /// Forward the packets, sleep this many microseconds, and end the callback.
    SleepThenReturn(u64),
    /// The source is removed (`sr_session_source_remove`): the loop ends.
    Remove,
}

/// Identity of a device instance (`struct sr_dev_inst`).
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceInfo {
    pub vendor: String,
    pub model: String,
    pub version: Option<String>,
    pub dev_type: i32,
    pub status: i32,
    pub mode: Mode,
    /// `sdi->path`: the pattern or capture file, `None` when unset.
    pub path: Option<String>,
}

/// One device of a driver, the Rust side of an `sr_dev_inst` plus the
/// driver's vtable entries for it. Methods mirror `struct sr_dev_driver`.
pub trait Device: Send {
    fn info(&self) -> DeviceInfo;
    /// `sdi->status`, written by the library as well as the driver.
    fn set_status(&mut self, status: i32);
    /// `dev_mode_list`: the modes in `sr_mode_list` order (logic, analog, DSO).
    fn mode_list(&self) -> Vec<Mode>;

    /// `sdi->channels`. The library and the GUI write some fields (enabled,
    /// name) directly, so the C seam copies them in before each call.
    fn channels(&self) -> &[Channel];
    fn channels_mut(&mut self) -> &mut [Channel];
    /// Changes whenever the channel list is rebuilt (C frees and recreates
    /// its `sr_channel`s at those points).
    fn channel_generation(&self) -> u64;

    fn open(&mut self) -> SrResult<()>;
    fn close(&mut self) -> SrResult<()>;

    /// `config_get`. `Ok(None)` is C's "SR_OK without setting *data".
    fn config_get(&self, key: i32, ch: Option<usize>) -> SrResult<Option<ConfValue>>;
    fn config_set(&mut self, key: i32, value: &ConfValue, ch: Option<usize>) -> SrResult<()>;
    fn config_list(&self, key: i32) -> SrResult<ConfValue>;
    fn status(&self, prg: bool) -> SrResult<Status>;

    /// `dev_acquisition_start`: prepares the capture (some drivers send
    /// packets here already); the session then calls [`Device::poll`] until
    /// it returns [`Step::Remove`] or is aborted.
    fn start(&mut self, out: &mut Vec<Packet>) -> SrResult<()>;
    /// One run of the source callback. `abort` is C's `revents == -1`: the
    /// session was told to stop and this is the last call.
    fn poll(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step;
    /// `dev_acquisition_stop`, called when the session loop ends.
    fn stop(&mut self, out: &mut Vec<Packet>) -> SrResult<()>;
}
