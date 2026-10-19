//! An analog (DAQ) trace (`pv/view/analogsignal.cpp`): the code ↔ pixel
//! mapping inside the trace's own row, the trace (a polyline below 256
//! samples per pixel, the store's envelope above, both following the ring
//! of a loop capture), the value rule and the hover value.

use dsview_store::analog::{AnalogStore, EnvelopeSection};

/// `AnalogSignal::EnvelopeThreshold`
pub const ENVELOPE_THRESHOLD: f64 = 16.0;

/// A trace's row and calibration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    /// `get_y()`, `get_totalHeight()`
    pub y: i32,
    pub height: i32,
    pub ref_min: f64,
    pub ref_max: f64,
    /// `_scale`: pixels per code.
    pub scale: f32,
}

impl Geom {
    /// `load_settings` and `set_scale(get_totalHeight())`.
    pub fn new(y: i32, height: i32, ref_min: Option<u32>, ref_max: Option<u32>, bits: u8) -> Geom {
        let ref_min = ref_min.map_or(1.0, |v| v as f64);
        let ref_max = ref_max.map_or(((1u32 << bits.clamp(1, 16)) - 1) as f64, |v| v as f64);
        Geom {
            y,
            height,
            ref_min,
            ref_max,
            scale: (height as f64 / (ref_max - ref_min)) as f32,
        }
    }
    fn top_i(&self) -> i32 {
        (self.y as f64 - self.height as f64 * 0.5) as i32
    }
    /// The row's top and bottom as `paint_mid` takes them (floats).
    pub fn top_bottom(&self) -> (f32, f32) {
        (
            (self.y as f64 - self.height as f64 * 0.5) as f32,
            (self.y as f64 + self.height as f64 * 0.5) as f32,
        )
    }
    pub fn ratio2value(&self, ratio: f64) -> i32 {
        (ratio * (self.ref_max - self.ref_min) + self.ref_min) as i32
    }
    pub fn ratio2pos(&self, ratio: f64) -> i32 {
        (ratio * self.height as f64 + self.top_i() as f64) as i32
    }
    pub fn value2ratio(&self, value: i32) -> f64 {
        0.0f64.max((value as f64 - self.ref_min) / (self.ref_max - self.ref_min))
    }
    pub fn pos2ratio(&self, pos: i32) -> f64 {
        let top = self.y - self.height / 2;
        (pos - top).clamp(0, self.height) as f64 / self.height as f64
    }
    pub fn zero_vpos(&self, zero_offset: u16) -> i32 {
        self.ratio2pos(self.value2ratio(zero_offset as i32))
    }
}

/// The view's horizontal state as `paint_mid` uses it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub samplerate: f64,
    pub scale: f64,
    pub x_offset: i64,
    /// `right - left + 1`
    pub width: i32,
    /// `cur_samplelimits()`
    pub sample_limit: u64,
}

/// What `paint_mid` draws.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Polyline(Vec<(f32, f32)>),
    /// (x, y, width 1, height)
    Envelope(Vec<(f32, f32, f32, f32)>),
}

/// `paint_mid` for the channel at storage position `order`.
pub fn shape(
    store: &AnalogStore,
    order: usize,
    g: &Geom,
    zero_offset: u16,
    hw_offset: i32,
    f: &Frame,
) -> Option<Shape> {
    let count = store.sample_count();
    if count == 0 {
        return None;
    }
    let (top, bottom) = g.top_bottom();
    let zero_y = g.ratio2pos(g.value2ratio(zero_offset as i32)) as f32 as i32;
    let spp = f.samplerate * f.scale;
    let ring_start = store.ring_start();
    let index_offset = f.x_offset as f64 * spp;
    let start_index = (ring_start + index_offset.floor() as u64) % count;
    let start_pixel = ((index_offset.floor() - index_offset) / spp) as i64;
    let show_length = ((count as f64 - index_offset.floor()).floor())
        .min((f.width as f64 * spp + 1.0).ceil()) as i64;
    if show_length <= 0 {
        return None;
    }
    if spp < ENVELOPE_THRESHOLD {
        Some(trace(
            store,
            order,
            zero_y,
            start_pixel,
            start_index,
            show_length,
            spp,
            g,
            top,
            bottom,
            hw_offset,
        ))
    } else {
        let e = store.get_envelope_section(start_index, show_length, spp as f32, order as i32);
        let samples = store.get_envelope_samples(spp as f32, order as i32);
        envelope(
            &e,
            samples,
            store.ring_end(),
            f.sample_limit,
            zero_y,
            start_pixel,
            spp,
            g,
            top,
            bottom,
            hw_offset,
            f.width,
        )
    }
}

