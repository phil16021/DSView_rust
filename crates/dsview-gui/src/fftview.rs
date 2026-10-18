//! The FFT pane below the oscilloscope view (`SpectrumTrace` in its own
//! viewport) and the FFT options dialog (`pv/dialogs/fftoptions.cpp`).
//! The spectrum is recomputed when a new DSO frame arrives or a setting
//! changes (`SigSession` runs `calc_fft` per frame).

use crate::i18n::tr;
use dsview_core::{channel_type, key};
use dsview_dsp::spectrum::{Window, power_spectrum, volts_per_code};
use dsview_session::Session;
use dsview_viewmodel::dso;
use dsview_viewmodel::spectrum::{
    self, DBV_RANGES, RIGHT_MARGIN, Spectrum, TICK_HEIGHT, VIEW_MODES, WINDOWS,
};
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Ui, pos2, vec2};

use crate::timeview::{HEADER_WIDTH, wheel_angle};

/// The dialog's working copy: channel and settings.
#[derive(Clone, Debug)]
pub struct FftDialog {
    channel: u16,
    spec: Spectrum,
}

#[derive(Default)]
pub struct FftState {
    /// Per DSO channel (index 0 and 1).
    pub specs: [Spectrum; 2],
    bins: [Vec<f64>; 2],
    /// What the bins were computed from: (data sequence, settings).
    made: [Option<(u64, Spectrum, u64, u64)>; 2],
    pub dialog: Option<FftDialog>,
    pan_last: Option<f32>,
    hover: Option<usize>,
}

/// The channel's V/div, probe factor and hw offset.
fn chan(s: &Session, index: u16) -> Option<(usize, u64, u64, i32)> {
    let chans = s.channels();
    let pos = chans
        .iter()
        .position(|c| c.ty == channel_type::DSO && c.index == index && c.enabled)?;
    let get = |k: i32| s.config(k, Some(pos));
    let c = &chans[pos];
    Some((
        pos,
        get(key::PROBE_VDIV).map_or(c.vdiv, |v| v.get_u64()),
        get(key::PROBE_FACTOR).map_or(1, |v| v.get_u64()).max(1),
        get(key::PROBE_HW_OFFSET).map_or(c.hw_offset, |v| v.get_u16()) as i32,
    ))
}

impl FftState {
    /// The enabled spectrum, if any (Qt shows one at a time).
    pub fn active(&self) -> Option<usize> {
        self.specs.iter().position(|s| s.enabled)
    }

    /// `calc_fft` when the data or the settings changed.
    fn update(&mut self, s: &Session, i: usize) {
        let seq = {
            let st = s.state();
            st.dso_seq.wrapping_add(st.started_seq << 32)
        };
        let spec = self.specs[i].clone();
        let Some((_, vdiv, factor, hw_offset)) = chan(s, i as u16) else {
            self.bins[i].clear();
            return;
        };
        let key = (seq, spec.clone(), vdiv, factor);
        if self.made[i].as_ref() == Some(&key) {
            return;
        }
        self.made[i] = Some(key);
        let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        let n = spec.sample_num as usize;
        let step = spec.interval as usize;
        self.bins[i] = if store.sample_count() < (n * step) as u64 {
            Vec::new()
        } else {
            store
                .get_samples(0, i as i32)
                .and_then(|b| {
                    power_spectrum(
                        b,
                        hw_offset,
                        volts_per_code(vdiv, factor),
                        n,
                        step,
                        Window::from_index(spec.window),
                    )
                })
                .unwrap_or_default()
        };
    }

