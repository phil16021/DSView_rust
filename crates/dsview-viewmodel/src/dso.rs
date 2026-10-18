//! An oscilloscope trace (`pv/view/dsosignal.cpp`): the code ↔ pixel
//! mapping of a channel, the trace itself (a polyline below 2 samples per
//! pixel, one bar per pixel column up to 256, the store's envelope above),
//! the grid, the trigger's sub-sample offset and the voltage texts.
//!
//! Coordinates are in the trace area (x from its left edge, y from its top),
//! as Qt's viewport. The float/int types follow the C++ so the shapes come
//! out pixel for pixel the same.

use dsview_store::dso::EnvelopeSection;

/// `DsoSignal::UpMargin`: room above the grid for the zoom marker.
pub const UP_MARGIN: i32 = 30;
/// `DsoSignal::DownMargin`
pub const DOWN_MARGIN: i32 = 0;
/// `DsoSignal::RightMargin`: room right of the grid for the trigger square.
pub const RIGHT_MARGIN: i32 = 30;
/// `DsoSignal::EnvelopeThreshold`
pub const ENVELOPE_THRESHOLD: f64 = 256.0;
/// `Trace::SquareWidth`
pub const SQUARE_WIDTH: i32 = 20;
/// `DsoSignal::TrigHRng`: samples searched back for the trigger crossing.
pub const TRIG_H_RNG: u16 = 2;
/// `DsoSignal::HoverPointSize`
pub const HOVER_POINT_SIZE: i32 = 3;
/// `Trace::SampleDotMinSpacing`, `Trace::SampleDotGrow`
pub const SAMPLE_DOT_MIN_SPACING: f64 = 10.0;
pub const SAMPLE_DOT_GROW: f64 = 3.0;
/// `DS_CONF_DSO_VDIVS`, `DS_CONF_DSO_HDIVS`
pub const VDIVS: i32 = 10;
pub const HDIVS: i32 = 10;
/// `DsoSignal::DefaultBits`
pub const DEFAULT_BITS: u8 = 8;
/// `DSO_TRIGGER_RISING`, `DSO_TRIGGER_FALLING`
pub const TRIGGER_RISING: u8 = 0;
pub const TRIGGER_FALLING: u8 = 1;

/// `DsoSignal::SignalColours`: yellow, green, red, blue by channel index.
pub const SIGNAL_COLOURS: [[u8; 3]; 4] =
    [[238, 178, 17], [0, 153, 37], [213, 15, 37], [17, 133, 209]];

/// An integer rectangle (`QRect`): x, y, width, height.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub fn top(&self) -> i32 {
        self.y
    }
    /// `QRect::bottom()`: the last row inside, one less than `y + h`.
    pub fn bottom(&self) -> i32 {
        self.y + self.h - 1
    }
    /// `QRect::right()`
    pub fn right(&self) -> i32 {
        self.x + self.w - 1
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        // QRectF(QRect).contains: the closed float rectangle
        x >= self.x as f64
            && x <= (self.x + self.w) as f64
            && y >= self.y as f64
            && y <= (self.y + self.h) as f64
    }
}

/// `DsoSignal::get_view_rect()`: the grid of a channel. Split: the
/// channel's own band (`v_offset`, `total_height`); overlaid: the whole
/// viewport above the `covered` pixels of the measurement bar.
pub fn view_rect(
    viewport_w: i32,
    viewport_h: i32,
    split: Option<(i32, i32)>,
    covered: i32,
) -> IRect {
    match split {
        Some((v_offset, total_height)) => IRect {
            x: 0,
            y: v_offset - total_height / 2 + UP_MARGIN,
            w: viewport_w - RIGHT_MARGIN,
            h: (total_height - UP_MARGIN - DOWN_MARGIN).max(1),
        },
        None => IRect {
            x: 0,
            y: UP_MARGIN,
            w: viewport_w - RIGHT_MARGIN,
            h: (viewport_h - UP_MARGIN - DOWN_MARGIN - covered).max(1),
        },
    }
}

/// A channel's calibration and its grid: what `ratio2pos` and friends need.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub rect: IRect,
    /// `_ref_min`, `_ref_max`: the code range the grid spans.
    pub ref_min: f64,
    pub ref_max: f64,
    /// `_scale`: pixels per code (`set_scale`).
    pub scale: f32,
}

impl Geom {
    /// `load_settings` for the reference range: the device's
    /// `REF_MIN`/`REF_MAX`, else 1 and `2^bits - 1`.
    pub fn new(rect: IRect, ref_min: Option<u32>, ref_max: Option<u32>, bits: u8) -> Geom {
        let mut g = Geom {
            rect,
            ref_min: ref_min.map_or(1.0, |v| v as f64),
            ref_max: ref_max.map_or(((1u32 << bits.clamp(1, 16)) - 1) as f64, |v| v as f64),
            scale: 0.0,
        };
        g.set_scale(rect.h, 1.0);
        g
    }

    /// `set_scale(height)` with `_stop_scale`: the trace is stretched while
    /// stopped when V/div changed after the capture.
    pub fn set_scale(&mut self, height: i32, stop_scale: f32) {
        self.scale = (height as f64 / (self.ref_max - self.ref_min) * stop_scale as f64) as f32;
    }

