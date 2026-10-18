//! Reference waveforms (`Viewport::paint_ref_waves`, `SigSession::
//! add_ref_wave`) and their dialog (`pv/dialogs/refoptions.cpp`): frozen
//! copies of a DSO channel, drawn translucent on the channel's current
//! V/div and zero line.

use dsview_session::Session;
use dsview_viewmodel::dso::{self, IRect, RefWave};
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, pos2};

use crate::dsoview::ChanView;
use crate::timeview::TimeView;

#[derive(Default)]
pub struct RefState {
    pub waves: Vec<RefWave>,
    /// The dialog's channel, while open.
    pub dialog: Option<u16>,
}

impl RefState {
    /// `add_ref_wave` for channel `c` as shown.
    fn save(&mut self, s: &Session, c: &ChanView) {
        let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        let n = store.sample_count() as usize;
        let Some(buf) = store.get_samples(0, c.sig.index as i32) else {
            return;
        };
        if n == 0 || c.geom.rect.h <= 0 {
            return;
        }
        let colour = c.colour;
        self.waves.push(RefWave {
            index: c.sig.index,
            samples: buf[..n.min(buf.len())].to_vec(),
            samplerate: s.state().cur_snap_samplerate as f64,
            hw_offset: c.hw_offset,
            mv_per_code: RefWave::mv_per_code_for(c.geom.scale, c.vdiv, c.factor, c.geom.rect.h),
            colour: [colour.r(), colour.g(), colour.b()],
            name: format!("Ref {}", self.waves.len() + 1),
        });
    }

    /// The dialog: channel, save, clear all, count.
    pub(crate) fn dialog(&mut self, ctx: &egui::Context, s: &Session, chans: &[ChanView]) {
        let Some(mut ch) = self.dialog else { return };
        let mut open = true;
        egui::Window::new("Reference Waveforms")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let l = ui.label("Channel");
                    let cur = chans
                        .iter()
                        .find(|c| c.sig.index == ch)
                        .map_or(String::new(), |c| c.sig.name.clone());
                    egui::ComboBox::from_id_salt("ref-ch")
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            for c in chans {
                                ui.selectable_value(&mut ch, c.sig.index, &c.sig.name);
                            }
                        })
                        .response
                        .labelled_by(l.id);
                });
                ui.horizontal(|ui| {
                    if ui.button("Save reference").clicked()
                        && let Some(c) = chans.iter().find(|c| c.sig.index == ch)
                    {
                        self.save(s, c);
                    }
                    if ui.button("Clear all").clicked() {
                        self.waves.clear();
                    }
                });
                ui.label(format!("Stored references: {}", self.waves.len()));
            });
        self.dialog = open.then_some(ch);
    }
}

impl TimeView {
    /// Every reference over its (enabled) channel, with its name at the
    /// left, stacked per channel.
    pub(crate) fn paint_refs(&self, p: &egui::Painter, area: Rect, chans: &[ChanView], width: i32) {
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let font = egui::FontId::proportional(12.0);
        let mut rows: Vec<i32> = Vec::new();
        for rw in &self.dso.refs.waves {
            let Some(c) = chans.iter().find(|c| c.sig.index == rw.index && c.enabled) else {
                continue;
            };
            let mv_per_div = (c.vdiv * c.factor) as f64;
            let zero_y = c.zero_y();
            let Some((pts, dense)) = dso::ref_wave_shape(
                rw,
                &c.geom,
                zero_y,
                mv_per_div,
                self.vp.scale,
                self.vp.x_offset,
                self.vp.trig_hoff,
                width,
            ) else {
                continue;
            };
            let [r, g, b] = rw.colour;
            let col = Color32::from_rgba_unmultiplied(r, g, b, 130);
            let pts: Vec<Pos2> = pts.into_iter().map(|(x, y)| o(x, y)).collect();
            if dense {
                p.add(egui::Shape::line(pts, Stroke::new(1.0, col)));
            } else {
                // dash and gap of five pen widths
                p.extend(egui::Shape::dashed_line(
                    &pts,
                    Stroke::new(1.0, col),
                    5.0,
                    5.0,
                ));
            }
            if rows.len() <= rw.index as usize {
                rows.resize(rw.index as usize + 1, 0);
            }
            let row = rows[rw.index as usize];
            rows[rw.index as usize] += 1;
            let top: IRect = c.geom.rect;
            p.text(
                o(4.0, top.y as f32 + 2.0 + row as f32 * 15.0),
                egui::Align2::LEFT_TOP,
                &rw.name,
                font.clone(),
                Color32::from_rgba_unmultiplied(r, g, b, 230),
            );
        }
    }
}
