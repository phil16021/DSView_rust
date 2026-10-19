//! Measurements at the mouse (`LogicSignal::measure`, `edges`,
//! `is_by_edge`, `Viewport::measure`): the pulse under the pointer, edge
//! counts between two points, and snapping to the nearest edge.
//!
//! `x` is a trace-area pixel; `spp` samples per pixel and `x_offset` the
//! viewport's offset. Which trace the pointer is over is the caller's
//! business (C++ checks it first in each function).

use dsview_store::logic::LogicStore;

use crate::ruler::{format_freq, format_real_time, format_samples};

/// `View::Unknown_Str`
pub const UNKNOWN: &str = "########";

fn pixel_index(spp: f64, x_offset: i64, x: f64) -> u64 {
    (spp * (x_offset as f64 + x)) as u64
}

/// `LogicSignal::measure`: (start of the pulse under `x`, its end, the end
/// of the next pulse, the level of the pulse). The third index is 0 when the
/// capture ends first; the second is `end + 1` when there is no edge after
/// `x` (and then the third is 0).
pub fn pulse_at(
    store: &LogicStore,
    ch: u16,
    spp: f64,
    x_offset: i64,
    x: f64,
) -> Option<(u64, u64, u64, bool)> {
    let sig = ch as i32;
    if store.empty() || !store.has_data(sig) {
        return None;
    }
    let end = store.ring_sample_count() - 1;
    let mut index = pixel_index(spp, x_offset, x);
    if index > end {
        return None;
    }
    let mut sample = store.get_sample(index, sig);
    let edge0 = sample;
    let index0 = if index == 0 {
        index
    } else {
        index -= 1;
        if store.get_pre_edge(&mut index, sample, 1.0, sig) {
            index
        } else {
            0
        }
    };
    sample = store.get_sample(index, sig);
    index += 1;
    let index1 = if store.get_nxt_edge(&mut index, sample, end, 1.0, sig) {
        index
    } else {
        if index0 == 0 {
            return None;
        }
        return Some((index0, end + 1, 0, edge0));
    };
    sample = store.get_sample(index, sig);
    index += 1;
    let index2 = if store.get_nxt_edge(&mut index, sample, end, 1.0, sig) {
        index
    } else {
        end + 1
    };
    Some((index0, index1, index2, edge0))
}

/// The mouse-measure readouts (`_mm_*`), formatted as DSView shows them.
#[derive(Clone, Debug, PartialEq)]
pub struct MouseMeasure {
    pub start: u64,
    pub end: u64,
    pub next: u64,
    pub high: bool,
    pub width_time: String,
    pub width_samples: String,
    pub period_time: String,
    pub period_samples: String,
    pub frequency: String,
    pub duty: String,
}

/// `Viewport::measure` on one logic trace.
pub fn mouse_measure(
    store: &LogicStore,
    ch: u16,
    spp: f64,
    x_offset: i64,
    x: f64,
    samplerate: u64,
) -> Option<MouseMeasure> {
    let (i0, i1, i2, edge0) = pulse_at(store, ch, spp, x_offset, x)?;
    let width = i1 as i64 - i0 as i64;
    let period = (i2 != 0).then(|| i2 as i64 - i0 as i64);
    let duty = match period {
        Some(p) => {
            let pos = if edge0 { i1 - i0 } else { i2 - i1 };
            let pct = pos as f64 * 100.0 / p as f64;
            format!("+{pct:.2}% / -{:.2}%", 100.0 - pct)
        }
        None => UNKNOWN.into(),
    };
    Some(MouseMeasure {
        start: i0,
        end: i1,
        next: i2,
        high: edge0,
        width_time: format_real_time(width, samplerate),
        width_samples: format_samples(width),
        period_time: period.map_or(UNKNOWN.into(), |p| format_real_time(p, samplerate)),
        period_samples: period.map_or(UNKNOWN.into(), format_samples),
        frequency: period.map_or(UNKNOWN.into(), |p| {
            format_freq(p as f64 / samplerate as f64, 2)
        }),
        duty,
    })
}