    /// `ratio2value`
    pub fn ratio2value(&self, ratio: f64) -> i32 {
        (ratio * (self.ref_max - self.ref_min) + self.ref_min) as i32
    }
    /// `ratio2pos`
    pub fn ratio2pos(&self, ratio: f64) -> i32 {
        (ratio * self.rect.h as f64 + self.rect.y as f64) as i32
    }
    /// `value2ratio`
    pub fn value2ratio(&self, value: i32) -> f64 {
        0.0f64.max((value as f64 - self.ref_min) / (self.ref_max - self.ref_min))
    }
    /// `pos2ratio`
    pub fn pos2ratio(&self, pos: i32) -> f64 {
        (pos - self.rect.y).clamp(0, self.rect.h) as f64 / self.rect.h as f64
    }
    /// `get_zero_ratio()` for `PROBE_OFFSET` `zero_offset`.
    pub fn zero_ratio(&self, zero_offset: u16) -> f64 {
        self.value2ratio(zero_offset as i32)
    }
    /// `get_zero_vpos()`
    pub fn zero_vpos(&self, zero_offset: u16) -> i32 {
        self.ratio2pos(self.zero_ratio(zero_offset))
    }
    /// `get_trig_vrate()`: a DSLogic's analog trigger is relative to the
    /// zero line.
    pub fn trig_vrate(&self, trig_value: u8, zero_offset: u16, hardware_logic: bool) -> f64 {
        if hardware_logic {
            self.value2ratio(trig_value as i32 - self.ratio2value(0.5))
                + self.zero_ratio(zero_offset)
        } else {
            self.value2ratio(trig_value as i32)
        }
    }
    /// `set_trig_ratio`: the trigger value for a level at `ratio`.
    pub fn trig_value_for(&self, ratio: f64, zero_offset: u16, hardware_logic: bool) -> u8 {
        if hardware_logic {
            let delta = (ratio - self.zero_ratio(zero_offset)).clamp(-0.5, 0.5);
            self.ratio2value(delta + 0.5) as u8
        } else {
            let delta = ratio.clamp(0.06f32 as f64, 0.945f32 as f64);
            self.ratio2value(delta) as u8
        }
    }
    /// `set_zero_ratio`: the `PROBE_OFFSET` for a zero line at `ratio`.
    pub fn zero_offset_for(&self, ratio: f64) -> u16 {
        self.ratio2value(ratio) as u16
    }
    /// The y of code `v`, clamped to the grid (`paint_trace`'s `to_y`).
    pub fn code_y(&self, v: u8, zero_y: i32, hw_offset: i32) -> f32 {
        let (top, bottom) = (self.rect.top() as f32, self.rect.bottom() as f32);
        (zero_y as f32 + (v as i32 - hw_offset) as f32 * self.scale)
            .max(top)
            .min(bottom)
    }
}

/// The samples a frame shows (`paint_mid`): first and last sample index
/// and samples per pixel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
    pub samples_per_pixel: f64,
    pub pixels_offset: f64,
    pub trig_hoff: f64,
}

/// `paint_mid`'s range: `scale` seconds per pixel, `x_offset` pixels, the
/// data's `samplerate` and `sample_count`.
pub fn span(
    sample_count: u64,
    samplerate: f64,
    scale: f64,
    x_offset: i64,
    trig_hoff: f64,
    width: i32,
) -> Span {
    let last_sample = (sample_count as i64 - 1).max(0);
    let spp = samplerate * scale;
    let start = x_offset as f64 * spp - trig_hoff;
    let end = start + spp * width as f64;
    Span {
        start: (start.floor() as i64).max(0).min(last_sample),
        end: ((end.ceil() as i64) + 1).max(0).min(last_sample),
        samples_per_pixel: spp,
        pixels_offset: x_offset as f64,
        trig_hoff,
    }
}

/// A trace's shape for one frame.
#[derive(Clone, Debug, PartialEq)]
pub enum TraceShape {
    /// Up to 2 samples per pixel: a polyline, with a dot per sample from
    /// 10 pixels per sample on (`dot` is the diameter).
    Polyline {
        points: Vec<(f32, f32)>,
        dots: usize,
        dot: f64,
    },
    /// 2-256 samples per pixel: one bar per pixel column.
    Bars(Vec<IRect>),
    /// 256 samples per pixel and more: one rect per envelope entry
    /// (x, top, width 1, height; the height may be negative).
    Envelope(Vec<(f32, f32, f32, f32)>),
}

/// `paint_trace`: `samples` are the channel's samples from `sp.start` to
/// `sp.end` inclusive; `left` is the trace area's left edge (0 in Qt).
pub fn trace_shape(
    g: &Geom,
    samples: &[u8],
    sp: &Span,
    zero_y: i32,
    hw_offset: i32,
    left: i32,
    line_width: f32,
) -> Option<TraceShape> {
    let sample_count = sp.end - sp.start + 1;
    if sample_count <= 0 {
        return None;
    }
    let samples = samples.get(..sample_count as usize)?;
    let spp = sp.samples_per_pixel;
    let right = g.rect.right() as f32;
    let pps = 1.0 / spp;
    let mut x =
        ((sp.start as f64 / spp - sp.pixels_offset) + left as f64 + sp.trig_hoff * pps) as f32;

    if spp > 2.0 {
        let x0 = x as f64;
        let columns = (sample_count as f64 * pps).ceil() as i64 + 2;
        let line_width = line_width.max(1.0);
        let mut rects = Vec::with_capacity(columns as usize);
        let mut sample = 0i64;
        let mut prev_last: Option<u8> = None;
        let mut col = 0i64;
        while col < columns && sample < sample_count {
            let col_x = x0 + col as f64;
            if col_x > right as f64 {
                break;
            }
            let col_end = (((col + 1) as f64 * spp).ceil() as i64).min(sample_count);
            if col_end <= sample {
                col += 1;
                continue;
            }
            let s = &samples[sample as usize..col_end as usize];
            let (mut vmin, mut vmax) = s
                .iter()
                .fold((s[0], s[0]), |(lo, hi), &v| (lo.min(v), hi.max(v)));
            // reach back to where the previous column ended
            if let Some(p) = prev_last {
                vmin = vmin.min(p);
                vmax = vmax.max(p);
            }
            prev_last = Some(samples[col_end as usize - 1]);
            let (mut y0, mut y1) = (
                g.code_y(vmin, zero_y, hw_offset),
                g.code_y(vmax, zero_y, hw_offset),
            );
            if y0 > y1 {
                std::mem::swap(&mut y0, &mut y1);
            }
            // a flat stretch is still drawn at the line width
            if y1 - y0 < line_width {
                let mid = (y0 + y1) / 2.0;
                y0 = mid - line_width / 2.0;
                y1 = mid + line_width / 2.0;
            }
            let y_px = y0.floor() as i32;
            rects.push(IRect {
                x: col_x.floor() as i32,
                y: y_px,
                w: 1,
                h: 1.max(y1.ceil() as i32 - y_px),
            });
            sample = col_end;
            col += 1;
        }
        return Some(TraceShape::Bars(rects));
    }

    let mut points: Vec<(f32, f32)> = Vec::with_capacity(sample_count as usize);
    let mut clipped = false;
    for &value in samples {
        let y = g.code_y(value, zero_y, hw_offset);
        if x > right {
            let Some(&(px, py)) = points.last() else {
                break;
            };
            let last_y = py + (y - py) / (x - px) * (right - px);
            points.push((right, last_y));
            clipped = true;
            break;
        }
        points.push((x, y));
        x = (x as f64 + pps) as f32;
    }
    let (dots, dot) = if pps >= SAMPLE_DOT_MIN_SPACING {
        (
            points.len() - usize::from(clipped),
            4.0f64.max(line_width as f64 + SAMPLE_DOT_GROW),
        )
    } else {
        (0, 0.0)
    };
    Some(TraceShape::Polyline { points, dots, dot })
}

