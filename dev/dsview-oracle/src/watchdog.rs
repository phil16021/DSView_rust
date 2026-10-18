//! Per-stack time limit for the decode runners: a decoder that loops
//! forever (in either engine) ends the process with a `timeout` line naming
//! the stack, so the differential harness can attribute the hang and resume
//! after it (`--first`).

use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Exit code after a stack timed out.
pub const TIMEOUT_EXIT: i32 = 3;

unsafe extern "C" {
    /// Ends the process without exit handlers: a looping decoder thread
    /// holds the GIL, and handlers that need it would wait forever.
    fn _exit(code: i32) -> !;
}

pub struct Watchdog {
    /// Stack index + 1 (0: idle).
    current: Arc<AtomicU64>,
    started: Arc<Mutex<Instant>>,
}

impl Watchdog {
    /// `out` is flushed before the timeout line is written.
    pub fn start(limit: Duration, out: Arc<Mutex<dyn Write + Send>>) -> Watchdog {
        let current = Arc::new(AtomicU64::new(0));
        let started = Arc::new(Mutex::new(Instant::now()));
        let (c, s) = (Arc::clone(&current), Arc::clone(&started));
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let idx = c.load(Ordering::SeqCst);
                if idx == 0 {
                    continue;
                }
                if s.lock().unwrap().elapsed() > limit {
                    let mut w = out.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = writeln!(w, "{{\"t\":\"timeout\",\"index\":{}}}", idx - 1);
                    let _ = w.flush();
                    // SAFETY: plain process exit after the output is flushed.
                    unsafe { _exit(TIMEOUT_EXIT) };
                }
            }
        });
        Watchdog { current, started }
    }

    pub fn begin(&self, index: usize) {
        *self.started.lock().unwrap() = Instant::now();
        self.current.store(index as u64 + 1, Ordering::SeqCst);
    }

    pub fn end(&self) {
        self.current.store(0, Ordering::SeqCst);
    }
}
