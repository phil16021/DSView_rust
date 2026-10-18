//! The settings (`AppConfig`: `appOptions`, `frameOptions`, `userHistory`)
//! and the application options dialog (`pv/dialogs/applicationpardlg.cpp`),
//! kept in `$XDG_CONFIG_HOME/DreamSourceLab/dsview-rs.json`. Without that
//! file the Qt DSView's `DSView.conf` next to it is imported once.

use crate::i18n::tr;
use std::path::{Path, PathBuf};

use dsview_viewmodel::qsettings::{self, QSettings};
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
    /// Log: level 0-5, written to the log file, appended to it.
    pub log_level: i32,
    pub able_save_log: bool,
    pub append_log_mode: bool,
    /// Decoder option texts translated (`transDecoderDlg`).
    pub trans_decoder_dlg: bool,
    /// Logic trace height factor (Ctrl+Shift+wheel).
    pub trace_height_factor: f64,
    pub dso_split_channels: bool,
    pub frame: FrameOptions,
    pub history: UserHistory,
    /// The file the settings were loaded from and are saved to; None for
    /// settings not loaded (tests, benchmarks), which are never saved.
    #[serde(skip)]
    pub file: Option<PathBuf>,
}

/// The docks open in one work mode (`DockOptions`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DockOptions {
    pub decode: bool,
    pub trigger: bool,
    pub measure: bool,
    pub search: bool,
}

impl DockOptions {
    /// The flag of the dock a shortcut opens (T, D, M, R).
    pub fn flag(&mut self, dock: dsview_viewmodel::input::Shortcut) -> &mut bool {
        use dsview_viewmodel::input::Shortcut as K;
        match dock {
            K::Trigger => &mut self.trigger,
            K::Protocol => &mut self.decode,
            K::Measure => &mut self.measure,
            _ => &mut self.search,
        }
    }
}

/// `FrameOptions`: theme, language, the normal window rectangle and the
/// docks per work mode.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameOptions {
    /// dark, light, latte or frappe.
    pub style: String,
    /// 25 (cn), 31 (en), 7 (de); -1 for the locale's.
    pub language: i64,
    pub is_max: bool,
    /// x, y, width, height of the normal window, once known.
    pub window: Option<[i64; 4]>,
    pub logic_dock: DockOptions,
    pub analog_dock: DockOptions,
    pub dso_dock: DockOptions,
}

impl Default for FrameOptions {
    fn default() -> Self {
        FrameOptions {
            style: "dark".into(),
            language: -1,
            is_max: false,
            window: None,
            logic_dock: DockOptions::default(),
            analog_dock: DockOptions::default(),
            dso_dock: DockOptions::default(),
        }
    }
}

/// `UserHistory`: the directories last used per file dialog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UserHistory {
    pub open_dir: String,
    pub save_dir: String,
    pub export_dir: String,
    pub export_format: String,
    pub session_dir: String,
    /// The last screenshot's file (Qt keeps the file, not the directory).
    pub screenshot_path: String,
    pub protocol_export_path: String,
    pub show_documents: bool,
    pub show_driver_hint: bool,
}

impl Default for UserHistory {
    fn default() -> Self {
        UserHistory {
            open_dir: String::new(),
            save_dir: String::new(),
            export_dir: String::new(),
            export_format: String::new(),
            session_dir: String::new(),
            screenshot_path: String::new(),
            protocol_export_path: String::new(),
            show_documents: true,
            show_driver_hint: true,
        }
    }
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
            log_level: 3,
            able_save_log: false,
            append_log_mode: false,
            trans_decoder_dlg: true,
            trace_height_factor: 1.0,
            dso_split_channels: false,
            frame: FrameOptions::default(),
            history: UserHistory::default(),
            file: None,
        }
    }
}