/// `paint_envelope`: one rect per envelope entry, overlapping the next so
/// steep edges leave no gaps.
pub fn envelope_shape(
    g: &Geom,
    e: &EnvelopeSection<'_>,
    sp: &Span,
    zero_y: i32,
    hw_offset: i32,
    left: i32,
) -> Option<TraceShape> {
    if e.length < 2 {
        return None;
    }
    let (top, bottom) = (g.rect.top() as f32, g.rect.bottom() as f32);
    let spp = sp.samples_per_pixel;
    let n = (e.length as usize).min(e.samples.len());
    let mut rects = Vec::with_capacity(n);
    for i in 0..n.saturating_sub(1) {
        let x = ((e.scale as f64 * i as f64 + e.start as f64) / spp - sp.pixels_offset
            + left as f64
            + sp.trig_hoff / spp) as f32;
        let (s, t) = (e.samples[i], e.samples[i + 1]);
        let to_y = |v: u8| -> f32 {
            ((v as i32 - hw_offset) as f32 * g.scale + zero_y as f32)
                .max(top)
                .min(bottom)
        };
        let b = to_y(s.max.max(t.min));
        let tp = to_y(s.min.min(t.max));
        let mut h = b - tp;
        if (0.0..=1.0).contains(&h) {
            h = 1.0;
        }
        if (-1.0..=0.0).contains(&h) {
            h = -1.0;
        }
        rects.push((x, tp, 1.0, h));
    }
    Some(TraceShape::Envelope(rects))
}

/// `MathTrace::paint_trace`: `values` from `sp.start` on, drawn down from
/// `zero_y` at `scale` pixels per unit. Above 2 samples per pixel each
/// column becomes first, min/max (in the order they occur) and last.
pub fn math_shape(
    g: &Geom,
    values: &[f64],
    sp: &Span,
    zero_y: f32,
    scale: f32,
    left: i32,
) -> Option<TraceShape> {
    let sample_count = sp.end - sp.start + 1;
    if sample_count <= 0 || values.len() < sample_count as usize {
        return None;
    }
    let (top, bottom) = (g.rect.top() as f64, g.rect.bottom() as f64);
    let right = g.rect.right() as f64;
    let spp = sp.samples_per_pixel;
    let pps = 1.0 / spp;
    let mut x =
        ((sp.start as f64 / spp - sp.pixels_offset) + left as f64 + sp.trig_hoff / spp) as f32;
    let to_y = |v: f64| (zero_y as f64 - v * scale as f64).max(top).min(bottom);
    let mut points: Vec<(f32, f32)> = Vec::new();
    if spp > 2.0 {
        let x0 = x as f64;
        let mut sample = 0i64;
        while sample < sample_count {
            let col = (sample as f64 * pps).floor() as i64;
            let col_x = x0 + col as f64;
            if col_x > right {
                break;
            }
            let col_end = (((col + 1) as f64 * spp).ceil() as i64)
                .max(sample + 1)
                .min(sample_count);
            let first = values[sample as usize];
            let last = values[col_end as usize - 1];
            let (mut vmin, mut vmax, mut imin, mut imax) = (first, first, sample, sample);
            for i in sample + 1..col_end {
                let v = values[i as usize];
                if v < vmin {
                    vmin = v;
                    imin = i;
                } else if v > vmax {
                    vmax = v;
                    imax = i;
                }
            }
            let cx = col_x as f32;
            points.push((cx, to_y(first) as f32));
            if imin < imax {
                points.push((cx, to_y(vmin) as f32));
                points.push((cx, to_y(vmax) as f32));
            } else {
                points.push((cx, to_y(vmax) as f32));
                points.push((cx, to_y(vmin) as f32));
            }
            points.push((cx, to_y(last) as f32));
            sample = col_end;
        }
        return Some(TraceShape::Polyline {
            points,
            dots: 0,
            dot: 0.0,
        });
    }
    for &v in &values[..sample_count as usize] {
        let y = to_y(v) as f32;
        if x as f64 > right {
            if let Some(&(px, py)) = points.last() {
                let r = right as f32;
                points.push((r, py + (y - py) / (x - px) * (r - px)));
            }
            break;
        }
        points.push((x, y));
        x = (x as f64 + pps) as f32;
    }
    Some(TraceShape::Polyline {
        points,
        dots: 0,
        dot: 0.0,
    })
}

/// `MathTrace::paint_envelope`: `section` is (start, scale, entries) of
/// `dsview_dsp::math::envelope_section`.
pub fn math_envelope_shape(
    g: &Geom,
    section: (u64, u64, &[(f64, f64)]),
    sp: &Span,
    zero_y: f32,
    scale: f32,
    left: i32,
) -> Option<TraceShape> {
    let (start, step, e) = section;
    if e.len() < 2 {
        return None;
    }
    let (top, bottom) = (g.rect.top() as f64, g.rect.bottom() as f64);
    let spp = sp.samples_per_pixel;
    let mut rects = Vec::with_capacity(e.len());
    for i in 0..e.len() - 1 {
        let x = ((step as f64 * i as f64 + start as f64) / spp - sp.pixels_offset
            + left as f64
            + sp.trig_hoff / spp) as f32;
        let (s, t) = (e[i], e[i + 1]);
        let to_y = |v: f64| (zero_y as f64 - v * scale as f64).max(top).min(bottom) as f32;
        let b = to_y(s.1.max(t.0));
        let tp = to_y(s.0.min(t.1));
        let mut h = b - tp;
        if (0.0..=1.0).contains(&h) {
            h = 1.0;
        }
        if (-1.0..=0.0).contains(&h) {
            h = -1.0;
        }
        rects.push((x, tp, 1.0, h));
    }
    Some(TraceShape::Envelope(rects))
}

/// `SigSession::RefWave`: a frozen copy of a channel with the calibration
/// it was shown at.
#[derive(Clone, Debug, PartialEq)]
pub struct RefWave {
    pub index: u16,
    pub samples: Vec<u8>,
    pub samplerate: f64,
    /// The code of 0 V at capture time.
    pub hw_offset: i32,
    /// mV = (hw_offset - code) × mv_per_code
    pub mv_per_code: f64,
    pub colour: [u8; 3],
    pub name: String,
}

impl RefWave {
    /// `add_ref_wave`: the frozen calibration of the trace as shown
    /// (`scale` includes the stop-scale stretch).
    pub fn mv_per_code_for(scale: f32, vdiv_mv: u64, factor: u64, height: i32) -> f64 {
        scale as f64 * vdiv_mv as f64 * factor as f64 * VDIVS as f64 / height as f64
    }
}

