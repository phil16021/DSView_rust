//! The application options (`AppConfig::appOptions`) and their dialog
//! (`pv/dialogs/applicationpardlg.cpp`), kept in
//! `$XDG_CONFIG_HOME/DreamSourceLab/dsview-rs.json`.

use std::path::PathBuf;

use eframe::egui::{self, Ui};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppOptions {
    pub quick_scroll: bool,
    pub trig_pos_display_in_mid: bool,
    pub display_profile_in_bar: bool,
    pub swap_back_buffer_always: bool,
    pub auto_scroll_latest_data: bool,
    pub logic_channel_divider: bool,
    pub vertical_scroll_is_zoom: bool,
    pub antialias: bool,
    pub dont_ask_save_on_exit: bool,
    pub font_size: f32,
    /// Ruler and cursor units: samples instead of time.
    pub ruler_samples: bool,
    pub decoder_dynamic_font_width: bool,
    pub min_decoder_font_width_percent: i32,
    pub max_decoder_font_width_percent: i32,
    pub logic_signal_line_width: f32,
    pub dso_signal_line_width: f32,
    pub dso_persistence_ms: u32,
    /// Export: every sample (else changes only).
    pub original_data: bool,
    /// The protocol list's number format by protocol id.
    pub protocol_formats: std::collections::BTreeMap<String, String>,
}

impl Default for AppOptions {
    /// `AppConfig::FromAppJson`'s defaults.
    fn default() -> Self {
        AppOptions {
            quick_scroll: true,
            trig_pos_display_in_mid: true,
            display_profile_in_bar: false,
            swap_back_buffer_always: false,
            auto_scroll_latest_data: true,
            logic_channel_divider: true,
            vertical_scroll_is_zoom: true,
            antialias: true,
            dont_ask_save_on_exit: false,
            font_size: 9.0,
            ruler_samples: false,
            decoder_dynamic_font_width: false,
            min_decoder_font_width_percent: 75,
            max_decoder_font_width_percent: 125,
            logic_signal_line_width: 1.0,
            dso_signal_line_width: 1.0,
            dso_persistence_ms: 0,
            original_data: false,
            protocol_formats: Default::default(),
        }
    }
}

