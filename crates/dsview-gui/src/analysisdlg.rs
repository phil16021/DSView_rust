//! The DSO analysis dialogs: histogram with jitter
//! (`pv/dialogs/dsohistogram.cpp`) and channel-to-channel measure
//! (`pv/dialogs/chanmeasure.cpp`), computed by `dsview_dsp::analysis`.

use crate::i18n::tr;
use dsview_dsp::analysis::{self, Histogram};
use dsview_session::Session;
use eframe::egui::{self, Color32, Rect, Stroke, Ui, pos2, vec2};

use crate::dsoview::ChanView;

#[derive(Default)]
pub struct AnalysisDialogs {
    /// The histogram dialog: channel and unit (true: V).
    pub histogram: Option<(u16, bool)>,
    /// The channel measure dialog: the two sources.
    pub chan_measure: Option<(u16, u16)>,
}

/// The codes, hw offset, mV per code and rate of channel `c` as shown.
fn source(s: &Session, c: &ChanView) -> Option<(Vec<u8>, i32, f64, f64)> {
    let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
    let n = store.sample_count() as usize;
    let buf = store.get_samples(0, c.sig.index as i32)?;
    let mv = dsview_viewmodel::dso::RefWave::mv_per_code_for(
        c.geom.scale,
        c.vdiv,
        c.factor,
        c.geom.rect.h,
    );
    Some((
        buf[..n.min(buf.len())].to_vec(),
        c.hw_offset,
        mv,
        s.state().cur_snap_samplerate as f64,
    ))
}

/// `HistogramPlot`: the bars, three dotted grid lines, the mean.
fn plot(ui: &mut Ui, title: &str, h: Option<&Histogram>, fac: f64, unit: &str) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width().max(300.0), 140.0),
        egui::Sense::hover(),
    );
    let p = ui.painter_at(rect);
    let fore = ui.visuals().text_color();
    let font = egui::FontId::proportional(11.0);
    p.text(
        rect.left_top(),
        egui::Align2::LEFT_TOP,
        title,
        font.clone(),
        fore,
    );
    let area = Rect::from_min_max(
        pos2(rect.left(), rect.top() + 16.0),
        pos2(rect.right(), rect.bottom() - 16.0),
    );
    p.rect_stroke(area, 0.0, Stroke::new(1.0, fore), egui::StrokeKind::Inside);
    let Some(h) = h.filter(|h| !h.bins.is_empty()) else {
        p.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            "No data",
            font,
            fore,
        );
        return;
    };
    for k in 1..=3 {
        let y = area.bottom() - area.height() * k as f32 / 4.0;
        p.extend(egui::Shape::dotted_line(
            &[pos2(area.left(), y), pos2(area.right(), y)],
            Color32::GRAY,
            3.0,
            0.6,
        ));
    }
    let peak = h.bins.iter().copied().fold(0.0, f64::max).max(1.0);
    let bw = area.width() / h.bins.len() as f32;
    let accent = Color32::from_rgb(17, 133, 209);
    for (i, &v) in h.bins.iter().enumerate() {
        let bh = (v / peak * (area.height() - 1.0) as f64) as f32;
        if bh > 0.0 {
            let x = area.left() + i as f32 * bw;
            p.rect_filled(
                Rect::from_min_max(
                    pos2(x, area.bottom() - bh),
                    pos2(x + bw.max(1.0), area.bottom()),
                ),
                0.0,
                accent,
            );
        }
    }
    let fmt = |v: f64| format!("{:.2} {unit}", v * fac);
    if h.max > h.min {
        let rate = ((h.mean - h.min) / (h.max - h.min)) as f32;
        let x = area.left() + rate * area.width();
        let orange = Color32::from_rgb(238, 178, 17);
        p.extend(egui::Shape::dashed_line(
            &[pos2(x, area.top()), pos2(x, area.bottom())],
            Stroke::new(1.0, orange),
            4.0,
            2.0,
        ));
        p.text(
            pos2(x + 3.0, area.top() + 2.0),
            egui::Align2::LEFT_TOP,
            format!("Mean {}", fmt(h.mean)),
            font.clone(),
            orange,
        );
    }
    p.text(
        pos2(area.left(), area.bottom() + 2.0),
        egui::Align2::LEFT_TOP,
        fmt(h.min),
        font.clone(),
        fore,
    );
    p.text(
        pos2(area.right(), area.bottom() + 2.0),
        egui::Align2::RIGHT_TOP,
        fmt(h.max),
        font,
        fore,
    );
}

