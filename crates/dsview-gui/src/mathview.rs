//! The oscilloscope's math trace (`pv/view/mathtrace.cpp`) and its options
//! dialog (`pv/dialogs/mathoptions.cpp`): the result of `dsview_dsp::math`
//! over the shown frame, drawn in red with its own V/div dial and zero line.

use dsview_dsp::math::{self, Dial, MathType, Source};
use dsview_session::Session;
use dsview_viewmodel::dso::{self, IRect, TraceShape};
use dsview_viewmodel::viewport::Timebase;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Ui, pos2, vec2};

use crate::dsoview::ChanView;
use crate::timeview::{HEADER_WIDTH, TimeView, wheel_angle};

/// `View::Red`
const RED: Color32 = Color32::from_rgb(213, 15, 37);

#[derive(Clone, Debug, PartialEq)]
pub struct MathDialog {
    enabled: bool,
    ty: MathType,
    src1: u16,
    src2: u16,
    filter_width: i32,
}

/// What the result was computed from.
#[derive(Clone, Debug, PartialEq)]
struct Made {
    seq: u64,
    ty: MathType,
    src: (u16, u16),
    filter_width: i32,
    factor: u64,
    scales: (u64, u64, u64, u64, u32, u32),
}

#[derive(Default)]
pub struct MathState {
    pub enabled: bool,
    pub ty: MathType,
    pub src1: u16,
    pub src2: u16,
    pub filter_width: i32,
    /// The dial (`get_vDial()`): entries, selected one, factor.
    pub dial_values: Vec<u64>,
    pub sel: usize,
    pub factor: u64,
    /// `_zero_vrate`
    pub zero_vrate: f64,
    values: Vec<f64>,
    levels: Vec<Vec<(f64, f64)>>,
    made: Option<Made>,
    pub dialog: Option<MathDialog>,
    drag_zero: Option<(f32, f64)>,
    rebuild: bool,
}

impl MathState {
    pub fn vdial(&self) -> u64 {
        self.dial_values.get(self.sel).copied().unwrap_or(1000)
    }

    pub fn open_dialog(&mut self) {
        self.dialog = Some(MathDialog {
            enabled: true,
            ty: self.ty,
            src1: self.src1,
            src2: if self.src2 == self.src1 {
                1 - self.src1.min(1)
            } else {
                self.src2
            },
            filter_width: if self.filter_width == 0 {
                10
            } else {
                self.filter_width
            },
        });
    }

