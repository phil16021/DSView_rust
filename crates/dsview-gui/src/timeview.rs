//! The time view: header, ruler and logic traces (`pv/view/view.cpp`,
//! `header.cpp`, `ruler.cpp`, `viewport.cpp`), painted from
//! `dsview-viewmodel` with the session's data.

use std::time::Instant;

use dsview_core::channel_type;
use dsview_session::Session;
use std::sync::Arc;

use dsview_session::session::{DecodeTrace, Signal, TraceRef, trig};
use dsview_viewmodel::cursors::{Cursors, cursor_hsva};
use dsview_viewmodel::decode::{self as vdec, AnnShape};
use dsview_viewmodel::input::{self, DRAG_TIMER_INTERVAL_MS, Kinetic, Mods, WheelAction};
use dsview_viewmodel::layout::{self, LayoutParams, Slot, TraceKind};
use dsview_viewmodel::logic::{EdgeBuffers, logic_shape};
use dsview_viewmodel::measure::{self, MouseMeasure};
use dsview_viewmodel::ruler::{self, LabelSlot, TextMeasure, TickKind};
use dsview_viewmodel::viewport::{Timebase, Viewport};
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Ui, pos2, vec2};

pub const HEADER_WIDTH: f32 = 150.0;
pub const RULER_HEIGHT: f32 = 46.0;
/// Pixel distance within which a click counts as "on the edge".
const EDGE_RADIUS: f64 = 10.0;

/// `PROBE_COLORS` (`trace.cpp`): the default trace colours.
const PROBE_COLOURS: [[u8; 3]; 8] = [
    [0x50, 0x50, 0x50],
    [0x8F, 0x52, 0x02],
    [0xCC, 0x00, 0x00],
    [0xF5, 0x79, 0x00],
    [0xED, 0xD4, 0x00],
    [0x73, 0xD2, 0x16],
    [0x34, 0x65, 0xA4],
    [0x75, 0x50, 0x7B],
];

/// What a press-and-release started.
#[derive(Clone, Debug, Default, PartialEq)]
enum Action {
    #[default]
    None,
    /// Left drag: pan (with the kinetic scroll after a swipe).
    Pan { last_x: f32, last_t: Instant },
    /// Right drag: rubber-band zoom from `x0`.
    Zoom { x0: f32 },
    /// Dragging cursor `n`.
    Cursor(usize),
    /// LOGIC_EDGE: counting edges from `start` on trace `ch`.
    Edges { ch: u16, start: u64, y: f32 },
    /// LOGIC_JUMP: from the edge at `start` on trace `ch`.
    Jump { ch: u16, start: u64, y: f32 },
}

/// A trace placed in the view this frame.
#[derive(Clone)]
struct Row {
    r: TraceRef,
    sig: Option<Signal>,
    /// A decoder trace: the trace and its drawn rows.
    dec: Option<(Arc<DecodeTrace>, Vec<DecRow>)>,
    /// Vertical centre and height, in trace-area coordinates.
    y: f32,
    height: f32,
}

/// A row a decoder trace draws (`paint_mid`'s loop).
#[derive(Clone, Debug)]
enum DecRow {
    /// Stack row `row`; its colour index is its place in the row map.
    Row { row: usize, title: String },
    /// A hidden decoder.
    Unshown(String),
}

/// The rows `DecodeTrace::paint_mid` draws (and `rows_size` counts).
fn decoder_rows(t: &DecodeTrace) -> Vec<DecRow> {
    let st = &t.stack;
    let mut out = Vec::new();
    for d in st.decoders() {
        if !d.shown {
            out.push(DecRow::Unshown(d.info.name.clone()));
            continue;
        }
        for (i, r) in st.rows().iter().enumerate() {
            if r.decoder.id == d.info.id && st.row_stats(i).0 > 0 && r.shown {
                out.push(DecRow::Row {
                    row: i,
                    title: r.title(),
                });
            }
        }
    }
    out
}

pub struct TimeView {
    pub vp: Viewport,
    pub cursors: Cursors,
    kinetic: Kinetic,
    kinetic_last: Option<Instant>,
    action: Action,
    v_scroll: f32,
    pub height_factor: f64,
    bufs: EdgeBuffers,
    /// The mouse measurement under the pointer.
    pub measure: Option<MouseMeasure>,
    /// Edge counts of an LOGIC_EDGE action (rising, falling).
    pub edges: Option<(u64, u64)>,
    /// The jump measurement: (samples, end).
    pub jump: Option<(i64, u64)>,
    rename: Option<(u16, i32, String)>,
    colour_edit: Option<(u16, i32)>,
    header_drag: Option<(TraceRef, f32)>,
    /// The capture the view was fitted to.
    fitted_capture: u64,
    pub vertical_scroll_is_zoom: bool,
    pub quick_scroll: bool,
    pub dividers: bool,
    pub show_samples: bool,
    /// Oscilloscope mode.
    pub dso: crate::dsoview::DsoState,
    /// Analog mode.
    pub analog: crate::analogview::AnalogState,
    /// The pointer over the DSO traces.
    pub(crate) dso_mouse: Option<Pos2>,
    /// The DSO device is hardware (AUTO shows).
    pub(crate) dso_is_hardware: bool,
    /// The floating mouse measurement (`_fen_checkBox`).
    pub measure_enabled: bool,
}

