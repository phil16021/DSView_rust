//! The time view in oscilloscope mode (`DsoSignal` in `pv/view`): the
//! grid, the traces, zero lines and trigger levels, the channel controls in
//! the header (V/div dial, probe factor, enable, coupling), the hover value;
//! wheel zoom steps the timebase.

use crate::i18n::tr;
use dsview_core::{ConfValue, channel_type, key};
use dsview_session::Session;
use dsview_session::session::Signal;
use dsview_viewmodel::dso::{self, Geom, IRect, TraceShape, Volts, WheelAccumulator};
use dsview_viewmodel::input::{self, Mods, WheelAction};
use dsview_viewmodel::layout::{self, LayoutParams, Slot, TraceKind};
use dsview_viewmodel::viewport::Timebase;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Ui, pos2, vec2};

use crate::timeview::{HEADER_WIDTH, RULER_HEIGHT, TimeView, timebase};

/// What the view keeps per DSO channel (index 0-3).
#[derive(Clone, Copy, Debug, Default)]
struct Chan {
    /// `set_data_scale`: the trace's scale when the capture started.
    data_scale: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
    Pan {
        last_x: f32,
    },
    /// The zero line of channel `index` (the header label).
    Zero {
        index: u16,
        y0: f32,
        zero0: i32,
    },
    /// The trigger level of channel `index`.
    Trig {
        index: u16,
    },
}

#[derive(Default)]
pub struct DsoState {
    /// The DSO channel whose V/div dial ↑/↓ steps (`vDialActive`, keys 0/1).
    pub active_dial: Option<u16>,
    chans: [Chan; 4],
    zoom: WheelAccumulator,
    drag: Option<Drag>,
    /// `get_dso_trig_moved()`: the trigger level line shows while dragged.
    trig_moved: bool,
    started_seq: u64,
    /// `get_dso_split_channels()`: one band per channel.
    pub split: bool,
    /// The measurement bar's ten slots (`ViewStatus::_mrects`): channel
    /// (-1: none) and `DSO_MEASURE_TYPE` (0: none).
    pub measures: [(i32, usize); 10],
    /// Which slot's chooser is open.
    measure_pick: Option<usize>,
    /// Measurements per channel, for the data sequence they were made for.
    measure_cache: Vec<(i32, u64, serde_json::Value)>,
    /// The value under the mouse: channel, sample, code, text.
    pub hover: Option<(u16, u64, u8, String)>,
    /// The math trace.
    pub math: crate::mathview::MathState,
    /// The Lissajous figure.
    pub lissajous: crate::lissajousview::LissajousState,
    /// Reference waveforms.
    pub refs: crate::refview::RefState,
    /// The X measurement (double click, then two clicks).
    pub xm: Option<dso::XMeasure>,
    /// Persistence of the frames before (`dsoPersistenceMs`).
    pub persist: Persist,
    /// The histogram and channel measure dialogs.
    pub analysis: crate::analysisdlg::AnalysisDialogs,
    /// Auto-set per channel, and the frames still to skip after a step
    /// (`data_auto_lock`) with the frame last counted.
    pub auto: [dsview_viewmodel::autoset::AutoSet; 4],
    auto_lock: u32,
    lock_seq: u64,
    /// The math trace's header row (`get_y()`), when shown.
    math_y: Option<i32>,
}

impl DsoState {
    pub fn new() -> DsoState {
        DsoState {
            zoom: WheelAccumulator::new(1.0),
            started_seq: u64::MAX,
            measures: [(-1, 0); 10],
            ..DsoState::default()
        }
    }
}

/// A DSO channel as the frame sees it.
#[derive(Clone, Debug)]
pub(crate) struct ChanView {
    pub(crate) sig: Signal,
    /// Its position in the device's channel list (for per-channel config).
    pub(crate) pos: usize,
    pub(crate) enabled: bool,
    pub(crate) vdiv: u64,
    pub(crate) factor: u64,
    coupling: u8,
    zero_offset: u16,
    pub(crate) hw_offset: i32,
    trig_value: u8,
    pub(crate) geom: Geom,
    /// The header row's centre (`get_y()`), in trace-area coordinates.
    y: i32,
    pub(crate) colour: Color32,
    /// The device's V/div list (the dial's entries).
    pub(crate) vdivs: std::sync::Arc<Vec<u64>>,
    /// The V/div the shown data was taken at (the session's `cap_vdiv`),
    /// 0 before a capture.
    pub(crate) data_vdiv: u64,
}

impl ChanView {
    pub(crate) fn zero_y(&self) -> i32 {
        self.geom.zero_vpos(self.zero_offset)
    }
}

pub(crate) fn colour(sig: &Signal) -> Color32 {
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
    channel_layout(s, area_w, area_h, st).0
}