impl AppOptions {
    fn config_dir() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("DreamSourceLab"))
    }

    fn path() -> Option<PathBuf> {
        Some(Self::config_dir()?.join("dsview-rs.json"))
    }

    /// The saved settings; on the first run the Qt DSView's `DSView.conf`
    /// imported (and saved); else the defaults.
    pub fn load() -> AppOptions {
        let Some(path) = Self::path() else {
            return AppOptions::default();
        };
        Self::load_from(&path, &path.with_file_name("DSView.conf"), &locale())
    }

    fn load_from(json: &Path, qt_conf: &Path, locale: &str) -> AppOptions {
        let mut o = if let Ok(t) = std::fs::read_to_string(json) {
            let mut o: AppOptions = serde_json::from_str(&t).unwrap_or_default();
            o.frame.language = qsettings::language(Some(o.frame.language), locale);
            o
        } else if let Ok(t) = std::fs::read_to_string(qt_conf) {
            let mut o = AppOptions::from_qt(&QSettings::parse(&t), locale);
            dsview_drivers::sys::log(3, &format!("Imported {}", qt_conf.display()));
            o.file = Some(json.to_owned());
            o.save();
            o
        } else {
            let mut o = AppOptions::default();
            o.frame.language = qsettings::language(None, locale);
            o
        };
        o.file = Some(json.to_owned());
        o
    }

    /// `AppConfig::Load` from the Qt settings: `_loadApp` (with its
    /// clamping), `_loadFrame`, `_loadHistory`. Keys not present keep the
    /// defaults; `windowState` (Qt's dock blob) is not used.
    pub fn from_qt(s: &QSettings, locale: &str) -> AppOptions {
        let d = AppOptions::default();
        let b = |k: &str, v: bool| s.bool(&format!("Application/{k}")).unwrap_or(v);
        let f = |k: &str, v: f64| s.float(&format!("Application/{k}")).unwrap_or(v);
        let i = |k: &str, v: i64| s.int(&format!("Application/{k}")).unwrap_or(v);
        let mut o = AppOptions {
            quick_scroll: b("quickScroll", d.quick_scroll),
            original_data: b("originalData", d.original_data),
            able_save_log: b("ableSaveLog", d.able_save_log),
            append_log_mode: b("appendLogMode", d.append_log_mode),
            log_level: i("logLevel", 3) as i32,
            trans_decoder_dlg: b("transDecoderDlg", d.trans_decoder_dlg),
            trig_pos_display_in_mid: b("trigPosDisplayInMid", d.trig_pos_display_in_mid),
            display_profile_in_bar: b("displayProfileInBar", d.display_profile_in_bar),
            swap_back_buffer_always: b("swapBackBufferAlways", d.swap_back_buffer_always),
            font_size: f("fontSize", 9.0) as f32,
            auto_scroll_latest_data: b("autoScrollLatestData", d.auto_scroll_latest_data),
            vertical_scroll_is_zoom: b("verticalScrollIsZoom", d.vertical_scroll_is_zoom),
            trace_height_factor: f("traceHeightFactor", 1.0),
            ruler_samples: s.get("Application/rulerTimeUnits") == Some("Samples"),
            antialias: b("antialias", d.antialias),
            decoder_dynamic_font_width: b("decoderDynamicFontWidth", false),
            max_decoder_font_width_percent: i("maxDecoderFontWidthPercent", 125) as i32,
            min_decoder_font_width_percent: i("minDecoderFontWidthPercent", 75) as i32,
            dont_ask_save_on_exit: b("dontAskSaveOnExit", false),
            logic_signal_line_width: f("logicSignalLineWidth", 1.0) as f32,
            logic_channel_divider: b("logicChannelDivider", d.logic_channel_divider),
            dso_split_channels: b("dsoSplitChannels", false),
            dso_signal_line_width: f("dsoSignalLineWidth", 1.0) as f32,
            // `_loadApp`: outside 0-10000 ms it is 0
            dso_persistence_ms: u32::try_from(i("dsoPersistenceMs", 0))
                .ok()
                .filter(|v| *v <= 10000)
                .unwrap_or(0),
            protocol_formats: s
                .get("Application/protocalFormats")
                .map(|v| qsettings::protocol_formats(v).into_iter().collect())
                .unwrap_or_default(),
            ..d
        };
        // `_loadApp`'s corrections (Linux font range 8-16)
        if i("version", 1) == 1 || !(8.0..=16.0).contains(&o.font_size) {
            o.font_size = 12.0;
        }
        if !(0.1..=20.0).contains(&o.trace_height_factor) {
            o.trace_height_factor = 1.0;
        }
        if !(1.0..=4.0).contains(&o.logic_signal_line_width) {
            o.logic_signal_line_width = 1.0;
        }
        if !(1.0..=4.0).contains(&o.dso_signal_line_width) {
            o.dso_signal_line_width = 1.0;
        }
        // `_loadFrame`
        let fi = |k: &str| s.int(&format!("MainFrame/{k}"));
        let dock = |g: &str| {
            let fb = |k: &str| s.bool(&format!("MainFrame/{g}/{k}")).unwrap_or(false);
            DockOptions {
                decode: fb("decodeDoc"),
                trigger: fb("triggerDoc"),
                measure: fb("measureDoc"),
                search: fb("searchDoc"),
            }
        };
        o.frame = FrameOptions {
            style: s.get("MainFrame/style").unwrap_or("dark").to_owned(),
            language: qsettings::language(fi("language"), locale),
            is_max: s.bool("MainFrame/isMax").unwrap_or(false),
            window: qsettings::window_rect(
                fi("left").unwrap_or(0),
                fi("top").unwrap_or(0),
                fi("right").unwrap_or(0),
                fi("bottom").unwrap_or(0),
                fi("x").unwrap_or(qsettings::NO_POINT_VALUE),
                fi("y").unwrap_or(qsettings::NO_POINT_VALUE),
            )
            .map(|(x, y, w, h)| [x, y, w, h]),
            logic_dock: dock("LOGIC_DOCK"),
            analog_dock: dock("ANALOG_DOCK"),
            dso_dock: dock("DSO_DOCK"),
        };
        // `_loadHistory`
        let h = |k: &str| s.get(&format!("UserHistory/{k}")).unwrap_or("").to_owned();
        let hb = |k: &str| s.bool(&format!("UserHistory/{k}")).unwrap_or(true);
        o.history = UserHistory {
            open_dir: h("openDir"),
            save_dir: h("saveDir"),
            export_dir: h("exportDir"),
            export_format: h("exportFormat"),
            session_dir: h("sessionDir"),
            screenshot_path: h("screenShotPath"),
            protocol_export_path: h("protocolExportPath"),
            show_documents: hb("showDocuments"),
            show_driver_hint: hb("showDriverHint"),
        };
        o
    }

    /// `GetTraceFontSize`: the font size inside Linux's range.
    pub fn font_size(&self) -> f32 {
        self.font_size.clamp(8.0, 16.0)
    }

    /// Puts the options into the view and the window's text styles (the
    /// font size scales every text, as Qt sets it on bars, docks and
    /// traces).
    pub fn apply(&self, view: &mut crate::timeview::TimeView, ctx: &egui::Context) {
        // `switchTheme`: the widgets, the view colours, the icon set
        let theme = crate::theme::Theme::from_style(&self.frame.style);
        crate::theme::apply(ctx, theme);
        crate::icons::set_dark(theme.is_dark());
        view.palette = theme.palette();
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
        view.height_factor = self.trace_height_factor;
        view.dso.split = self.dso_split_channels;
        view.quick_scroll = self.quick_scroll;
        view.dividers = self.logic_channel_divider;
        view.vertical_scroll_is_zoom = self.vertical_scroll_is_zoom;
        view.show_samples = self.ruler_samples;
        view.dso.persist.ms = self.dso_persistence_ms;
        // antialiasing off: no feathering (the default here, PLAN.md §5.1)
        let aa = self.antialias;
        ctx.tessellation_options_mut(|o| o.feathering = aa);
    }

    /// Writes the settings back to the file they came from (nothing for
    /// settings that were not loaded).
    pub fn save(&self) {
        let Some(p) = &self.file else {
            return;
        };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, t);
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
        ui.strong(tr("Logic"));
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
                    ui.radio_value(&mut self.vertical_scroll_is_zoom, true, tr("Zoom"));
                    ui.radio_value(&mut self.vertical_scroll_is_zoom, false, tr("Scroll"));
                });
            });
            row(ui, "Ruler / Cursor units", &mut |ui, _| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.ruler_samples, false, tr("Time"));
                    ui.radio_value(&mut self.ruler_samples, true, tr("Samples"));
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
        ui.strong(tr("Scope"));
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
        ui.strong(tr("UI"));
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
            if ui.button(tr("OK")).clicked() {
                done = Some(true);
            }
            if ui.button(tr("Cancel")).clicked() || crate::msgbox::esc(ui.ctx()) {
                done = Some(false);
            }
        });
        done
    }
}

