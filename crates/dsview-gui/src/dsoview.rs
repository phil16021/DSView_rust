//! The time view in oscilloscope mode (`DsoSignal` in `pv/view`): the
//! grid, the traces, zero lines and trigger levels, the channel controls in
//! the header (V/div dial, probe factor, enable, coupling), the hover value;
//! wheel zoom steps the timebase.


use dsview_core::{ConfValue, channel_type, key};
use dsview_session::Session;
use dsview_session::session::Signal;
use dsview_viewmodel::dso::{self, Geom, IRect, TraceShape, Volts, WheelAccumulator};
use dsview_viewmodel::layout::{self, LayoutParams, Slot, TraceKind};
use dsview_viewmodel::input::{self, Mods, WheelAction};
use dsview_viewmodel::viewport::Timebase;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Ui, pos2, vec2};

use crate::timeview::{HEADER_WIDTH, RULER_HEIGHT, TimeView, timebase};

/// What the view keeps per DSO channel (index 0-3).
#[derive(Clone, Copy, Debug)]
struct Chan {
    /// `_stop_scale`: V/div changed after the capture stretches the trace.
    stop_scale: f32,
    /// `set_measure_voltage_factor`, `set_data_scale`: V/div and the
    /// trace's scale when the capture started.
    cap_vdiv: u64,
    data_scale: f32,
}

