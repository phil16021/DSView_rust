//! DSO auto calibration (`SamplingBar::zero_adj` with
//! `pv/dialogs/waitingdialog.cpp`): every channel on, 2 µs/div, a running
//! capture while the driver calibrates (it clears `ZERO` when done), then
//! Save (`ZERO_SET`) or Abort (`ZERO_LOAD`), and the timebase back.

use crate::i18n::tr;
use std::sync::mpsc::{Receiver, channel};

use dsview_core::{ConfValue, channel_type, key};
use dsview_session::Session;
use eframe::egui;

/// `SamplingBar::ZeroTimeBase`: 2 µs per division.
pub const ZERO_TIMEBASE_NS: f64 = 2000.0;

pub struct ZeroCal {
    /// The duration selected before.
    back: usize,
    started: std::time::Instant,
    /// Save or Abort running: the window it shows.
    finishing: Option<(&'static str, Receiver<()>)>,
}

impl ZeroCal {
    /// `zero_adj()` up to the waiting dialog.
    pub fn start(s: &Session) -> ZeroCal {
        let chans = s.channels();
        for (pos, c) in chans.iter().enumerate() {
            if c.ty == channel_type::DSO && !c.enabled {
                s.set_config(key::PROBE_EN, ConfValue::Bool(true), Some(pos));
            }
        }
        s.reload_signals();
        let (list, back) = s.duration_selector();
        let i = list
            .iter()
            .position(|d| d.0 == ZERO_TIMEBASE_NS)
            .unwrap_or(list.len());
        s.select_duration(i);
        if !s.is_working() {
            s.start_capture(false);
        }
        ZeroCal {
            back,
            started: std::time::Instant::now(),
            finishing: None,
        }
    }

    /// The waiting dialog; false once it is over.
    pub fn show(&mut self, ctx: &egui::Context, s: &Session) -> bool {
        if let Some((title, rx)) = &self.finishing {
            if rx.try_recv().is_err() {
                egui::Window::new(*title)
                    .collapsible(false)
                    .show(ctx, |ui| {
                        ui.spinner();
                    });
                ctx.request_repaint();
                return true;
            }
            // after the dialog: stop, timebase back
            if s.is_working() {
                s.stop_capture();
            }
            s.select_duration(self.back);
            return false;
        }
        let done = !s.config(key::ZERO, None).is_some_and(|v| v.get_bool());
        let mut choice = None;
        egui::Window::new(tr("Auto Calibration"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.strong(tr("Don't connect any probes!"));
                if done {
                    ui.label(tr("Finished!"));
                } else {
                    ui.spinner();
                    // "Waiting", then a dot every 300 ms
                    let dots = (self.started.elapsed().as_millis() / 300 % 4) as usize;
                    ui.label(format!("Waiting{}", ".".repeat(dots)));
                    ctx.request_repaint_after(std::time::Duration::from_millis(300));
                }
                ui.horizontal(|ui| {
                    if done && ui.button(tr("Save")).clicked() {
                        choice = Some(true);
                    }
                    if ui.button(tr("Abort")).clicked() {
                        choice = Some(false);
                    }
                });
            });
        if let Some(save) = choice {
            let (tx, rx) = channel();
            let s2 = s.clone();
            std::thread::spawn(move || {
                if save {
                    s2.set_config(key::ZERO_SET, ConfValue::Bool(true), None);
                } else {
                    // reject(): calibration off, the stored values again
                    s2.set_config(key::ZERO, ConfValue::Bool(false), None);
                    s2.set_config(key::ZERO_LOAD, ConfValue::Bool(true), None);
                }
                let _ = tx.send(());
            });
            self.finishing = Some((
                if save {
                    "Save calibration results... It can take a while."
                } else {
                    "Load current setting... It can take a while."
                },
                rx,
            ));
        }
        true
    }
}