impl Default for TimeView {
    fn default() -> Self {
        TimeView {
            vp: Viewport::default(),
            cursors: Cursors::default(),
            kinetic: Kinetic::default(),
            kinetic_last: None,
            action: Action::None,
            v_scroll: 0.0,
            height_factor: 1.0,
            bufs: EdgeBuffers::default(),
            measure: None,
            edges: None,
            jump: None,
            rename: None,
            colour_edit: None,
            header_drag: None,
            fitted_capture: u64::MAX,
            vertical_scroll_is_zoom: true,
            quick_scroll: true,
            dividers: true,
            show_samples: false,
            dso: crate::dsoview::DsoState::new(),
            analog: crate::analogview::AnalogState::new(),
            dso_mouse: None,
            dso_is_hardware: false,
            measure_enabled: true,
        }
    }
}

/// This frame's wheel events in Qt's `angleDelta` units (120 per notch):
/// a line is a notch, 50 points (a trackpad's pixels) are one too. Read from
/// the events, as egui's smoothed delta spreads a notch over frames.
pub(crate) fn wheel_angle(i: &egui::InputState) -> (i32, i32) {
    let (mut x, mut y) = (0.0f32, 0.0f32);
    for e in &i.events {
        if let egui::Event::MouseWheel { unit, delta, .. } = e {
            let k = match unit {
                egui::MouseWheelUnit::Line => 120.0,
                egui::MouseWheelUnit::Point => 120.0 / 50.0,
                egui::MouseWheelUnit::Page => 120.0 * 3.0,
            };
            x += delta.x * k;
            y += delta.y * k;
        }
    }
    (x.round() as i32, y.round() as i32)
}

struct Font<'a>(&'a egui::Painter, egui::FontId);

impl TextMeasure for Font<'_> {
    fn width(&self, text: &str) -> f32 {
        self.0
            .layout_no_wrap(text.to_owned(), self.1.clone(), Color32::WHITE)
            .size()
            .x
    }
    fn height(&self) -> f32 {
        self.0
            .layout_no_wrap("8".into(), self.1.clone(), Color32::WHITE)
            .size()
            .y
    }
}

pub fn timebase(s: &Session) -> Timebase {
    let st = s.state();
    Timebase {
        snap_samplerate: (st.cur_snap_samplerate as f64).max(1.0),
        samplerate: (s.sample_rate() as f64).max(1.0),
        sample_limit: st.cur_samplelimits.max(1),
        dso_view_time: None,
    }
}

fn trace_colour(sig: &Signal) -> Color32 {
    let [r, g, b] = sig
        .colour
        .unwrap_or(PROBE_COLOURS[sig.index as usize % PROBE_COLOURS.len()]);
    Color32::from_rgb(r, g, b)
}

/// A colour that reads on the dark trace background (the default greys
/// would vanish).
fn line_colour(sig: &Signal) -> Color32 {
    let c = trace_colour(sig);
    if sig.colour.is_none() && sig.index.is_multiple_of(8) {
        Color32::from_rgb(0xb0, 0xb0, 0xb0)
    } else {
        c
    }
}

/// Cursor `number`'s colour (from 1).
pub fn cursor_colour(number: usize) -> Color32 {
    hsva(number)
}

fn hsva(number: usize) -> Color32 {
    let (h, s, v, a) = cursor_hsva(number);
    let hsva = egui::ecolor::Hsva::new(
        h as f32 / 360.0,
        s as f32 / 255.0,
        v as f32 / 255.0,
        a as f32 / 255.0,
    );
    Color32::from(hsva)
}

impl TimeView {
    /// Fits the view to a new capture (Qt: a virtual device's first view
    /// shows 2048 samples per pixel).
    fn fit(&mut self, s: &Session, tb: &Timebase) {
        let seq = s.state().ended_seq;
        if seq == self.fitted_capture {
            return;
        }
        self.fitted_capture = seq;
        self.vp.update_limits(tb);
        let virt = s
            .state()
            .device
            .as_ref()
            .is_some_and(|d| d.is_demo() || d.is_file());
        if virt {
            self.vp.reset_for_virtual_device(tb);
            self.vp.update_limits(tb);
        }
    }

    pub fn show(&mut self, ui: &mut Ui, s: &Session) -> bool {
        if s.mode() == dsview_core::Mode::Dso as i32 {
            return self.show_dso(ui, s);
        }
        if s.mode() == dsview_core::Mode::Analog as i32 {
            return self.show_analog(ui, s);
        }
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
            self.vp.update_limits(&tb);
        }
        self.fit(s, &tb);

