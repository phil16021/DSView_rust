//! Manual calibration of a DSO front end (`pv/dialogs/calibration.cpp`):
//! per channel the gain (VGAIN, around its default), the offset (VOFF) and,
//! where the device has it, the comb compensation (VCOMB), written to the
//! device as the sliders move; Save, Abort (reload the stored values),
//! Reset (the defaults) and Exit.

use std::sync::mpsc::{Receiver, channel};

use dsview_core::{ConfValue, channel_type, key};
use dsview_session::Session;
use eframe::egui::{self, Ui};

/// One channel's sliders: (value, min, max) each.
#[derive(Clone, Debug, PartialEq)]
struct Row {
    index: u16,
    pos: usize,
    gain: (i64, i64, i64),
    off: (i64, i64, i64),
    comp: Option<(i64, i64, i64)>,
    vgain_default: u64,
}

#[derive(Default)]
pub struct Calibration {
    rows: Vec<Row>,
    /// Save or Abort running in a thread: its title and end.
    busy: Option<(&'static str, Receiver<()>)>,
    confirm_reset: bool,
    pub open: bool,
}

impl Calibration {
    /// `show_calibration`: opens with the device's values.
    pub fn open(s: &Session) -> Calibration {
        let mut c = Calibration {
            open: true,
            ..Calibration::default()
        };
        c.update_device_info(s);
        c
    }

    /// `update_device_info`
    fn update_device_info(&mut self, s: &Session) {
        let comb = s
            .config(key::PROBE_COMB_COMP_EN, None)
            .is_some_and(|v| v.get_bool());
        self.rows = s
            .channels()
            .iter()
            .enumerate()
            .filter(|(_, c)| c.ty == channel_type::DSO)
            .map(|(pos, c)| {
                let get = |k: i32| s.config(k, Some(pos));
                let vgain = get(key::PROBE_VGAIN).map_or(0, |v| v.get_u64());
                let vgain_default = get(key::PROBE_VGAIN_DEFAULT).map_or(0, |v| v.get_u64());
                let range = get(key::PROBE_VGAIN_RANGE).map_or(0, |v| v.get_u16()) as i64;
                let off = get(key::PROBE_PREOFF).map_or(0, |v| v.get_u16()) as i64;
                let margin = get(key::PROBE_PREOFF_MARGIN).map_or(0, |v| v.get_u16()) as i64;
                // QSlider clamps the value into its range
                let clamp = |v: i64, lo: i64, hi: i64| v.clamp(lo, hi.max(lo));
                let g = vgain as i64 - vgain_default as i64;
                Row {
                    index: c.index,
                    pos,
                    gain: (clamp(g, -range / 2, range / 2), -range / 2, range / 2),
                    off: (clamp(off, 0, margin), 0, margin),
                    comp: comb.then(|| {
                        let v = get(key::PROBE_COMB_COMP).map_or(0, |v| v.get_i16()) as i64;
                        (clamp(v, -127, 127), -127, 127)
                    }),
                    vgain_default,
                }
            })
            .collect();
    }

    pub fn show(&mut self, ctx: &egui::Context, s: &Session) {
        if !self.open {
            return;
        }
        if let Some((title, rx)) = &self.busy {
            if rx.try_recv().is_err() {
                egui::Window::new(*title)
                    .collapsible(false)
                    .show(ctx, |ui| {
                        ui.spinner();
                    });
                ctx.request_repaint();
                return;
            }
            self.busy = None;
            self.update_device_info(s);
        }
        let mut exit = false;
        egui::Window::new("Manual Calibration")
            .collapsible(false)
            .default_width(620.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| {
                        for r in self.rows.iter_mut() {
                            let name = |what: &str| format!("Channel{} {what}", r.index);
                            if slider(ui, &name("VGAIN"), &mut r.gain) {
                                s.set_config(
                                    key::PROBE_VGAIN,
                                    ConfValue::U64((r.gain.0 + r.vgain_default as i64) as u64),
                                    Some(r.pos),
                                );
                            }
                            if slider(ui, &name("VOFF"), &mut r.off) {
                                s.set_config(
                                    key::PROBE_PREOFF,
                                    ConfValue::U16(r.off.0 as u16),
                                    Some(r.pos),
                                );
                            }
                            if let Some(c) = &mut r.comp
                                && slider(ui, &name("VCOMB"), c)
                            {
                                s.set_config(
                                    key::PROBE_COMB_COMP,
                                    ConfValue::I16(c.0 as i16),
                                    Some(r.pos),
                                );
                            }
                            ui.add_space(10.0);
                        }
                    });
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        self.run(
                            s,
                            "Save calibration results... It can take a while.",
                            key::ZERO_SET,
                        );
                    }
                    if ui.button("Abort").clicked() {
                        self.run(
                            s,
                            "Reload last calibration results... It can take a while.",
                            key::ZERO_LOAD,
                        );
                    }
                    if ui.button("Reset").clicked() {
                        self.confirm_reset = true;
                    }
                    if ui.button("Exit").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    {
                        exit = true;
                    }
                });
            });
        if self.confirm_reset {
            egui::Window::new("Reset calibration")
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("All calibration settings will become the defualt values!");
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            s.set_config(key::ZERO_DEFAULT, ConfValue::Bool(true), None);
                            self.update_device_info(s);
                            self.confirm_reset = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_reset = false;
                        }
                    });
                });
        }
        if exit {
            // reject(): calibration mode off
            s.set_config(key::CALI, ConfValue::Bool(false), None);
            self.open = false;
        }
    }

    /// `on_save` / `on_abort`: the request in a thread, a progress window
    /// until it returns, then the values again.
    fn run(&mut self, s: &Session, title: &'static str, k: i32) {
        let (tx, rx) = channel();
        let s2 = s.clone();
        std::thread::spawn(move || {
            s2.set_config(k, ConfValue::Bool(true), None);
            let _ = tx.send(());
        });
        self.busy = Some((title, rx));
    }
}

/// A labelled slider with its value; true when moved.
fn slider(ui: &mut Ui, label: &str, v: &mut (i64, i64, i64)) -> bool {
    ui.horizontal(|ui| {
        let l = ui.label(label);
        let (lo, hi) = (v.1, v.2.max(v.1));
        ui.spacing_mut().slider_width = 450.0;
        ui.add(egui::Slider::new(&mut v.0, lo..=hi))
            .labelled_by(l.id)
            .changed()
    })
    .inner
}