fn channel_combo(ui: &mut Ui, id: &str, label: &str, v: &mut u16, chans: &[ChanView]) {
    ui.horizontal(|ui| {
        let l = ui.label(label);
        let cur = chans
            .iter()
            .find(|c| c.sig.index == *v)
            .map_or(String::new(), |c| c.sig.name.clone());
        egui::ComboBox::from_id_salt(id)
            .selected_text(cur)
            .show_ui(ui, |ui| {
                for c in chans {
                    ui.selectable_value(v, c.sig.index, &c.sig.name);
                }
            })
            .response
            .labelled_by(l.id);
    });
}

impl AnalysisDialogs {
    pub(crate) fn show(&mut self, ctx: &egui::Context, s: &Session, chans: &[ChanView]) {
        if let Some((mut ch, mut volts)) = self.histogram {
            let mut open = !crate::msgbox::esc(ctx);
            egui::Window::new(tr("Histogram"))
                .open(&mut open)
                .default_width(420.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        channel_combo(ui, "hist-ch", "Channel", &mut ch, chans);
                        ui.selectable_value(&mut volts, false, tr("mV"));
                        ui.selectable_value(&mut volts, true, tr("V"));
                    });
                    let c = chans.iter().find(|c| c.sig.index == ch);
                    let Some(c) = c.filter(|c| c.enabled) else {
                        ui.label(tr("No enabled DSO channel selected."));
                        return;
                    };
                    let Some(h) = source(s, c).and_then(|(codes, hw, mv, rate)| {
                        analysis::histograms(&codes, hw, mv, rate)
                    }) else {
                        ui.label(tr("No captured data. Run an acquisition first."));
                        return;
                    };
                    let (fac, unit, prec) = if volts {
                        (0.001, "V", 4)
                    } else {
                        (1.0, "mV", 2)
                    };
                    plot(ui, "Value distribution", Some(&h.value), fac, unit);
                    plot(
                        ui,
                        "Period distribution (jitter)",
                        h.period.as_ref(),
                        1.0,
                        "ns",
                    );
                    let v = |x: f64| format!("{:.*} {unit}", prec, x * fac);
                    egui::Grid::new("hist-stats")
                        .num_columns(2)
                        .striped(true)
                        .show(ui, |ui| {
                            let mut row = |k: &str, val: String| {
                                ui.label(k);
                                ui.label(val);
                                ui.end_row();
                            };
                            row("Channel", format!("CH{}", c.sig.name));
                            row("Samples", h.samples.to_string());
                            row("Vmax", v(h.value.max));
                            row("Vmin", v(h.value.min));
                            row("Vpp", v(h.value.max - h.value.min));
                            row("Vmean", v(h.value.mean));
                            match &h.period {
                                Some(p) => {
                                    row("Edges", h.edges.to_string());
                                    row("Mean period", analysis::fmt_time_ns(p.mean));
                                    row("Frequency", analysis::fmt_freq_hz(1e9 / p.mean));
                                    row("RMS jitter", analysis::fmt_time_ns(h.rms_jitter));
                                    row("Pk-pk jitter", analysis::fmt_time_ns(h.pk_pk_jitter));
                                }
                                None => {
                                    row("Not enough edges for jitter analysis.", String::new());
                                    row("Edges found", h.edges.to_string());
                                }
                            }
                        });
                });
            self.histogram = open.then_some((ch, volts));
        }
        if let Some((mut a, mut b)) = self.chan_measure {
            let mut open = !crate::msgbox::esc(ctx);
            egui::Window::new(tr("Channel-to-channel measure"))
                .open(&mut open)
                .show(ctx, |ui| {
                    channel_combo(ui, "cm-a", "1st Source", &mut a, chans);
                    channel_combo(ui, "cm-b", "2st Source", &mut b, chans);
                    let (ca, cb) = (
                        chans.iter().find(|c| c.sig.index == a),
                        chans.iter().find(|c| c.sig.index == b),
                    );
                    let text = match (ca, cb) {
                        (Some(_), Some(_)) if a == b => {
                            "Select two different channels.".to_string()
                        }
                        (Some(ca), Some(cb)) => {
                            let e = |c: &ChanView| {
                                source(s, c).filter(|_| c.enabled).and_then(
                                    |(codes, hw, mv, rate)| {
                                        analysis::edges(
                                            &codes,
                                            hw,
                                            mv,
                                            if rate > 0.0 { 1e9 / rate } else { 0.0 },
                                        )
                                    },
                                )
                            };
                            match (e(ca), e(cb)) {
                                (Some(ea), Some(eb)) => {
                                    analysis::channel_measure(&ca.sig.name, &ea, &cb.sig.name, &eb)
                                }
                                _ => tr("No captured data. Run an acquisition first."),
                            }
                        }
                        _ => tr("Select two DSO channels."),
                    };
                    ui.label(text);
                });
            self.chan_measure = open.then_some((a, b));
        }
    }
}