        // the rows, from the session's signals and decoders
        let st = s.state();
        let signals: Vec<Signal> = st
            .signals
            .iter()
            .filter(|x| x.ty == channel_type::LOGIC)
            .cloned()
            .collect();
        let decoders: Vec<Arc<DecodeTrace>> = st.decoders.clone();
        drop(st);
        let dec_rows: Vec<Vec<DecRow>> = decoders.iter().map(|t| decoder_rows(t)).collect();
        let mut slots: Vec<Slot> = signals
            .iter()
            .map(|x| Slot {
                kind: TraceKind::Logic,
                rows: 1,
                enabled: true,
                view_index: x.view_index,
            })
            .collect();
        for (t, r) in decoders.iter().zip(&dec_rows) {
            slots.push(Slot {
                kind: TraceKind::Decoder,
                rows: r.len().max(1),
                enabled: true,
                view_index: *t.view_index.lock().unwrap(),
            });
        }
        let font_id = egui::FontId::proportional(13.0);
        let painter = ui.painter_at(full);
        let font_h = Font(&painter, font_id.clone()).height() as f64;
        let max_height_value = s
            .config(dsview_core::key::MAX_HEIGHT_VALUE, None)
            .map(|v| v.get_byte());
        let l = layout::layout(
            &slots,
            &LayoutParams {
                viewport_height: area.height() as f64,
                time_pane_height: area.height() as f64,
                dividers: self.dividers,
                font_height: font_h,
                max_height_value,
                height_factor: self.height_factor,
                dso_split: false,
                logic_mode: true,
                dso_mode_overlap: None,
            },
        );
        let rows: Vec<Row> = l
            .placed
            .iter()
            .map(|p| {
                if p.slot < signals.len() {
                    let sig = signals[p.slot].clone();
                    Row {
                        r: TraceRef::Signal {
                            index: sig.index,
                            ty: sig.ty,
                        },
                        sig: Some(sig),
                        dec: None,
                        y: p.v_offset as f32,
                        height: p.total_height as f32,
                    }
                } else {
                    let k = p.slot - signals.len();
                    Row {
                        r: TraceRef::Decoder(k),
                        sig: None,
                        dec: Some((decoders[k].clone(), dec_rows[k].clone())),
                        y: p.v_offset as f32,
                        height: p.total_height as f32,
                    }
                }
            })
            .collect();
        let content_h = rows
            .last()
            .map_or(0.0, |r| r.y + r.height / 2.0 + l.span_y as f32 / 2.0);
        self.v_scroll = self
            .v_scroll
            .clamp(0.0, (content_h - area.height()).max(0.0));

        let mut changed = self.input(ui, s, &tb, area, &rows);
        changed |= self.ruler_input(ui, ruler_rect, area, &tb);
        changed |= self.header(ui, s, header, area, &rows);

