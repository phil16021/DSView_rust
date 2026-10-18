//! The Lissajous figure (`pv/view/lissajoustrace.cpp`), drawn instead of the
//! DSO traces while enabled, and its options dialog
//! (`pv/dialogs/lissajousoptions.cpp`).

use dsview_session::Session;
use dsview_viewmodel::dso;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, pos2, vec2};

/// `View::Blue`
const BLUE: Color32 = Color32::from_rgb(17, 133, 209);

#[derive(Clone, Debug, Default)]
pub struct LissajousState {
    pub enabled: bool,
    pub x: u16,
    pub y: u16,
    pub percent: i32,
    pub dialog: Option<(bool, u16, u16, i32)>,
}

impl LissajousState {
    pub fn open_dialog(&mut self, s: &Session) {
        let min = dso::lissajous_min_percent(s.state().cur_samplelimits);
        let (x, y) = if self.x == self.y {
            (0, 1)
        } else {
            (self.x, self.y)
        };
        self.dialog = Some((true, x, y, self.percent.max(min)));
    }

    pub fn dialog(&mut self, ctx: &egui::Context, s: &Session, names: &[(u16, String)]) {
        let Some((mut en, mut x, mut y, mut percent)) = self.dialog.take() else {
            return;
        };
        let min = dso::lissajous_min_percent(s.state().cur_samplelimits);
        let mut keep = true;
        egui::Window::new("Lissajous Options")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.checkbox(&mut en, "Enable");
                ui.horizontal(|ui| {
                    let l = ui.label("Data Percentage");
                    ui.add_enabled(
                        min < 100,
                        egui::Slider::new(&mut percent, min..=100).suffix("%"),
                    )
                    .labelled_by(l.id);
                });
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label("X-axis");
                        for (i, n) in names {
                            ui.radio_value(&mut x, *i, n);
                        }
                    });
                    ui.vertical(|ui| {
                        ui.label("Y-axis");
                        for (i, n) in names {
                            ui.radio_value(&mut y, *i, n);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        (self.enabled, self.x, self.y, self.percent) = (en, x, y, percent);
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() || crate::msgbox::esc(ui.ctx()) {
                        keep = false;
                    }
                });
            });
        if keep {
            self.dialog = Some((en, x, y, percent));
        }
    }

    /// `paint_back` and `paint_mid` over the trace area.
    pub fn paint(&self, p: &egui::Painter, s: &Session, area: Rect, width: i32) {
        let (border, lines) = dso::lissajous_grid(width, area.height() as i32);
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let br = Rect::from_min_size(
            o(border.x as f32, border.y as f32),
            vec2(border.w as f32, border.h as f32),
        );
        p.rect_filled(br, 0.0, Color32::from_gray(12));
        p.rect_stroke(
            br,
            0.0,
            Stroke::new(1.0, Color32::from_rgba_unmultiplied(220, 220, 220, 100)),
            egui::StrokeKind::Inside,
        );
        let dashed = Stroke::new(1.0, Color32::from_rgba_unmultiplied(220, 220, 220, 60));
        for (x0, y0, x1, y1) in lines {
            p.extend(egui::Shape::dashed_line(
                &[o(x0, y0), o(x1, y1)],
                dashed,
                4.0,
                2.0,
            ));
        }
        let font = egui::FontId::proportional(12.0);
        p.text(
            o(border.x as f32 + 10.0, border.y as f32 + 10.0),
            egui::Align2::LEFT_TOP,
            "Lissajous Figure",
            font.clone(),
            Color32::from_gray(220),
        );
        let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        let n = store.sample_count();
        if n == 0 {
            return;
        }
        let msg = |t: &str| {
            p.text(
                o(border.x as f32 + 10.0, border.y as f32 + 30.0),
                egui::Align2::LEFT_TOP,
                t,
                font.clone(),
                Color32::from_rgb(213, 15, 37),
            );
        };
        if store.channel_num() < 2 {
            msg("Requires the data of two channels.");
            return;
        }
        let (Some(xs), Some(ys)) = (
            store.get_samples(0, self.x as i32),
            store.get_samples(0, self.y as i32),
        ) else {
            msg("Data source error.");
            return;
        };
        let pts: Vec<Pos2> = dso::lissajous_points(border, xs, ys, n, self.percent)
            .into_iter()
            .map(|(x, y)| o(x, y))
            .collect();
        if pts.len() > 1 {
            p.add(egui::Shape::line(pts, Stroke::new(1.0, BLUE)));
        }
    }
}