impl AppOptions {
    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("DreamSourceLab").join("dsview-rs.json"))
    }

    /// The saved options, else the defaults.
    pub fn load() -> AppOptions {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// `GetTraceFontSize`: the font size inside Linux's range.
    pub fn font_size(&self) -> f32 {
        self.font_size.clamp(8.0, 16.0)
    }

    /// Puts the options into the view and the window's text styles (the
    /// font size scales every text, as Qt sets it on bars, docks and
    /// traces).
    pub fn apply(&self, view: &mut crate::timeview::TimeView, ctx: &egui::Context) {
        let scale = self.font_size() / 9.0;
        view.font_scale = scale;
        let base = egui::Style::default().text_styles;
        ctx.all_styles_mut(|st| {
            for (k, f) in st.text_styles.iter_mut() {
                if let Some(b) = base.get(k) {
                    f.size = b.size * scale;
                }
            }
        });
        view.logic_line_width = self.logic_signal_line_width;
        view.dso_line_width = self.dso_signal_line_width;
        view.decoder_font_width = self.decoder_dynamic_font_width.then_some((
            self.min_decoder_font_width_percent,
            self.max_decoder_font_width_percent,
        ));
        view.trig_in_middle = self.trig_pos_display_in_mid;
        view.quick_scroll = self.quick_scroll;
        view.dividers = self.logic_channel_divider;
        view.vertical_scroll_is_zoom = self.vertical_scroll_is_zoom;
        view.show_samples = self.ruler_samples;
        view.dso.persist.ms = self.dso_persistence_ms;
        // antialiasing off: no feathering (the default here, PLAN.md §5.1)
        let aa = self.antialias;
        ctx.tessellation_options_mut(|o| o.feathering = aa);
    }

    pub fn save(&self) {
        if let Some(p) = Self::path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            if let Ok(t) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, t);
            }
        }
    }

    /// `ApplicationParamDlg`'s groups; true when OK was pressed.
    pub fn dialog(&mut self, ui: &mut Ui) -> Option<bool> {
        let mut done = None;
        let row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui, egui::Id)| {
            let l = ui.label(label);
            add(ui, l.id);
            ui.end_row();
        };
        ui.strong("Logic");
        egui::Grid::new("opt-logic").num_columns(2).show(ui, |ui| {
            row(ui, "Quick scroll", &mut |ui, l| {
                ui.checkbox(&mut self.quick_scroll, "").labelled_by(l);
            });
            row(ui, "Used abort data", &mut |ui, l| {
                ui.checkbox(&mut self.swap_back_buffer_always, "")
                    .labelled_by(l);
            });
            row(ui, "Auto scoll latest", &mut |ui, l| {
                ui.checkbox(&mut self.auto_scroll_latest_data, "")
                    .labelled_by(l);
            });
            row(ui, "Vertical Scroll Action", &mut |ui, _| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.vertical_scroll_is_zoom, true, "Zoom");
                    ui.radio_value(&mut self.vertical_scroll_is_zoom, false, "Scroll");
                });
            });
            row(ui, "Ruler / Cursor units", &mut |ui, _| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.ruler_samples, false, "Time");
                    ui.radio_value(&mut self.ruler_samples, true, "Samples");
                });
            });
            row(ui, "Signal line width", &mut |ui, l| {
                ui.add(
                    egui::DragValue::new(&mut self.logic_signal_line_width)
                        .range(1.0..=4.0)
                        .speed(0.5)
                        .fixed_decimals(1),
                )
                .labelled_by(l);
            });
            row(ui, "Channel divider line", &mut |ui, l| {
                ui.checkbox(&mut self.logic_channel_divider, "")
                    .labelled_by(l);
            });
            row(ui, "Decoder adaptive font width", &mut |ui, l| {
                ui.checkbox(&mut self.decoder_dynamic_font_width, "")
                    .labelled_by(l);
            });
            row(ui, "Min font width:", &mut |ui, l| {
                ui.add(
                    egui::Slider::new(&mut self.min_decoder_font_width_percent, 25..=250)
                        .suffix("%"),
                )
                .labelled_by(l);
            });
            row(ui, "Max font width:", &mut |ui, l| {
                ui.add(
                    egui::Slider::new(&mut self.max_decoder_font_width_percent, 25..=250)
                        .suffix("%"),
                )
                .labelled_by(l);
            });
        });
        ui.separator();
        ui.strong("Scope");
        egui::Grid::new("opt-dso").num_columns(2).show(ui, |ui| {
            row(ui, "Tig pos in middle", &mut |ui, l| {
                ui.checkbox(&mut self.trig_pos_display_in_mid, "")
                    .labelled_by(l);
            });
            row(ui, "Signal line width", &mut |ui, l| {
                ui.add(
                    egui::DragValue::new(&mut self.dso_signal_line_width)
                        .range(1.0..=4.0)
                        .speed(0.5)
                        .fixed_decimals(1),
                )
                .labelled_by(l);
            });
            row(ui, "Persistence (fade-out time)", &mut |ui, l| {
                ui.add(
                    egui::DragValue::new(&mut self.dso_persistence_ms)
                        .range(0..=10000)
                        .speed(100)
                        .suffix(" ms"),
                )
                .labelled_by(l);
            });
        });
        ui.separator();
        ui.strong("UI");
        egui::Grid::new("opt-ui").num_columns(2).show(ui, |ui| {
            row(ui, "Profile in bar", &mut |ui, l| {
                ui.checkbox(&mut self.display_profile_in_bar, "")
                    .labelled_by(l);
            });
            row(ui, "Font size", &mut |ui, l| {
                ui.add(
                    egui::DragValue::new(&mut self.font_size)
                        .range(8.0..=16.0)
                        .speed(1.0)
                        .fixed_decimals(0),
                )
                .labelled_by(l);
            });
            row(ui, "Antialiasing", &mut |ui, l| {
                ui.checkbox(&mut self.antialias, "").labelled_by(l);
            });
            row(ui, "Do not ask to save captured data", &mut |ui, l| {
                ui.checkbox(&mut self.dont_ask_save_on_exit, "")
                    .labelled_by(l);
            });
        });
        ui.horizontal(|ui| {
            if ui.button("OK").clicked() {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() {
                done = Some(false);
            }
        });
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_missing_keys() {
        let o = AppOptions {
            dso_persistence_ms: 300,
            ruler_samples: true,
            ..AppOptions::default()
        };
        let t = serde_json::to_string(&o).unwrap();
        assert_eq!(serde_json::from_str::<AppOptions>(&t).unwrap(), o);
        // an older file without new keys gets the defaults for them
        let old: AppOptions = serde_json::from_str(r#"{"quick_scroll": false}"#).unwrap();
        assert!(!old.quick_scroll);
        assert_eq!(old.max_decoder_font_width_percent, 125);
    }

    #[test]
    fn apply_reaches_the_view_and_the_fonts() {
        let ctx = egui::Context::default();
        let mut view = crate::timeview::TimeView::default();
        let o = AppOptions {
            font_size: 12.0,
            logic_signal_line_width: 2.5,
            dso_signal_line_width: 1.5,
            decoder_dynamic_font_width: true,
            trig_pos_display_in_mid: false,
            ..AppOptions::default()
        };
        o.apply(&mut view, &ctx);
        assert!((view.font_scale - 12.0 / 9.0).abs() < 1e-6);
        assert_eq!(view.logic_line_width, 2.5);
        assert_eq!(view.dso_line_width, 1.5);
        assert_eq!(view.decoder_font_width, Some((75, 125)));
        assert!(!view.trig_in_middle);
        let body = ctx.global_style().text_styles[&egui::TextStyle::Body].size;
        let base = egui::Style::default().text_styles[&egui::TextStyle::Body].size;
        assert!((body - base * 12.0 / 9.0).abs() < 1e-4);
        // the size is kept inside Linux's range
        let big = AppOptions {
            font_size: 40.0,
            ..AppOptions::default()
        };
        assert_eq!(big.font_size(), 16.0);
    }
}