/// `paint_ref_waves`' dash limit: denser than 3 px per sample is drawn
/// solid, one min/max pair per column.
pub const REF_DASH_MIN_PIXELS_PER_SAMPLE: f64 = 3.0;

/// `Viewport::paint_ref_waves` for one reference on its channel's grid
/// `g`, zero line `zero_y` and current V/div × factor `mv_per_div`: the
/// polyline and whether it is dense (solid) or dashed. None if nothing
/// shows.
#[allow(clippy::too_many_arguments)]
pub fn ref_wave_shape(
    rw: &RefWave,
    g: &Geom,
    zero_y: i32,
    mv_per_div: f64,
    scale: f64,
    x_offset: i64,
    trig_hoff: f64,
    width: i32,
) -> Option<(Vec<(f32, f32)>, bool)> {
    let n = rw.samples.len() as i64;
    if n < 2 || rw.samplerate <= 0.0 || rw.mv_per_code <= 0.0 || g.rect.h <= 0 || mv_per_div <= 0.0 {
        return None;
    }
    let px_per_code = rw.mv_per_code * g.rect.h as f64 / (VDIVS as f64 * mv_per_div);
    let (top, bottom) = (g.rect.top() as f32, g.rect.bottom() as f32);
    let spp = rw.samplerate * scale;
    if spp <= 0.0 {
        return None;
    }
    let pps = 1.0 / spp;
    let last = n - 1;
    let start = x_offset as f64 * spp - trig_hoff;
    let end = start + spp * width as f64;
    let s0 = (start.floor() as i64).max(0).min(last);
    let s1 = ((end.ceil() as i64) + 1).max(0).min(last);
    if s1 <= s0 {
        return None;
    }
    let count = s1 - s0 + 1;
    let dense = pps < REF_DASH_MIN_PIXELS_PER_SAMPLE;
    let capacity = if dense { (2 * count).min(2 * width as i64 + 16) } else { count } as usize;
    let mut x = ((s0 as f64 / spp - x_offset as f64) + trig_hoff * pps) as f32;
    let to_y = |c: u8| (zero_y as f32 + ((c as i32 - rw.hw_offset) as f64 * px_per_code) as f32).max(top).min(bottom);
    let mut pts: Vec<(f32, f32)> = Vec::with_capacity(capacity);
    if !dense {
        for s in s0..=s1 {
            pts.push((x, to_y(rw.samples[s as usize])));
            x = (x as f64 + pps) as f32;
        }
    } else {
        let (mut col, mut have_col) = (0i32, false);
        let (mut ymin, mut ymax) = (0.0f32, 0.0f32);
        let (mut last_y, mut have_last) = (0.0f32, false);
        // the column's extremes, nearest one first, so the line joins its
        // neighbours without crossing diagonals
        let flush = |pts: &mut Vec<(f32, f32)>, col: i32, ymin: f32, ymax: f32, last_y: &mut f32, have_last: &mut bool| {
            if pts.len() + 2 > capacity {
                return;
            }
            let (mut a, mut b) = (ymin, ymax);
            if *have_last && (*last_y - b).abs() < (*last_y - a).abs() {
                std::mem::swap(&mut a, &mut b);
            }
            pts.push((col as f32, a));
            if b != a {
                pts.push((col as f32, b));
            }
            *last_y = b;
            *have_last = true;
        };
        for s in s0..=s1 {
            let y = to_y(rw.samples[s as usize]);
            let c = x.floor() as i32;
            if !have_col || c != col {
                if have_col {
                    flush(&mut pts, col, ymin, ymax, &mut last_y, &mut have_last);
                }
                col = c;
                ymin = y;
                ymax = y;
                have_col = true;
            } else {
                ymin = ymin.min(y);
                ymax = ymax.max(y);
            }
            x = (x as f64 + pps) as f32;
        }
        if have_col {
            flush(&mut pts, col, ymin, ymax, &mut last_y, &mut have_last);
        }
    }
    Some((pts, dense))
}

/// The X measurement (`DSO_XM_STEP0..2`): a double click sets the start,
/// two more clicks the end of the high part and of the period. Indexes are
/// kept sorted as Qt sorts them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XMeasure {
    /// Clicks made so far: 1 after the double click, 3 when complete.
    pub stage: usize,
    pub index: [u64; 3],
    /// The row the measurement is drawn at.
    pub y: f32,
}

impl XMeasure {
    pub fn start(index: u64, y: f32) -> XMeasure {
        XMeasure { stage: 1, index: [index, 0, 0], y }
    }

    pub fn is_complete(&self) -> bool {
        self.stage >= 3
    }

    /// The next click (`DSO_XM_STEP1`, `DSO_XM_STEP2`).
    pub fn click(&mut self, index: u64) {
        match self.stage {
            1 => {
                self.index[1] = index;
                let max = self.index[0].max(self.index[1]);
                self.index[0] = self.index[0].min(self.index[1]);
                self.index[1] = max;
                self.stage = 2;
            }
            2 => {
                self.index[2] = index;
                let max = self.index[1].max(self.index[2]);
                self.index[1] = self.index[1].min(self.index[2]);
                self.index[2] = max;
                let max = self.index[0].max(self.index[1]);
                self.index[0] = self.index[0].min(self.index[1]);
                self.index[1] = max;
                self.stage = 3;
            }
            _ => {}
        }
    }

    /// The labels: "W=…" from two clicks on, and "P=…", "F=…", "D=…" once
    /// complete.
    pub fn texts(&self, samplerate: u64) -> (Option<String>, Option<[String; 3]>) {
        let w = (self.stage >= 2).then(|| {
            format!(
                "W={}",
                crate::ruler::format_real_time(self.index[1] as i64 - self.index[0] as i64, samplerate)
            )
        });
        let rest = self.is_complete().then(|| {
            let period = self.index[2] as i64 - self.index[0] as i64;
            [
                format!("P={}", crate::ruler::format_real_time(period, samplerate)),
                // `Ruler::pricision`: 2
                format!(
                    "F={}",
                    crate::ruler::format_freq(period as f64 / samplerate.max(1) as f64, 2)
                ),
                format!(
                    "D={:.2}%",
                    (self.index[1] - self.index[0]) as f64 * 100.0 / period as f64
                ),
            ]
        });
        (w, rest)
    }
}

/// `LissajousTrace::DIV_NUM`
pub const LISSAJOUS_DIVS: i32 = 10;
/// `LissajousOptions::WellLen`: the percent slider keeps at least 16 k
/// samples.
pub const LISSAJOUS_WELL_LEN: u64 = 16 * 1024;

