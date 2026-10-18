//! The GUI's side of the control API (`dsview-control::api::ViewHooks`):
//! calls arrive on the control server's threads and are handed to the GUI
//! thread, which applies them on its next frame.

use std::sync::{Arc, Mutex, OnceLock};

use dsview_control::api::ViewHooks;
use eframe::egui;

#[derive(Default)]
pub struct Pending {
    pub region: Option<(u64, u64)>,
    pub cursors: Option<Vec<u64>>,
    /// The open modal dialog's title, if any.
    pub dialog: Option<String>,
}

#[derive(Clone, Default)]
pub struct Shared {
    pub pending: Arc<Mutex<Pending>>,
    pub ctx: Arc<OnceLock<egui::Context>>,
}

impl Shared {
    pub fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wake(&self) {
        if let Some(c) = self.ctx.get() {
            c.request_repaint();
        }
    }
}

pub struct GuiHooks(pub Shared);

impl ViewHooks for GuiHooks {
    fn dialog_open(&self) -> Option<String> {
        self.0.lock().dialog.clone()
    }
    fn show_region(&self, start: u64, end: u64) {
        self.0.lock().region = Some((start, end));
        self.0.wake();
    }
    fn set_cursors(&self, samples: &[u64]) {
        self.0.lock().cursors = Some(samples.to_vec());
        self.0.wake();
    }
}