#[allow(clippy::too_many_arguments)]
fn trace(
    store: &AnalogStore,
    order: usize,
    zero_y: i32,
    start_pixel: i64,
    start_index: u64,
    sample_count: i64,
    spp: f64,
    g: &Geom,
    top: f32,
    bottom: f32,
    hw_offset: i32,
) -> Shape {
    let channel_num = store.channel_num() as u64;
    let unit_bytes = store.unit_bytes() as u64;
    let data = store.data();
    let count = store.sample_count();
    let ring_end = store.ring_end();
    let mut points = Vec::with_capacity(sample_count as usize);
    let mut yindex = start_index;
    let mut x = start_pixel as f32;
    let pps = 1.0 / spp;
    for _ in 0..sample_count {
        let mut index = ((yindex * channel_num + order as u64) * unit_bytes) as usize;
        let mut yv = data.get(index).copied().unwrap_or(0) as f32;
        for i in 1..unit_bytes {
            index += 1;
            yv += ((data.get(index).copied().unwrap_or(0) as u32) << (i * 8)) as f32;
        }
        let y = (zero_y as f32 + (yv - hw_offset as f32) * g.scale)
            .max(top)
            .min(bottom);
        points.push((x, y));
        if yindex == ring_end {
            break;
        }
        yindex = (yindex + 1) % count;
        x = (x as f64 + pps) as f32;
    }
    Shape::Polyline(points)
}

#[allow(clippy::too_many_arguments)]
fn envelope(
    e: &EnvelopeSection,
    samples: &[dsview_store::analog::EnvelopeSample],
    ring_end_sample: u64,
    sample_limit: u64,
    zero_y: i32,
    start_pixel: i64,
    spp: f64,
    g: &Geom,
    top: f32,
    bottom: f32,
    hw_offset: i32,
    width: i32,
) -> Option<Shape> {
    if e.samples_num == 0 || samples.is_empty() || e.scale == 0 {
        return None;
    }
    let mut rects = Vec::with_capacity(width as usize + 10);
    let mut px: i32 = -1;
    let mut pre_px: i32;
    let (mut y_min, mut y_max, mut pre_y_min, mut pre_y_max) =
        (zero_y as f32, zero_y as f32, zero_y as f32, zero_y as f32);
    let step = e.scale as f64 / spp;
    let ring_end = (ring_end_sample as i64 / e.scale as i64 - 1).max(0) as u64;
    let ring_len = (sample_limit / e.scale as u64).max(1);
    let mut x = start_pixel as f32;
    let clamp = |v: f32| v.max(top).min(bottom);
    for sample in 0..e.length {
        let ring_index = (e.start + sample) % ring_len;
        if sample != 0 && ring_index == ring_end {
            break;
        }
        let ev = samples[((e.start + sample) % e.samples_num) as usize % samples.len()];
        let b = clamp((zero_y as f32 + (ev.max as i32 - hw_offset) as f32 * g.scale) + 0.5);
        let t = clamp((zero_y as f32 + (ev.min as i32 - hw_offset) as f32 * g.scale) + 0.5);
        pre_px = px;
        if px != x.floor() as i32 {
            if pre_px != -1 {
                // overlap with the previous column so steep edges leave no gap
                let r = if pre_y_min > y_max {
                    (pre_px as f32, y_min, 1.0, pre_y_min - y_min + 1.0)
                } else if pre_y_max < y_min {
                    (pre_px as f32, pre_y_max, 1.0, y_max - pre_y_max + 1.0)
                } else {
                    (pre_px as f32, y_min, 1.0, y_max - y_min + 1.0)
                };
                rects.push(r);
                pre_y_min = y_min;
                pre_y_max = y_max;
            } else {
                pre_y_max = clamp(b);
                pre_y_min = clamp(t);
            }
            px = x as i32;
            y_max = clamp(b);
            y_min = clamp(t);
        }
        if px == pre_px {
            y_max = b.max(y_max);
            y_min = t.min(y_min);
        }
        x = (x as f64 + step) as f32;
    }
    Some(Shape::Envelope(rects))
}

/// `paint_back`'s rule: (y, is a major tick, label) at the row's left and
/// right edges; labels at the top, the middle and the bottom.
pub fn rule(
    g: &Geom,
    zero_offset: u16,
    map_min: f64,
    map_max: f64,
    unit: &str,
) -> Vec<(f32, bool, Option<String>)> {
    const DIVS: i32 = 10;
    const MIN_DIVS: i32 = 5;
    let height = g.height as f64;
    let steps = height / (DIVS * MIN_DIVS) as f64;
    let map_steps = (map_max - map_min) / DIVS as f64;
    let zero_ratio = g.value2ratio(zero_offset as i32);
    let mut y = g.y as f64 - height * 0.5;
    let mut v = map_max + (zero_ratio - 0.5) * (map_max - map_min);
    let mut out = Vec::new();
    // drawLine(int, double, ...): the y is truncated
    for i in 0..DIVS {
        let label = (i == 0 || i == DIVS / 2).then(|| format!("{v:.2}{unit}"));
        out.push((y as i32 as f32, true, label));
        for _ in 0..MIN_DIVS - 1 {
            y += steps;
            out.push((y as i32 as f32, false, None));
        }
        y += steps;
        v -= map_steps;
    }
    out.push((y as i32 as f32, true, Some(format!("{v:.2}{unit}"))));
    out
}