    /// The options dialog: applied on OK (`math_rebuild`).
    pub fn dialog(&mut self, ctx: &egui::Context, names: &[(u16, String)]) {
        let Some(mut d) = self.dialog.take() else {
            return;
        };
        let mut keep = true;
        egui::Window::new("Math Options")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.checkbox(&mut d.enabled, "Enable");
                ui.separator();
                let src = |ui: &mut Ui, id: &str, v: &mut u16| {
                    let cur = names
                        .iter()
                        .find(|n| n.0 == *v)
                        .map_or(String::new(), |n| n.1.clone());
                    egui::ComboBox::from_id_salt(id)
                        .selected_text(cur)
                        .show_ui(ui, |ui| {
                            for (i, n) in names {
                                ui.selectable_value(v, *i, n);
                            }
                        });
                };
                egui::Grid::new("math-options")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("Source 1");
                        src(ui, "math-src1", &mut d.src1);
                        ui.end_row();
                        ui.label("Source 2");
                        ui.add_enabled_ui(!d.ty.is_unary(), |ui| src(ui, "math-src2", &mut d.src2));
                        ui.end_row();
                    });
                ui.separator();
                egui::Grid::new("math-types").num_columns(4).show(ui, |ui| {
                    for (k, t) in MathType::ALL.iter().enumerate() {
                        ui.radio_value(&mut d.ty, *t, t.name());
                        if k % 4 == 3 {
                            ui.end_row();
                        }
                    }
                });
                ui.separator();
                ui.add_enabled_ui(
                    matches!(d.ty, MathType::LowPass | MathType::HighPass),
                    |ui| {
                        ui.horizontal(|ui| {
                            let l = ui.label("Filter window (samples)");
                            ui.add(egui::DragValue::new(&mut d.filter_width).range(1..=100000))
                                .labelled_by(l.id);
                        });
                    },
                );
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        self.enabled = d.enabled;
                        self.ty = d.ty;
                        self.src1 = d.src1;
                        self.src2 = d.src2;
                        self.filter_width = d.filter_width;
                        self.zero_vrate = 0.5;
                        self.rebuild = true;
                        keep = false;
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });
        if keep {
            self.dialog = Some(d);
        }
    }

    /// `math_rebuild` + `update_vDial`: the dial from the sources.
    fn rebuild_dial(&mut self, chans: &[ChanView]) {
        let dial = |i: u16| {
            chans.iter().find(|c| c.sig.index == i).map(|c| Dial {
                value: c.vdiv,
                factor: c.factor,
                min: c.vdivs.first().copied().unwrap_or(c.vdiv),
                max: c.vdivs.last().copied().unwrap_or(c.vdiv),
            })
        };
        let (Some(d1), Some(d2)) = (dial(self.src1), dial(self.src2).or(dial(self.src1))) else {
            return;
        };
        self.dial_values = math::dial_values(self.ty, d1, d2);
        let v = math::default_vdial(self.ty, d1, d2);
        self.sel = self.dial_values.iter().position(|&x| x == v).unwrap_or(0);
        self.factor = math::default_factor(self.ty, d1, d2);
        self.made = None;
    }

    /// `calc_math` when the frame or a setting changed.
    fn update(&mut self, s: &Session, chans: &[ChanView], rect: IRect, spp: f64) {
        if self.rebuild || self.dial_values.is_empty() {
            self.rebuild = false;
            self.rebuild_dial(chans);
        }
        let seq = {
            let st = s.state();
            st.dso_seq.wrapping_add(st.started_seq << 32)
        };
        let (Some(c1), c2) = (
            chans.iter().find(|c| c.sig.index == self.src1 && c.enabled),
            chans.iter().find(|c| c.sig.index == self.src2 && c.enabled),
        ) else {
            self.values.clear();
            return;
        };
        let c2v = c2.map_or((0, 0, 0, 0.0), |c| {
            (c.vdiv, c.factor, c.hw_offset, c.geom.scale)
        });
        let made = Made {
            seq,
            ty: self.ty,
            src: (self.src1, self.src2),
            filter_width: self.filter_width,
            factor: self.factor,
            scales: (
                c1.vdiv,
                c1.factor,
                c2v.0,
                c2v.1,
                c1.geom.scale.to_bits(),
                c2v.3.to_bits(),
            ),
        };
        if self.made.as_ref() != Some(&made) {
            self.made = Some(made);
            let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
            let n = store.sample_count() as usize;
            let src = |c: &ChanView| -> Option<(Vec<u8>, f64, f64)> {
                let codes = store.get_samples(0, c.sig.index as i32)?;
                let scale = Source::scale_for(c.vdiv, c.factor, c.geom.scale as f64, rect.h);
                Some((
                    codes[..n.min(codes.len())].to_vec(),
                    scale,
                    c.hw_offset as f64 * scale,
                ))
            };
            let a = src(c1);
            let b = c2.and_then(src);
            let rate = s.state().cur_snap_samplerate as f64;
            self.values = match &a {
                Some((codes, scale, delta)) => math::calc(
                    self.ty,
                    Source {
                        codes,
                        scale: *scale,
                        delta: *delta,
                    },
                    b.as_ref().map(|(codes, scale, delta)| Source {
                        codes,
                        scale: *scale,
                        delta: *delta,
                    }),
                    n,
                    rate,
                    self.filter_width,
                    self.factor,
                )
                .unwrap_or_default(),
                None => Vec::new(),
            };
            self.levels.clear();
        }
        if spp >= dso::ENVELOPE_THRESHOLD && self.levels.is_empty() && !self.values.is_empty() {
            self.levels = math::envelope(&self.values);
        }
    }
}

