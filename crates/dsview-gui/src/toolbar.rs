//! The sampling bar (`pv/toolbars/samplingbar.cpp`): device, mode,
//! channels, duration and rate, collect mode, start/stop and single shot.

use dsview_core::{Mode, channel_type};
use dsview_formats::cfmt::{samplerate_string, time_string};
use dsview_session::Session;
use dsview_session::session::CollectMode;
use eframe::egui::{self, Ui};

pub struct SamplingBar {
    pub collect: CollectMode,
    pub repeat_interval: f64,
    channels_open: bool,
    /// The device options dialog while open.
    pub options: Option<crate::deviceoptions::DeviceOptions>,
    /// "Manual Calibration" was chosen: the app opens the calibration.
    pub calibrate: bool,
    /// "Auto Calibration": waiting for the user's confirmation.
    pub confirm_zero: bool,
    pub message: Option<String>,
}

impl Default for SamplingBar {
    fn default() -> Self {
        SamplingBar {
            collect: CollectMode::Single,
            repeat_interval: 1.0,
            channels_open: false,
            options: None,
            calibrate: false,
            confirm_zero: false,
            message: None,
        }
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Logic => "Logic Analyzer",
        Mode::Dso => "Oscilloscope",
        Mode::Analog => "Data Acquisition",
    }
}

impl SamplingBar {
    /// `action_run_stop` / `action_instant_stop`.
    pub fn run_stop(&mut self, s: &Session, instant: bool) {
        if s.is_working() {
            s.stop_capture();
            return;
        }
        s.set_collect_mode(
            if instant {
                CollectMode::Single
            } else {
                self.collect
            },
            self.repeat_interval,
        );
        s.start_capture(instant);
    }

    pub fn show(&mut self, ui: &mut Ui, s: &Session) {
        self.options_dialog(ui.ctx(), s);
        let working = s.is_working();
        ui.horizontal(|ui| {
            // device
            let (list, active) = s.devices();
            let current = list
                .get(active.max(0) as usize)
                .map_or("no device".to_owned(), |e| e.name.clone());
            ui.add_enabled_ui(!working, |ui| {
                egui::ComboBox::from_id_salt("device")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (i, e) in list.iter().enumerate() {
                            if ui.selectable_label(i as i32 == active, &e.name).clicked()
                                && i as i32 != active
                            {
                                s.set_device(e.handle);
                            }
                        }
                    });
                // work mode (devices with more than one)
                let modes = s.modes();
                if modes.len() > 1 {
                    let mode = s.mode();
                    egui::ComboBox::from_id_salt("mode")
                        .selected_text(
                            modes
                                .iter()
                                .find(|m| **m as i32 == mode)
                                .map_or("", |m| mode_name(*m)),
                        )
                        .show_ui(ui, |ui| {
                            for m in &modes {
                                if ui
                                    .selectable_label(*m as i32 == mode, mode_name(*m))
                                    .clicked()
                                {
                                    s.switch_work_mode(*m as i32);
                                }
                            }
                        });
                }
                if ui
                    .button("Options")
                    .on_hover_text("Device options")
                    .clicked()
                    && self.options.is_none()
                {
                    self.options = Some(crate::deviceoptions::DeviceOptions::new(s));
                }
                if ui.button("Channels").clicked() {
                    self.channels_open = !self.channels_open;
                }
                // duration (DSO: per division) @ rate
                let (durations, d) = s.duration_selector();
                let dso = s.mode() == Mode::Dso as i32;
                let label = |(ns, rle): (f64, bool)| {
                    let t = time_string(ns as u64);
                    if dso {
                        format!("{t} / div")
                    } else if rle {
                        format!("{t}(RLE)")
                    } else {
                        t
                    }
                };
                egui::ComboBox::from_id_salt("duration")
                    .selected_text(durations.get(d).copied().map(label).unwrap_or_default())
                    .show_ui(ui, |ui| {
                        for (i, x) in durations.iter().enumerate() {
                            if ui.selectable_label(i == d, label(*x)).clicked() {
                                s.select_duration(i);
                            }
                        }
                    });
                ui.label("@");
                let (rates, r) = s.rate_selector();
                egui::ComboBox::from_id_salt("rate")
                    .selected_text(
                        rates
                            .get(r)
                            .map(|x| samplerate_string(*x))
                            .unwrap_or_default(),
                    )
                    .show_ui(ui, |ui| {
                        for (i, x) in rates.iter().enumerate() {
                            if ui.selectable_label(i == r, samplerate_string(*x)).clicked() {
                                s.select_rate(i);
                            }
                        }
                    });
                // collect mode (logic only)
                if s.mode() == Mode::Logic as i32 {
                    egui::ComboBox::from_id_salt("collect")
                        .selected_text(match self.collect {
                            CollectMode::Single => "Single",
                            CollectMode::Repeat => "Repeat",
                            CollectMode::Loop => "Loop",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.collect, CollectMode::Single, "Single");
                            ui.selectable_value(&mut self.collect, CollectMode::Repeat, "Repeat");
                            ui.selectable_value(&mut self.collect, CollectMode::Loop, "Loop");
                        });
                    if self.collect == CollectMode::Repeat {
                        ui.add(
                            egui::DragValue::new(&mut self.repeat_interval)
                                .range(0.1..=10.0)
                                .speed(0.1)
                                .suffix(" s"),
                        );
                    }
                }
            });
            let run = if working { "Stop" } else { "Start" };
            if ui.button(run).clicked() {
                self.run_stop(s, false);
            }
            if s.mode() != Mode::Analog as i32 && !working && ui.button("Single").clicked() {
                self.run_stop(s, true);
            }
            if working {
                let st = s.capture_status();
                match st {
                    Some((true, p)) => ui.label(format!("Triggered {p}%")),
                    Some((false, p)) if p > 0 => ui.label(format!("{p}%")),
                    _ => ui.label(if s.state().triged {
                        "Capturing"
                    } else {
                        "Waiting for trigger"
                    }),
                };
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(200));
            }
        });
        if self.channels_open {
            let mut open = true;
            egui::Window::new("Channels")
                .open(&mut open)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    let chans = s.channels();
                    let ty = match s.mode() {
                        1 => channel_type::DSO,
                        2 => channel_type::ANALOG,
                        _ => channel_type::LOGIC,
                    };
                    egui::Grid::new("chans").num_columns(8).show(ui, |ui| {
                        for (k, c) in chans.iter().filter(|c| c.ty == ty).enumerate() {
                            let mut en = c.enabled;
                            if ui
                                .add_enabled(
                                    !working,
                                    egui::Checkbox::new(&mut en, c.index.to_string()),
                                )
                                .changed()
                            {
                                if ty == channel_type::DSO {
                                    let pos =
                                        chans.iter().position(|x| x.index == c.index && x.ty == ty);
                                    s.set_config(
                                        dsview_core::key::PROBE_EN,
                                        dsview_core::ConfValue::Bool(en),
                                        pos,
                                    );
                                } else {
                                    s.enable_channel(c.index, en);
                                }
                                s.reload_signals();
                                s.device_updated();
                            }
                            if k % 8 == 7 {
                                ui.end_row();
                            }
                        }
                    });
                });
            self.channels_open = open;
        }
    }
}