/// `get_voltage(v, p, scaled)`: `v` in pixels (`scaled`) or codes.
pub fn value_text(
    g: &Geom,
    v: f64,
    precision: usize,
    scaled: bool,
    map_min: f64,
    map_max: f64,
    unit: &str,
) -> String {
    let range = (map_max - map_min) * 1000.0;
    let v = if scaled {
        v / g.height as f64 * range
    } else {
        v * g.scale as f64 / g.height as f64 * range
    };
    if v.abs() >= 1000.0 {
        format!("{:.*}{unit}", precision, v / 1000.0)
    } else {
        format!("{v:.precision$}m{unit}")
    }
}

/// `measure`: the sample under pixel `x`.
pub fn hover_index(x: f64, f: &Frame) -> u64 {
    let spp = f.samplerate * f.scale;
    ((x + f.x_offset as f64) * spp + 0.5).floor().max(0.0) as u64
}

/// `get_point`: the code of sample `index` and where it is drawn.
pub fn point(
    store: &AnalogStore,
    order: usize,
    index: u64,
    g: &Geom,
    zero_offset: u16,
    hw_offset: i32,
    f: &Frame,
) -> Option<(u8, f32, f32)> {
    let count = store.sample_count();
    if index >= count {
        return None;
    }
    let ring_index = (store.ring_start() + index) % count;
    let value = *store
        .get_samples(ring_index)?
        .get(order * store.unit_bytes() as usize)?;
    let spp = f.samplerate * f.scale;
    let (top, bottom) = g.top_bottom();
    let x = (index as f64 / spp - f.x_offset as f64) as f32;
    let y = (g.zero_vpos(zero_offset) as f32 + (value as i32 - hw_offset) as f32 * g.scale)
        .max(top)
        .min(bottom);
    Some((value, x, y))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsview_store::analog::AnalogChannel;

    fn store(n: u64) -> AnalogStore {
        let mut s = AnalogStore::new();
        let chans = [AnalogChannel {
            index: 0,
            enabled: true,
        }];
        let data: Vec<u8> = (0..n).map(|i| (i % 256) as u8).collect();
        s.first_payload(&data, n, 8, 1, n, &chans);
        s
    }

    #[test]
    fn mapping() {
        let g = Geom::new(100, 200, Some(0), Some(255), 8);
        assert_eq!(g.ratio2pos(0.0), 0);
        assert_eq!(g.ratio2pos(1.0), 200);
        assert_eq!(g.pos2ratio(100), 0.5);
        assert_eq!(g.zero_vpos(128), (128.0 / 255.0 * 200.0) as i32);
        let r = rule(&g, 128, -1.0, 1.0, "V");
        assert_eq!(r.len(), 51);
        assert_eq!(
            r[0].2.as_deref(),
            Some(format!("{:.2}V", 1.0 + (128.0 / 255.0 - 0.5) * 2.0).as_str())
        );
        assert_eq!(r[50].0, 200.0);
        assert_eq!(value_text(&g, 100.0, 2, true, -1.0, 1.0, "V"), "1.00V");
        assert_eq!(value_text(&g, 10.0, 2, true, -1.0, 1.0, "V"), "100.00mV");
    }

    #[test]
    fn polyline_follows_the_samples() {
        let s = store(1000);
        let g = Geom::new(100, 200, Some(0), Some(255), 8);
        let f = Frame {
            samplerate: 1e6,
            scale: 1e-6,
            x_offset: 0,
            width: 500,
            sample_limit: 1000,
        };
        let Some(Shape::Polyline(p)) = shape(&s, 0, &g, 128, 128, &f) else {
            panic!()
        };
        assert_eq!(p.len(), 501);
        assert_eq!(p[0].0, 0.0);
        assert_eq!(p[3].0, 3.0);
        let zero_y = g.zero_vpos(128) as f32;
        assert_eq!(p[3].1, (zero_y + (3.0 - 128.0) * g.scale).max(0.0));
        let (v, x, _) = point(&s, 0, 3, &g, 128, 128, &f).unwrap();
        assert_eq!((v, x), (3, 3.0));
        assert_eq!(hover_index(2.6, &f), 3);
    }

    #[test]
    fn envelope_columns() {
        let s = store(1 << 16);
        let g = Geom::new(100, 200, Some(0), Some(255), 8);
        let f = Frame {
            samplerate: 1e6,
            scale: 256e-6,
            x_offset: 0,
            width: 200,
            sample_limit: 1 << 16,
        };
        let Some(Shape::Envelope(r)) = shape(&s, 0, &g, 128, 128, &f) else {
            panic!()
        };
        assert!(!r.is_empty());
        // one rect per pixel column, in order
        assert!(r.windows(2).all(|w| w[1].0 == w[0].0 + 1.0));
    }
}