impl TimeView {
    /// The math trace over the DSO grid (`MathTrace::paint_mid/_fore`).
    pub(crate) fn paint_math(
        &mut self,
        p: &egui::Painter,
        s: &Session,
        area: Rect,
        rect: IRect,
        tb: &Timebase,
        chans: &[ChanView],
    ) {
        if !self.dso.math.enabled {
            return;
        }
        let spp = tb.snap_samplerate * self.vp.scale;
        self.dso.math.update(s, chans, rect, spp);
        let m = &self.dso.math;
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let zero_y = (m.zero_vrate * rect.h as f64 + rect.y as f64) as f32;
        // the zero line
        p.extend(egui::Shape::dotted_line(
            &[o(0.0, zero_y), o(rect.w as f32, zero_y)],
            Color32::from_rgba_unmultiplied(220, 220, 220, 100),
            3.0,
            0.8,
        ));
        if m.values.is_empty() {
            return;
        }
        let g = dso::Geom::new(rect, Some(0), Some(255), 8);
        let sp = dso::span(
            m.values.len() as u64,
            tb.snap_samplerate,
            self.vp.scale,
            self.vp.x_offset,
            self.vp.trig_hoff,
            rect.w,
        );
        let scale = (rect.h as f64 * math::MATH_SCALE * 1000.0 / m.vdial() as f64) as f32;
        let shape = if spp < dso::ENVELOPE_THRESHOLD {
            dso::math_shape(
                &g,
                &m.values[sp.start.max(0) as usize..],
                &sp,
                zero_y,
                scale,
                0,
            )
        } else {
            let sec = math::envelope_section(&m.levels, sp.start as u64, sp.end as u64, spp as f32);
            dso::math_envelope_shape(&g, sec, &sp, zero_y, scale, 0)
        };
        match shape {
            Some(TraceShape::Polyline { points, .. }) => {
                let pts: Vec<Pos2> = points.into_iter().map(|(x, y)| o(x, y)).collect();
                p.add(egui::Shape::line(pts, Stroke::new(1.0, RED)));
            }
            Some(TraceShape::Envelope(rects)) => {
                let mut mesh = egui::Mesh::default();
                for (x, t, w, h) in rects {
                    let (y0, y1) = if h >= 0.0 { (t, t + h) } else { (t + h, t) };
                    mesh.add_colored_rect(Rect::from_min_max(o(x, y0), o(x + w, y1)), RED);
                }
                p.add(egui::Shape::mesh(mesh));
            }
            _ => {}
        }
    }

    /// The math trace's dial (click or wheel on its right half) and the
    /// zero label to drag.
    pub(crate) fn math_header(
        &mut self,
        ui: &mut Ui,
        s: &Session,
        header: Rect,
        area: Rect,
        rect: IRect,
        math_y: Option<i32>,
    ) -> bool {
        let _ = s;
        let (Some(y), true) = (math_y, self.dso.math.enabled) else {
            return false;
        };
        let m = &mut self.dso.math;
        let zero_y = (m.zero_vrate * rect.h as f64 + rect.y as f64) as i32;
        let hr = dso::header_rects(y, zero_y, HEADER_WIDTH as i32);
        let ho = |r: dso::FRect| {
            Rect::from_min_size(pos2(header.left() + r.x, area.top() + r.y), vec2(r.w, r.h))
        };
        let mut changed = false;
        let dial = ho(hr.vdial);
        let resp = ui.interact(
            dial,
            ui.make_persistent_id("math-dial"),
            egui::Sense::click(),
        );
        let step = |m: &mut MathState, up: bool| {
            if up && m.sel + 1 < m.dial_values.len() {
                m.sel += 1;
            } else if !up && m.sel > 0 {
                m.sel -= 1;
            }
        };
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos()
            && p.x > dial.center().x
        {
            step(m, p.y > dial.center().y);
            changed = true;
        }
        if resp.hovered() {
            let dy = ui.input(wheel_angle).1 as f64 / 120.0;
            if dy > 0.5 {
                step(m, false);
                changed = true;
            } else if dy < -0.5 {
                step(m, true);
                changed = true;
            }
        }
        let label = ho(hr.label);
        let resp = ui.interact(
            label,
            ui.make_persistent_id("math-zero"),
            egui::Sense::drag(),
        );
        if resp.drag_started() {
            m.drag_zero = resp.interact_pointer_pos().map(|p| (p.y, m.zero_vrate));
        }
        if resp.dragged()
            && let (Some((y0, r0)), Some(p)) = (m.drag_zero, resp.interact_pointer_pos())
        {
            // set_zero_vpos: the ratio within the grid
            let pos = (r0 * rect.h as f64) as i32 + (p.y - y0) as i32;
            m.zero_vrate = pos.clamp(0, rect.h) as f64 / rect.h.max(1) as f64;
            changed = true;
        }
        if resp.drag_stopped() {
            m.drag_zero = None;
        }
        changed
    }