impl SamplingBar {
    /// The device options dialog and what follows OK
    /// (`SamplingBar::on_configure`).
    fn options_dialog(&mut self, ctx: &egui::Context, s: &Session) {
        use crate::deviceoptions::Outcome;
        if let Some(d) = &mut self.options {
            let out = d.show(ctx, s);
            if let Some(m) = d.message.take() {
                self.message = Some(m);
            }
            match out {
                Outcome::Open => {}
                Outcome::Cancelled => self.options = None,
                Outcome::Accepted | Outcome::AutoCalibration | Outcome::ManualCalibration => {
                    self.options = None;
                    if !s.have_view_data() {
                        s.commit_settings();
                    }
                    // DSV_MSG_DEVICE_OPTIONS_UPDATED: the session reloads
                    s.reload_signals();
                    s.device_updated();
                    match out {
                        Outcome::AutoCalibration => self.confirm_zero = true,
                        Outcome::ManualCalibration => {
                            s.set_config(
                                dsview_core::key::CALI,
                                dsview_core::ConfValue::Bool(true),
                                None,
                            );
                            self.calibrate = true;
                        }
                        _ => {}
                    }
                }
            }
        }
        if self.confirm_zero {
            egui::Window::new("Auto Calibration").collapsible(false).show(ctx, |ui| {
                ui.label("Auto Calibration program will be started. Don't connect any probes. \nIt can take a while!");
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        s.set_config(dsview_core::key::ZERO, dsview_core::ConfValue::Bool(true), None);
                        self.confirm_zero = false;
                    }
                    if ui.button("Cancel").clicked() {
                        s.set_config(dsview_core::key::ZERO, dsview_core::ConfValue::Bool(false), None);
                        self.confirm_zero = false;
                    }
                });
            });
        }
    }
}
