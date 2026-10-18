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
use dsview_viewmodel::layout::{self as vlayout, LayoutParams, Slot, TraceKind};
use dsview_viewmodel::logic::{EdgeBuffers, logic_shape};
use dsview_viewmodel::measure::{self, MouseMeasure};
use dsview_viewmodel::progress as vprog;
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
    /// Dragging the search cursor (it then marks no hit).
    Search,
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
    for (k, d) in st.decoders().iter().enumerate() {
        if !st.decoder_shown(k) {
            out.push(DecRow::Unshown(d.info.name.clone()));
            continue;
        }
        for (i, r) in st.rows().iter().enumerate() {
            if r.decoder.id == d.info.id && st.row_stats(i).0 > 0 && st.row_shown(i) {
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
    /// Map Zoom "Fixed": `show_region` keeps the scale.
    pub map_zoom_fixed: bool,
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
    header_drag: Option<HeaderDrag>,
    /// Traces selected in the header (`Trace::select`, Ctrl+click adds).
    selected: Vec<TraceRef>,
    /// The logic view's traces and their centres on screen, as last drawn.
    shown_rows: Vec<(TraceRef, f32)>,
    pub(crate) pinch: input::Pinch,
    /// The measured trace's centre y (screen) and height.
    measure_row: Option<(f32, f32)>,
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
    /// The search cursor while the search dock shows: position and hit.
    pub search: Option<(u64, bool)>,
    /// `cur_samplelimits - 1`, for the decode region's end.
    last_sample: u64,
    /// When the running capture started (`start_trigger_timer`).
    wait_since: Option<Instant>,
    /// The ruler's cursor selector and go-to strips.
    strips: ruler::RulerStrips,
    /// The cursor whose ruler label is dragged (`_grabbed_marker`), and a
    /// label or close box was pressed (`_hitCursor`).
    ruler_grab: Option<usize>,
    ruler_hit: bool,
    /// When the Y-scale badge was flashed (`flash_yscale_badge`).
    yscale_flash: Option<Instant>,
}

/// A header drag (`_drag_traces`): the pointer's y at the press, the
/// traces with their offsets then (in the traces' coordinates) and where
/// they are now, once moved.
#[derive(Clone, Debug)]
struct HeaderDrag {
    down_y: f32,
    starts: Vec<(TraceRef, f64)>,
    moved: Option<Vec<(TraceRef, f64)>>,
}

impl HeaderDrag {
    /// Where trace `r` is drawn (screen y), if it is being dragged.
    fn y_of(&self, r: TraceRef, top: f32) -> Option<f32> {
        self.moved
            .as_ref()?
            .iter()
            .find(|x| x.0 == r)
            .map(|x| top + x.1 as f32)
    }
}

/// A mark on a logic channel (`LogicSignal::paint_mark`): the channel,
/// the annotation's pixel span, the channel type's kind (`type / 100`:
/// 0 clock, 1 serial data, 2 another data line) and its fill colour.
#[derive(Clone, Copy, Debug)]
struct ChannelMark {
    channel: u16,
    x0: f64,
    x1: f64,
    kind: i32,
    colour: Color32,
}

/// `DecodeTrace::draw_annotation` for a type-2xx annotation: a mark on each
/// bound channel whose type matches it (`type % 100`, or 0 for any).
fn channel_marks(
    st: &dsview_session::decode::DecoderStack,
    ann_ty: i32,
    x0: f64,
    x1: f64,
    colour: Color32,
) -> Vec<ChannelMark> {
    let mut out = Vec::new();
    for d in st.decoders() {
        for (id, index) in d.bound_probes() {
            let Some(ty) = d
                .info
                .channels
                .iter()
                .chain(&d.info.opt_channels)
                .find(|c| c.id == id)
                .map(|c| c.ty)
            else {
                continue;
            };
            // SRD_CHANNEL_COMMON, or another kind of data
            if ty == -1 || (ty % 100 != ann_ty % 100 && ty % 100 != 0) {
                continue;
            }
            out.push(ChannelMark {
                channel: index as u16,
                x0,
                x1,
                kind: ty / 100,
                colour,
            });
        }
    }
    out
}

impl Default for TimeView {
    fn default() -> Self {
        TimeView {
            vp: Viewport::default(),
            map_zoom_fixed: false,
            wait_since: None,
            strips: ruler::RulerStrips::default(),
            ruler_grab: None,
            ruler_hit: false,
            yscale_flash: None,
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
            selected: Vec::new(),
            shown_rows: Vec::new(),
            pinch: input::Pinch::default(),
            measure_row: None,
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
            search: None,
            last_sample: 0,
        }
    }
}

/// This frame's wheel events in Qt's `angleDelta` units (120 per notch):
/// a line is a notch, 50 points (a trackpad's pixels) are one too. Read from
/// the events, as egui's smoothed delta spreads a notch over frames.
/// The pinch gestures' zoom factors of this frame (`Event::Zoom`, from a
/// touchpad; Ctrl+wheel arrives as wheel events).
pub(crate) fn zoom_factors(i: &egui::InputState) -> Vec<f32> {
    i.events
        .iter()
        .filter_map(|e| match e {
            egui::Event::Zoom(f) => Some(*f),
            _ => None,
        })
        .collect()
}

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
        self.last_sample = tb.sample_limit.saturating_sub(1);
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
        let l = vlayout::layout(
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

        self.shown_rows = rows
            .iter()
            .map(|r| (r.r, area.top() + r.y - self.v_scroll))
            .collect();
        let mut changed = self.input(ui, s, &tb, area, &rows);
        changed |= self.ruler_input(ui, s, ruler_rect, area, &tb);
        let badge = self.yscale_badge(ui, area);
        changed |= badge.is_some_and(|(_, clicked, _)| clicked);
        changed |= self.header(ui, s, header, area, &rows);

        // paint
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, Color32::from_gray(24));
        painter.rect_filled(header, 0.0, Color32::from_gray(34));
        painter.rect_filled(ruler_rect, 0.0, Color32::from_gray(30));
        let store = s.data().logic.lock().unwrap_or_else(|e| e.into_inner());
        let end_align = store.ring_sample_count().saturating_sub(1);
        let tpainter = ui.painter_at(area);
        let (working, repeat, limit) = {
            let st = s.state();
            (
                st.working,
                st.collect_mode == dsview_session::session::CollectMode::Repeat,
                st.cur_samplelimits,
            )
        };
        if !working {
            self.wait_since = None;
        } else if self.wait_since.is_none() {
            self.wait_since = Some(Instant::now());
        }
        let screen = vprog::screen(
            working,
            repeat,
            s.is_realtime_refresh(),
            store.sample_count() > 0,
        );
        if screen == vprog::Screen::Progress {
            let received = store.sample_count();
            drop(store);
            self.paint_progress(&tpainter, area, s, received, limit);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(vprog::TRIGGER_TIMER_MS));
            self.paint_ruler(&painter, ruler_rect, s, &tb);
            self.paint_header(&painter, header, area, &rows, &font_id);
            return changed;
        }
        let mut marks = Vec::new();
        for row in &rows {
            if let Some((t, drows)) = &row.dec {
                marks.extend(self.paint_decoder(
                    &tpainter,
                    area,
                    row,
                    t,
                    drows,
                    l.signal_height as f32,
                ));
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
        // `LogicSignal::paint_mark`: the type-2xx annotations' marks on the
        // channels they belong to
        for m in marks {
            let Some(r) = rows.iter().find(|r| {
                matches!(r.r, TraceRef::Signal { index, ty } if index == m.channel && ty == channel_type::LOGIC)
            }) else {
                continue;
            };
            let y = area.top() + r.y - self.v_scroll;
            let at = |x: f64| area.left() + x as f32;
            match m.kind {
                // SRD_CHANNEL_SCLK: a small triangle
                0 => {
                    let x = at(m.x0).round();
                    tpainter.add(egui::Shape::convex_polygon(
                        vec![
                            pos2(x, y - 2.5),
                            pos2(x + 3.5, y + 1.5),
                            pos2(x - 3.5, y + 1.5),
                        ],
                        m.colour,
                        Stroke::NONE,
                    ));
                }
                1 => {
                    tpainter.circle(pos2(at(m.x0), y), 3.0, m.colour, Stroke::new(1.0, m.colour));
                }
                2 => {
                    tpainter.circle(
                        pos2(at((m.x0 + m.x1) / 2.0), y),
                        3.0,
                        m.colour,
                        Stroke::new(1.0, m.colour),
                    );
                }
                _ => {}
            }
        }

        self.paint_ruler(&painter, ruler_rect, s, &tb);
        self.paint_cursors(&painter, s, ruler_rect, area, &tb);
        self.paint_actions(&tpainter, area, &tb);
        if let Some((r, _, opacity)) = badge {
            self.paint_yscale_badge(&tpainter, r, opacity);
        }
        self.paint_header(&painter, header, area, &rows, &font_id);
        changed
    }

    /// The Y-scale badge (`paintYScaleBadge`) after a vertical zoom: its
    /// rectangle, whether it was clicked (which resets the zoom,
    /// `reset_yscale`) and its opacity. Hovering keeps it.
    fn yscale_badge(&mut self, ui: &mut Ui, area: Rect) -> Option<(Rect, bool, f32)> {
        let t = self.yscale_flash?;
        let Some(opacity) = input::yscale_badge_opacity(t.elapsed().as_millis() as u64) else {
            self.yscale_flash = None;
            return None;
        };
        let font = egui::FontId::proportional(11.0);
        let m = Font(&ui.painter_at(area), font);
        let text = input::yscale_badge_text(self.height_factor);
        let (pad_x, pad_y, margin) = (8.0, 4.0, 6.0);
        // the text, then the reset arrow (U+27F2 in Qt) a text height wide
        let (w, h) = (
            m.width(&text) + 4.0 + m.height() + 2.0 * pad_x,
            m.height() + 2.0 * pad_y,
        );
        let r = Rect::from_min_size(
            pos2(area.right() - w - margin, area.top() + margin),
            vec2(w, h),
        );
        let resp = ui.interact(r, ui.make_persistent_id("yscale"), egui::Sense::click());
        let mut clicked = false;
        if resp.clicked() {
            self.height_factor = 1.0;
            clicked = true;
        }
        if resp.hovered() || clicked {
            self.yscale_flash = Some(Instant::now());
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(40));
        Some((r, clicked, opacity as f32))
    }

    fn paint_yscale_badge(&self, p: &egui::Painter, r: Rect, opacity: f32) {
        let blue = Color32::from_rgb(17, 133, 209).gamma_multiply(opacity);
        let font = egui::FontId::proportional(11.0);
        p.rect(
            r,
            4.0,
            Color32::from_rgba_unmultiplied(24, 24, 24, 220).gamma_multiply(opacity),
            Stroke::new(1.0, blue),
            egui::StrokeKind::Inside,
        );
        let text = input::yscale_badge_text(self.height_factor);
        let g = p.layout_no_wrap(
            text,
            font.clone(),
            Color32::from_gray(220).gamma_multiply(opacity),
        );
        let (w, th) = (g.size().x, g.size().y);
        p.galley(
            pos2(r.left() + 8.0, r.center().y - th / 2.0),
            g,
            Color32::WHITE,
        );
        // an anticlockwise arrow: three quarters of a circle, head at the top
        let c = pos2(r.left() + 8.0 + w + 4.0 + th / 2.0, r.center().y);
        let rad = th * 0.32;
        let pts: Vec<Pos2> = (0..=24)
            .map(|i| {
                let a = std::f32::consts::PI * (-0.5 + 1.5 * i as f32 / 24.0);
                c + vec2(rad * a.cos(), -rad * a.sin())
            })
            .collect();
        let head = pts[0];
        p.add(egui::Shape::line(pts, Stroke::new(1.5, blue)));
        p.add(egui::Shape::convex_polygon(
            vec![
                head + vec2(-3.5, -3.0),
                head + vec2(-3.5, 3.0),
                head + vec2(1.5, 0.0),
            ],
            blue,
            Stroke::NONE,
        ));
    }

    /// `Viewport::paintProgress`: the logo in a circle, the received
    /// samples as a green arc; until data arrives, the blinking dots, the
    /// device's trigger status and its capture progress as a blue arc.
    fn paint_progress(
        &self,
        p: &egui::Painter,
        area: Rect,
        s: &Session,
        received: u64,
        limit: u64,
    ) {
        let (w, h) = (area.width(), area.height());
        let g = vprog::Geom::new(w, h);
        let at = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let green = Color32::from_rgb(0, 153, 37);
        let blue = Color32::from_rgb(17, 133, 209);
        let grey = Color32::from_rgb(160, 160, 164);
        let fore = Color32::from_gray(220);
        let arc = |r: f32, percent: f64, c: Color32| {
            let pts: Vec<Pos2> = g.arc(r, percent).iter().map(|&(x, y)| at(x, y)).collect();
            if pts.len() > 1 {
                p.add(egui::Shape::line(pts, Stroke::new(4.0, c)));
            }
        };
        let progress100 = vprog::progress100(received, limit);
        p.circle_stroke(at(g.cx, g.cy), g.radius - 2.0, Stroke::new(1.0, grey));
        arc(g.radius, progress100 as f64, green);
        for ((x, y0, y1), (dx, dy, r)) in g.logo().into_iter().zip(g.logo_dots()) {
            p.line_segment([at(x, y0), at(x, y1)], Stroke::new(1.0, grey));
            p.circle_stroke(at(dx, dy), r, Stroke::new(1.0, grey));
        }
        let mut captured = 0;
        if received == 0 {
            let (dots, r) = g.wait_dots(w, h);
            let lit = self
                .wait_since
                .map_or(0, |t| vprog::lit_dot(t.elapsed().as_millis() as u64));
            for (i, (x, y)) in dots.into_iter().enumerate() {
                let c = if i == lit {
                    fore
                } else {
                    fore.gamma_multiply(100.0 / 255.0)
                };
                p.circle_filled(at(x, y), r, c);
            }
            if let Some((triggered, progress)) = s.capture_status() {
                captured = progress;
                p.text(
                    at(g.cx, g.cy + g.radius * 0.4 + g.radius * 0.25),
                    egui::Align2::CENTER_CENTER,
                    vprog::status_text(triggered, progress),
                    egui::FontId::proportional(13.0),
                    blue,
                );
            }
        } else {
            p.text(
                at(g.cx, g.cy),
                egui::Align2::CENTER_CENTER,
                format!("{progress100}%"),
                egui::FontId::proportional(66.0),
                green,
            );
        }
        arc((g.radius - 4.0).max(0.0), captured as f64, blue);
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
    ) -> Vec<ChannelMark> {
        let mut marks = Vec::new();
        let st = &t.stack;
        let rate = (st.sample_rate() as f64).max(1.0);
        let spp = rate * self.vp.scale;
        let off = self.vp.x_offset;
        let font = egui::FontId::proportional(11.0);
        let m = Font(painter, font.clone());
        let (left, right) = (0.0, area.width() as f64);
        let start = ((left + off as f64) * spp).max(0.0) as u64;
        let end = ((right + off as f64) * spp).max(0.0) as u64;
        // the decode region (`paint_back`): blue lines with a triangle on
        // top, at the region's start and end
        {
            const W: f32 = 5.0; // ControlRectWidth
            let snap_spp = (rate * self.vp.scale).max(f64::MIN_POSITIVE);
            let (r0, r1) = st.region();
            let last = self.last_sample;
            let r1 = if r1 == u64::MAX { last } else { r1 };
            let top = area.top() + row.y - self.v_scroll - row.height / 2.0 - W;
            let blue = Color32::from_rgb(17, 133, 209);
            for smp in [r0, r1] {
                let x = area.left() + (smp as f64 / snap_spp - off as f64) as f32;
                if x < area.left() - W || x > area.right() + W {
                    continue;
                }
                painter.line_segment(
                    [pos2(x, top), pos2(x, top + row.height + W)],
                    Stroke::new(1.0, blue),
                );
                painter.add(egui::Shape::convex_polygon(
                    vec![pos2(x - W, top), pos2(x + W, top), pos2(x, top + W)],
                    blue,
                    Stroke::NONE,
                ));
            }
        }
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
            return marks;
        }
        let grey = Color32::from_gray(170);
        let mark = st.mark();
        let bottom = area.top() + row.y - self.v_scroll + row.height / 2.0 + 1.0;
        let blue = Color32::from_rgb(17, 133, 209);
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
                            let lines = a.lines_in(st.format());
                            let shape = vdec::annotation_shape(
                                a.start,
                                a.end,
                                &lines,
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
                            let px = |smp: u64| smp as f64 / spp - off as f64;
                            // the annotation chosen in the protocol list: a
                            // blue triangle under the trace
                            if !matches!(shape, AnnShape::None)
                                && mark == ((a.start + a.end) / 2) as i64
                            {
                                let x = (x0 as f64 + (px(a.start) + px(a.end)) / 2.0) as f32;
                                painter.add(egui::Shape::convex_polygon(
                                    vec![
                                        pos2(x, bottom),
                                        pos2(x + 2.5, bottom + 2.5),
                                        pos2(x - 2.5, bottom + 2.5),
                                    ],
                                    blue,
                                    Stroke::NONE,
                                ));
                            }
                            if let AnnShape::Range { start, end, .. } = &shape
                                && a.ty / 100 == 2
                                && end - start > 20.0
                            {
                                marks.extend(channel_marks(st, a.ty, *start, px(a.end), fill_c));
                            }
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
        marks
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
        // pinch zoom around the pointer
        if let Some(steps) = self.pinch.feed(&ui.input(zoom_factors))
            && let Some(p) = hover
            && self.vp.zoom(steps, (p.x - area.left()) as i32, tb)
        {
            self.pinch.applied();
            changed = true;
        }

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
                    self.yscale_flash = Some(Instant::now());
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
            // where the button went down (`mousePressEvent`), not where the
            // drag threshold was passed
            let p = ui
                .input(|i| i.pointer.press_origin())
                .or(resp.interact_pointer_pos())
                .unwrap_or_default();
            let x = (p.x - area.left()) as f64;
            let primary = ui.input(|i| i.pointer.primary_down());
            let grabbed = self
                .cursors
                .grab(x, |smp| self.vp.index_to_pixel(smp, false, tb));
            // the search cursor first (`HitCursorMargin`)
            let search = self
                .search
                .is_some_and(|(smp, _)| (self.vp.index_to_pixel(smp, false, tb) - x).abs() <= 10.0);
            self.action = match (primary, grabbed) {
                (true, _) if search => Action::Search,
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
                Action::Search => {
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
                    // the same snapping as a cursor (`set_search_pos(.., false)`)
                    let mut c = Cursors {
                        samples: vec![pos as u64],
                    };
                    c.drag(1, pos, pulse);
                    self.search = Some((c.samples[0], false));
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
        self.measure_row = None;
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
                    // measured whether or not the popup shows (the dock and
                    // the status bar read it)
                    if let Some(r) = self.row_at(rows, area, p.y)
                        && let Some(sig) = &r.sig
                    {
                        self.measure_row = Some((area.top() + r.y - self.v_scroll, r.height));
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
    /// The cursors' ruler labels (`Cursor::get_label_rect`): number and box
    /// in ruler coordinates, with the text shown (time or samples from the
    /// trigger).
    fn cursor_labels(
        &self,
        painter: &egui::Painter,
        s: &Session,
        ruler: Rect,
        tb: &Timebase,
    ) -> Vec<(usize, ruler::Bx, String)> {
        let trig = s.state().trigger_pos as i64;
        let m = Font(painter, egui::FontId::proportional(11.0));
        let (spp, width) = (tb.snap_samplerate * self.vp.scale, ruler.width() as f64);
        self.cursors
            .samples
            .iter()
            .enumerate()
            .filter_map(|(i, &smp)| {
                // off the view: no label
                let off = smp as f64 / spp;
                let x0 = self.vp.x_offset as f64;
                if off < x0 || off > x0 + width {
                    return None;
                }
                let text = if self.show_samples {
                    ruler::format_samples(smp as i64 - trig)
                } else {
                    ruler::format_real_time(smp as i64 - trig, tb.snap_samplerate as u64)
                };
                let x = self.vp.index_to_pixel(smp, false, tb) as i64;
                let r = ruler::cursor_label_rect(
                    x,
                    m.width(&text) as f64,
                    m.height() as f64,
                    ruler.height() as f64,
                );
                Some((i + 1, r, text))
            })
            .collect()
    }

    /// `Ruler::mousePressEvent` / `mouseReleaseEvent` / `leaveEvent`: drag a
    /// cursor by its label, delete it with its close box, or work the
    /// cursor strips (left click: add or move a cursor here; right click: go
    /// to a cursor, or delete one in the selector).
    pub(crate) fn ruler_input(
        &mut self,
        ui: &mut Ui,
        s: &Session,
        ruler: Rect,
        area: Rect,
        tb: &Timebase,
    ) -> bool {
        let resp = ui.interact(
            ruler,
            ui.make_persistent_id("ruler"),
            egui::Sense::click_and_drag(),
        );
        let h = ruler.height() as f64;
        let Some(p) = ui.input(|i| i.pointer.interact_pos()) else {
            return false;
        };
        let (lx, ly) = ((p.x - ruler.left()) as f64, (p.y - ruler.top()) as f64);
        let n = self.cursors.samples.len();
        let to_index = |x: f64| {
            self.vp
                .pixel_to_index(x.clamp(0.0, area.width() as f64), tb)
        };
        let (pressed, released, sec_released) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.secondary_released(),
            )
        });
        let mut act = ruler::RulerAction::None;
        let mut changed = false;
        if pressed && resp.hovered() && !self.strips.sel_visible && n > 0 {
            let labels = self.cursor_labels(&ui.painter_at(ruler), s, ruler, tb);
            for (k, r, _) in labels {
                if ruler::cursor_close_rect(r).contains(lx, ly) {
                    act = ruler::RulerAction::Delete(k);
                    self.ruler_hit = true;
                    break;
                }
                if r.contains(lx, ly) {
                    self.ruler_grab = Some(k);
                    self.strips.leave();
                    self.ruler_hit = true;
                    break;
                }
            }
        }
        if let Some(k) = self.ruler_grab
            && ui.input(|i| i.pointer.primary_down())
            && let Some(slot) = self.cursors.samples.get_mut(k - 1)
        {
            let at = to_index(lx);
            if *slot != at {
                *slot = at;
                changed = true;
            }
        }
        if released {
            if !self.ruler_hit && self.ruler_grab.is_none() && resp.hovered() {
                act = self.strips.left_release(lx, ly, n, h);
            }
            self.ruler_grab = None;
            self.ruler_hit = false;
        }
        if sec_released && resp.hovered() {
            act = self.strips.right_release(lx, ly, n, h);
        }
        if !resp.hovered() && self.ruler_grab.is_none() && self.strips.visible() {
            self.strips.leave();
        }
        match act {
            ruler::RulerAction::None => {}
            ruler::RulerAction::Add(x) => {
                self.cursors.add(to_index(x));
            }
            ruler::RulerAction::Move(k, x) => {
                if let Some(slot) = self.cursors.samples.get_mut(k - 1) {
                    *slot = to_index(x);
                }
            }
            ruler::RulerAction::Delete(k) => self.cursors.remove(k),
            ruler::RulerAction::GoTo(k) => {
                if let Some(&smp) = self.cursors.samples.get(k - 1) {
                    let off = smp as f64 / (tb.snap_samplerate * self.vp.scale)
                        - (self.vp.width / 2) as f64;
                    self.vp.set_scale_offset(self.vp.scale, off as i64, tb);
                }
            }
        }
        changed || act != ruler::RulerAction::None
    }

    /// The header: name (double-click renames), colour swatch (click picks),
    /// the five trigger squares of a logic trace, drag to reorder.
    fn header(&mut self, ui: &mut Ui, s: &Session, header: Rect, area: Rect, rows: &[Row]) -> bool {
        let mut changed = false;
        let (pressed, ctrl, pointer) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.modifiers.ctrl || i.modifiers.command,
                i.pointer.interact_pos(),
            )
        });
        // the traces' offsets (`get_v_offset`) in content coordinates
        let offset = |r: &Row| (r.y) as f64;
        for row in rows {
            let cy = area.top() + row.y - self.v_scroll;
            if cy < area.top() - 20.0 || cy > area.bottom() + 20.0 {
                continue;
            }
            let name = Rect::from_min_max(
                pos2(header.left() + 22.0, cy - 9.0),
                pos2(header.right() - 64.0, cy + 9.0),
            );
            let resp = ui.interact(
                name,
                ui.make_persistent_id(("name", row.r)),
                egui::Sense::click_and_drag(),
            );
            // `mousePressEvent` on the label: select it and drag it, with
            // Ctrl also the traces selected before
            if pressed
                && resp.hovered()
                && let Some(p) = pointer
            {
                let mut starts: Vec<(TraceRef, f64)> = Vec::new();
                if ctrl {
                    for r in rows.iter().filter(|r| self.selected.contains(&r.r)) {
                        starts.push((r.r, offset(r)));
                    }
                    if !self.selected.contains(&row.r) {
                        self.selected.push(row.r);
                    }
                } else {
                    self.selected = vec![row.r];
                }
                if !starts.iter().any(|x| x.0 == row.r) {
                    starts.push((row.r, offset(row)));
                }
                self.header_drag = Some(HeaderDrag {
                    down_y: p.y,
                    starts,
                    moved: None,
                });
            }
            let Some(sig) = &row.sig else { continue };
            let swatch = Rect::from_center_size(pos2(header.left() + 12.0, cy), vec2(12.0, 12.0));
            if resp.double_clicked() {
                self.rename = Some((sig.index, sig.ty, sig.name.clone()));
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
        // `mouseMoveEvent`: the dragged traces follow on the 10 px grid;
        // `mouseReleaseEvent`: a move renumbers the view order by y and
        // unselects all
        if let Some(d) = &mut self.header_drag {
            let released = ui.input(|i| i.pointer.any_released());
            if let Some(p) = pointer
                && let Some(m) = vlayout::header_drag(&d.starts, (p.y - d.down_y) as f64, ctrl)
            {
                d.moved = Some(m);
                changed = true;
            }
            if released {
                if let Some(moved) = self.header_drag.take().and_then(|d| d.moved) {
                    let mut order: Vec<(f64, TraceRef)> = rows
                        .iter()
                        .map(|x| {
                            let y = moved.iter().find(|m| m.0 == x.r).map_or(offset(x), |m| m.1);
                            (y, x.r)
                        })
                        .collect();
                    order.sort_by(|a, b| a.0.total_cmp(&b.0));
                    let refs: Vec<TraceRef> = order.into_iter().map(|x| x.1).collect();
                    s.set_view_order(&refs);
                    self.selected.clear();
                    changed = true;
                } else {
                    self.header_drag = None;
                }
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
        let top = area.top() - self.v_scroll;
        let drag_y = |r: TraceRef| self.header_drag.as_ref().and_then(|d| d.y_of(r, top));
        for row in rows {
            let cy = drag_y(row.r).unwrap_or(area.top() + row.y - self.v_scroll);
            if self.selected.contains(&row.r) {
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(header.left() + 2.0, cy - 10.0),
                        pos2(header.right() - 2.0, cy + 10.0),
                    ),
                    3.0,
                    Color32::from_rgba_unmultiplied(17, 133, 209, 60),
                );
            }
            if let Some((t, _)) = &row.dec {
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

    /// The cursors (dashed lines, `TimeMarker::paint`), their ruler labels
    /// (`Cursor::paint_label`), the trigger ("T") and search ("S") labels
    /// (`paint_fix_label`) and the open cursor strip (`draw_cursor_sel`).
    pub(crate) fn paint_cursors(
        &self,
        painter: &egui::Painter,
        s: &Session,
        ruler: Rect,
        area: Rect,
        tb: &Timebase,
    ) {
        let hover = painter.ctx().pointer_hover_pos();
        let local = |p: Pos2| ((p.x - ruler.left()) as f64, (p.y - ruler.top()) as f64);
        let rr = |b: ruler::Bx| {
            Rect::from_min_size(
                pos2(ruler.left() + b.x as f32, ruler.top() + b.y as f32),
                vec2(b.w as f32, b.h as f32),
            )
        };
        let orange = Color32::from_rgb(238, 178, 17);
        let red = Color32::from_rgb(213, 15, 37);
        let blue = Color32::from_rgb(17, 133, 209);
        let font = egui::FontId::proportional(11.0);
        let dashed = |x: f32, c: Color32, w: f32| {
            painter.extend(egui::Shape::dashed_line(
                &[pos2(x, area.top()), pos2(x, area.bottom())],
                Stroke::new(w, c),
                4.0,
                2.0,
            ));
        };
        // the label's arrow down to the ruler's bottom and the text under it
        let arrow = |r: Rect, c: Color32, below: &str, below_c: Color32| {
            let a = ruler::ARROW_SIZE as f32;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pos2(r.center().x - a, r.bottom()),
                    pos2(r.center().x + a, r.bottom()),
                    pos2(r.center().x, ruler.bottom()),
                ],
                c,
                Stroke::NONE,
            ));
            painter.text(
                pos2(r.center().x, r.bottom() + a / 2.0),
                egui::Align2::CENTER_CENTER,
                below,
                font.clone(),
                below_c,
            );
        };
        for (i, &smp) in self.cursors.samples.iter().enumerate() {
            let x = area.left() + self.vp.index_to_pixel(smp, false, tb) as f32;
            if x >= area.left() && x <= area.right() {
                let grabbed = self.ruler_grab == Some(i + 1)
                    || matches!(self.action, Action::Cursor(n) if n == i + 1);
                let c = hsva(i + 1);
                if grabbed {
                    dashed(x, c.gamma_multiply(1.5), 2.0);
                } else {
                    dashed(x, c, 1.0);
                }
            }
        }
        for (k, b, text) in self.cursor_labels(painter, s, ruler, tb) {
            let r = rr(b);
            let close = rr(ruler::cursor_close_rect(b));
            let (in_close, in_label) = hover.map_or((false, false), |p| {
                let (x, y) = local(p);
                (ruler::cursor_close_rect(b).contains(x, y), b.contains(x, y))
            });
            let c = hsva(k);
            let fill = if !in_close && in_label { orange } else { c };
            painter.rect_filled(r, 0.0, fill);
            arrow(r, fill, &k.to_string(), Color32::BLACK);
            painter.rect_filled(close, 0.0, if in_close { red } else { orange });
            let st = Stroke::new(1.0, Color32::BLACK);
            painter.line_segment([close.min + vec2(2.0, 2.0), close.max - vec2(2.0, 2.0)], st);
            painter.line_segment(
                [
                    pos2(close.left() + 2.0, close.bottom() - 2.0),
                    pos2(close.right() - 2.0, close.top() + 2.0),
                ],
                st,
            );
            painter.text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                text,
                font.clone(),
                Color32::BLACK,
            );
        }
        // fixed labels: the trigger (logic after a trigger, DSO always) and
        // the search cursor (blue on a hit), with the time for the latter
        let dso = s.mode() == dsview_core::Mode::Dso as i32;
        let (trig, show_trig) = {
            let st = s.state();
            (st.trigger_pos, st.trig_cursor)
        };
        let m = Font(painter, font.clone());
        let mut fixed = Vec::new();
        if show_trig || dso {
            fixed.push((
                trig,
                'T',
                Color32::from_rgba_unmultiplied(213, 15, 37, 200),
                None,
            ));
        }
        if let Some((pos, hit)) = self.search {
            let c = if hit {
                blue
            } else {
                Color32::from_rgba_unmultiplied(220, 220, 220, 100)
            };
            let text = if self.show_samples {
                ruler::format_samples(pos as i64)
            } else {
                ruler::format_real_time(pos as i64, tb.snap_samplerate as u64)
            };
            fixed.push((pos, 'S', c, Some(text)));
        }
        for (smp, label, c, text) in fixed {
            let x = self.vp.index_to_pixel(smp, false, tb);
            if x < 0.0 || x > area.width() as f64 {
                continue;
            }
            // the box is as wide as the time text even where none is shown
            let t = ruler::format_real_time(smp as i64, tb.snap_samplerate as u64);
            let b = ruler::cursor_label_rect(
                x as i64,
                m.width(text.as_deref().unwrap_or(&t)) as f64,
                m.height() as f64,
                ruler.height() as f64,
            );
            let r = rr(b);
            let ax = area.left() + x as f32;
            dashed(ax, c, 1.0);
            painter.rect_filled(r, 0.0, c);
            arrow(r, c, &label.to_string(), Color32::WHITE);
            if let Some(text) = text {
                painter.text(
                    r.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    font.clone(),
                    Color32::WHITE,
                );
            }
        }
        // the strip: box 0 adds (or, for go-to, points back), then a box per
        // cursor; orange under the pointer
        if self.strips.visible()
            && let Some(sx) = self.strips.sel_x
        {
            let h = ruler.height() as f64;
            let n = self.cursors.samples.len();
            let over = hover.and_then(|p| {
                let (x, y) = local(p);
                self.strips.hit(x, y, n, h)
            });
            let b0 = ruler::cursor_sel_rect(sx, 0, h);
            let o = |x: f64, y: f64| pos2(ruler.left() + x as f32, ruler.top() + y as f32);
            let (l, t, r, btm) = (b0.x, b0.y, b0.x + b0.w, b0.y + b0.h);
            let pts = if self.strips.go_visible {
                let w = ruler::CURSOR_SEL_WIDTH;
                vec![
                    o(sx + w / 2.0, t + w / 2.0),
                    o((l + r) / 2.0, t),
                    o(l, t),
                    o(l, btm),
                    o((l + r) / 2.0, btm),
                ]
            } else {
                vec![o(sx, h), o(l, btm), o(l, t), o(r, t), o(r, btm)]
            };
            let fill0 = if over == Some(0) { orange } else { blue };
            painter.add(egui::Shape::convex_polygon(pts, fill0, Stroke::NONE));
            for k in 1..=n {
                let br = rr(ruler::cursor_sel_rect(sx, k, h));
                let fill = if over == Some(k) { orange } else { hsva(k) };
                painter.rect_filled(br, 0.0, fill);
                painter.extend(egui::Shape::dashed_line(
                    &[
                        pos2(br.left(), br.top() + 3.0),
                        pos2(br.left(), br.bottom() - 3.0),
                    ],
                    Stroke::new(1.0, Color32::BLACK),
                    1.0,
                    1.0,
                ));
                painter.text(
                    br.center(),
                    egui::Align2::CENTER_CENTER,
                    k.to_string(),
                    font.clone(),
                    Color32::BLACK,
                );
            }
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
                if area.contains(p) {
                    self.paint_measure(painter, area, p, tb);
                }
            }
        }
    }

    /// `Viewport::paintMeasure` (logic): the dashed line at the pointer;
    /// over a pulse its width and period arrows and edge lines, and with
    /// the floating measurement on, the popup with width, period, frequency
    /// and duty cycle.
    fn paint_measure(&self, painter: &egui::Painter, area: Rect, p: Pos2, tb: &Timebase) {
        let dashed = |x: f32, y0: f32, y1: f32, c: Color32| {
            painter.extend(egui::Shape::dashed_line(
                &[pos2(x, y0), pos2(x, y1)],
                Stroke::new(1.0, c),
                4.0,
                2.0,
            ));
        };
        dashed(
            p.x,
            area.top(),
            area.bottom(),
            Color32::from_rgba_unmultiplied(220, 220, 220, 140),
        );
        let (Some(m), Some((mid_y, height))) = (&self.measure, self.measure_row) else {
            return;
        };
        let active = Color32::from_rgb(238, 178, 17);
        let active2 = Color32::from_rgb(255, 255, 0);
        let rising = Color32::from_rgba_unmultiplied(213, 15, 37, 200);
        let falling = Color32::from_rgba_unmultiplied(17, 133, 209, 200);
        let x_of = |i: u64| (area.left() as f64 + self.vp.index_to_pixel(i, false, tb)) as f32;
        let (pre, aft, thd) = (x_of(m.start), x_of(m.end), x_of(m.next));
        let (top, bottom) = (mid_y - height / 2.0, mid_y + height / 2.0);
        // an arrow between x1 and x2, the heads outside when it is short
        let arrow = |x1: f32, x2: f32, y: f32, c: Color32| {
            const HEAD: f32 = 2.0;
            let o = if x2 - x1 < 3.0 * HEAD + 1.0 {
                -HEAD
            } else {
                HEAD
            };
            let st = Stroke::new(1.0, c);
            painter.line_segment([pos2(x1, y), pos2(x2, y)], st);
            painter.line_segment([pos2(x1, y), pos2(x1 + o, y - o)], st);
            painter.line_segment([pos2(x1, y), pos2(x1 + o, y + o)], st);
            painter.line_segment([pos2(x2 - o, y - o), pos2(x2, y)], st);
            painter.line_segment([pos2(x2 - o, y + o), pos2(x2, y)], st);
        };
        arrow(pre, aft, mid_y - 4.0, active);
        let (c0, c1) = if m.high {
            (rising, falling)
        } else {
            (falling, rising)
        };
        dashed(pre, area.top(), area.bottom(), c0);
        painter.vline(pre, top..=bottom, Stroke::new(1.0, active2));
        dashed(aft, area.top(), area.bottom(), c1);
        painter.vline(aft, top..=bottom, Stroke::new(1.0, active));
        if m.next != 0 {
            arrow(pre, thd, mid_y + 4.0, active2);
            dashed(thd, area.top(), area.bottom(), c0);
            painter.vline(thd, top..=bottom, Stroke::new(1.0, active2));
        }
        if !self.measure_enabled {
            return;
        }
        let font = egui::FontId::proportional(11.0);
        let f = Font(painter, font.clone());
        let labels = ["Width: ", "Period: ", "Frequency: ", "Duty Cycle: "];
        let values = [&m.width_time, &m.period_time, &m.frequency, &m.duty];
        let vw = values.iter().map(|v| f.width(v)).fold(0.0, f32::max);
        let lw = labels.iter().map(|l| f.width(l)).fold(0.0, f32::max);
        let w = vw + lw + 40.0;
        let (box_h, pad, row_h) = (140.0, 5.0, 20.0);
        // `MousePointerClearance`, `MouseEdgeClearance`
        let (pointer_gap, edge_gap) = (25.0, 3.0);
        let (lx, ly) = (p.x - area.left(), p.y - area.top());
        let (aw, ah) = (area.width(), area.height());
        let mut hx = if lx + w > aw {
            lx - w - edge_gap
        } else {
            lx + pointer_gap
        };
        let hy = if ly + box_h > ah {
            if lx + w <= aw {
                hx = lx + edge_gap;
            }
            ly - box_h - pointer_gap
        } else {
            ly + pointer_gap
        };
        let org = pos2(area.left() + hx, area.top() + hy);
        painter.rect_filled(
            Rect::from_min_size(org, vec2(w, box_h)),
            0.0,
            Color32::from_rgba_unmultiplied(17, 133, 209, 200),
        );
        let samples = |v: &str| format!("{v} Samples");
        let rows: [(f32, Option<&str>, String, Color32); 6] = [
            (pad, Some(labels[0]), m.width_time.clone(), active),
            (25.0, None, samples(&m.width_samples), active),
            (50.0, Some(labels[1]), m.period_time.clone(), active2),
            (70.0, None, samples(&m.period_samples), active2),
            (95.0, Some(labels[2]), m.frequency.clone(), active),
            (115.0, Some(labels[3]), m.duty.clone(), active),
        ];
        for (dy, label, value, c) in rows {
            let y = org.y + dy + row_h / 2.0;
            if let Some(l) = label {
                painter.text(
                    pos2(org.x + pad, y),
                    egui::Align2::LEFT_CENTER,
                    l,
                    font.clone(),
                    c,
                );
            }
            painter.text(
                pos2(org.x + w - pad, y),
                egui::Align2::RIGHT_CENTER,
                value,
                font.clone(),
                c,
            );
        }
    }

    /// `show_region` from the control API.
    pub fn show_region(&mut self, s: &Session, start: u64, end: u64) {
        let tb = timebase(s);
        self.vp.show_region(start, end, self.map_zoom_fixed, &tb);
    }

    /// The logic traces in view order with their centres' screen y.
    pub fn shown_rows(&self) -> &[(TraceRef, f32)] {
        &self.shown_rows
    }

    /// `set_cursor_middle`: sample `index` in the middle, the scale kept.
    pub fn center_on(&mut self, s: &Session, index: u64) {
        let tb = timebase(s);
        let off = index as f64 / (tb.snap_samplerate * self.vp.scale) - (self.vp.width / 2) as f64;
        self.vp.set_scale_offset(self.vp.scale, off as i64, &tb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsview_session::Paths;
    use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};
    use egui_kittest::Harness;

    struct NoUsb;

    impl UsbBackend for NoUsb {
        fn list(&self) -> Result<Vec<Info>, UsbError> {
            Ok(Vec::new())
        }
        fn open(&self, _: u8, _: u8, _: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
            Err(UsbError::NotFound)
        }
    }

    /// The progress screen before any data (the demo reports no trigger
    /// status) and with 40% of the samples received.
    #[test]
    fn progress_screen() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let s = Session::new(
            Paths {
                data_dir: root.join("DSView"),
                profile_dir: None,
                version: "test".into(),
            },
            Arc::new(NoUsb),
        );
        let mut results = egui_kittest::SnapshotResults::new();
        for (received, name) in [(0, "progress_waiting"), (400, "progress_receiving")] {
            let tv = TimeView::default();
            let s2 = s.clone();
            let mut h = Harness::builder()
                .with_size(egui::vec2(600.0, 360.0))
                .wgpu()
                .build_ui(move |ui| {
                    let r = ui.max_rect();
                    ui.painter().rect_filled(r, 0.0, Color32::from_gray(24));
                    tv.paint_progress(&ui.painter_at(r), r, &s2, received, 1000);
                });
            h.run_steps(2);
            h.snapshot(name);
            results.extend_harness(&mut h);
        }
        s.exit();
    }
}
