//! A logic trace's shape (`LogicSignal::paint_mid_align`): the store's
//! per-pixel edge summary (`get_display_edges`) turned into line segments,
//! plus sample dots when zoomed in far enough.
//!
//! Coordinates are relative to the trace area's left edge; y is in the same
//! space as the trace's `y` and `height`.

use dsview_store::logic::LogicStore;

use crate::viewport::{Timebase, Viewport};

/// `LogicSignal::TogMaxScale`: up to one toggle per 10 pixels the trace is
/// drawn from the toggle list; denser, from the per-pixel summary.
pub const TOG_MAX_SCALE: u16 = 10;
/// `Trace::SampleDotMinSpacing`: dots from 10 pixels per sample.
pub const SAMPLE_DOT_MIN_SPACING: f64 = 10.0;

/// One straight line of the trace.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Line {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogicShape {
    pub lines: Vec<Line>,
    /// Sample positions (x, y), when samples are at least 10 px apart.
    pub dots: Vec<(f32, f32)>,
}

/// Reusable buffers for [`logic_shape`] (the store fills them per frame).
#[derive(Default)]
pub struct EdgeBuffers {
    pulses: Vec<(bool, bool)>,
    togs: Vec<(u16, bool)>,
}

/// The shape of channel `ch` over `width` pixels. `y` is the trace's
/// vertical centre (`get_y() + totalHeight / 2`), `height` its total height;
/// `end_align_sample` is the last sample to draw (the first channel's ring
/// count in time view).
#[allow(clippy::too_many_arguments)]
pub fn logic_shape(
    store: &LogicStore,
    ch: u16,
    vp: &Viewport,
    tb: &Timebase,
    width: u16,
    y: i32,
    height: i32,
    end_align_sample: u64,
    buf: &mut EdgeBuffers,
) -> LogicShape {
    let mut out = LogicShape::default();
    let rate = tb.snap_samplerate;
    if store.empty() || rate == 0.0 || vp.scale <= 0.0 || !store.has_data(ch as i32) {
        return out;
    }
    let ring = store.ring_sample_count();
    let last_sample = end_align_sample.min(ring - 1) as i64;
    let high = (y as f32 - height as f32 + 0.5) as i32;
    let low = (y as f32 + 0.5) as i32;
    let spp = rate * vp.scale;
    let offset = vp.x_offset;
    let start = offset as f64 * spp;
    let end = (offset + width as i64 + 1) as f64 * spp;
    let end_index = (end.floor() as i64).max(0).min(last_sample) as u64;
    let start_index = start.floor().max(0.0) as u64;
    if start_index > end_index {
        return out;
    }
    let width = width.min(((end_index + 1) as f64 / spp - offset as f64).ceil() as u16);
    let max_togs = width / TOG_MAX_SCALE;
    let first = store.get_display_edges(
        &mut buf.pulses,
        &mut buf.togs,
        start_index,
        end_index,
        width,
        max_togs,
        offset as f64,
        spp,
        ch,
    );
    let level = |b: bool| if b { high } else { low } as f32;
    let line = |out: &mut LogicShape, x0: i32, y0: f32, x1: i32, y1: f32| {
        out.lines.push(Line {
            x0: x0 as f32,
            y0,
            x1: x1 as f32,
            y1,
        });
    };
    let (mut pre_x, mut pre_y, mut x) = (0i32, level(first), 0i32);
    if buf.togs.len() < max_togs as usize {
        let n = buf.togs.len();
        for &(tx, l) in &buf.togs[1..n - 1] {
            x = tx as i32;
            line(&mut out, pre_x, pre_y, x, pre_y);
            line(&mut out, x, high as f32, x, low as f32);
            pre_x = x;
            pre_y = level(l);
        }
        x = buf.togs[n - 1].0 as i32;
        line(&mut out, pre_x, pre_y, x, pre_y);
    } else if !buf.pulses.is_empty() {
        for &(edge, l) in &buf.pulses[..buf.pulses.len() - 1] {
            if edge {
                line(&mut out, pre_x, pre_y, x, pre_y);
                line(&mut out, x, high as f32, x, low as f32);
                pre_x = x;
                pre_y = level(l);
            }
            x += 1;
        }
        line(&mut out, pre_x, pre_y, x, pre_y);
    }
    if 1.0 / spp >= SAMPLE_DOT_MIN_SPACING {
        for i in start_index..=end_index {
            let sx = i as f64 / spp - offset as f64;
            out.dots
                .push((sx as f32, level(store.get_sample(i, ch as i32))));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store with one channel holding `bits` (LSB first).
    fn store(bits: &[bool]) -> LogicStore {
        let n = bits.len().div_ceil(64) * 64;
        let mut words = vec![0u64; n / 64];
        for (i, &b) in bits.iter().enumerate() {
            if b {
                words[i / 64] |= 1 << (i % 64);
            }
        }
        let data: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        let mut s = LogicStore::new();
        s.first_payload(&data, bits.len() as u64, &[0], true);
        s.capture_ended();
        s
    }

    fn tb(n: u64) -> Timebase {
        Timebase {
            snap_samplerate: 1e6,
            samplerate: 1e6,
            sample_limit: n,
            dso_view_time: None,
        }
    }

    #[test]
    fn sparse_square_wave_is_drawn_from_toggles() {
        // 1024 samples, period 128: 15 edges inside, drawn over 1024 px
        let bits: Vec<bool> = (0..1024).map(|i| (i / 64) % 2 == 1).collect();
        let s = store(&bits);
        let t = tb(1024);
        let mut v = Viewport {
            width: 1024,
            ..Viewport::default()
        };
        v.update_limits(&t);
        v.set_scale_offset(1e-6, 0, &t); // one sample per pixel
        let shape = logic_shape(
            &s,
            0,
            &v,
            &t,
            1024,
            50,
            40,
            1023,
            &mut EdgeBuffers::default(),
        );
        let verticals: Vec<f32> = shape
            .lines
            .iter()
            .filter(|l| l.x0 == l.x1)
            .map(|l| l.x0)
            .collect();
        let expected: Vec<f32> = (1..16).map(|k| (k * 64) as f32).collect();
        assert_eq!(verticals, expected);
        // levels: low (y + 0.5 -> 50) until 64, then high (50 - 40 + 0.5 -> 10)
        assert_eq!(
            shape.lines[0],
            Line {
                x0: 0.0,
                y0: 50.0,
                x1: 64.0,
                y1: 50.0
            }
        );
        assert!(shape.dots.is_empty());
    }

    #[test]
    fn zoomed_in_shows_sample_dots() {
        let bits: Vec<bool> = (0..256).map(|i| i % 2 == 0).collect();
        let s = store(&bits);
        let t = tb(256);
        let mut v = Viewport {
            width: 400,
            ..Viewport::default()
        };
        v.update_limits(&t);
        v.set_scale_offset(1e-6 / 20.0, 0, &t); // 20 px per sample
        let shape = logic_shape(&s, 0, &v, &t, 400, 50, 40, 255, &mut EdgeBuffers::default());
        assert_eq!(shape.dots.len(), 21);
        assert_eq!(shape.dots[1], (20.0, 50.0));
    }
}