/// The channels and the math trace's header row.
fn channel_layout(
    s: &Session,
    area_w: i32,
    area_h: i32,
    st: &DsoState,
) -> (Vec<ChanView>, Option<i32>) {
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
    let enabled = |sig: &Signal| {
        chans
            .iter()
            .any(|c| c.index == sig.index && c.ty == channel_type::DSO && c.enabled)
    };
    let mut slots: Vec<Slot> = signals
        .iter()
        .map(|x| Slot {
            kind: TraceKind::Dso,
            rows: 1,
            enabled: enabled(x),
            view_index: x.view_index,
        })
        .collect();
    // the math trace takes a header row after the channels
    if st.math.enabled {
        slots.push(Slot {
            kind: TraceKind::Dso,
            rows: 1,
            enabled: true,
            view_index: i32::MAX,
        });
    }
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
        let Some(sig) = signals.get(p.slot).cloned() else {
            continue;
        };
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
        let vdiv = get(key::PROBE_VDIV).map_or(c.vdiv, |v| v.get_u64());
        let data_vdiv = s
            .state()
            .cap_vdiv
            .iter()
            .find(|x| x.0 == sig.index)
            .map_or(0, |x| x.1);
        // `_stop_scale` (F31): data taken at another V/div than the dial's
        // shows at the dial's scale, whichever way V/div changed (dial,
        // device options, auto-set, control socket)
        let stop = if data_vdiv > 0 && vdiv > 0 {
            (data_vdiv as f64 / vdiv as f64) as f32
        } else {
            1.0
        };
        geom.set_scale(rect.h, stop);
        out.push(ChanView {
            pos,
            enabled: c.enabled,
            vdiv,
            factor: get(key::PROBE_FACTOR).map_or(1, |v| v.get_u64()).max(1),
            coupling: get(key::PROBE_COUPLING).map_or(c.coupling, |v| v.get_byte()),
            zero_offset: get(key::PROBE_OFFSET).map_or(c.offset, |v| v.get_u16()),
            hw_offset: get(key::PROBE_HW_OFFSET).map_or(c.hw_offset, |v| v.get_u16()) as i32,
            trig_value: get(key::TRIGGER_VALUE).map_or(c.trig_value, |v| v.get_byte()),
            geom,
            y,
            colour: colour(&sig),
            vdivs: vdivs.clone(),
            data_vdiv,
            sig,
        });
    }
    let math_y = l
        .placed
        .iter()
        .find(|p| p.slot == signals.len())
        .map(|p| p.v_offset as i32);
    (out, math_y)
}

impl TimeView {
    /// Keys 0/1: channel `index`'s dial becomes the active one, or none if
    /// it was; a disabled channel's dial cannot be active
    /// (`set_vDialActive`).
    pub fn toggle_dial(&mut self, s: &Session, index: u16) {
        let enabled = s
            .channels()
            .iter()
            .any(|c| c.ty == channel_type::DSO && c.index == index && c.enabled);
        self.dso.active_dial = if self.dso.active_dial == Some(index) || !enabled {
            None
        } else {
            Some(index)
        };
    }

    /// ↑/↓: the active dial one V/div step up or down (`go_vDialNext`/`Pre`).
    pub fn step_active_dial(&mut self, s: &Session, up: bool) -> bool {
        let Some(index) = self.dso.active_dial else {
            return false;
        };
        let chans = channel_views(s, 1000, 600, &self.dso);
        let Some(c) = chans.iter().find(|c| c.sig.index == index && c.enabled) else {
            return false;
        };
        step_vdiv(s, c, up)
    }
}