/// `LissajousTrace::paint_back`: the centred square and its division
/// lines (dashed in Qt).
pub fn lissajous_grid(width: i32, height: i32) -> (IRect, Vec<Seg>) {
    let square = width.min(height);
    let border = IRect {
        x: if width > square { (width - square) / 2 } else { 0 },
        y: if height > square { (height - square) / 2 } else { 0 },
        w: square,
        h: square,
    };
    let span = (square / LISSAJOUS_DIVS) as f64;
    let mut lines = Vec::new();
    for i in 1..LISSAJOUS_DIVS {
        // drawLine(int, double, int, double): truncated
        let y = (border.y as f64 + span * i as f64) as i32 as f32;
        lines.push((border.x as f32, y, border.right() as f32, y));
    }
    for i in 1..LISSAJOUS_DIVS {
        let x = (border.x as f64 + span * i as f64) as i32 as f32;
        lines.push((x, border.y as f32, x, border.bottom() as f32));
    }
    (border, lines)
}

/// `LissajousTrace::paint_mid`: the first `percent` % of the samples, x
/// from one channel and y from the other, 0..255 across the square.
pub fn lissajous_points(border: IRect, xs: &[u8], ys: &[u8], sample_count: u64, percent: i32) -> Vec<(f32, f32)> {
    let n = (sample_count as f64 * (percent as f64 / 100.0).min(1.0)) as usize;
    let n = n.min(xs.len()).min(ys.len());
    let scale = border.w as f64 / 255.0;
    let (left, bottom) = (border.x as f64, border.bottom() as f64);
    (0..n)
        .map(|i| ((left + xs[i] as f64 * scale) as f32, (bottom - ys[i] as f64 * scale) as f32))
        .collect()
}

/// `LissajousOptions`: the percent slider's minimum for `sample_limit`.
pub fn lissajous_min_percent(sample_limit: u64) -> i32 {
    if sample_limit > LISSAJOUS_WELL_LEN {
        (LISSAJOUS_WELL_LEN as f64 * 100.0 / sample_limit as f64).ceil() as i32
    } else {
        100
    }
}

/// `paint_prepare`: the trigger's sub-sample offset (`trig_hoff`), from the
/// samples just before the trigger cursor at `trig_index`. None when the
/// cursor is past the data or no crossing is found (Qt keeps the old
/// value then).
pub fn trig_hoff(samples: &[u8], trig_index: i64, trig_value: u8, slope: u8) -> Option<f64> {
    if trig_index >= samples.len() as i64 {
        return None;
    }
    for i in 0..TRIG_H_RNG as i64 {
        let (i0, i1) = (trig_index - i - 1, trig_index - i);
        // C reads samples[-1] when i1 == 0; stop there instead
        if i1 < 0 || i0 < 0 {
            break;
        }
        let (t0, t1) = (samples[i0 as usize], samples[i1 as usize]);
        let tv = trig_value;
        if (slope == TRIGGER_RISING && t0 >= tv && t1 <= tv)
            || (slope == TRIGGER_FALLING && t0 <= tv && t1 >= tv)
        {
            let xoff = if t1 == t0 {
                0.0
            } else {
                (tv as i32 - t0 as i32) as f64 / (t1 as i32 - t0 as i32) as f64
            };
            return Some((i + 1) as f64 - xoff);
        }
    }
    None
}

/// A straight line (x0, y0, x1, y1).
pub type Seg = (f32, f32, f32, f32);

/// `paint_back`: the grid of one channel's rect.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Grid {
    /// The frame, filled with the background.
    pub frame: IRect,
    /// The division lines (dashed in Qt).
    pub dividers: Vec<Seg>,
    /// The minor ticks on the centre lines.
    pub ticks: Vec<Seg>,
    /// The zoom marker above the grid: the two rails, the edge brackets
    /// (polylines) and the shown part (x, y, w, h).
    pub rails: Vec<Seg>,
    pub brackets: [[(f32, f32); 4]; 2],
    pub shown: (f32, f32, f32, f32),
}

/// `paint_back(left, right)`: `sample_len` is `cur_samplelimits`,
/// `samplerate` `cur_snap_samplerate`.
pub fn grid(
    rect: IRect,
    left: i32,
    right: i32,
    sample_len: u64,
    samplerate: f64,
    scale: f64,
    x_offset: i64,
) -> Grid {
    let height = rect.h;
    let width = right - left;
    let top = rect.y;
    let mut g = Grid {
        frame: IRect {
            x: left,
            y: top,
            w: width,
            h: height,
        },
        ..Grid::default()
    };

    // the zoom marker
    let spp = samplerate * scale;
    let sample_len = sample_len.max(1) as f64;
    let shown_rate = (spp * width as f64 / sample_len).min(1.0);
    let start = x_offset as f64 * spp;
    let shown_offset = (start / sample_len).min(1.0) * width as f64;
    let shown_len = (shown_rate * width as f64).max(6.0);
    let marker_y = (top - UP_MARGIN / 2) as f64;
    // QPoint(double, double) rounds
    let pt = |x: f64, y: f64| (x.round() as f32, y.round() as f32);
    g.brackets = [
        [
            pt(shown_offset + 3.0, marker_y - 6.0),
            pt(shown_offset, marker_y - 6.0),
            pt(shown_offset, marker_y + 6.0),
            pt(shown_offset + 3.0, marker_y + 6.0),
        ],
        [
            pt(shown_offset + shown_len - 3.0, marker_y - 6.0),
            pt(shown_offset + shown_len, marker_y - 6.0),
            pt(shown_offset + shown_len, marker_y + 6.0),
            pt(shown_offset + shown_len - 3.0, marker_y + 6.0),
        ],
    ];
    // drawLine(int, int, int, int): the doubles are truncated
    let t = |v: f64| v as i32 as f32;
    g.rails
        .push((left as f32, t(marker_y), t(shown_offset), t(marker_y)));
    g.rails.push((
        t(shown_offset + shown_len),
        t(marker_y),
        (left + width) as f32,
        t(marker_y),
    ));
    // drawRect(int, int, int, int)
    g.shown = (t(shown_offset), t(marker_y - 3.0), t(shown_len), 6.0);

    // the divisions
    let span_y = height as f64 / VDIVS as f64;
    for i in 1..=VDIVS {
        let pos_y = span_y * i as f64 + top as f64;
        if i != VDIVS {
            g.dividers
                .push((left as f32, t(pos_y), right as f32, t(pos_y)));
        }
        let mini = span_y / 5.0;
        for j in 1..5 {
            let y = t(pos_y - mini * j as f64);
            g.ticks.push((
                (width as f32 / 2.0 - 5.0) as i32 as f32,
                y,
                (width as f32 / 2.0 + 5.0) as i32 as f32,
                y,
            ));
        }
    }
    let span_x = width as f64 / HDIVS as f64;
    for i in 1..=HDIVS {
        let pos_x = span_x * i as f64;
        if i != HDIVS {
            g.dividers
                .push((t(pos_x), top as f32, t(pos_x), (height + top) as f32));
        }
        let mini = span_x / 5.0;
        for j in 1..5 {
            let x = t(pos_x - mini * j as f64);
            g.ticks.push((
                x,
                (height as f32 / 2.0 + top as f32 - 5.0) as i32 as f32,
                x,
                (height as f32 / 2.0 + top as f32 + 5.0) as i32 as f32,
            ));
        }
    }
    g
}

