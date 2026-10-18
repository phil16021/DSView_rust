//! The time view in analog (DAQ) mode (`AnalogSignal` in `pv/view`): one
//! row per channel with its value rule, the trace (polyline or envelope,
//! following the ring of a loop capture), the zero line to drag in the
//! header, and the value under the mouse.

use dsview_core::{channel_type, key};
use dsview_session::Session;
use dsview_session::session::Signal;
use dsview_viewmodel::analog::{self, Frame, Geom, Shape};
use dsview_viewmodel::dso;
use dsview_viewmodel::input::{self, Mods, WheelAction};
use dsview_viewmodel::layout::{self, LayoutParams, Slot, TraceKind};
use dsview_viewmodel::viewport::Timebase;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Ui, pos2, vec2};

use crate::timeview::{HEADER_WIDTH, RULER_HEIGHT, TimeView, timebase, wheel_angle};

/// An analog channel as the frame sees it.
#[derive(Clone, Debug)]
struct Row {
    sig: Signal,
    pos: usize,
    zero_offset: u16,
    hw_offset: i32,
    map_min: f64,
    map_max: f64,
    unit: String,
    geom: Geom,
    colour: Color32,
}

#[derive(Default)]
pub struct AnalogState {
    started_seq: u64,
    /// Dragging the zero line of channel `index`: (index, press y, zero y).
    drag_zero: Option<(u16, f32, i32)>,
    pan_last: Option<f32>,
    /// The value under the mouse: (channel, sample, text).
    pub hover: Option<(u16, u64, String)>,
}

impl AnalogState {
    pub fn new() -> AnalogState {
        AnalogState {
            started_seq: u64::MAX,
            ..AnalogState::default()
        }
    }
}

fn rows(s: &Session, area_h: i32, v_scroll: f32) -> Vec<Row> {
    let chans = s.channels();
    let signals: Vec<Signal> = s
        .state()
        .signals
        .iter()
        .filter(|x| x.ty == channel_type::ANALOG)
        .cloned()
        .collect();
    let bits = s
        .config(key::UNIT_BITS, None)
        .map_or(dso::DEFAULT_BITS, |v| v.get_byte());
    let ref_min = s.config(key::REF_MIN, None).map(|v| v.get_u32());
    let ref_max = s.config(key::REF_MAX, None).map(|v| v.get_u32());
    let slots: Vec<Slot> = signals
        .iter()
        .map(|x| Slot {
            kind: TraceKind::Analog,
            rows: 1,
            enabled: true,
            view_index: x.view_index,
        })
        .collect();
    let l = layout::layout(
        &slots,
        &LayoutParams {
            viewport_height: area_h as f64,
            time_pane_height: area_h as f64,
            dividers: true,
            font_height: 14.0,
            max_height_value: None,
            height_factor: 1.0,
            dso_split: false,
            logic_mode: false,
            dso_mode_overlap: None,
        },
    );
    let mut out = Vec::new();
    for p in &l.placed {
        let sig = signals[p.slot].clone();
        let Some(pos) = chans
            .iter()
            .position(|c| c.index == sig.index && c.ty == channel_type::ANALOG)
        else {
            continue;
        };
        let c = &chans[pos];
        let get = |k: i32| s.config(k, Some(pos));
        let y = (p.v_offset - v_scroll as f64) as i32;
        let [r, g, b] = sig
            .colour
            .unwrap_or(dso::SIGNAL_COLOURS[sig.index as usize % dso::SIGNAL_COLOURS.len()]);
        out.push(Row {
            pos,
            zero_offset: get(key::PROBE_OFFSET).map_or(c.offset, |v| v.get_u16()),
            hw_offset: get(key::PROBE_HW_OFFSET).map_or(c.hw_offset, |v| v.get_u16()) as i32,
            map_min: get(key::PROBE_MAP_MIN).map_or(-1.0, |v| v.get_f64()),
            map_max: get(key::PROBE_MAP_MAX).map_or(1.0, |v| v.get_f64()),
            unit: get(key::PROBE_MAP_UNIT)
                .and_then(|v| v.get_str().map(str::to_owned))
                .unwrap_or_default(),
            geom: Geom::new(y, p.total_height, ref_min, ref_max, bits),
            colour: Color32::from_rgb(r, g, b),
            sig,
        });
    }
    out
}