/// `LogicSignal::edges(end, start)`: (rising, falling) edges between two
/// samples; None past the capture.
pub fn edge_counts(store: &LogicStore, ch: u16, a: u64, b: u64) -> Option<(u64, u64)> {
    let sig = ch as i32;
    if store.empty() || !store.has_data(sig) {
        return None;
    }
    let count = store.ring_sample_count();
    let (start, end) = (a.min(b), a.max(b));
    if end > count - 1 {
        return None;
    }
    let mut index = start;
    let mut sample = store.get_sample(start, sig);
    let (mut rising, mut falling) = (0, 0);
    loop {
        if !store.get_nxt_edge(&mut index, sample, count, 1.0, sig) || index > end {
            break;
        }
        rising += u64::from(!sample);
        falling += u64::from(sample);
        sample = !sample;
        if index > end {
            break;
        }
    }
    Some((rising, falling))
}

/// `LogicSignal::is_by_edge`: the edge nearest to `x` if it lies within
/// `radius` pixels.
pub fn edge_near(
    store: &LogicStore,
    ch: u16,
    spp: f64,
    x_offset: i64,
    x: f64,
    radius: f64,
) -> Option<u64> {
    let sig = ch as i32;
    if store.empty() || !store.has_data(sig) {
        return None;
    }
    let end = store.ring_sample_count() - 1;
    let pos = spp * (x_offset as f64 + x);
    let mut index = (pos + 0.5).floor() as u64;
    if index > end {
        return None;
    }
    let mut sample = store.get_sample(index, sig);
    let pre = if index == 0 {
        index
    } else {
        index -= 1;
        if store.get_pre_edge(&mut index, sample, 1.0, sig) {
            index
        } else {
            0
        }
    };
    sample = store.get_sample(index, sig);
    index += 1;
    let nxt = if store.get_nxt_edge(&mut index, sample, end, 1.0, sig) {
        index
    } else {
        0
    };
    let idx = match (pre, nxt) {
        (0, 0) => return None,
        (p, n) if p > 0 && n > 0 => {
            if pos - p as f64 > n as f64 - pos {
                n
            } else {
                p
            }
        }
        (p, n) => p.max(n),
    };
    ((idx as f64 - pos).abs() / spp < radius).then_some(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // whole 64-sample groups: other lengths are shifted (F11)
    fn square() -> Vec<bool> {
        // low for 70 samples, high for 30
        (0..1024).map(|i| i % 100 >= 70).collect()
    }

    #[test]
    fn pulse_under_the_mouse() {
        let s = store(&square());
        // one sample per pixel, pointer at sample 250: low pulse 200..270
        let m = mouse_measure(&s, 0, 1.0, 0, 250.0, 1_000_000).unwrap();
        assert_eq!((m.start, m.end, m.next, m.high), (200, 270, 300, false));
        assert_eq!(m.width_samples, "+70");
        assert_eq!(m.period_samples, "+100");
        assert_eq!(m.frequency, "10.00KHz");
        assert_eq!(m.duty, "+30.00% / -70.00%");
    }

    #[test]
    fn counting_and_snapping() {
        let s = store(&square());
        // rising at 70, 170, .. 970, falling at 100, 200, .. 1000
        assert_eq!(edge_counts(&s, 0, 0, 1023), Some((10, 10)));
        assert_eq!(edge_counts(&s, 0, 1023, 0), Some((10, 10)));
        assert_eq!(edge_counts(&s, 0, 0, 150), Some((1, 1)));
        assert_eq!(edge_near(&s, 0, 1.0, 0, 265.0, 10.0), Some(270));
        assert_eq!(edge_near(&s, 0, 1.0, 0, 250.0, 10.0), None);
    }
}