/// `get_trig_rect`: the trigger level's square right of the grid.
pub fn trig_rect(g: &Geom, right: i32, trig_vrate: f64) -> (f32, f32, f32, f32) {
    let y = g.ratio2pos(trig_vrate) as f32;
    let sq = SQUARE_WIDTH as f32;
    (right as f32 + sq / 2.0, y - sq / 2.0, sq, sq)
}

/// `Trace::Margin`, `Trace::SquareNum`
pub const MARGIN: i32 = 3;
pub const SQUARE_NUM: i32 = 5;

/// A float rectangle (`QRectF`): x, y, width, height.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl FRect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// A DSO channel's header controls (`DsoSignal::get_rect`), around the
/// header row at `y`, for header width `right`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderRects {
    pub colour: FRect,
    pub vdial: FRect,
    pub x1: FRect,
    pub x10: FRect,
    pub x100: FRect,
    pub enable: FRect,
    pub coupling: FRect,
    pub auto: FRect,
    /// The zero-line label at the header's right edge (at `zero_y`).
    pub label: FRect,
}

/// `get_rect` with the default font (square 20, margin 3).
pub fn header_rects(y: i32, zero_y: i32, right: i32) -> HeaderRects {
    let (sq, m) = (SQUARE_WIDTH as f32, MARGIN as f32);
    let y = y as f32;
    let left_width = (SQUARE_WIDTH / 2 + MARGIN) as f32;
    let n = SQUARE_NUM as f32;
    let small = |x: f32, top: f32| FRect {
        x,
        y: top,
        w: sq * 1.75,
        h: sq,
    };
    HeaderRects {
        colour: FRect {
            x: 2.0,
            y: y - sq / 2.0,
            w: left_width - m,
            h: sq,
        },
        vdial: FRect {
            x: left_width + sq * 0.5 + m,
            y: y - sq * n + sq * 3.0,
            w: sq * (n - 1.0),
            h: sq * (n - 1.0),
        },
        x1: small(
            left_width + sq * 0.5,
            y - sq * 2.0 - sq * (n - 2.0) + sq * 3.0,
        ),
        x10: small(
            left_width + sq * 0.5,
            y - sq * 2.0 - sq * (n - 2.0) * 0.5 + sq * 3.0,
        ),
        x100: small(left_width + sq * 0.5, y - sq * 2.0 + sq * 3.0),
        enable: small(2.0, y - sq / 2.0 + sq * 3.0),
        coupling: small(2.0 + sq * 1.75 + m, y - sq / 2.0 + sq * 3.0),
        auto: small(2.0 + sq * 3.5 + m * 2.0, y - sq / 2.0 + sq * 3.0),
        label: FRect {
            x: right as f32 - 1.5 * sq,
            y: zero_y as f32 - sq / 2.0,
            w: sq,
            h: sq,
        },
    }
}

/// `dslDial::paint`'s pointer and ticks: the unit vector at fraction `f`
/// (0 = first entry, 1 = last) of the dial's 150° sweep, clockwise from
/// one o'clock; screen coordinates (y down).
pub fn dial_direction(f: f64) -> (f32, f32) {
    let t = (195.0 + 150.0 * f).to_radians();
    (-t.sin() as f32, t.cos() as f32)
}

/// What turns codes into millivolts: `get_voltage`'s factors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volts {
    /// `get_measure_voltage_factor`: V/div (mV) at capture time.
    pub vdiv_mv: u64,
    /// `get_data_scale`: the trace's `_scale` at capture time.
    pub data_scale: f32,
    /// `_vDial->get_factor()`: the probe factor.
    pub factor: u64,
    /// `get_view_rect().height()`
    pub height: i32,
}

impl Volts {
    /// `get_voltage(v, p, scaled)`: `v` in pixels (`scaled`) or codes.
    pub fn text(&self, v: f64, precision: usize, scaled: bool) -> String {
        let k = self.vdiv_mv as f64 * self.factor as f64 * VDIVS as f64 / self.height.max(1) as f64;
        let v = if scaled {
            v * k
        } else {
            v * self.data_scale as f64 * k
        };
        if v.abs() >= 1000.0 {
            format!("{:.*}V", precision, v / 1000.0)
        } else {
            format!("{:.*}mV", precision, v)
        }
    }

    /// `get_voltage(index)`: the millivolts of code `value`.
    pub fn mv(&self, value: u8, hw_offset: i32) -> f64 {
        (hw_offset as f64 - value as f64)
            * self.data_scale as f64
            * self.vdiv_mv as f64
            * self.factor as f64
            * VDIVS as f64
            / self.height.max(1) as f64
    }
}

/// `get_point`: where code `value` of sample `index` is drawn.
pub fn point(g: &Geom, x: f64, value: u8, zero_offset: u16, hw_offset: i32) -> (f32, f32) {
    (
        x as f32,
        g.code_y(value, g.zero_vpos(zero_offset), hw_offset),
    )
}

/// `DsoSignal::get_time`: nanoseconds with Qt's unit spellings.
pub fn time_text(t: f64) -> String {
    let a = t.abs();
    if a > 1e9 {
        format!("{:.2}S", t / 1e9)
    } else if a > 1e6 {
        format!("{:.2}mS", t / 1e6)
    } else if a > 1e3 {
        format!("{:.2}uS", t / 1e3)
    } else {
        format!("{t:.2}nS")
    }
}

/// `dslDial::paint`'s text for V/div `value` (mV) times the probe
/// `factor`: whole units only (1500 mV reads "1V/div").
pub fn vdiv_text(value: u64, factor: u64) -> String {
    let mut v = value * factor;
    let mut unit = 0;
    while v / 1000 >= 1 && unit < 1 {
        v /= 1000;
        unit += 1;
    }
    format!("{v}{}/div", ["mV", "V"][unit])
}