    pub(crate) fn paint_math_header(
        &self,
        p: &egui::Painter,
        header: Rect,
        area: Rect,
        rect: IRect,
        math_y: Option<i32>,
    ) {
        let m = &self.dso.math;
        let (Some(y), true) = (math_y, m.enabled) else {
            return;
        };
        let zero_y = (m.zero_vrate * rect.h as f64 + rect.y as f64) as i32;
        let hr = dso::header_rects(y, zero_y, HEADER_WIDTH as i32);
        let ho = |r: dso::FRect| {
            Rect::from_min_size(pos2(header.left() + r.x, area.top() + r.y), vec2(r.w, r.h))
        };
        let font = egui::FontId::proportional(12.0);
        // the dial
        let dial = ho(hr.vdial);
        let (ctr, r) = (dial.center(), dial.width() / 2.0);
        let arc: Vec<Pos2> = (0..=32)
            .map(|i| {
                let a = (90.0 - 180.0 * i as f32 / 32.0).to_radians();
                pos2(ctr.x + r * a.cos(), ctr.y - r * a.sin())
            })
            .collect();
        let st = Stroke::new(1.0, RED);
        p.add(egui::Shape::line(arc, st));
        let count = m.dial_values.len().max(1);
        for i in 0..count {
            let (dx, dy) = dso::dial_direction(i as f64 / (count - 1).max(1) as f64);
            p.line_segment(
                [
                    pos2(ctr.x + dx * (r + 3.0), ctr.y + dy * (r + 3.0)),
                    pos2(ctr.x + dx * (r + 8.0), ctr.y + dy * (r + 8.0)),
                ],
                st,
            );
        }
        let (dx, dy) = dso::dial_direction(m.sel as f64 / (count - 1).max(1) as f64);
        p.circle_filled(ctr, 3.0, RED);
        p.line_segment(
            [ctr, pos2(ctr.x + dx * (r - 3.0), ctr.y + dy * (r - 3.0))],
            Stroke::new(2.0, RED),
        );
        // dslDial's text: whole units of the operator's unit
        let mut v = m.vdial() * m.factor.max(1);
        let mut unit = 0;
        while v / 1000 >= 1 && unit < 2 {
            v /= 1000;
            unit += 1;
        }
        p.text(
            pos2(header.left() + HEADER_WIDTH / 2.0, dial.top() - 12.0),
            egui::Align2::CENTER_CENTER,
            format!("{v}{}/div", m.ty.unit(unit)),
            font.clone(),
            Color32::from_gray(220),
        );
        // the "M" label at the zero line
        let l = ho(hr.label);
        p.add(egui::Shape::convex_polygon(
            vec![
                l.left_top(),
                l.right_top(),
                pos2(header.right(), area.top() + zero_y as f32),
                l.right_bottom(),
                l.left_bottom(),
            ],
            RED,
            Stroke::NONE,
        ));
        p.text(
            l.center(),
            egui::Align2::CENTER_CENTER,
            "M",
            font,
            Color32::WHITE,
        );
    }
}