impl TimeView {
    /// The time view in analog mode; true when something changed.
    pub fn show_analog(&mut self, ui: &mut Ui, s: &Session) -> bool {
        let tb = timebase(s);
        let full = ui.max_rect();
        let header = Rect::from_min_max(full.min, pos2(full.left() + HEADER_WIDTH, full.bottom()));
        let ruler_rect = Rect::from_min_max(
            pos2(header.right(), full.top()),
            pos2(full.right(), full.top() + RULER_HEIGHT),
        );
        let area = Rect::from_min_max(pos2(header.right(), ruler_rect.bottom()), full.max);
        let width = area.width().max(1.0) as i32;
        if self.vp.width != width {
            self.vp.width = width;
        }
        self.vp.update_limits(&tb);
        // capture_init(): a new capture shows all of it
        let started = s.state().started_seq;
        if started != self.analog.started_seq {
            self.analog.started_seq = started;
            let max = self.vp.max_scale;
            self.vp.set_scale_offset(max, 0, &tb);
        }
        let rows = rows(s, area.height() as i32, 0.0);
        let frame = Frame {
            samplerate: tb.snap_samplerate,
            scale: self.vp.scale,
            x_offset: self.vp.x_offset,
            width,
            sample_limit: tb.sample_limit,
        };

        let mut changed = self.analog_input(ui, s, &tb, area, &rows, &frame);
        changed |= self.analog_header(ui, s, header, area, &rows);
        changed |= self.ruler_input(ui, s, ruler_rect, area, &tb);

        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, Color32::from_gray(24));
        painter.rect_filled(header, 0.0, Color32::from_gray(34));
        painter.rect_filled(ruler_rect, 0.0, Color32::from_gray(30));
        let tp = ui.painter_at(area);
        self.paint_analog(&tp, s, area, &rows, &frame);
        self.paint_ruler(&painter, ruler_rect, s, &tb);
        self.paint_cursors(&painter, s, ruler_rect, area, &tb);
        self.paint_analog_header(&painter, header, area, &rows);
        changed
    }

    fn analog_input(
        &mut self,
        ui: &mut Ui,
        s: &Session,
        tb: &Timebase,
        area: Rect,
        rows: &[Row],
        frame: &Frame,
    ) -> bool {
        let resp = ui.interact(
            area,
            ui.make_persistent_id("analog-view"),
            egui::Sense::click_and_drag(),
        );
        let mut changed = false;
        let (mods, angle) = ui.input(|i| {
            (
                Mods {
                    ctrl: i.modifiers.command,
                    shift: i.modifiers.shift,
                },
                wheel_angle(i),
            )
        });
        if let Some(steps) = self.pinch.feed(&ui.input(crate::timeview::zoom_factors))
            && let Some(p) = resp.hover_pos()
            && self.vp.zoom(steps, (p.x - area.left()) as i32, tb)
        {
            self.pinch.applied();
            changed = true;
        }
        if let Some(p) = resp.hover_pos()
            && (angle.0 != 0 || angle.1 != 0)
        {
            let x = (p.x - area.left()) as i32;
            match input::wheel(
                angle.0,
                angle.1,
                x,
                mods,
                false,
                self.vertical_scroll_is_zoom,
            ) {
                WheelAction::Zoom { steps, x } => changed |= self.vp.zoom(steps, x, tb),
                WheelAction::Pan { dx } => {
                    changed |=
                        self.vp
                            .set_scale_offset(self.vp.scale, self.vp.x_offset + dx as i64, tb)
                }
                _ => {}
            }
        }
        if resp.drag_started() {
            self.analog.pan_last = resp.interact_pointer_pos().map(|p| p.x);
        }
        if resp.dragged()
            && let (Some(last), Some(p)) = (self.analog.pan_last, resp.interact_pointer_pos())
        {
            let dx = (last - p.x) as i64;
            if dx != 0 {
                changed |= self
                    .vp
                    .set_scale_offset(self.vp.scale, self.vp.x_offset + dx, tb);
                self.analog.pan_last = Some(p.x);
            }
        }
        if resp.drag_stopped() {
            self.analog.pan_last = None;
        }
        // the value under the mouse while stopped (`AnalogSignal::measure`)
        self.analog.hover = None;
        if let Some(p) = resp.hover_pos()
            && !s.is_working()
        {
            let (x, y) = (p.x - area.left(), p.y - area.top());
            let store = s.data().analog.lock().unwrap_or_else(|e| e.into_inner());
            let row = rows.iter().find(|r| {
                let (t, b) = r.geom.top_bottom();
                y >= t && y <= b
            });
            if let Some(r) = row
                && let Ok(order) = usize::try_from(store.ch_order(r.sig.index as i32))
            {
                let index = analog::hover_index(x as f64, frame);
                if let Some((v, _, _)) = analog::point(
                    &store,
                    order,
                    index,
                    &r.geom,
                    r.zero_offset,
                    r.hw_offset,
                    frame,
                ) {
                    let text = analog::value_text(
                        &r.geom,
                        (r.hw_offset - v as i32) as f64,
                        2,
                        false,
                        r.map_min,
                        r.map_max,
                        &r.unit,
                    );
                    self.analog.hover = Some((r.sig.index, index, text));
                }
            }
        }
        changed
    }

    fn paint_analog(
        &self,
        p: &egui::Painter,
        s: &Session,
        area: Rect,
        rows: &[Row],
        frame: &Frame,
    ) {
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let font = self.font(11.0);
        let fore = Color32::from_gray(200);
        let right = frame.width as f32 - 1.0;
        let store = s.data().analog.lock().unwrap_or_else(|e| e.into_inner());
        for r in rows {
            // paint_back: the rule at both edges
            let st = Stroke::new(1.0, fore);
            for (y, major, label) in
                analog::rule(&r.geom, r.zero_offset, r.map_min, r.map_max, &r.unit)
            {
                let len = if major { 10.0 } else { 5.0 };
                p.line_segment([o(0.0, y), o(len, y)], st);
                p.line_segment([o(right, y), o(right - len, y)], st);
                if let Some(t) = label {
                    p.text(
                        o(15.0, y),
                        egui::Align2::LEFT_CENTER,
                        &t,
                        font.clone(),
                        fore,
                    );
                    p.text(
                        o(right - 15.0, y),
                        egui::Align2::RIGHT_CENTER,
                        &t,
                        font.clone(),
                        fore,
                    );
                }
            }
            // paint_mid
            if store.has_enabled_channel(r.sig.index as i32)
                && let Ok(order) = usize::try_from(store.ch_order(r.sig.index as i32))
            {
                match analog::shape(&store, order, &r.geom, r.zero_offset, r.hw_offset, frame) {
                    Some(Shape::Polyline(pts)) => {
                        let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| o(x, y)).collect();
                        p.add(egui::Shape::line(pts, Stroke::new(1.0, r.colour)));
                    }
                    Some(Shape::Envelope(rects)) => {
                        let mut mesh = egui::Mesh::default();
                        for (x, y, w, h) in rects {
                            mesh.add_colored_rect(
                                Rect::from_min_size(o(x, y), vec2(w, h.max(1.0))),
                                r.colour,
                            );
                        }
                        p.add(egui::Shape::mesh(mesh));
                    }
                    None => {}
                }
            }
            // paint_fore: the zero line
            let zy = r.geom.zero_vpos(r.zero_offset) as f32;
            p.extend(egui::Shape::dotted_line(
                &[o(0.0, zy), o(right, zy)],
                Color32::from_rgba_unmultiplied(200, 200, 200, 100),
                3.0,
                0.8,
            ));
        }
        if let Some((index, sample, text)) = &self.analog.hover
            && let Some(r) = rows.iter().find(|r| r.sig.index == *index)
            && let Ok(order) = usize::try_from(store.ch_order(*index as i32))
            && let Some((_, x, y)) = analog::point(
                &store,
                order,
                *sample,
                &r.geom,
                r.zero_offset,
                r.hw_offset,
                frame,
            )
        {
            let (t, b) = r.geom.top_bottom();
            let text = if y <= t || y >= b {
                format!("{text}/out")
            } else {
                text.clone()
            };
            p.rect_stroke(
                Rect::from_center_size(o(x, y), vec2(3.0, 3.0)),
                0.0,
                Stroke::new(1.0, fore),
                egui::StrokeKind::Outside,
            );
            p.text(o(x + 5.0, y), egui::Align2::LEFT_CENTER, text, font, fore);
        }
    }

    fn analog_header(
        &mut self,
        ui: &mut Ui,
        s: &Session,
        header: Rect,
        area: Rect,
        rows: &[Row],
    ) -> bool {
        let mut changed = false;
        for r in rows {
            let zy = r.geom.zero_vpos(r.zero_offset);
            let hr = dso::header_rects(r.geom.y, zy, HEADER_WIDTH as i32);
            let l = hr.label;
            let rect =
                Rect::from_min_size(pos2(header.left() + l.x, area.top() + l.y), vec2(l.w, l.h));
            let resp = ui.interact(
                rect,
                ui.make_persistent_id(("analog-zero", r.sig.index)),
                egui::Sense::drag(),
            );
            if resp.drag_started() {
                self.analog.drag_zero = Some((
                    r.sig.index,
                    resp.interact_pointer_pos().map_or(0.0, |p| p.y),
                    zy,
                ));
            }
            if resp.dragged()
                && let Some((index, y0, zero0)) = self.analog.drag_zero
                && index == r.sig.index
                && let Some(p) = resp.interact_pointer_pos()
                && !s.is_working()
            {
                // set_zero_vpos (not while running)
                let ratio = r.geom.pos2ratio(zero0 + (p.y - y0) as i32);
                let zero = r.geom.ratio2value(ratio) as u16;
                s.set_config(
                    key::PROBE_OFFSET,
                    dsview_core::ConfValue::U16(zero),
                    Some(r.pos),
                );
                changed = true;
            }
            if resp.drag_stopped() {
                self.analog.drag_zero = None;
            }
        }
        changed
    }

    fn paint_analog_header(&self, p: &egui::Painter, header: Rect, area: Rect, rows: &[Row]) {
        let font = self.font(12.0);
        for r in rows {
            let zy = r.geom.zero_vpos(r.zero_offset);
            let hr = dso::header_rects(r.geom.y, zy, HEADER_WIDTH as i32);
            let ho = |f: dso::FRect| {
                Rect::from_min_size(pos2(header.left() + f.x, area.top() + f.y), vec2(f.w, f.h))
            };
            let cb = ho(hr.colour);
            p.rect_filled(cb, 0.0, r.colour);
            p.text(
                pos2(cb.right() + 6.0, cb.center().y),
                egui::Align2::LEFT_CENTER,
                &r.sig.name,
                font.clone(),
                Color32::from_gray(220),
            );
            let l = ho(hr.label);
            p.add(egui::Shape::convex_polygon(
                vec![
                    l.left_top(),
                    l.right_top(),
                    pos2(header.right(), area.top() + zy as f32),
                    l.right_bottom(),
                    l.left_bottom(),
                ],
                r.colour,
                Stroke::NONE,
            ));
            p.text(
                l.center(),
                egui::Align2::CENTER_CENTER,
                r.sig.index.to_string(),
                font.clone(),
                Color32::WHITE,
            );
        }
    }
}
