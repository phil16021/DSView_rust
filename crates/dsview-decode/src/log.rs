//! Log output of the engine (libsigrokdecode's `srd_err`/`srd_dbg`): sent to
//! a sink the embedder installs (the C seam forwards to DSView's xlog);
//! dropped when none is set.

use std::sync::RwLock;

/// Log levels as in libsigrokdecode (`SRD_LOG_ERR` .. `SRD_LOG_SPEW`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Err = 1,
    Warn = 2,
    Info = 3,
    Dbg = 4,
    Detail = 5,
}

type Sink = Box<dyn Fn(Level, &str) + Send + Sync>;

static SINK: RwLock<Option<Sink>> = RwLock::new(None);
/// Most verbose level passed to the sink (libsigrokdecode's log level).
static MAX_LEVEL: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(Level::Info as u8);

/// Sets the most verbose level that reaches the sink.
pub fn set_level(level: Level) {
    MAX_LEVEL.store(level as u8, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a message at `level` would be logged (format only then).
pub fn enabled(level: Level) -> bool {
    level as u8 <= MAX_LEVEL.load(std::sync::atomic::Ordering::Relaxed)
}

/// Installs the log sink (replacing any previous one).
pub fn set_sink(sink: impl Fn(Level, &str) + Send + Sync + 'static) {
    *SINK.write().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(sink));
}

pub(crate) fn log(level: Level, msg: &str) {
    if !enabled(level) {
        return;
    }
    if let Some(s) = SINK.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        s(level, msg);
    }
}

pub(crate) fn err(msg: &str) {
    log(Level::Err, msg);
}

pub(crate) fn warn(msg: &str) {
    log(Level::Warn, msg);
}

pub(crate) fn dbg(msg: &str) {
    log(Level::Dbg, msg);
}