    /// The pane: header label, spectrum, rulers, hover; wheel zooms,
    /// dragging pans.
    pub fn show(&mut self, ui: &mut Ui, s: &Session) {
        let Some(i) = self.active() else { return };
        self.update(s, i);
        let full = ui.max_rect();
        let header = Rect::from_min_max(full.min, pos2(full.left() + HEADER_WIDTH, full.bottom()));
        let pane = Rect::from_min_max(pos2(header.right(), full.top()), full.max);
        let w = (pane.width() - RIGHT_MARGIN as f32).max(1.0);
        let h = pane.height().max(1.0);
        let rect = Rect::from_min_size(pane.min, vec2(w, h));
        let Some((_, vdiv, factor, _)) = chan(s, i as u16) else {
            return;
        };
        let (vdiv, factor) = (vdiv as f64, factor as f64);
        let colour = {
            let sig = s
                .state()
                .signals
                .iter()
                .find(|x| x.ty == channel_type::DSO && x.index == i as u16)
                .cloned();
            let [r, g, b] = sig
                .and_then(|x| x.colour)
                .unwrap_or(dso::SIGNAL_COLOURS[i % dso::SIGNAL_COLOURS.len()]);
            Color32::from_rgb(r, g, b)
        };

        // input
        let resp = ui.interact(pane, ui.make_persistent_id("fft-pane"), egui::Sense::drag());
        let spec = &mut self.specs[i];
        if let Some(p) = resp.hover_pos() {
            let (_, dy) = ui.input(wheel_angle);
            if dy != 0 {
                spec.zoom(dy as f64 / 120.0, (p.x - rect.left()) as i32, w as i32);
            }
            self.hover = spec.hover_bin((p.x - rect.left()) as f64, w as f64);
        } else {
            self.hover = None;
        }
        if resp.drag_started() {
            self.pan_last = resp.interact_pointer_pos().map(|p| p.x);
        }
        if resp.dragged()
            && let (Some(last), Some(p)) = (self.pan_last, resp.interact_pointer_pos())
        {
            spec.pan((last - p.x) as f64, w as i32);
            self.pan_last = Some(p.x);
        }
        if resp.drag_stopped() {
            self.pan_last = None;
        }

        // paint
        let p = ui.painter_at(full);
        let fore = Color32::from_gray(220);
        let font = egui::FontId::proportional(11.0);
        p.rect_filled(full, 0.0, Color32::from_gray(24));
        p.rect_filled(header, 0.0, Color32::from_gray(34));
        p.rect_filled(rect, 0.0, Color32::from_gray(12));
        p.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(220, 220, 220, 100)),
            egui::StrokeKind::Inside,
        );
        // the header label ("FFT(n)")
        let label = Rect::from_center_size(
            pos2(header.right() - 25.0, rect.center().y),
            vec2(40.0, 20.0),
        );
        p.rect_filled(label, 0.0, colour);
        p.text(
            label.center(),
            egui::Align2::CENTER_CENTER,
            format!("FFT({i})"),
            font.clone(),
            Color32::WHITE,
        );
        let o = |x: f64, y: f64| pos2(rect.left() + x as f32, rect.top() + y as f32);
        let spec = &self.specs[i];
        let bins = &self.bins[i];
        let pts: Vec<Pos2> = spec
            .points(bins, vdiv, factor, w as f64, h as f64)
            .into_iter()
            .map(|(x, y)| o(x as f64, y as f64))
            .collect();
        if pts.len() > 1 {
            p.add(egui::Shape::line(pts, Stroke::new(1.0, colour)));
        }
        // rulers
        let snap = s.state().cur_snap_samplerate as f64;
        let st = Stroke::new(1.0, fore);
        for (x, major, text) in spec.freq_ticks(snap, w as f64) {
            let len = if major {
                TICK_HEIGHT
            } else {
                TICK_HEIGHT / 2.0
            };
            p.line_segment([o(x, 1.0), o(x, len)], st);
            if let Some(t) = text
                && x > 30.0
                && (w as f64 - x) > 30.0
            {
                p.text(
                    o(x, TICK_HEIGHT),
                    egui::Align2::CENTER_TOP,
                    t,
                    font.clone(),
                    fore,
                );
            }
        }
        for (y, text) in spec.level_ticks(vdiv, factor, h as f64) {
            p.line_segment([o(w as f64, y), o(w as f64 - TICK_HEIGHT / 2.0, y)], st);
            p.text(
                o(w as f64 - TICK_HEIGHT, y),
                egui::Align2::RIGHT_CENTER,
                text,
                font.clone(),
                fore,
            );
        }
        let df = spectrum::delta_freq(snap, spec.sample_num, spec.interval);
        p.text(
            o(w as f64, h as f64),
            egui::Align2::RIGHT_BOTTOM,
            format!(" \u{394}Freq: {}", spectrum::format_freq(df, 4)),
            font.clone(),
            fore,
        );
        if let Some(bin) = self.hover
            && let Some((x, y, t)) = spec.hover(bins, bin, vdiv, factor, w as f64, h as f64, df)
        {
            p.extend(egui::Shape::dashed_line(
                &[o(x, 0.0), o(x, h as f64)],
                st,
                4.0,
                3.0,
            ));
            p.circle_filled(o(x, y), 3.0, fore);
            p.text(o(x, y), egui::Align2::LEFT_BOTTOM, t, font, fore);
        }
    }

    /// The FFT options dialog; changes apply on OK (only one spectrum is
    /// enabled at a time, as in Qt).
    pub fn dialog(&mut self, ctx: &egui::Context, s: &Session) {
        let Some(mut d) = self.dialog.take() else {
            return;
        };
        let mut keep = true;
        let max_limits = s.config_u64(key::MAX_DSO_SAMPLELIMITS).unwrap_or(0);
        egui::Window::new(tr("FFT Options"))
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("fft-options")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label(tr("FFT Enable: "));
                        ui.checkbox(&mut d.spec.enabled, "");
                        ui.end_row();
                        ui.label(tr("FFT Source: "));
                        let names: Vec<(u16, String)> = s
                            .state()
                            .signals
                            .iter()
                            .filter(|x| x.ty == channel_type::DSO)
                            .map(|x| (x.index, x.name.clone()))
                            .collect();
                        let cur = names
                            .iter()
                            .find(|n| n.0 == d.channel)
                            .map_or(String::new(), |n| n.1.clone());
                        egui::ComboBox::from_id_salt("fft-src")
                            .selected_text(cur)
                            .show_ui(ui, |ui| {
                                for (i, n) in &names {
                                    if ui.selectable_label(*i == d.channel, n).clicked() && *i < 2 {
                                        d.channel = *i;
                                        d.spec = Spectrum {
                                            enabled: d.spec.enabled,
                                            ..self.specs[*i as usize].clone()
                                        };
                                    }
                                }
                            });
                        ui.end_row();
                        let (lengths, _) =
                            spectrum::lengths_and_intervals(max_limits, d.spec.sample_num);
                        ui.label(tr("FFT Length: "));
                        egui::ComboBox::from_id_salt("fft-len")
                            .selected_text(d.spec.sample_num.to_string())
                            .show_ui(ui, |ui| {
                                for l in &lengths {
                                    ui.selectable_value(&mut d.spec.sample_num, *l, l.to_string());
                                }
                            });
                        ui.end_row();
                        // the intervals follow the length
                        let (_, intervals2) =
                            spectrum::lengths_and_intervals(max_limits, d.spec.sample_num);
                        if !intervals2.contains(&d.spec.interval) {
                            d.spec.interval = intervals2.last().copied().unwrap_or(1);
                        }
                        ui.label(tr("Sample Interval: "));
                        egui::ComboBox::from_id_salt("fft-int")
                            .selected_text(d.spec.interval.to_string())
                            .show_ui(ui, |ui| {
                                for v in &intervals2 {
                                    ui.selectable_value(&mut d.spec.interval, *v, v.to_string());
                                }
                            });
                        ui.end_row();
                        ui.label(tr("FFT Window: "));
                        egui::ComboBox::from_id_salt("fft-win")
                            .selected_text(WINDOWS[d.spec.window.clamp(0, 4) as usize])
                            .show_ui(ui, |ui| {
                                for (k, w) in WINDOWS.iter().enumerate() {
                                    ui.selectable_value(&mut d.spec.window, k as i32, *w);
                                }
                            });
                        ui.end_row();
                        ui.label(tr("DC Ignored: "));
                        ui.checkbox(&mut d.spec.dc_ignore, "");
                        ui.end_row();
                        ui.label(tr("Y-axis Mode: "));
                        egui::ComboBox::from_id_salt("fft-mode")
                            .selected_text(VIEW_MODES[d.spec.view_mode.min(1)])
                            .show_ui(ui, |ui| {
                                for (k, m) in VIEW_MODES.iter().enumerate() {
                                    ui.selectable_value(&mut d.spec.view_mode, k, *m);
                                }
                            });
                        ui.end_row();
                        ui.label(tr("DBV Range: "));
                        ui.add_enabled_ui(d.spec.view_mode == 1, |ui| {
                            egui::ComboBox::from_id_salt("fft-dbv")
                                .selected_text(d.spec.dbv_range.to_string())
                                .show_ui(ui, |ui| {
                                    for r in DBV_RANGES {
                                        ui.selectable_value(
                                            &mut d.spec.dbv_range,
                                            r,
                                            r.to_string(),
                                        );
                                    }
                                });
                        });
                        ui.end_row();
                    });
                ui.horizontal(|ui| {
                    if ui.button(tr("OK")).clicked() {
                        // accept(): every trace off, the chosen one as set
                        for (k, sp) in self.specs.iter_mut().enumerate() {
                            sp.enabled = false;
                            if k == d.channel as usize {
                                let (scale, offset) = (sp.scale, sp.offset);
                                *sp = d.spec.clone();
                                // init_zoom when the length changed
                                if sp.sample_num == d.spec.sample_num {
                                    sp.scale = scale;
                                    sp.offset = offset;
                                }
                                sp.scale = sp.scale.max(100.0 / sp.sample_num as f64);
                            }
                        }
                        keep = false;
                    }
                    if ui.button(tr("Cancel")).clicked() || crate::msgbox::esc(ui.ctx()) {
                        keep = false;
                    }
                });
            });
        if keep {
            self.dialog = Some(d);
        }
    }

    /// Opens the dialog on the enabled spectrum (else channel 0).
    pub fn open_dialog(&mut self) {
        let ch = self.active().unwrap_or(0);
        self.dialog = Some(FftDialog {
            channel: ch as u16,
            spec: Spectrum {
                enabled: true,
                ..self.specs[ch].clone()
            },
        });
    }
}