/// `go_vDialPre` / `go_vDialNext`: one step of the V/div dial; while
/// stopped the shown trace stretches with it (`channel_views`).
fn step_vdiv(s: &Session, c: &ChanView, up: bool) -> bool {
    let list = &c.vdivs;
    let Some(i) = list.iter().position(|&v| v == c.vdiv) else {
        return false;
    };
    let next = if up { i + 1 } else { i.wrapping_sub(1) };
    let Some(&v) = list.get(next) else {
        return false;
    };
    s.set_config(key::PROBE_VDIV, ConfValue::U64(v), Some(c.pos));
    s.set_config(
        key::PROBE_OFFSET,
        ConfValue::U16(c.zero_offset),
        Some(c.pos),
    );
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
        let (mut chans, math_y) = channel_layout(s, aw, ah, &self.dso);
        self.dso.math_y = math_y;
        if started != self.dso.started_seq {
            self.dso.started_seq = started;
            for c in &chans {
                if let Some(ch) = self.dso.chans.get_mut(c.sig.index as usize) {
                    *ch = Chan {
                        data_scale: (c.geom.rect.h as f64 / (c.geom.ref_max - c.geom.ref_min))
                            as f32,
                    };
                }
            }
            chans = channel_views(s, aw, ah, &self.dso);
        }

        // the trigger's sub-sample offset (`paint_prepare`)
        self.update_trig_hoff(s, &chans);
        self.dso_is_hardware = s.state().device.as_ref().is_some_and(|d| d.is_hardware());
        let now = ui.input(|i| (i.time * 1000.0) as i64);
        self.run_autoset(s, &chans, now);

        let mut changed = self.dso_input(ui, s, &tb, area, rect, &chans);
        changed |= self.dso_header(ui, s, header, area, &chans);
        changed |= self.ruler_input(ui, s, ruler_rect, area, &tb);
        changed |= self.math_header(ui, s, header, area, rect, self.dso.math_y);

        // paint
        let painter = ui.painter_at(full);
        painter.rect_filled(full, 0.0, self.palette.bg);
        painter.rect_filled(header, 0.0, self.palette.header);
        painter.rect_filled(ruler_rect, 0.0, self.palette.ruler);
        let tpainter = ui.painter_at(area);
        if self.dso.lissajous.enabled {
            // the figure replaces the grid and the traces
            self.dso
                .lissajous
                .paint(&tpainter, s, area, rect.w, &self.palette);
        } else {
            if self.dso.split {
                for c in &chans {
                    self.paint_grid(&tpainter, area, c.geom.rect, &tb);
                }
            } else {
                self.paint_grid(&tpainter, area, rect, &tb);
            }
            self.paint_dso_traces(&tpainter, s, area, rect, &tb, &chans);
            self.paint_math(&tpainter, s, area, rect, &tb, &chans);
        }
        // hardware data past the ADC range (`dso_data_is_out_off_range`)
        if self.dso_is_hardware
            && s.data()
                .dso
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .data_out_off_range()
        {
            tpainter.text(
                pos2(area.left() + rect.w as f32, area.top() + rect.y as f32),
                egui::Align2::RIGHT_TOP,
                "Out off range! ",
                self.font(13.0),
                Color32::from_rgba_unmultiplied(255, 0, 0, 200),
            );
        }
        // frozen references over the live traces
        self.paint_refs(&tpainter, area, &chans, rect.w);
        let ctx = ui.ctx().clone();
        let mut refs = std::mem::take(&mut self.dso.refs);
        refs.dialog(&ctx, s, &chans);
        self.dso.refs = refs;
        let mut analysis = std::mem::take(&mut self.dso.analysis);
        analysis.show(&ctx, s, &chans);
        self.dso.analysis = analysis;
        self.paint_dso_fore(&tpainter, area, rect, &tb, &chans);
        self.paint_ruler(&painter, ruler_rect, s, &tb);
        self.paint_cursors(&painter, s, ruler_rect, area, &tb);
        self.paint_dso_header(
            &painter,
            header,
            area,
            &chans,
            ui.input(|i| i.pointer.hover_pos()),
        );
        self.paint_math_header(&painter, header, area, rect, self.dso.math_y);
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
            && let Some(h) =
                dso::trig_hoff(&buf[..n.min(buf.len())], trig_index, c.trig_value, slope)
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
        self.dso_mouse = hover;
        if let Some(steps) = self.pinch.feed(&ui.input(crate::timeview::zoom_factors))
            && let Some(p) = hover
            && self.dso_zoom(s, steps, (p.x - area.left()) as i32, now)
        {
            self.pinch.applied();
            changed = true;
        }
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
                        changed |=
                            self.vp
                                .set_scale_offset(self.vp.scale, self.vp.x_offset + dx, tb);
                        self.dso.drag = Some(Drag::Pan { last_x: p.x });
                    }
                }
                Some(Drag::Trig { index }) => {
                    if let Some(c) = chans.iter().find(|c| c.sig.index == index) {
                        let ratio = c.geom.pos2ratio(local(p).1 as i32);
                        let v = c
                            .geom
                            .trig_value_for(ratio, c.zero_offset, is_hardware_logic(s));
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

        // the X measurement: a double click starts it (or clears a finished
        // one), two clicks complete it, a right click drops it
        let have_data = s
            .data()
            .dso
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sample_count()
            > 0;
        if have_data
            && resp.double_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let (x, y) = local(p);
            self.dso.xm = match self.dso.xm {
                Some(_) => None,
                None => chans
                    .iter()
                    .any(|c| c.geom.rect.contains(x as f64, y as f64))
                    .then(|| dso::XMeasure::start(self.vp.pixel_to_index(x as f64, tb), y)),
            };
            changed = true;
        } else if let Some(m) = &mut self.dso.xm
            && !m.is_complete()
        {
            if resp.clicked()
                && let Some(p) = resp.interact_pointer_pos()
            {
                m.click(self.vp.pixel_to_index(local(p).0 as f64, tb));
                changed = true;
            } else if resp.secondary_clicked() {
                self.dso.xm = None;
                changed = true;
            } else if let Some(p) = resp.hover_pos() {
                // leaving the traces drops an unfinished measurement
                let (x, y) = local(p);
                if !chans
                    .iter()
                    .any(|c| c.geom.rect.contains(x as f64, y as f64))
                {
                    self.dso.xm = None;
                }
            }
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
        self.dso_timebase_steps(s, knob, px)
    }

    /// ←/→ in DSO mode: one timebase step around the middle.
    pub(crate) fn dso_key_zoom(&mut self, s: &Session, steps: f64) -> bool {
        if s.is_working() && s.state().instant {
            return false;
        }
        let px = self.vp.width / 2;
        self.dso_timebase_steps(s, steps.signum() as i32, px)
    }

    /// `knob` timebase steps (positive: shorter, zoomed in) around pixel
    /// `px`.
    fn dso_timebase_steps(&mut self, s: &Session, knob: i32, px: i32) -> bool {
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
        tb.dso_view_time =
            Some(s.config_u64(key::TIMEBASE).unwrap_or(0) as f64 * dso::HDIVS as f64 / 1e9);
        self.vp.pre_scale = self.vp.scale;
        self.vp.pre_offset = self.vp.x_offset;
        if let Some(t) = tb.dso_view_time {
            self.vp.scale = t / self.vp.width.max(1) as f64;
        }
        self.vp.zoom_offset(px, &tb);
        true
    }

    fn volts(&self, c: &ChanView) -> Volts {
        let ch = self
            .dso
            .chans
            .get(c.sig.index as usize)
            .copied()
            .unwrap_or_default();
        Volts {
            vdiv_mv: if c.data_vdiv == 0 {
                c.vdiv
            } else {
                c.data_vdiv
            },
            data_scale: if ch.data_scale == 0.0 {
                c.geom.scale
            } else {
                ch.data_scale
            },
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
        let fore = self.palette.text;
        let back_a = crate::theme::alpha(self.palette.text, 100);
        let f = g.frame;
        let fr = Rect::from_min_size(o(f.x as f32, f.y as f32), vec2(f.w as f32, f.h as f32));
        p.rect_filled(fr, 0.0, self.palette.deep);
        p.rect_stroke(fr, 0.0, Stroke::new(1.0, back_a), egui::StrokeKind::Inside);
        let dashed = Stroke::new(1.0, crate::theme::alpha(self.palette.text, 60));
        for &(x0, y0, x1, y1) in &g.dividers {
            p.extend(egui::Shape::dashed_line(
                &[o(x0, y0), o(x1, y1)],
                dashed,
                4.0,
                2.0,
            ));
        }
        for &(x0, y0, x1, y1) in &g.ticks {
            p.line_segment([o(x0, y0), o(x1, y1)], dashed);
        }
        let st = Stroke::new(1.0, fore);
        for &(x0, y0, x1, y1) in &g.rails {
            p.line_segment([o(x0, y0), o(x1, y1)], st);
        }
        for b in &g.brackets {
            p.add(egui::Shape::line(
                b.iter().map(|&(x, y)| o(x, y)).collect(),
                st,
            ));
        }
        let (x, y, w, h) = g.shown;
        p.rect_filled(Rect::from_min_size(o(x, y), vec2(w, h)), 0.0, fore);
    }

    fn paint_dso_traces(
        &mut self,
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
        let mut frame: Vec<egui::Shape> = Vec::new();
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
                    .and_then(|b| {
                        dso::trace_shape(
                            &c.geom,
                            b,
                            &sp,
                            zero_y,
                            c.hw_offset,
                            0,
                            self.dso_line_width,
                        )
                    })
            };
            let col = c.colour;
            match shape {
                Some(TraceShape::Bars(rects)) => {
                    let mut mesh = egui::Mesh::default();
                    for r in rects {
                        mesh.add_colored_rect(
                            Rect::from_min_size(
                                o(r.x as f32, r.y as f32),
                                vec2(r.w as f32, r.h as f32),
                            ),
                            col,
                        );
                    }
                    frame.push(egui::Shape::mesh(mesh));
                }
                Some(TraceShape::Envelope(rects)) => {
                    let mut mesh = egui::Mesh::default();
                    for (x, t, w, h) in rects {
                        let (y0, y1) = if h >= 0.0 { (t, t + h) } else { (t + h, t) };
                        mesh.add_colored_rect(Rect::from_min_max(o(x, y0), o(x + w, y1)), col);
                    }
                    frame.push(egui::Shape::mesh(mesh));
                }
                Some(TraceShape::Polyline { points, dots, dot }) => {
                    let pts: Vec<Pos2> = points.iter().map(|&(x, y)| o(x, y)).collect();
                    for &q in pts.iter().take(dots) {
                        frame.push(egui::Shape::circle_filled(q, dot as f32 / 2.0, col));
                    }
                    frame.push(egui::Shape::line(
                        pts,
                        Stroke::new(self.dso_line_width, col),
                    ));
                }
                None => {}
            }
        }
        drop(store);
        // persistence: the frames before fade out under the current one
        let seq = s.state().dso_seq;
        let sig = persist_signature(chans, area);
        self.dso
            .persist
            .paint(p, area, seq, sig, self.vp.scale, self.vp.x_offset, frame);
    }

    /// `paint_fore`: zero lines, trigger levels and the hover point.
    fn paint_dso_fore(
        &self,
        p: &egui::Painter,
        area: Rect,
        rect: IRect,
        tb: &Timebase,
        chans: &[ChanView],
    ) {
        let o = |x: f32, y: f32| pos2(area.left() + x, area.top() + y);
        let font = self.font(12.0);
        let fore = self.palette.text;
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
            p.text(
                o(rect.w as f32, trigp),
                egui::Align2::RIGHT_CENTER,
                t,
                font.clone(),
                fore,
            );
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
        // the X measurement, in red dotted lines (`paintMeasure`)
        if let Some(m) = &self.dso.xm {
            let red = Stroke::new(1.0, Color32::RED);
            let dot = |q: &[Pos2]| egui::Shape::dotted_line(q, Color32::RED, 3.0, 0.6);
            let px = |i: usize| self.vp.index_to_pixel(m.index[i], false, tb) as f32;
            let y = m.y;
            p.extend(dot(&[o(px(0), y - 10.0), o(px(0), y + 10.0)]));
            let (w, rest) = m.texts(tb.snap_samplerate as u64);
            if let Some(w) = w {
                p.extend(dot(&[o(px(1), y - 10.0), o(px(1), y + 10.0)]));
                p.extend(dot(&[o(px(0), y), o(px(1), y)]));
                p.text(
                    o(px(0) + 10.0, y),
                    egui::Align2::LEFT_BOTTOM,
                    w,
                    font.clone(),
                    Color32::RED,
                );
            }
            if let Some([per, freq, duty]) = rest {
                p.extend(dot(&[o(px(0), y + 20.0), o(px(0), y + 40.0)]));
                p.extend(dot(&[o(px(0), y + 30.0), o(px(2), y + 30.0)]));
                p.extend(dot(&[o(px(2), y + 20.0), o(px(2), y + 40.0)]));
                let pr = p.text(
                    o(px(0) + 10.0, y + 30.0),
                    egui::Align2::LEFT_BOTTOM,
                    per,
                    font.clone(),
                    Color32::RED,
                );
                p.text(
                    pos2(pr.right() + 10.0, area.top() + y + 30.0),
                    egui::Align2::LEFT_BOTTOM,
                    freq,
                    font.clone(),
                    Color32::RED,
                );
                p.text(
                    o(px(1) + 10.0, y),
                    egui::Align2::LEFT_CENTER,
                    duty,
                    font.clone(),
                    Color32::RED,
                );
            } else if let Some(mouse) = self.dso_mouse {
                // the rubber line to the pointer
                p.line_segment([o(px(m.stage - 1), y), pos2(mouse.x, area.top() + y)], red);
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
            p.rect_filled(r, 0.0, self.palette.bg);
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
    fn dso_header(
        &mut self,
        ui: &mut Ui,
        s: &Session,
        header: Rect,
        area: Rect,
        chans: &[ChanView],
    ) -> bool {
        let mut changed = false;
        let is_file = s.state().device.as_ref().is_some_and(|d| d.is_file());
        let ho = |r: dso::FRect| {
            Rect::from_min_size(pos2(header.left() + r.x, area.top() + r.y), vec2(r.w, r.h))
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
                changed |= step_vdiv(s, c, p.y > dial.center().y);
            }
            if resp.hovered() {
                // mouse_wheel: shift = delta / 120
                let dy = ui.input(|i| crate::timeview::wheel_angle(i).1) as f64 / 120.0;
                if dy > 0.5 {
                    changed |= step_vdiv(s, c, false);
                } else if dy < -0.5 {
                    changed |= step_vdiv(s, c, true);
                }
            }
            // AUTO (hardware only): starts auto-set while running
            if s.state().device.as_ref().is_some_and(|d| d.is_hardware())
                && click(ui, hr.auto, "auto")
            {
                let now = ui.input(|i| (i.time * 1000.0) as i64);
                let running = s.is_working();
                if let Some(a) = self.dso.auto.get_mut(c.sig.index as usize) {
                    let acts = a.start(running, now);
                    self.auto_actions(s, c, &acts, now);
                }
                changed = true;
            }
            if click(ui, hr.coupling, "acdc") && !is_file {
                s.set_config(
                    key::PROBE_COUPLING,
                    ConfValue::Byte((c.coupling + 1) % 2),
                    Some(c.pos),
                );
                changed = true;
            }
            for (r, f, name) in [
                (hr.x1, 1u64, "x1"),
                (hr.x10, 10, "x10"),
                (hr.x100, 100, "x100"),
            ] {
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
                let tv =
                    c.geom
                        .trig_value_for(trig_delta + c.geom.zero_ratio(zero), zero, hw_logic);
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
        let font = self.font(12.0);
        let fore = self.palette.text;
        let fore_back = crate::theme::alpha(self.palette.text, 100);
        let ho = |r: dso::FRect| {
            Rect::from_min_size(pos2(header.left() + r.x, area.top() + r.y), vec2(r.w, r.h))
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
                    [
                        pos2(ctr.x + dx * (r + 3.0), ctr.y + dy * (r + 3.0)),
                        pos2(ctr.x + dx * (r + 8.0), ctr.y + dy * (r + 8.0)),
                    ],
                    st,
                );
            }
            let (dx, dy) = dso::dial_direction(sel as f64 / (count - 1).max(1) as f64);
            p.circle_filled(ctr, 3.0, col);
            p.line_segment(
                [ctr, pos2(ctr.x + dx * (r - 3.0), ctr.y + dy * (r - 3.0))],
                Stroke::new(2.0, col),
            );
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
                    pos2(
                        dial.right() + 4.0,
                        ctr.y + if up { -r / 2.0 } else { r / 2.0 },
                    ),
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
                p.text(
                    rr.center(),
                    egui::Align2::CENTER_CENTER,
                    text,
                    font.clone(),
                    Color32::WHITE,
                );
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
            if self.dso_is_hardware {
                let on = self
                    .dso
                    .auto
                    .get(c.sig.index as usize)
                    .is_some_and(|a| a.active());
                button(hr.auto, c.enabled && !on, "AUTO");
            }
            // the zero label
            if c.enabled {
                let l = ho(hr.label);
                let zy = area.top() + c.zero_y() as f32;
                p.add(egui::Shape::convex_polygon(
                    vec![
                        l.left_top(),
                        l.right_top(),
                        pos2(header.right(), zy),
                        l.right_bottom(),
                        l.left_bottom(),
                    ],
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
    pub(crate) fn dial_pos(&self) -> (usize, usize) {
        let sel = self.vdivs.iter().position(|&v| v == self.vdiv).unwrap_or(0);
        (sel, self.vdivs.len().max(1))
    }
}

/// `DsoMeasure::get_ms_text` and the control API's key, by
/// `DSO_MEASURE_TYPE` (1..=20).
pub const MEASURES: [(&str, &str); 21] = [
    ("NULL", ""),
    ("Freq", "frequency"),
    ("Period", "period"),
    ("+Duty", "positive_duty"),
    ("-Duty", "negative_duty"),
    ("+Count", "pulse_count"),
    ("Rise", "rise_time"),
    ("Fall", "fall_time"),
    ("+Width", "positive_width"),
    ("-Width", "negative_width"),
    ("BrstW", "burst_width"),
    ("Ampl", "amplitude"),
    ("High", "high"),
    ("Low", "low"),
    ("RMS", "rms"),
    ("Mean", "mean"),
    ("PK-PK", "peak_to_peak"),
    ("Max", "max"),
    ("Min", "min"),
    ("+Over", "positive_overshoot"),
    ("-Over", "negative_overshoot"),
];

impl TimeView {
    /// The DSO measurement bar (`ViewStatus` in DSO mode): five columns,
    /// two rows; a click picks the channel and the measurement.
    pub fn dso_measure_bar(&mut self, ui: &mut Ui, s: &Session) {
        let seq = {
            let st = s.state();
            st.dso_seq.wrapping_add(st.started_seq << 32)
        };
        let signals: Vec<Signal> = s
            .state()
            .signals
            .iter()
            .filter(|x| x.ty == channel_type::DSO)
            .cloned()
            .collect();
        let enabled = |i: i32| {
            s.channels()
                .iter()
                .any(|c| c.ty == channel_type::DSO && c.index as i32 == i && c.enabled)
        };
        let width = ui.available_width() / 5.0 - 3.0;
        for row in 0..2 {
            ui.horizontal(|ui| {
                for col in 0..5 {
                    let i = row * 5 + col;
                    let (ch, ty) = self.dso.measures[i];
                    let sig = signals.iter().find(|x| x.index as i32 == ch);
                    let active = sig.is_some() && enabled(ch);
                    let text = if active && ty != 0 {
                        let v = self.measurement(s, ch, seq, MEASURES[ty].1);
                        format!("{}:{v}", MEASURES[ty].0)
                    } else {
                        format!("Measure{i}")
                    };
                    let colour = sig.filter(|_| active).map_or(self.palette.muted, colour);
                    let b = egui::Button::new(egui::RichText::new(text).color(colour))
                        .min_size(vec2(width, 18.0))
                        .stroke(Stroke::new(1.0, colour));
                    if ui.add(b).clicked() {
                        self.dso.measure_pick = Some(i);
                    }
                }
            });
        }
        // the chooser (`DsoMeasure`): a tab per channel, the twenty types
        if let Some(i) = self.dso.measure_pick {
            let mut open = !crate::msgbox::esc(ui.ctx());
            let mut done = false;
            egui::Window::new(format!("Measure{i}"))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    let (mut ch, _) = self.dso.measures[i];
                    if !signals.iter().any(|x| x.index as i32 == ch) {
                        ch = signals.first().map_or(0, |x| x.index as i32);
                    }
                    ui.horizontal(|ui| {
                        for x in &signals {
                            if ui.selectable_label(x.index as i32 == ch, &x.name).clicked() {
                                ch = x.index as i32;
                            }
                        }
                    });
                    self.dso.measures[i].0 = ch;
                    egui::Grid::new("measure-types").show(ui, |ui| {
                        for (k, (name, _)) in MEASURES.iter().enumerate().skip(1) {
                            if ui.button(*name).clicked() {
                                self.dso.measures[i] = (ch, k);
                                done = true;
                            }
                            if k % 5 == 0 {
                                ui.end_row();
                            }
                        }
                    });
                    if ui.button(tr("Reset")).clicked() {
                        self.dso.measures[i] = (-1, 0);
                        done = true;
                    }
                });
            if !open || done {
                self.dso.measure_pick = None;
            }
        }
    }

    /// One measurement of channel `ch`, computed once per capture frame.
    fn measurement(&mut self, s: &Session, ch: i32, seq: u64, key: &str) -> String {
        let cache = &mut self.dso.measure_cache;
        let hit = cache.iter().position(|c| c.0 == ch && c.1 == seq);
        let i = match hit {
            Some(i) => i,
            None => {
                cache.retain(|c| c.0 != ch);
                let m = dsview_control::api::dso_measurements(s, ch).unwrap_or_default();
                cache.push((ch, seq, m));
                cache.len() - 1
            }
        };
        cache[i]
            .2
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("--")
            .to_owned()
    }
}

/// One frame kept for persistence: when it was replaced, the horizontal
/// view it was drawn at, and its shapes.
struct Ghost {
    t: std::time::Instant,
    scale: f64,
    x_offset: i64,
    shapes: Vec<egui::Shape>,
}

/// The DSO persistence (`Viewport`'s `_persist_pixmap`): each replaced
/// frame stays as a ghost fading with exp(-age / ms); zoom and pan remap
/// the ghosts, any vertical change (V/div, zero, size) drops them.
#[derive(Default)]
pub struct Persist {
    /// `dsoPersistenceMs`, 0 = off (0..=10000).
    pub ms: u32,
    ghosts: Vec<Ghost>,
    last: Option<(u64, u64, Ghost)>,
}

/// `dso_persist_signature`: what makes old frames invalid.
fn persist_signature(chans: &[ChanView], area: Rect) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (area.width() as i32, area.height() as i32).hash(&mut h);
    for c in chans {
        (
            c.enabled,
            c.vdiv,
            c.zero_y(),
            c.hw_offset,
            c.geom.rect.y,
            c.geom.rect.h,
        )
            .hash(&mut h);
    }
    h.finish()
}

impl Persist {
    /// The frames fading out now.
    pub fn ghost_count(&self) -> usize {
        self.ghosts.len()
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        p: &egui::Painter,
        area: Rect,
        seq: u64,
        sig: u64,
        scale: f64,
        x_offset: i64,
        frame: Vec<egui::Shape>,
    ) {
        let now = std::time::Instant::now();
        if self.ms == 0 {
            self.ghosts.clear();
            self.last = None;
        } else {
            // a new frame: the one shown so far becomes a ghost
            if let Some((last_seq, last_sig, g)) = self.last.take() {
                if last_sig != sig {
                    self.ghosts.clear();
                } else if last_seq != seq {
                    self.ghosts.push(Ghost { t: now, ..g });
                } else {
                    self.last = Some((last_seq, last_sig, g));
                }
            }
            let ms = self.ms as f64;
            self.ghosts
                .retain(|g| (-(now - g.t).as_secs_f64() * 1000.0 / ms).exp() > 0.02);
            for g in &self.ghosts {
                let a = (-(now - g.t).as_secs_f64() * 1000.0 / ms).exp() as f32;
                // pixel x maps to (x + x_offset) × scale in time
                let k = (g.scale / scale) as f32;
                let dx = (g.x_offset as f64 * g.scale / scale - x_offset as f64) as f32;
                let left = area.left();
                let map = |q: Pos2| pos2(left + dx + (q.x - left) * k, q.y);
                for sh in &g.shapes {
                    p.add(remap(sh, &map, a));
                }
            }
            if self.last.is_none() {
                self.last = Some((
                    seq,
                    sig,
                    Ghost {
                        t: now,
                        scale,
                        x_offset,
                        shapes: frame.clone(),
                    },
                ));
            }
        }
        p.extend(frame);
    }
}

/// A ghost's shape moved by `map` with its alpha scaled by `a`.
fn remap(sh: &egui::Shape, map: &impl Fn(Pos2) -> Pos2, a: f32) -> egui::Shape {
    let fade = |c: Color32| c.gamma_multiply(a);
    match sh {
        egui::Shape::Mesh(m) => {
            let mut m = (**m).clone();
            for v in &mut m.vertices {
                v.pos = map(v.pos);
                v.color = fade(v.color);
            }
            egui::Shape::mesh(m)
        }
        egui::Shape::Path(path) => {
            let pts: Vec<Pos2> = path.points.iter().map(|&q| map(q)).collect();
            let colour = match path.stroke.color {
                egui::epaint::ColorMode::Solid(c) => c,
                _ => Color32::GRAY,
            };
            let st = Stroke::new(path.stroke.width, fade(colour));
            egui::Shape::line(pts, st)
        }
        egui::Shape::Circle(c) => egui::Shape::circle_filled(map(c.center), c.radius, fade(c.fill)),
        other => other.clone(),
    }
}

impl TimeView {
    /// `auto_set` for every channel with auto-set on, once per frame;
    /// `check_update` counts the lock down per new frame.
    fn run_autoset(&mut self, s: &Session, chans: &[ChanView], now: i64) {
        use dsview_viewmodel::autoset::Input;
        let seq = s.state().dso_seq;
        if seq != self.dso.lock_seq {
            self.dso.lock_seq = seq;
            self.dso.auto_lock = self.dso.auto_lock.saturating_sub(1);
        }
        if !self.dso.auto.iter().any(|a| a.active()) {
            return;
        }
        let tbase = s.config_u64(key::TIMEBASE).unwrap_or(0) as f64;
        let roll = s.config(key::ROLL, None).is_some_and(|v| v.get_bool());
        let atten = s
            .state()
            .device
            .as_ref()
            .is_some_and(|d| d.is_hardware() && d.name == "MSO-E8");
        for c in chans.iter().filter(|c| c.enabled) {
            let k = c.sig.index as usize;
            if !self.dso.auto.get(k).is_some_and(|a| a.active()) {
                continue;
            }
            let m = dsview_control::api::dso_measured(s, c.sig.index as i32).unwrap_or_default();
            let (sel, count) = c.dial_pos();
            let input = Input {
                running: s.is_working(),
                locked: self.dso.auto_lock > 0,
                zero_ratio: c.geom.zero_ratio(c.zero_offset),
                valid: m.valid,
                level_valid: m.level_valid,
                pcount: m.pcount,
                period: m.period,
                min: m.min as u8,
                max: m.max as u8,
                roll,
                hori_res: tbase,
                atten,
                hw_offset: c.hw_offset,
                factor: c.factor,
                vdiv_at_min: sel == 0,
                vdiv_at_max: sel + 1 >= count,
            };
            let acts = self.dso.auto[k].step(&input, now);
            self.auto_actions(s, c, &acts, now);
        }
    }

    /// Carries out auto-set's actions for channel `c`.
    fn auto_actions(
        &mut self,
        s: &Session,
        c: &ChanView,
        acts: &[dsview_viewmodel::autoset::Action],
        now: i64,
    ) {
        use dsview_viewmodel::autoset::{AUTO_LOCK, Action};
        let hw_logic = is_hardware_logic(s);
        let width = self.vp.width / 2;
        for a in acts {
            match *a {
                Action::ZeroToMiddle => {
                    let z = c.geom.zero_offset_for(0.5);
                    s.set_config(key::PROBE_OFFSET, ConfValue::U16(z), Some(c.pos));
                }
                Action::ZoomOut => {
                    self.dso_zoom(s, -1.0, width, now);
                }
                Action::ZoomIn => {
                    self.dso_zoom(s, 1.0, width, now);
                }
                Action::VdivUp | Action::VdivDown => {
                    // re-read: an earlier action may have moved the dial
                    let cur = s
                        .config(key::PROBE_VDIV, Some(c.pos))
                        .map_or(c.vdiv, |v| v.get_u64());
                    let mut c2 = c.clone();
                    c2.vdiv = cur;
                    step_vdiv(s, &c2, *a == Action::VdivUp);
                }
                Action::Factor(f) => {
                    s.set_config(key::PROBE_FACTOR, ConfValue::U64(f), Some(c.pos));
                }
                Action::TrigValue(v) => {
                    // set_trig_vpos(ratio2pos(get_trig_vrate())): clamped
                    let zero = s
                        .config(key::PROBE_OFFSET, Some(c.pos))
                        .map_or(c.zero_offset, |x| x.get_u16());
                    let ratio = c
                        .geom
                        .pos2ratio(c.geom.ratio2pos(c.geom.trig_vrate(v, zero, hw_logic)));
                    let t = c.geom.trig_value_for(ratio, zero, hw_logic);
                    s.set_config(key::TRIGGER_VALUE, ConfValue::Byte(t), Some(c.pos));
                }
                Action::AutoTrig => {
                    s.set_config(key::TRIGGER_SOURCE, ConfValue::Byte(0), None);
                    s.set_config(
                        key::TRIGGER_CHANNEL,
                        ConfValue::Byte(c.sig.index as u8),
                        None,
                    );
                }
                Action::Lock => self.dso.auto_lock = AUTO_LOCK,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsview_session::Paths;
    use dsview_usb::{Info, Open, UsbBackend, UsbDevice, UsbError};
    use std::sync::Arc;
    use std::time::Duration;

    struct NoUsb;

    impl UsbBackend for NoUsb {
        fn list(&self) -> Result<Vec<Info>, UsbError> {
            Ok(Vec::new())
        }
        fn open(&self, _: u8, _: u8, _: Open) -> Result<Box<dyn UsbDevice>, UsbError> {
            Err(UsbError::NotFound)
        }
    }

    /// F31: a stopped capture's trace follows V/div however it changed, not
    /// only through the header's dial; its voltages stay those of the
    /// capture.
    #[test]
    fn stopped_trace_follows_vdiv_from_any_path() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let s = Session::new(
            Paths {
                data_dir: root.join("assets"),
                profile_dir: None,
                version: "test".into(),
            },
            Arc::new(NoUsb),
        );
        let (list, _) = s.devices();
        let e = list.iter().find(|e| e.name == "Demo Logic").unwrap();
        assert!(s.set_device(e.handle));
        s.switch_work_mode(dsview_core::Mode::Dso as i32);
        assert!(s.start_capture(true));
        assert!(s.wait(Duration::from_secs(60), |st| st.ended_seq > 0
            && !st.working));
        let st = DsoState::default();
        let view = |s: &Session| {
            channel_views(s, 1000, 600, &st)
                .into_iter()
                .find(|c| c.sig.index == 0)
                .unwrap()
        };
        let c0 = view(&s);
        assert_eq!(c0.data_vdiv, c0.vdiv);
        let i = c0.vdivs.iter().position(|&v| v == c0.vdiv).unwrap();
        let next = c0.vdivs[i + 1];
        // as the device options dialog or the control socket set it
        assert!(s.set_config(key::PROBE_VDIV, ConfValue::U64(next), Some(c0.pos)));
        let c1 = view(&s);
        assert_eq!(c1.vdiv, next);
        assert_eq!(c1.data_vdiv, c0.vdiv, "a stopped capture keeps its V/div");
        let ratio = c1.geom.scale as f64 / c0.geom.scale as f64;
        let want = c0.vdiv as f64 / next as f64;
        assert!(
            (ratio - want).abs() < 1e-4,
            "trace scale {ratio}, want {want}"
        );
        s.exit();
    }
}