impl Default for Chan {
    fn default() -> Self {
        Chan {
            stop_scale: 1.0,
            cap_vdiv: 0,
            data_scale: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Pan { last_x: f32 },
    /// The zero line of channel `index` (the header label).
    Zero { index: u16, y0: f32, zero0: i32 },
    /// The trigger level of channel `index`.
    Trig { index: u16 },
}

#[derive(Default)]
pub struct DsoState {
    chans: [Chan; 4],
    zoom: WheelAccumulator,
    drag: Option<Drag>,
    /// `get_dso_trig_moved()`: the trigger level line shows while dragged.
    trig_moved: bool,
    started_seq: u64,
    /// `get_dso_split_channels()`: one band per channel.
    pub split: bool,
    /// The value under the mouse: channel, sample, code, text.
    pub hover: Option<(u16, u64, u8, String)>,
}

impl DsoState {
    pub fn new() -> DsoState {
        DsoState {
            zoom: WheelAccumulator::new(1.0),
            started_seq: u64::MAX,
            ..DsoState::default()
        }
    }
}

/// A DSO channel as the frame sees it.
#[derive(Clone, Debug)]
struct ChanView {
    sig: Signal,
    /// Its position in the device's channel list (for per-channel config).
    pos: usize,
    enabled: bool,
    vdiv: u64,
    factor: u64,
    coupling: u8,
    zero_offset: u16,
    hw_offset: i32,
    trig_value: u8,
    geom: Geom,
    /// The header row's centre (`get_y()`), in trace-area coordinates.
    y: i32,
    colour: Color32,
    /// The device's V/div list (the dial's entries).
    vdivs: std::sync::Arc<Vec<u64>>,
}

impl ChanView {
    fn zero_y(&self) -> i32 {
        self.geom.zero_vpos(self.zero_offset)
    }
}

fn colour(sig: &Signal) -> Color32 {
    let [r, g, b] = sig
        .colour
        .unwrap_or(dso::SIGNAL_COLOURS[sig.index as usize % dso::SIGNAL_COLOURS.len()]);
    Color32::from_rgb(r, g, b)
}

/// `QColor::darker()` (factor 200 in HSV value).
fn darker(c: Color32) -> Color32 {
    Color32::from_rgb(c.r() / 2, c.g() / 2, c.b() / 2)
}

fn vdiv_list(s: &Session) -> Vec<u64> {
    match s.config_list(key::PROBE_VDIV) {
        Some(ConfValue::U64s(v)) => v,
        Some(ConfValue::Dict(d)) => d
            .into_iter()
            .find_map(|(_, v)| match v {
                ConfValue::U64s(v) => Some(v),
                _ => None,
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn is_hardware_logic(s: &Session) -> bool {
    s.state()
        .device
        .as_ref()
        .is_some_and(|d| d.is_hardware() && d.driver == "DSLogic")
}

/// The DSO channels and their settings (`load_settings`).
fn channel_views(s: &Session, area_w: i32, area_h: i32, st: &DsoState) -> Vec<ChanView> {
    let chans = s.channels();
    let signals: Vec<Signal> = s
        .state()
        .signals
        .iter()
        .filter(|x| x.ty == channel_type::DSO)
        .cloned()
        .collect();
    let bits = s
        .config(key::UNIT_BITS, None)
        .map_or(dso::DEFAULT_BITS, |v| v.get_byte());
    let ref_min = s.config(key::REF_MIN, None).map(|v| v.get_u32());
    let ref_max = s.config(key::REF_MAX, None).map(|v| v.get_u32());
    let vdivs = std::sync::Arc::new(vdiv_list(s));
    // `View::signals_changed`: the header rows (and the bands when split)
    let enabled = |sig: &Signal| chans.iter().any(|c| c.index == sig.index && c.ty == channel_type::DSO && c.enabled);
    let slots: Vec<Slot> = signals
        .iter()
        .map(|x| Slot {
            kind: TraceKind::Dso,
            rows: 1,
            enabled: enabled(x),
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
            dso_split: st.split,
            logic_mode: false,
            dso_mode_overlap: Some(0.0),
        },
    );
    let mut out = Vec::new();
    for p in &l.placed {
        let sig = signals[p.slot].clone();
        let (y, total) = (p.v_offset as i32, p.total_height);
        let rect = dso::view_rect(area_w, area_h, st.split.then_some((y, total)), 0);
        let Some(pos) = chans
            .iter()
            .position(|c| c.index == sig.index && c.ty == channel_type::DSO)
        else {
            continue;
        };
        let c = &chans[pos];
        let get = |k: i32| s.config(k, Some(pos));
        let mut geom = Geom::new(rect, ref_min, ref_max, bits);
        let stop = st.chans.get(sig.index as usize).map_or(1.0, |c| c.stop_scale);
        geom.set_scale(rect.h, stop);
        out.push(ChanView {
            pos,
            enabled: c.enabled,
            vdiv: get(key::PROBE_VDIV).map_or(c.vdiv, |v| v.get_u64()),
            factor: get(key::PROBE_FACTOR).map_or(1, |v| v.get_u64()).max(1),
            coupling: get(key::PROBE_COUPLING).map_or(c.coupling, |v| v.get_byte()),
            zero_offset: get(key::PROBE_OFFSET).map_or(c.offset, |v| v.get_u16()),
            hw_offset: get(key::PROBE_HW_OFFSET).map_or(c.hw_offset, |v| v.get_u16()) as i32,
            trig_value: get(key::TRIGGER_VALUE).map_or(c.trig_value, |v| v.get_byte()),
            geom,
            y,
            colour: colour(&sig),
            vdivs: vdivs.clone(),
            sig,
        });
    }
    out
}

/// `go_vDialPre` / `go_vDialNext`: one step of the V/div dial; while
/// stopped the shown trace stretches with it.
fn step_vdiv(s: &Session, st: &mut DsoState, c: &ChanView, up: bool) -> bool {
    let list = &c.vdivs;
    let Some(i) = list.iter().position(|&v| v == c.vdiv) else {
        return false;
    };
    let next = if up { i + 1 } else { i.wrapping_sub(1) };
    let Some(&v) = list.get(next) else {
        return false;
    };
    s.set_config(key::PROBE_VDIV, ConfValue::U64(v), Some(c.pos));
    if !s.is_working()
        && let Some(ch) = st.chans.get_mut(c.sig.index as usize)
    {
        ch.stop_scale *= (c.vdiv as f64 / v as f64) as f32;
    }
    s.set_config(key::PROBE_OFFSET, ConfValue::U16(c.zero_offset), Some(c.pos));
    true
}

impl TimeView {
    /// The time view in DSO mode; true when something changed.
    pub fn show_dso(&mut self, ui: &mut Ui, s: &Session) -> bool {
        let mut tb = timebase(s);
        let tbase_ns = s.config_u64(key::TIMEBASE).unwrap_or(0);
        tb.dso_view_time = Some(tbase_ns as f64 * dso::HDIVS as f64 / 1e9);
        let full = ui.max_rect();
        let header = Rect::from_min_max(full.min, pos2(full.left() + HEADER_WIDTH, full.bottom()));
        let ruler_rect = Rect::from_min_max(
            pos2(header.right(), full.top()),
            pos2(full.right(), full.top() + RULER_HEIGHT),
        );
        let area = Rect::from_min_max(pos2(header.right(), ruler_rect.bottom()), full.max);
        let (aw, ah) = (area.width() as i32, area.height() as i32);
        let rect = dso::view_rect(aw, ah, None, 0);
        if self.vp.width != rect.w.max(1) {
            self.vp.width = rect.w.max(1);
        }
        self.vp.update_limits(&tb);

        // a new capture: its scale and V/div convert codes to volts
        let started = s.state().started_seq;
        let mut chans = channel_views(s, aw, ah, &self.dso);
        if started != self.dso.started_seq {
            self.dso.started_seq = started;
            for c in &chans {
                if let Some(ch) = self.dso.chans.get_mut(c.sig.index as usize) {
                    *ch = Chan {
                        stop_scale: 1.0,
                        cap_vdiv: c.vdiv,
                        data_scale: (c.geom.rect.h as f64 / (c.geom.ref_max - c.geom.ref_min)) as f32,
                    };
                }
            }
            chans = channel_views(s, aw, ah, &self.dso);
        }

        // the trigger's sub-sample offset (`paint_prepare`)
        self.update_trig_hoff(s, &chans);

        let mut changed = self.dso_input(ui, s, &tb, area, rect, &chans);
        changed |= self.dso_header(ui, s, header, area, &chans);

        // paint
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, Color32::from_gray(24));
        painter.rect_filled(header, 0.0, Color32::from_gray(34));
        painter.rect_filled(ruler_rect, 0.0, Color32::from_gray(30));
        let tpainter = ui.painter_at(area);
        if self.dso.split {
            for c in &chans {
                self.paint_grid(&tpainter, area, c.geom.rect, &tb);
            }
        } else {
            self.paint_grid(&tpainter, area, rect, &tb);
        }
        self.paint_dso_traces(&tpainter, s, area, rect, &tb, &chans);
        self.paint_dso_fore(&tpainter, area, rect, &tb, &chans);
        self.paint_ruler(&painter, ruler_rect, s, &tb);
        self.paint_cursors(&painter, ruler_rect, area, &tb);
        self.paint_dso_header(&painter, header, area, &chans, ui.input(|i| i.pointer.hover_pos()));
        changed
    }

    fn update_trig_hoff(&mut self, s: &Session, chans: &[ChanView]) {
        let (flag, ch) = {
            let st = s.state();
            (st.trig_flag, st.trig_ch)
        };
        if !flag {
            self.vp.trig_hoff = 0.0;
            return;
        }
        let Some(c) = chans.iter().find(|c| c.sig.index == ch as u16) else {
            return;
        };
        let slope = s
            .config(key::TRIGGER_SLOPE, None)
            .map_or(dso::TRIGGER_RISING, |v| v.get_byte());
        let trig_index = s.state().trigger_pos as i64;
        let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        let n = store.sample_count() as usize;
        if let Some(buf) = store.get_samples(0, c.sig.index as i32)
            && let Some(h) = dso::trig_hoff(&buf[..n.min(buf.len())], trig_index, c.trig_value, slope)
        {
            self.vp.trig_hoff = h;
        }
    }

    /// Wheel: the timebase knob; drag: pan, or move a trigger level.
    fn dso_input(
        &mut self,
        ui: &mut Ui,
        s: &Session,
        tb: &Timebase,
        area: Rect,
        rect: IRect,
        chans: &[ChanView],
    ) -> bool {
        let id = ui.make_persistent_id("dso-view");
        let resp = ui.interact(area, id, egui::Sense::click_and_drag());
        let mut changed = false;
        let (mods, scroll, now) = ui.input(|i| {
            (
                Mods {
                    ctrl: i.modifiers.command,
                    shift: i.modifiers.shift,
                },
                crate::timeview::wheel_angle(i),
                (i.time * 1000.0) as i64,
            )
        });
        let hover = resp.hover_pos();
        let local = |p: Pos2| (p.x - area.left(), p.y - area.top());

        if let Some(p) = hover
            && scroll.1 != 0
        {
            let x = (p.x - area.left()) as i32;
            if let WheelAction::Zoom { steps, x } =
                input::wheel(0, scroll.1, x, mods, false, self.vertical_scroll_is_zoom)
            {
                changed |= self.dso_zoom(s, steps, x, now);
            }
        }

        // press: on a trigger square, else pan
        if resp.drag_started()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let (x, y) = local(p);
            let hw_logic = is_hardware_logic(s);
            let on_trig = chans.iter().filter(|c| c.enabled).find(|c| {
                let (tx, ty, w, h) = dso::trig_rect(
                    &c.geom,
                    rect.w,
                    c.geom.trig_vrate(c.trig_value, c.zero_offset, hw_logic),
                );
                x >= tx && x <= tx + w && y >= ty && y <= ty + h
            });
            self.dso.drag = Some(match on_trig {
                Some(c) => Drag::Trig { index: c.sig.index },
                None => Drag::Pan { last_x: p.x },
            });
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            match self.dso.drag {
                Some(Drag::Pan { last_x }) => {
                    let dx = (last_x - p.x) as i64;
                    if dx != 0 {
                        changed |= self.vp.set_scale_offset(self.vp.scale, self.vp.x_offset + dx, tb);
                        self.dso.drag = Some(Drag::Pan { last_x: p.x });
                    }
                }
                Some(Drag::Trig { index }) => {
                    if let Some(c) = chans.iter().find(|c| c.sig.index == index) {
                        let ratio = c.geom.pos2ratio(local(p).1 as i32);
                        let v = c.geom.trig_value_for(ratio, c.zero_offset, is_hardware_logic(s));
                        s.set_config(key::TRIGGER_VALUE, ConfValue::Byte(v), Some(c.pos));
                        self.dso.trig_moved = true;
                        changed = true;
                    }
                }
                _ => {}
            }
        }
        if resp.drag_stopped() {
            self.dso.drag = None;
            self.dso.trig_moved = false;
        }

        // the value under the mouse while stopped (`DsoSignal::measure`)
        self.dso.hover = None;
        if let Some(p) = hover
            && !s.is_working()
        {
            let (x, y) = local(p);
            let store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
            if store.sample_count() > 0 {
                let index = self.vp.pixel_to_index(x as f64, tb);
                for c in chans
                    .iter()
                    .filter(|c| c.enabled && c.geom.rect.contains(x as f64, y as f64))
                {
                    if index >= store.sample_count() || !store.has_data(c.sig.index as i32) {
                        continue;
                    }
                    let Some(v) = store
                        .get_samples(index, c.sig.index as i32)
                        .and_then(|b| b.first().copied())
                    else {
                        continue;
                    };
                    let volts = self.volts(c);
                    let text = volts.text((c.hw_offset - v as i32) as f64, 2, false);
                    self.dso.hover = Some((c.sig.index, index, v, text));
                    break;
                }
            }
        }
        changed
    }

    /// `View::zoom` in DSO mode: whole knob steps of the timebase list.
    fn dso_zoom(&mut self, s: &Session, steps: f64, px: i32, now_ms: i64) -> bool {
        if s.is_working() && s.state().instant {
            return false;
        }
        let knob = self.dso.zoom.take(steps, now_ms);
        if knob == 0 {
            return false;
        }
        let (list, mut i) = s.duration_selector();
        for _ in 0..knob.abs() {
            // zooming in is a shorter time per division: further down
            if knob > 0 && i + 1 < list.len() {
                i += 1;
            } else if knob < 0 && i > 0 {
                i -= 1;
            }
        }
        s.select_duration(i);
        let mut tb = timebase(s);
        tb.dso_view_time = Some(s.config_u64(key::TIMEBASE).unwrap_or(0) as f64 * dso::HDIVS as f64 / 1e9);
        self.vp.pre_scale = self.vp.scale;
        self.vp.pre_offset = self.vp.x_offset;
        if let Some(t) = tb.dso_view_time {
            self.vp.scale = t / self.vp.width.max(1) as f64;
        }
        self.vp.zoom_offset(px, &tb);
        true
    }

    fn volts(&self, c: &ChanView) -> Volts {
        let ch = self.dso.chans.get(c.sig.index as usize).copied().unwrap_or_default();
        Volts {
            vdiv_mv: if ch.cap_vdiv == 0 { c.vdiv } else { ch.cap_vdiv },
            data_scale: if ch.data_scale == 0.0 { c.geom.scale } else { ch.data_scale },
            factor: c.factor,
            height: c.geom.rect.h,
        }
    }

    fn paint_grid(&self, p: &egui::Painter, area: Rect, rect: IRect, tb: &Timebase) {
        let g = dso::grid(
            rect,
            0,
            rect.w,
            tb.sample_limit,
            tb.snap_samplerate,
            self.vp.scale,
            self.vp.x_offset,
        );
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let fore = Color32::from_gray(220);
        let back_a = Color32::from_rgba_unmultiplied(220, 220, 220, 100);
        let f = g.frame;
        let fr = Rect::from_min_size(o(f.x as f32, f.y as f32), vec2(f.w as f32, f.h as f32));
        p.rect_filled(fr, 0.0, Color32::from_gray(12));
        p.rect_stroke(fr, 0.0, Stroke::new(1.0, back_a), egui::StrokeKind::Inside);
        let dashed = Stroke::new(1.0, Color32::from_rgba_unmultiplied(220, 220, 220, 60));
        for &(x0, y0, x1, y1) in &g.dividers {
            p.extend(egui::Shape::dashed_line(&[o(x0, y0), o(x1, y1)], dashed, 4.0, 2.0));
        }
        for &(x0, y0, x1, y1) in &g.ticks {
            p.line_segment([o(x0, y0), o(x1, y1)], dashed);
        }
        let st = Stroke::new(1.0, fore);
        for &(x0, y0, x1, y1) in &g.rails {
            p.line_segment([o(x0, y0), o(x1, y1)], st);
        }
        for b in &g.brackets {
            p.add(egui::Shape::line(b.iter().map(|&(x, y)| o(x, y)).collect(), st));
        }
        let (x, y, w, h) = g.shown;
        p.rect_filled(Rect::from_min_size(o(x, y), vec2(w, h)), 0.0, fore);
    }

    fn paint_dso_traces(
        &self,
        p: &egui::Painter,
        s: &Session,
        area: Rect,
        rect: IRect,
        tb: &Timebase,
        chans: &[ChanView],
    ) {
        let mut store = s.data().dso.lock().unwrap_or_else(|e| e.into_inner());
        let n = store.sample_count();
        if n == 0 {
            return;
        }
        let sp = dso::span(
            n,
            tb.snap_samplerate,
            self.vp.scale,
            self.vp.x_offset,
            self.vp.trig_hoff,
            rect.w,
        );
        let envelope = sp.samples_per_pixel >= dso::ENVELOPE_THRESHOLD;
        store.enable_envelope(envelope);
        let num_channels = store.channel_num().max(1) as usize;
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        for c in chans.iter().filter(|c| c.enabled) {
            if !store.has_data(c.sig.index as i32) {
                continue;
            }
            let zero_y = c.zero_y();
            let shape = if envelope {
                let e = store.get_envelope_section(
                    sp.start as u64,
                    sp.end as u64,
                    sp.samples_per_pixel as f32,
                    c.sig.index as usize % num_channels,
                );
                dso::envelope_shape(&c.geom, &e, &sp, zero_y, c.hw_offset, 0)
            } else {
                store
                    .get_samples(sp.start as u64, c.sig.index as i32)
                    .and_then(|b| dso::trace_shape(&c.geom, b, &sp, zero_y, c.hw_offset, 0, 1.0))
            };
            let col = c.colour;
            match shape {
                Some(TraceShape::Bars(rects)) => {
                    let mut mesh = egui::Mesh::default();
                    for r in rects {
                        mesh.add_colored_rect(
                            Rect::from_min_size(o(r.x as f32, r.y as f32), vec2(r.w as f32, r.h as f32)),
                            col,
                        );
                    }
                    p.add(egui::Shape::mesh(mesh));
                }
                Some(TraceShape::Envelope(rects)) => {
                    let mut mesh = egui::Mesh::default();
                    for (x, t, w, h) in rects {
                        let (y0, y1) = if h >= 0.0 { (t, t + h) } else { (t + h, t) };
                        mesh.add_colored_rect(Rect::from_min_max(o(x, y0), o(x + w, y1)), col);
                    }
                    p.add(egui::Shape::mesh(mesh));
                }
                Some(TraceShape::Polyline { points, dots, dot }) => {
                    let pts: Vec<Pos2> = points.iter().map(|&(x, y)| o(x, y)).collect();
                    for &q in pts.iter().take(dots) {
                        p.circle_filled(q, dot as f32 / 2.0, col);
                    }
                    p.add(egui::Shape::line(pts, Stroke::new(1.0, col)));
                }
                None => {}
            }
        }
    }

    /// `paint_fore`: zero lines, trigger levels and the hover point.
    fn paint_dso_fore(&self, p: &egui::Painter, area: Rect, rect: IRect, tb: &Timebase, chans: &[ChanView]) {
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let font = egui::FontId::proportional(12.0);
        let fore = Color32::from_gray(220);
        for c in chans {
            let zy = c.zero_y() as f32;
            p.extend(egui::Shape::dotted_line(
                &[o(0.0, zy), o(rect.w as f32, zy)],
                c.colour,
                3.0,
                0.8,
            ));
            if !c.enabled {
                continue;
            }
            let vrate = c.geom.trig_vrate(c.trig_value, c.zero_offset, false);
            let (tx, ty, w, h) = dso::trig_rect(&c.geom, rect.w, vrate);
            let trigp = c.geom.ratio2pos(vrate) as f32;
            let pts = vec![
                o(rect.w as f32, trigp),
                o(tx, ty),
                o(tx + w, ty),
                o(tx + w, ty + h),
                o(tx, ty + h),
            ];
            p.add(egui::Shape::convex_polygon(pts, c.colour, Stroke::NONE));
            p.text(
                o(tx + w / 2.0, ty + h / 2.0),
                egui::Align2::CENTER_CENTER,
                "T",
                font.clone(),
                fore,
            );
            let volts = self.volts(c);
            let t = volts.text((zy - trigp) as f64, 2, true);
            p.text(o(rect.w as f32, trigp), egui::Align2::RIGHT_CENTER, t, font.clone(), fore);
            if self.dso.trig_moved
                && matches!(self.dso.drag, Some(Drag::Trig { index }) if index == c.sig.index)
            {
                p.extend(egui::Shape::dotted_line(
                    &[o(0.0, trigp), o(rect.w as f32 - 60.0, trigp)],
                    c.colour,
                    3.0,
                    0.8,
                ));
            }
        }
        // the hover value: the point (`paint_hover_measure`) and the value
        // (Qt's floating measure panel)
        if let Some((index, sample, value, text)) = &self.dso.hover
            && let Some(c) = chans.iter().find(|c| c.sig.index == *index)
        {
            let x = self.vp.index_to_pixel(*sample, true, tb);
            let (px, py) = dso::point(&c.geom, x, *value, c.zero_offset, c.hw_offset);
            let sz = dso::HOVER_POINT_SIZE as f32;
            let r = Rect::from_min_size(o(px - 1.0, py - 1.0), vec2(sz, sz));
            p.rect_filled(r, 0.0, Color32::from_gray(24));
            p.rect_stroke(r, 0.0, Stroke::new(1.0, fore), egui::StrokeKind::Outside);
            p.text(
                o(rect.w as f32 - 8.0, 8.0),
                egui::Align2::RIGHT_TOP,
                format!("{}: {text}", c.sig.name),
                font,
                c.colour,
            );
        }
    }

    /// The header: per channel the name, V/div dial, probe factor,
    /// enable and coupling, and the zero-line label to drag.
    fn dso_header(&mut self, ui: &mut Ui, s: &Session, header: Rect, area: Rect, chans: &[ChanView]) -> bool {
        let mut changed = false;
        let is_file = s.state().device.as_ref().is_some_and(|d| d.is_file());
        let ho = |r: dso::FRect| {
            Rect::from_min_size(
                pos2(header.left() + r.x, area.top() + r.y),
                vec2(r.w, r.h),
            )
        };
        for c in chans {
            let hr = dso::header_rects(c.y, c.zero_y(), HEADER_WIDTH as i32);
            let click = |ui: &mut Ui, r: dso::FRect, what: &str| {
                ui.interact(
                    ho(r),
                    ui.make_persistent_id(("dso", what, c.sig.index)),
                    egui::Sense::click(),
                )
                .clicked()
            };
            if click(ui, hr.enable, "en") && !is_file {
                let running = s.is_working();
                if running {
                    s.stop_capture();
                }
                s.set_config(key::PROBE_EN, ConfValue::Bool(!c.enabled), Some(c.pos));
                // update_hori_res(): the timebase is committed again
                let (_, i) = s.duration_selector();
                s.select_duration(i);
                s.reload_signals();
                if running {
                    s.start_capture(false);
                }
                changed = true;
                continue;
            }
            if !c.enabled {
                continue;
            }
            // the dial: the right half steps (lower half: next, upper: previous)
            let dial = ho(hr.vdial);
            let resp = ui.interact(
                dial,
                ui.make_persistent_id(("dso", "dial", c.sig.index)),
                egui::Sense::click(),
            );
            if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
                && p.x > dial.center().x
            {
                changed |= step_vdiv(s, &mut self.dso, c, p.y > dial.center().y);
            }
            if resp.hovered() {
                // mouse_wheel: shift = delta / 120
                let dy = ui.input(|i| crate::timeview::wheel_angle(i).1) as f64 / 120.0;
                if dy > 0.5 {
                    changed |= step_vdiv(s, &mut self.dso, c, false);
                } else if dy < -0.5 {
                    changed |= step_vdiv(s, &mut self.dso, c, true);
                }
            }
            if click(ui, hr.coupling, "acdc") && !is_file {
                s.set_config(
                    key::PROBE_COUPLING,
                    ConfValue::Byte((c.coupling + 1) % 2),
                    Some(c.pos),
                );
                changed = true;
            }
            for (r, f, name) in [(hr.x1, 1u64, "x1"), (hr.x10, 10, "x10"), (hr.x100, 100, "x100")] {
                if click(ui, r, name) && c.factor != f {
                    s.set_config(key::PROBE_FACTOR, ConfValue::U64(f), Some(c.pos));
                    changed = true;
                }
            }
            // the zero label: drag to move the zero line
            let label = ho(hr.label);
            let resp = ui.interact(
                label,
                ui.make_persistent_id(("dso", "zero", c.sig.index)),
                egui::Sense::drag(),
            );
            if resp.drag_started() {
                self.dso.drag = Some(Drag::Zero {
                    index: c.sig.index,
                    y0: resp.interact_pointer_pos().map_or(0.0, |p| p.y),
                    zero0: c.zero_y(),
                });
            }
            if resp.dragged()
                && let Some(Drag::Zero { index, y0, zero0 }) = self.dso.drag
                && index == c.sig.index
                && let Some(p) = resp.interact_pointer_pos()
            {
                // set_zero_vpos: the trigger level keeps its distance
                let hw_logic = is_hardware_logic(s);
                let trig_delta = c.geom.trig_vrate(c.trig_value, c.zero_offset, hw_logic)
                    - c.geom.zero_ratio(c.zero_offset);
                let y = zero0 + (p.y - y0) as i32;
                let ratio = c.geom.pos2ratio(y);
                let zero = c.geom.zero_offset_for(ratio);
                s.set_config(key::PROBE_OFFSET, ConfValue::U16(zero), Some(c.pos));
                let tv = c.geom.trig_value_for(trig_delta + c.geom.zero_ratio(zero), zero, hw_logic);
                s.set_config(key::TRIGGER_VALUE, ConfValue::Byte(tv), Some(c.pos));
                changed = true;
            }
            if resp.drag_stopped() {
                self.dso.drag = None;
            }
        }
        changed
    }

    fn paint_dso_header(
        &self,
        p: &egui::Painter,
        header: Rect,
        area: Rect,
        chans: &[ChanView],
        pointer: Option<Pos2>,
    ) {
        let font = egui::FontId::proportional(12.0);
        let fore = Color32::from_gray(220);
        let fore_back = Color32::from_rgba_unmultiplied(220, 220, 220, 100);
        let ho = |r: dso::FRect| {
            Rect::from_min_size(
                pos2(header.left() + r.x, area.top() + r.y),
                vec2(r.w, r.h),
            )
        };
        let hovered = |r: Rect| pointer.is_some_and(|q| r.contains(q));
        for c in chans {
            let hr = dso::header_rects(c.y, c.zero_y(), HEADER_WIDTH as i32);
            let col = c.colour;
            // colour box with the name
            let cb = ho(hr.colour);
            p.rect_filled(cb, 0.0, if c.enabled { col } else { fore_back });
            p.text(
                cb.center(),
                egui::Align2::CENTER_CENTER,
                &c.sig.name,
                font.clone(),
                if c.enabled { Color32::WHITE } else { fore_back },
            );
            // the dial
            let dial = ho(hr.vdial);
            let ctr = dial.center();
            let r = dial.width() / 2.0;
            let arc: Vec<Pos2> = (0..=32)
                .map(|i| {
                    let a = (90.0 - 180.0 * i as f32 / 32.0).to_radians();
                    pos2(ctr.x + r * a.cos(), ctr.y - r * a.sin())
                })
                .collect();
            let st = Stroke::new(1.0, col);
            p.add(egui::Shape::line(arc, st));
            let (sel, count) = c.dial_pos();
            for i in 0..count {
                let (dx, dy) = dso::dial_direction(i as f64 / (count - 1).max(1) as f64);
                p.line_segment(
                    [pos2(ctr.x + dx * (r + 3.0), ctr.y + dy * (r + 3.0)), pos2(ctr.x + dx * (r + 8.0), ctr.y + dy * (r + 8.0))],
                    st,
                );
            }
            let (dx, dy) = dso::dial_direction(sel as f64 / (count - 1).max(1) as f64);
            p.circle_filled(ctr, 3.0, col);
            p.line_segment([ctr, pos2(ctr.x + dx * (r - 3.0), ctr.y + dy * (r - 3.0))], Stroke::new(2.0, col));
            p.text(
                pos2(header.left() + HEADER_WIDTH / 2.0, dial.top() - 12.0),
                egui::Align2::CENTER_CENTER,
                dso::vdiv_text(c.vdiv, c.factor),
                font.clone(),
                fore,
            );
            if hovered(dial) && pointer.is_some_and(|q| q.x > ctr.x) {
                let up = pointer.is_some_and(|q| q.y < ctr.y);
                p.text(
                    pos2(dial.right() + 4.0, ctr.y + if up { -r / 2.0 } else { r / 2.0 }),
                    egui::Align2::LEFT_CENTER,
                    if up { "-" } else { "+" },
                    font.clone(),
                    col,
                );
            }
            // buttons
            let button = |r: dso::FRect, on: bool, text: &str| {
                let rr = ho(r);
                let fill = if on {
                    if hovered(rr) { darker(col) } else { col }
                } else if hovered(rr) {
                    darker(col)
                } else {
                    fore_back
                };
                p.rect_filled(rr, 0.0, fill);
                p.text(rr.center(), egui::Align2::CENTER_CENTER, text, font.clone(), Color32::WHITE);
            };
            button(hr.enable, true, if c.enabled { "EN" } else { "DIS" });
            button(
                hr.coupling,
                c.enabled,
                match c.coupling {
                    2 => "GND",
                    0 => "DC",
                    _ => "AC",
                },
            );
            button(hr.x1, c.enabled && c.factor == 1, "x1");
            button(hr.x10, c.enabled && c.factor == 10, "x10");
            button(hr.x100, c.enabled && c.factor == 100, "x100");
            // the zero label
            if c.enabled {
                let l = ho(hr.label);
                let zy = area.top() + c.zero_y() as f32;
                p.add(egui::Shape::convex_polygon(
                    vec![l.left_top(), l.right_top(), pos2(header.right(), zy), l.right_bottom(), l.left_bottom()],
                    col,
                    Stroke::NONE,
                ));
                p.text(
                    l.center(),
                    egui::Align2::CENTER_CENTER,
                    c.sig.index.to_string(),
                    font.clone(),
                    Color32::WHITE,
                );
            }
        }
    }
}

impl ChanView {
    /// The dial's position: (selected entry, entries).
    fn dial_pos(&self) -> (usize, usize) {
        let sel = self.vdivs.iter().position(|&v| v == self.vdiv).unwrap_or(0);
        (sel, self.vdivs.len().max(1))
    }
}