/// `WheelAccumulator`: whole steps from high-resolution wheel deltas.
#[derive(Clone, Debug, Default)]
pub struct WheelAccumulator {
    unit: f64,
    accum: f64,
    last_ms: i64,
}

/// `WheelAccumulator::IdleResetMs`
pub const WHEEL_IDLE_RESET_MS: i64 = 500;

impl WheelAccumulator {
    pub fn new(unit: f64) -> WheelAccumulator {
        WheelAccumulator {
            unit,
            accum: 0.0,
            last_ms: 0,
        }
    }

    /// `take(amount)` at `now_ms`: the whole steps, the rest kept.
    pub fn take(&mut self, amount: f64, now_ms: i64) -> i32 {
        if amount == 0.0 {
            return 0;
        }
        if now_ms - self.last_ms > WHEEL_IDLE_RESET_MS
            || (self.accum != 0.0 && (self.accum > 0.0) != (amount > 0.0))
        {
            self.accum = 0.0;
        }
        self.last_ms = now_ms;
        self.accum += amount / self.unit;
        let steps = if self.accum > 0.0 {
            self.accum.floor()
        } else {
            self.accum.ceil()
        } as i32;
        self.accum -= steps as f64;
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsview_store::dso::EnvelopeSample;

    fn geom() -> Geom {
        // 8-bit, the demo's 0..255 reference, a 500 px grid
        let r = view_rect(1030, 530, None, 0);
        Geom::new(r, Some(0), Some(255), 8)
    }

    #[test]
    fn rect_and_mapping() {
        let g = geom();
        assert_eq!(
            g.rect,
            IRect {
                x: 0,
                y: 30,
                w: 1000,
                h: 500
            }
        );
        assert!((g.scale - 500.0 / 255.0).abs() < 1e-6);
        // zero at mid-range
        assert_eq!(g.zero_vpos(128), (128.0 / 255.0 * 500.0 + 30.0) as i32);
        assert_eq!(g.pos2ratio(30), 0.0);
        assert_eq!(g.pos2ratio(10_000), 1.0);
        assert_eq!(g.ratio2value(0.5), 127);
        // trigger clamps to 0.06..0.945
        assert_eq!(
            g.trig_value_for(0.0, 128, false),
            g.ratio2value(0.06f32 as f64) as u8
        );
        assert_eq!(
            g.trig_value_for(1.0, 128, false),
            g.ratio2value(0.945f32 as f64) as u8
        );
        // DSLogic: relative to zero, ±0.5
        let v = g.trig_value_for(g.zero_ratio(100), 100, true);
        assert_eq!(v, g.ratio2value(0.5) as u8);
        assert!((g.trig_vrate(v, 100, true) - g.zero_ratio(100)).abs() < 0.01);
        // split: the channel's band
        let s = view_rect(1030, 530, Some((200, 300)), 0);
        assert_eq!((s.y, s.h), (200 - 150 + 30, 270));
    }

    #[test]
    fn polyline_and_dots() {
        let g = geom();
        let samples: Vec<u8> = (0..60).map(|i| (i * 4) as u8).collect();
        // 20 px per sample: dots
        let sp = span(60, 1e6, 1e-6 / 20.0, 0, 0.0, 1000);
        assert_eq!((sp.start, sp.end), (0, 51));
        let Some(TraceShape::Polyline { points, dots, dot }) =
            trace_shape(&g, &samples, &sp, 285, 128, 0, 1.0)
        else {
            panic!()
        };
        // sample 50 is at 1000 px > right (999): clipped at the edge
        assert_eq!(points.len(), 51);
        assert_eq!(points[0], (0.0, g.code_y(0, 285, 128)));
        assert_eq!(points[1].0, 20.0);
        assert_eq!(points.last().unwrap().0, 999.0);
        assert_eq!(dots, 50);
        assert_eq!(dot, 4.0);
        // the y follows the code, clamped to the grid
        assert_eq!(g.code_y(0, 285, 128), (285.0 - 128.0 * g.scale).max(30.0));
        assert_eq!(g.code_y(255, 285, 128), 529.0);
    }

    #[test]
    fn bars_cover_each_column() {
        let g = geom();
        // a square wave, period 20 samples, 10 samples per pixel
        let samples: Vec<u8> = (0..10_000)
            .map(|i| if (i / 10) % 2 == 0 { 50 } else { 200 })
            .collect();
        let sp = span(10_000, 1e6, 10e-6, 0, 0.0, 1000);
        let Some(TraceShape::Bars(rects)) = trace_shape(&g, &samples, &sp, 285, 128, 0, 1.0) else {
            panic!()
        };
        assert_eq!(rects.len(), 1000);
        // column 0 is flat at 50: a 1 px bar; column 1 steps up to 200 and
        // reaches back to column 0's last value
        let y50 = g.code_y(50, 285, 128);
        let y200 = g.code_y(200, 285, 128);
        assert_eq!(rects[0].x, 0);
        // flat: the line width around the level
        assert_eq!(rects[0].y, (y50 - 0.5).floor() as i32);
        assert_eq!(rects[0].y + rects[0].h, (y50 + 0.5).ceil() as i32);
        assert_eq!(rects[1].y, y50.floor() as i32);
        assert_eq!(rects[1].y + rects[1].h, y200.ceil() as i32);
        assert!(rects.windows(2).all(|w| w[1].x == w[0].x + 1));
    }

    #[test]
    fn envelope_rects() {
        let g = geom();
        let es = [
            EnvelopeSample { min: 10, max: 20 },
            EnvelopeSample { min: 30, max: 40 },
            EnvelopeSample { min: 30, max: 30 },
        ];
        let e = EnvelopeSection {
            start: 0,
            scale: 256,
            length: 3,
            samples: &es,
        };
        let sp = span(768, 1e6, 256e-6, 0, 0.0, 1000);
        let Some(TraceShape::Envelope(r)) = envelope_shape(&g, &e, &sp, 285, 0, 0) else {
            panic!()
        };
        assert_eq!(r.len(), 2);
        // first: max(20, 30)=30 to min(10, 40)=10
        let (x, t, w, h) = r[0];
        assert_eq!((x, w), (0.0, 1.0));
        assert_eq!(t, 285.0 + 10.0 * g.scale);
        assert!((h - 20.0 * g.scale).abs() < 1e-3);
        assert_eq!(r[1].0, 1.0);
    }

    #[test]
    fn trigger_offset() {
        // rising slope means falling codes (a higher code is a lower voltage)
        let s = [200u8, 150, 100, 50];
        assert_eq!(
            trig_hoff(&s, 2, 120, TRIGGER_RISING),
            Some(1.0 - (120.0 - 150.0) / (100.0 - 150.0))
        );
        assert_eq!(trig_hoff(&s, 9, 120, TRIGGER_RISING), None);
        assert_eq!(trig_hoff(&s, 0, 120, TRIGGER_RISING), None);
    }

    #[test]
    fn grid_lines() {
        let r = view_rect(1030, 530, None, 0);
        let gr = grid(r, 0, 1000, 1000, 1e6, 1e-6, 0);
        assert_eq!(gr.dividers.len(), 18);
        assert_eq!(gr.ticks.len(), 80);
        assert_eq!(gr.dividers[0], (0.0, 80.0, 1000.0, 80.0));
        // the whole capture is shown
        assert_eq!(gr.shown, (0.0, 12.0, 1000.0, 6.0));
    }

    #[test]
    fn header_geometry() {
        let h = header_rects(100, 120, 150);
        assert_eq!(
            h.vdial,
            FRect {
                x: 26.0,
                y: 60.0,
                w: 80.0,
                h: 80.0
            }
        );
        assert_eq!(h.x1.y, 60.0);
        assert_eq!(h.x10.y, 90.0);
        assert_eq!(h.x100.y, 120.0);
        assert_eq!(
            h.enable,
            FRect {
                x: 2.0,
                y: 150.0,
                w: 35.0,
                h: 20.0
            }
        );
        assert_eq!(h.auto.x, 2.0 + 70.0 + 6.0);
        assert_eq!(
            h.label,
            FRect {
                x: 120.0,
                y: 110.0,
                w: 20.0,
                h: 20.0
            }
        );
        // the sweep: first entry up-right, last down-right
        let (x, y) = dial_direction(0.0);
        assert!(x > 0.2 && y < -0.9);
        let (x, y) = dial_direction(1.0);
        assert!(x > 0.2 && y > 0.9);
    }

    #[test]
    fn math_columns() {
        let g = geom();
        // a ramp, 10 samples per pixel: first, min, max, last per column
        let v: Vec<f64> = (0..1000).map(|i| i as f64 / 100.0).collect();
        let sp = span(1000, 1e6, 10e-6, 0, 0.0, 1000);
        let Some(TraceShape::Polyline { points, .. }) = math_shape(&g, &v, &sp, 280.0, 10.0, 0)
        else {
            panic!()
        };
        assert_eq!(points.len(), 400);
        assert_eq!(points[0], (0.0, 280.0));
        // rising: min first, then max
        assert_eq!(points[1].1, 280.0);
        assert_eq!(points[3].1, (280.0 - 0.09 * 10.0) as f32);
        let l = vec![(0.0, 1.0), (1.0, 2.0), (2.0, 3.0)];
        let sp = span(768, 1e6, 256e-6, 0, 0.0, 1000);
        let Some(TraceShape::Envelope(r)) =
            math_envelope_shape(&g, (0, 256, &l), &sp, 280.0, 10.0, 0)
        else {
            panic!()
        };
        assert_eq!(r.len(), 2);
        // values grow upwards: C's "bottom" lies above "top", the height is negative
        assert_eq!(r[0], (0.0, 280.0, 1.0, -10.0));
    }

    #[test]
    fn reference_waves() {
        let g = geom();
        let rw = RefWave {
            index: 0,
            samples: (0..1000).map(|i| (i % 256) as u8).collect(),
            samplerate: 1e6,
            hw_offset: 128,
            mv_per_code: 1000.0 * 10.0 / 255.0,
            colour: [1, 2, 3],
            name: "Ref 1".into(),
        };
        // 10 px per sample: dashed, one point per sample
        let (pts, dense) = ref_wave_shape(&rw, &g, 280, 1000.0, 1e-7, 0, 0.0, 1000).unwrap();
        assert!(!dense);
        // samples 0 ..= ceil(end) + 1
        assert_eq!(pts.len(), 102);
        assert_eq!(pts[1].0, 10.0);
        // at the same V/div the code maps as the live trace does
        assert!((pts[0].1 - g.code_y(0, 280, 128)).abs() < 1e-3);
        // 10 samples per pixel: a min/max pair per column
        let (pts, dense) = ref_wave_shape(&rw, &g, 280, 1000.0, 1e-5, 0, 0.0, 1000).unwrap();
        assert!(dense);
        assert!(pts.len() <= 2 * 1000 + 16);
        assert_eq!(pts[0].0, 0.0);
    }

    #[test]
    fn x_measure() {
        let mut m = XMeasure::start(500, 100.0);
        assert_eq!(m.texts(1_000_000), (None, None));
        m.click(200);
        assert_eq!(m.index[..2], [200, 500]);
        m.click(900);
        assert!(m.is_complete());
        assert_eq!(m.index, [200, 500, 900]);
        let (w, rest) = m.texts(1_000_000);
        assert_eq!(w.as_deref(), Some("W=+300μs"));
        let [p, f, d] = rest.unwrap();
        assert_eq!(p, "P=+700μs");
        assert_eq!(d, "D=42.86%");
        assert_eq!(f, "F=1.43KHz");
    }

    #[test]
    fn lissajous() {
        let (b, lines) = lissajous_grid(1000, 500);
        assert_eq!(b, IRect { x: 250, y: 0, w: 500, h: 500 });
        assert_eq!(lines.len(), 18);
        assert_eq!(lines[0], (250.0, 50.0, 749.0, 50.0));
        let p = lissajous_points(b, &[0, 255], &[0, 255], 2, 100);
        assert_eq!(p, vec![(250.0, 499.0), (750.0, -1.0)]);
        assert_eq!(lissajous_points(b, &[0; 10], &[0; 10], 10, 50).len(), 5);
        assert_eq!(lissajous_min_percent(1_000_000), 2);
        assert_eq!(lissajous_min_percent(10_000), 100);
    }

    #[test]
    fn texts() {
        let v = Volts {
            vdiv_mv: 500,
            data_scale: 1.0,
            factor: 1,
            height: 500,
        };
        assert_eq!(v.text(50.0, 2, true), "500.00mV");
        assert_eq!(v.text(200.0, 2, true), "2.00V");
        assert_eq!(vdiv_text(500, 10), "5V/div");
        assert_eq!(vdiv_text(20, 1), "20mV/div");
        assert_eq!(vdiv_text(1500, 1), "1V/div");
        assert_eq!(time_text(1500.0), "1.50uS");
        let mut w = WheelAccumulator::new(1.0);
        assert_eq!(w.take(0.5, 1000), 0);
        assert_eq!(w.take(0.6, 1100), 1);
        assert_eq!(w.take(-0.5, 1200), 0);
        assert_eq!(w.take(0.9, 2000), 0);
    }
}