/// The locale's name (`LC_ALL`, `LC_MESSAGES`, `LANG`), as Qt's `QLocale`
/// takes it on Linux.
fn locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_default()
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

    #[test]
    fn imports_the_qt_conf_once() {
        let dir = std::env::temp_dir().join(format!("dsv-opts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (json, conf) = (dir.join("dsview-rs.json"), dir.join("DSView.conf"));
        std::fs::write(
            &conf,
            "[Application]\nfontSize=11\nversion=3\nlogLevel=5\nableSaveLog=true\n\
             traceHeightFactor=40\nlogicSignalLineWidth=2.5\ndsoPersistenceMs=20000\n\
             rulerTimeUnits=Samples\nprotocalFormats=\"0:uart=hex\"\n\
             [MainFrame]\nDSO_DOCK\\triggerDoc=true\nLOGIC_DOCK\\decodeDoc=true\n\
             left=10\ntop=20\nright=1210\nbottom=820\nx=-10000\nisMax=true\nstyle=latte\n\
             [UserHistory]\nopenDir=/data\nshowDriverHint=false\n",
        )
        .unwrap();
        let o = AppOptions::load_from(&json, &conf, "de_DE.UTF-8");
        assert_eq!(o.font_size, 11.0);
        assert_eq!(o.log_level, 5);
        assert!(o.able_save_log);
        // out of range: the defaults, as `_loadApp`
        assert_eq!(o.trace_height_factor, 1.0);
        assert_eq!(o.dso_persistence_ms, 0);
        assert_eq!(o.logic_signal_line_width, 2.5);
        assert!(o.ruler_samples);
        assert_eq!(
            o.protocol_formats.get("0:uart").map(String::as_str),
            Some("hex")
        );
        assert!(o.frame.dso_dock.trigger && o.frame.logic_dock.decode);
        assert!(!o.frame.logic_dock.search);
        assert_eq!(o.frame.window, Some([10, 20, 1200, 800]));
        assert!(o.frame.is_max);
        assert_eq!(o.frame.style, "latte");
        // no language stored: the locale's
        assert_eq!(o.frame.language, qsettings::LAN_DE);
        assert_eq!(o.history.open_dir, "/data");
        assert!(!o.history.show_driver_hint && o.history.show_documents);
        // saved as JSON; the next start reads that, not the Qt file
        assert!(json.is_file());
        std::fs::write(&conf, "[Application]\nfontSize=15\nversion=3\n").unwrap();
        let again = AppOptions::load_from(&json, &conf, "de_DE.UTF-8");
        assert_eq!(again, o);
        // version 1 files get the middle font size
        let v1 = AppOptions::from_qt(&QSettings::parse("[Application]\nfontSize=10\n"), "C");
        assert_eq!(v1.font_size, 12.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