        // paint
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, Color32::from_gray(24));
        painter.rect_filled(header, 0.0, Color32::from_gray(34));
        painter.rect_filled(ruler_rect, 0.0, Color32::from_gray(30));
        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
        let end_align = store.ring_sample_count().saturating_sub(1);
        let tpainter = ui.painter_at(area);
        for row in &rows {
            if let Some((t, drows)) = &row.dec {
                self.paint_decoder(&tpainter, area, row, t, drows, l.signal_height as f32);
                continue;
            }
            let Some(sig) = &row.sig else { continue };
            let y = area.top() + row.y - self.v_scroll;
            if y + row.height < area.top() || y - row.height > area.bottom() {
                continue;
            }
            if self.dividers {
                // between this row and the next, in the middle of the margins
                let yb = y + l.span_y as f32 / 2.0;
                tpainter.hline(area.x_range(), yb, Stroke::new(1.0, Color32::from_gray(48)));
            }
            let shape = logic_shape(
                &store,
                sig.index,
                &self.vp,
                &tb,
                width as u16,
                (row.y - self.v_scroll + row.height / 2.0) as i32,
                row.height as i32,
                end_align,
                &mut self.bufs,
            );
            let colour = line_colour(sig);
            let mut mesh = egui::Mesh::default();
            for ln in &shape.lines {
                let (x0, x1) = (ln.x0.min(ln.x1), ln.x0.max(ln.x1));
                let (y0, y1) = (ln.y0.min(ln.y1), ln.y0.max(ln.y1));
                mesh.add_colored_rect(
                    Rect::from_min_max(
                        pos2(area.left() + x0, area.top() + y0),
                        pos2(area.left() + x1 + 1.0, area.top() + y1 + 1.0),
                    ),
                    colour,
                );
            }
            tpainter.add(egui::Shape::mesh(mesh));
            for &(x, dy) in &shape.dots {
                tpainter.circle_filled(pos2(area.left() + x, area.top() + dy), 2.0, colour);
            }
        }
        drop(store);

        self.paint_ruler(&painter, ruler_rect, s, &tb);
        self.paint_cursors(&painter, ruler_rect, area, &tb);
        self.paint_actions(&tpainter, area, &tb);
        self.paint_header(&painter, header, area, &rows, &font_id);
        changed
    }

    /// `DecodeTrace::paint_mid`: each drawn row, detailed or "Zoom in for
    /// details"; the error message instead when the stack failed.
    fn paint_decoder(
        &self,
        painter: &egui::Painter,
        area: Rect,
        row: &Row,
        t: &DecodeTrace,
        drows: &[DecRow],
        h: f32,
    ) {
        let st = &t.stack;
        let rate = (st.sample_rate() as f64).max(1.0);
        let spp = rate * self.vp.scale;
        let off = self.vp.x_offset;
        let font = egui::FontId::proportional(11.0);
        let m = Font(painter, font.clone());
        let (left, right) = (0.0, area.width() as f64);
        let start = ((left + off as f64) * spp).max(0.0) as u64;
        let end = ((right + off as f64) * spp).max(0.0) as u64;
        let mut y = area.top() + row.y - self.v_scroll - (row.height - h) / 2.0;
        let err = st.error_message();
        if !err.is_empty() {
            painter.text(
                pos2(area.left() + 4.0, y),
                egui::Align2::LEFT_CENTER,
                format!("Error: {err}"),
                font,
                Color32::from_rgb(0xEF, 0x29, 0x29),
            );
            return;
        }
        let grey = Color32::from_gray(170);
        let x0 = area.left();
        for dr in drows {
            match dr {
                DecRow::Unshown(name) => {
                    painter.text(
                        pos2(x0 + 4.0, y),
                        egui::Align2::LEFT_CENTER,
                        format!("{name}: Unshown"),
                        font.clone(),
                        grey,
                    );
                }
                DecRow::Row { row: ri, title } => {
                    let local_row = *ri;
                    let (_, min_len, max_len) = st.row_stats(*ri);
                    if vdec::row_detailed(min_len, max_len, spp) {
                        let base = vdec::row_base_colour(local_row);
                        let min_w = min_len as f64 / spp;
                        let mut last_x = -1.0;
                        for a in st.annotations(*ri, start, end) {
                            let shape = vdec::annotation_shape(
                                a.start,
                                a.end,
                                &a.lines,
                                spp,
                                off,
                                left,
                                right,
                                h as f64,
                                min_w,
                                &mut last_x,
                                &m,
                            );
                            let (fill, outline) = vdec::annotation_colours(base, local_row, a.ty);
                            let rgb = |c: [u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
                            let (fill_c, out_c) = (rgb(fill), rgb(outline));
                            let text_c = rgb(vdec::text_colour(fill));
                            match shape {
                                AnnShape::None => {}
                                AnnShape::Line { x } => {
                                    painter.vline(
                                        x0 + x as f32,
                                        (y - h / 2.0)..=(y + h / 2.0),
                                        Stroke::new(1.0, out_c),
                                    );
                                }
                                AnnShape::Instant { x, w, text } => {
                                    let r = Rect::from_center_size(
                                        pos2(x0 + x as f32, y),
                                        vec2(w as f32, h),
                                    );
                                    painter.rect_filled(r, h / 2.0, fill_c);
                                    painter.text(
                                        r.center(),
                                        egui::Align2::CENTER_CENTER,
                                        text,
                                        font.clone(),
                                        text_c,
                                    );
                                }
                                AnnShape::Range {
                                    start,
                                    end,
                                    cap,
                                    text,
                                } => {
                                    let (s, e, c) =
                                        (x0 + start as f32, x0 + end as f32, cap as f32);
                                    let (top, bot) = (y - h / 2.0 + 0.5, y + h / 2.0 + 0.5);
                                    painter.add(egui::Shape::convex_polygon(
                                        vec![
                                            pos2(s, y),
                                            pos2(s + c, top),
                                            pos2(e - c, top),
                                            pos2(e, y),
                                            pos2(e - c, bot),
                                            pos2(s + c, bot),
                                        ],
                                        fill_c,
                                        Stroke::new(1.0, out_c),
                                    ));
                                    if let Some((txt, faded)) = text {
                                        let r = Rect::from_min_max(
                                            pos2(s + c / 2.0, top),
                                            pos2(e - c / 2.0, bot),
                                        );
                                        let p = painter.with_clip_rect(r.intersect(area));
                                        if faded {
                                            p.text(
                                                pos2(r.left(), y),
                                                egui::Align2::LEFT_CENTER,
                                                txt,
                                                font.clone(),
                                                text_c,
                                            );
                                        } else {
                                            p.text(
                                                r.center(),
                                                egui::Align2::CENTER_CENTER,
                                                txt,
                                                font.clone(),
                                                text_c,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        // draw_nodetail: a line with the hint in the middle
                        painter.hline(area.x_range(), y, Stroke::new(1.0, Color32::from_gray(90)));
                        let r = painter.text(
                            pos2(area.center().x, y),
                            egui::Align2::CENTER_CENTER,
                            "Zoom in for details",
                            font.clone(),
                            grey,
                        );
                        painter.rect_filled(r.expand(2.0), 0.0, Color32::from_gray(24));
                        painter.text(
                            pos2(area.center().x, y),
                            egui::Align2::CENTER_CENTER,
                            "Zoom in for details",
                            font.clone(),
                            grey,
                        );
                    }
                    // the row heading at the left edge (Qt: paint_back)
                    painter.text(
                        pos2(x0 + 2.0, y - h / 2.0 + 1.0),
                        egui::Align2::LEFT_TOP,
                        title,
                        egui::FontId::proportional(9.0),
                        Color32::from_gray(140),
                    );
                }
            }
            y += h;
        }
    }

    fn row_at(&self, rows: &[Row], area: Rect, y: f32) -> Option<Row> {
        rows.iter()
            .find(|r| ((area.top() + r.y - self.v_scroll) - y).abs() < r.height / 2.0)
            .cloned()
    }

    /// Mouse and keyboard in the trace area; true when the view changed.
    fn input(&mut self, ui: &mut Ui, s: &Session, tb: &Timebase, area: Rect, rows: &[Row]) -> bool {
        let id = ui.make_persistent_id("time-view");
        let resp = ui.interact(area, id, egui::Sense::click_and_drag());
        let mut changed = false;
        let (mods, scroll) = ui.input(|i| {
            (
                Mods {
                    ctrl: i.modifiers.command,
                    shift: i.modifiers.shift,
                },
                wheel_angle(i),
            )
        });
        let spp = self.vp.samples_per_pixel(tb);
        let rate = tb.snap_samplerate as u64;
        let hover = resp.hover_pos();

        // the wheel (egui gives points; Qt's notch is 120 eighths of a degree)
        if let Some(p) = hover
            && (scroll.0 != 0 || scroll.1 != 0)
        {
            let x = (p.x - area.left()) as i32;
            match input::wheel(
                scroll.0,
                scroll.1,
                x,
                mods,
                false,
                self.vertical_scroll_is_zoom,
            ) {
                WheelAction::Zoom { steps, x } => changed |= self.vp.zoom(steps, x, tb),
                WheelAction::ScrollVertical { dy } => {
                    self.v_scroll += dy as f32;
                    changed = true;
                }
                WheelAction::VerticalZoom { steps } => {
                    self.height_factor =
                        (self.height_factor * 1.5f64.powf(steps * 0.5)).clamp(0.5, 20.0);
                    changed = true;
                }
                WheelAction::Pan { dx } => {
                    changed |=
                        self.vp
                            .set_scale_offset(self.vp.scale, self.vp.x_offset + dx as i64, tb)
                }
                WheelAction::None => {}
            }
        }

        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
        // press: cursor grab, else pan or zoom
        if resp.drag_started() {
            let p = resp.interact_pointer_pos().unwrap_or_default();
            let x = (p.x - area.left()) as f64;
            let primary = ui.input(|i| i.pointer.primary_down());
            let grabbed = self
                .cursors
                .grab(x, |smp| self.vp.index_to_pixel(smp, false, tb));
            self.action = match (primary, grabbed) {
                (true, Some(n)) => Action::Cursor(n),
                (true, None) => {
                    self.kinetic = Kinetic::default();
                    Action::Pan {
                        last_x: p.x,
                        last_t: Instant::now(),
                    }
                }
                (false, _) => Action::Zoom { x0: p.x },
            };
        }
        if resp.dragged() {
            let p = resp.interact_pointer_pos().unwrap_or_default();
            match &mut self.action {
                Action::Pan { last_x, last_t } => {
                    let dx = (*last_x - p.x) as i32;
                    let now = Instant::now();
                    self.kinetic
                        .track(dx, now.duration_since(*last_t).as_millis() as u64);
                    *last_x = p.x;
                    *last_t = now;
                    changed |=
                        self.vp
                            .set_scale_offset(self.vp.scale, self.vp.x_offset + dx as i64, tb);
                }
                Action::Cursor(n) => {
                    let n = *n;
                    let x = (p.x - area.left()) as f64;
                    let pos = self.vp.pixel_to_index(x, tb) as f64;
                    let pulse = self.row_at(rows, area, p.y).and_then(|r| {
                        let (i0, i1, _, _) = measure::pulse_at(
                            &store,
                            r.sig.as_ref()?.index,
                            spp,
                            self.vp.x_offset,
                            x,
                        )?;
                        let px = |i| (self.vp.index_to_pixel(i, false, tb) - x).abs();
                        Some(((i0, px(i0)), (i1, px(i1))))
                    });
                    self.cursors.drag(n, pos, pulse);
                    changed = true;
                }
                _ => {}
            }
        }
        if resp.drag_stopped() {
            match std::mem::take(&mut self.action) {
                Action::Pan { .. } if self.quick_scroll => {
                    if self.kinetic.release() {
                        self.kinetic_last = Some(Instant::now());
                    }
                }
                Action::Zoom { x0 } => {
                    let x1 = resp.interact_pointer_pos().map_or(x0, |p| p.x);
                    changed |= input::rubber_band_zoom(
                        &mut self.vp,
                        (x0 - area.left()) as i32,
                        (x1 - area.left()) as i32,
                        tb,
                    );
                }
                _ => {}
            }
        }
        // kinetic scroll ticks
        if let Some(last) = self.kinetic_last {
            let n = last.elapsed().as_millis() as u64 / DRAG_TIMER_INTERVAL_MS;
            let mut running = true;
            for _ in 0..n {
                running = self.kinetic.tick(&mut self.vp, tb);
                if !running {
                    break;
                }
            }
            if n > 0 {
                self.kinetic_last = Some(Instant::now());
                changed = true;
            }
            if running {
                ui.ctx().request_repaint();
            } else {
                self.kinetic_last = None;
            }
        }

        // clicks: double-click adds a cursor / toggles the full view;
        // a single click starts or ends the edge measurements
        if resp.double_clicked_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            let x = (p.x - area.left()) as f64;
            let at = self
                .row_at(rows, area, p.y)
                .and_then(|r| {
                    measure::edge_near(
                        &store,
                        r.sig.as_ref()?.index,
                        spp,
                        self.vp.x_offset,
                        x,
                        EDGE_RADIUS,
                    )
                })
                .unwrap_or_else(|| self.vp.pixel_to_index(x, tb));
            self.cursors.add(at);
            self.action = Action::None;
            changed = true;
        } else if resp.double_clicked_by(egui::PointerButton::Secondary) {
            let full = self.vp.scale >= self.vp.max_scale && self.vp.x_offset == 0;
            changed |= if full {
                self.vp.restore_previous(tb)
            } else {
                self.vp.set_scale_offset(self.vp.max_scale, 0, tb)
            };
        } else if resp.clicked_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            match self.action {
                Action::Edges { .. } | Action::Jump { .. } => self.action = Action::None,
                _ => {
                    let x = (p.x - area.left()) as f64;
                    if let Some(r) = self.row_at(rows, area, p.y)
                        && let Some(sig) = &r.sig
                    {
                        self.action = match measure::edge_near(
                            &store,
                            sig.index,
                            spp,
                            self.vp.x_offset,
                            x,
                            EDGE_RADIUS,
                        ) {
                            Some(start) => Action::Jump {
                                ch: sig.index,
                                start,
                                y: p.y,
                            },
                            None => Action::Edges {
                                ch: sig.index,
                                start: self.vp.pixel_to_index(x, tb),
                                y: p.y,
                            },
                        };
                    }
                }
            }
            changed = true;
        }

        // the measurements at the pointer
        self.measure = None;
        self.edges = None;
        self.jump = None;
        if let Some(p) = hover {
            let x = (p.x - area.left()) as f64;
            match self.action {
                Action::Edges { ch, start, .. } => {
                    let end = (spp * (self.vp.x_offset as f64 + x)) as u64;
                    self.edges = measure::edge_counts(&store, ch, end, start);
                }
                Action::Jump { ch, start, .. } => {
                    let end = measure::edge_near(&store, ch, spp, self.vp.x_offset, x, EDGE_RADIUS)
                        .unwrap_or_else(|| self.vp.pixel_to_index(x, tb));
                    self.jump = Some((end as i64 - start as i64, end));
                }
                _ => {
                    if self.measure_enabled
                        && let Some(r) = self.row_at(rows, area, p.y)
                        && let Some(sig) = &r.sig
                    {
                        self.measure = measure::mouse_measure(
                            &store,
                            sig.index,
                            spp,
                            self.vp.x_offset,
                            x,
                            rate,
                        );
                    }
                }
            }
        }
        drop(store);

        // keyboard: PgUp/PgDn pan a view width, arrows zoom (MainWindow)
        let (pgup, pgdn, left, right) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
            )
        });
        let w = self.vp.width as i64;
        if pgup {
            changed |= self
                .vp
                .set_scale_offset(self.vp.scale, self.vp.x_offset - w, tb);
        }
        if pgdn {
            changed |= self
                .vp
                .set_scale_offset(self.vp.scale, self.vp.x_offset + w, tb);
        }
        if left {
            changed |= self.vp.zoom(1.0, self.vp.width / 2, tb);
        }
        if right {
            changed |= self.vp.zoom(-1.0, self.vp.width / 2, tb);
        }
        changed
    }

    /// The cursor labels in the ruler: drag moves the cursor, a right click
    /// deletes it (Qt: the label's close mark).
    fn ruler_input(&mut self, ui: &mut Ui, ruler: Rect, area: Rect, tb: &Timebase) -> bool {
        let mut changed = false;
        let mut delete = None;
        let samples = self.cursors.samples.clone();
        for (i, &smp) in samples.iter().enumerate() {
            let x = area.left() + self.vp.index_to_pixel(smp, false, tb) as f32;
            let label = Rect::from_center_size(pos2(x, ruler.bottom() - 8.0), vec2(18.0, 14.0));
            let r = ui.interact(
                label,
                ui.make_persistent_id(("cursor", i)),
                egui::Sense::click_and_drag(),
            );
            if r.secondary_clicked() {
                delete = Some(i + 1);
            }
            if r.drag_started() {
                self.action = Action::Cursor(i + 1);
            }
            if r.dragged()
                && let Some(p) = r.interact_pointer_pos()
            {
                let pos = self.vp.pixel_to_index((p.x - area.left()) as f64, tb) as f64;
                self.cursors.drag(i + 1, pos, None);
                changed = true;
            }
            if r.drag_stopped() {
                self.action = Action::None;
            }
        }
        if let Some(n) = delete {
            self.cursors.remove(n);
            changed = true;
        }
        changed
    }

    /// The header: name (double-click renames), colour swatch (click picks),
    /// the five trigger squares of a logic trace, drag to reorder.
    fn header(&mut self, ui: &mut Ui, s: &Session, header: Rect, area: Rect, rows: &[Row]) -> bool {
        let mut changed = false;
        for row in rows {
            let Some(sig) = &row.sig else { continue };
            let cy = area.top() + row.y - self.v_scroll;
            if cy < area.top() - 20.0 || cy > area.bottom() + 20.0 {
                continue;
            }
            let swatch = Rect::from_center_size(pos2(header.left() + 12.0, cy), vec2(12.0, 12.0));
            let name = Rect::from_min_max(
                pos2(header.left() + 22.0, cy - 9.0),
                pos2(header.right() - 64.0, cy + 9.0),
            );
            let resp = ui.interact(
                name,
                ui.make_persistent_id(("name", sig.index)),
                egui::Sense::click_and_drag(),
            );
            if resp.double_clicked() {
                self.rename = Some((sig.index, sig.ty, sig.name.clone()));
            }
            if resp.drag_started() {
                self.header_drag = Some((row.r, cy));
            }
            let sw = ui.interact(
                swatch,
                ui.make_persistent_id(("swatch", sig.index)),
                egui::Sense::click(),
            );
            if sw.clicked() {
                self.colour_edit = Some((sig.index, sig.ty));
            }
            // trigger squares: rising, high, falling, low, edge
            for (k, t) in [
                trig::RISING,
                trig::HIGH,
                trig::FALLING,
                trig::LOW,
                trig::EDGE,
            ]
            .into_iter()
            .enumerate()
            {
                let r = Rect::from_center_size(
                    pos2(header.right() - 56.0 + k as f32 * 11.0, cy),
                    vec2(10.0, 10.0),
                );
                let tr = ui.interact(
                    r,
                    ui.make_persistent_id(("trig", sig.index, k)),
                    egui::Sense::click(),
                );
                if tr.clicked() {
                    s.set_signal_trigger(sig.index, if sig.trig == t { trig::NONE } else { t });
                    changed = true;
                }
            }
        }
        // header drag: drop between rows renumbers the view order
        if let Some((r, _)) = self.header_drag {
            let pointer = ui.input(|i| i.pointer.interact_pos());
            let released = ui.input(|i| i.pointer.any_released());
            if let Some(p) = pointer {
                self.header_drag = Some((r, p.y));
            }
            if released && let Some((dragged, y)) = self.header_drag.take() {
                let mut order: Vec<(f32, TraceRef)> = rows
                    .iter()
                    .map(|x| {
                        let cy = area.top() + x.y - self.v_scroll;
                        (if x.r == dragged { y } else { cy }, x.r)
                    })
                    .collect();
                order.sort_by(|a, b| a.0.total_cmp(&b.0));
                let refs: Vec<TraceRef> = order.into_iter().map(|x| x.1).collect();
                s.set_view_order(&refs);
                changed = true;
            }
        }
        // rename and colour popups
        if let Some((index, ty, mut text)) = self.rename.take() {
            let mut keep = true;
            egui::Window::new("Rename")
                .collapsible(false)
                .resizable(false)
                .show(ui.ctx(), |ui| {
                    let r = ui.text_edit_singleline(&mut text);
                    r.request_focus();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
                    if enter {
                        s.set_signal_name(index, ty, text.trim());
                        keep = false;
                    } else if esc {
                        keep = false;
                    }
                });
            if keep {
                self.rename = Some((index, ty, text));
            }
            changed = true;
        }
        if let Some((index, ty)) = self.colour_edit {
            let sig = rows
                .iter()
                .filter_map(|r| r.sig.clone())
                .find(|x| x.index == index && x.ty == ty);
            let mut open = true;
            if let Some(sig) = sig {
                let mut c = trace_colour(&sig);
                egui::Window::new("Colour")
                    .collapsible(false)
                    .resizable(false)
                    .open(&mut open)
                    .show(ui.ctx(), |ui| {
                        if egui::color_picker::color_picker_color32(
                            ui,
                            &mut c,
                            egui::color_picker::Alpha::Opaque,
                        ) {
                            s.set_signal_colour(index, ty, Some([c.r(), c.g(), c.b()]));
                        }
                        if ui.button("Default").clicked() {
                            s.set_signal_colour(index, ty, None);
                        }
                    });
            }
            if !open {
                self.colour_edit = None;
            }
            changed = true;
        }
        changed
    }

    fn paint_header(
        &self,
        painter: &egui::Painter,
        header: Rect,
        area: Rect,
        rows: &[Row],
        font: &egui::FontId,
    ) {
        for row in rows {
            if let Some((t, _)) = &row.dec {
                let cy = area.top() + row.y - self.v_scroll;
                painter.text(
                    pos2(header.left() + 22.0, cy),
                    egui::Align2::LEFT_CENTER,
                    t.name(),
                    font.clone(),
                    Color32::from_rgb(0x8c, 0xaa, 0xee),
                );
                continue;
            }
            let Some(sig) = &row.sig else { continue };
            let mut cy = area.top() + row.y - self.v_scroll;
            if let Some((r, y)) = self.header_drag
                && r == row.r
            {
                cy = y;
            }
            if cy < area.top() - 20.0 || cy > area.bottom() + 20.0 {
                continue;
            }
            painter.rect_filled(
                Rect::from_center_size(pos2(header.left() + 12.0, cy), vec2(12.0, 12.0)),
                2.0,
                trace_colour(sig),
            );
            painter.text(
                pos2(header.left() + 22.0, cy),
                egui::Align2::LEFT_CENTER,
                &sig.name,
                font.clone(),
                Color32::from_gray(220),
            );
            for (k, t) in [
                trig::RISING,
                trig::HIGH,
                trig::FALLING,
                trig::LOW,
                trig::EDGE,
            ]
            .into_iter()
            .enumerate()
            {
                let r = Rect::from_center_size(
                    pos2(header.right() - 56.0 + k as f32 * 11.0, cy),
                    vec2(10.0, 10.0),
                );
                let on = sig.trig == t;
                painter.rect_stroke(
                    r,
                    1.0,
                    Stroke::new(1.0, Color32::from_gray(if on { 230 } else { 90 })),
                    egui::StrokeKind::Inside,
                );
                if on {
                    painter.rect_filled(r.shrink(2.0), 0.0, Color32::from_rgb(0x11, 0x85, 0xd1));
                }
                // a glyph per trigger type
                let (l, rr, top, bot) = (
                    r.left() + 2.0,
                    r.right() - 2.0,
                    r.top() + 2.0,
                    r.bottom() - 2.0,
                );
                let m = r.center().x;
                let st = Stroke::new(1.0, Color32::from_gray(200));
                let pts: Vec<Pos2> = match t {
                    trig::RISING => vec![pos2(l, bot), pos2(m, bot), pos2(m, top), pos2(rr, top)],
                    trig::FALLING => vec![pos2(l, top), pos2(m, top), pos2(m, bot), pos2(rr, bot)],
                    trig::HIGH => vec![pos2(l, top), pos2(rr, top)],
                    trig::LOW => vec![pos2(l, bot), pos2(rr, bot)],
                    _ => vec![
                        pos2(l, bot),
                        pos2(m - 1.0, bot),
                        pos2(m - 1.0, top),
                        pos2(m + 1.0, top),
                        pos2(m + 1.0, bot),
                        pos2(rr, bot),
                    ],
                };
                painter.add(egui::Shape::line(pts, st));
            }
        }
    }

    pub(crate) fn paint_ruler(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        s: &Session,
        tb: &Timebase,
    ) {
        let small = egui::FontId::proportional(11.0);
        let m = Font(painter, small.clone());
        let trig_pos = s.state().trigger_pos;
        let ticks = ruler::logic_ticks(
            self.vp.scale,
            self.vp.x_offset,
            self.vp.width,
            rect.width() as i32,
            rect.height() as i32,
            tb.snap_samplerate as u64,
            trig_pos,
            self.show_samples,
            &m,
        );
        let fg = Color32::from_gray(170);
        for t in &ticks.ticks {
            let x = rect.left() + t.x as f32;
            if x < rect.left() - 1.0 || x > rect.right() + 1.0 {
                continue;
            }
            let y1 = rect.top()
                + match t.kind {
                    TickKind::Major => ticks.major_y1,
                    TickKind::Minor => ticks.minor_y1,
                } as f32;
            painter.vline(x, y1..=rect.bottom(), Stroke::new(1.0, fg));
            if let Some((text, slot)) = &t.label {
                let y = match slot {
                    LabelSlot::Top => rect.top() + 10.0,
                    LabelSlot::Relative => rect.top() + ticks.minor_y1 as f32 - 13.0,
                };
                painter.text(
                    pos2(x, y),
                    egui::Align2::CENTER_TOP,
                    text,
                    small.clone(),
                    fg,
                );
            }
        }
        // hover arrow
        if let Some(p) = painter.ctx().pointer_hover_pos()
            && p.x >= rect.left()
            && p.x <= rect.right()
        {
            let b = rect.bottom();
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(p.x, b),
                    pos2(p.x - 4.0, b - 4.0),
                    pos2(p.x + 4.0, b - 4.0),
                ],
                fg,
                Stroke::NONE,
            ));
        }
    }

    pub(crate) fn paint_cursors(
        &self,
        painter: &egui::Painter,
        ruler: Rect,
        area: Rect,
        tb: &Timebase,
    ) {
        for (i, &smp) in self.cursors.samples.iter().enumerate() {
            let x = area.left() + self.vp.index_to_pixel(smp, false, tb) as f32;
            if x < area.left() || x > area.right() {
                continue;
            }
            let c = hsva(i + 1);
            painter.vline(x, area.y_range(), Stroke::new(1.0, c));
            let label = Rect::from_center_size(pos2(x, ruler.bottom() - 8.0), vec2(18.0, 14.0));
            painter.rect_filled(label, 2.0, c);
            painter.text(
                label.center(),
                egui::Align2::CENTER_CENTER,
                (i + 1).to_string(),
                egui::FontId::proportional(10.0),
                Color32::BLACK,
            );
        }
    }

    fn paint_actions(&self, painter: &egui::Painter, area: Rect, tb: &Timebase) {
        let accent = Color32::from_rgb(0x11, 0x85, 0xd1);
        let Some(p) = painter.ctx().pointer_hover_pos() else {
            return;
        };
        let x_of = |i: u64| area.left() + self.vp.index_to_pixel(i, false, tb) as f32;
        let small = egui::FontId::proportional(11.0);
        match &self.action {
            Action::Edges { start, y, .. } => {
                let x0 = x_of(*start);
                painter.line_segment([pos2(x0, *y), pos2(p.x, *y)], Stroke::new(1.0, accent));
                if let Some((r, f)) = self.edges {
                    painter.text(
                        pos2(p.x + 8.0, *y - 18.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("Rising: {r}  Falling: {f}  Edges: {}", r + f),
                        small,
                        Color32::WHITE,
                    );
                }
            }
            Action::Jump { start, y, .. } => {
                if let Some((delta, end)) = self.jump {
                    let (x0, x1) = (x_of(*start), x_of(end));
                    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                        [
                            pos2(x0, *y),
                            pos2((x0 + x1) / 2.0, *y),
                            pos2((x0 + x1) / 2.0, p.y),
                            pos2(x1, p.y),
                        ],
                        false,
                        Color32::TRANSPARENT,
                        Stroke::new(1.0, accent),
                    ));
                    painter.text(
                        pos2((x0 + x1) / 2.0, (*y).min(p.y) - 6.0),
                        egui::Align2::CENTER_BOTTOM,
                        format!(
                            "{}/{delta}",
                            ruler::format_real_time(delta, tb.snap_samplerate as u64)
                        ),
                        small,
                        Color32::WHITE,
                    );
                }
            }
            Action::Zoom { x0 } => {
                let r = Rect::from_x_y_ranges(x0.min(p.x)..=x0.max(p.x), area.y_range());
                painter.rect_filled(
                    r,
                    0.0,
                    Color32::from_rgba_unmultiplied(0x11, 0x85, 0xd1, 40),
                );
            }
            _ => {
                if let Some(m) = &self.measure
                    && area.contains(p)
                {
                    let (x0, x1) = (x_of(m.start), x_of(m.end));
                    painter.hline(x0..=x1, p.y, Stroke::new(1.0, accent));
                }
            }
        }
    }

    /// `show_region` from the control API.
    pub fn show_region(&mut self, s: &Session, start: u64, end: u64) {
        let tb = timebase(s);
        self.vp.show_region(start, end, false, &tb);
    }

    /// `set_cursor_middle`: sample `index` in the middle, the scale kept.
    pub fn center_on(&mut self, s: &Session, index: u64) {
        let tb = timebase(s);
        let off = index as f64 / (tb.snap_samplerate * self.vp.scale) - (self.vp.width / 2) as f64;
        self.vp.set_scale_offset(self.vp.scale, off as i64, &tb);
    }
}
