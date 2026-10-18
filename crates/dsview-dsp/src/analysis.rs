//! The DSO analysis dialogs: the value and period histograms with jitter
//! (`pv/dialogs/dsohistogram.cpp`) and the channel-to-channel delay, skew
//! and phase (`pv/dialogs/chanmeasure.cpp`).

use crate::measure::detect_edges;

/// `MaxSamples`: deep captures are analysed up to 8 M samples.
pub const MAX_SAMPLES: usize = 8_000_000;
/// `NBINS`, `TBINS`
pub const BINS: usize = 128;

/// A histogram over `min..=max` (`HistogramPlot::set_data`).
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub bins: Vec<f64>,
    pub min: f64,
    pub max: f64,
    pub mean: f64,
}

fn histogram(values: impl Iterator<Item = f64> + Clone, n: usize) -> Histogram {
    let (mut min, mut max, mut sum) = (1e300f64, -1e300f64, 0.0);
    for v in values.clone() {
        min = min.min(v);
        max = max.max(v);
        sum += v;
    }
    let span = if max > min { max - min } else { 1.0 };
    let mut bins = vec![0.0; BINS];
    for v in values {
        let b = (((v - min) / span * (BINS - 1) as f64) as i32).clamp(0, BINS as i32 - 1);
        bins[b as usize] += 1.0;
    }
    Histogram {
        bins,
        min,
        max,
        mean: sum / n as f64,
    }
}

/// `DsoHistogram::compute`'s result.
#[derive(Clone, Debug, PartialEq)]
pub struct HistogramStats {
    pub samples: usize,
    /// mV
    pub value: Histogram,
    /// Rising edges (on the raw codes).
    pub edges: usize,
    /// ns; None with fewer than 3 edges.
    pub period: Option<Histogram>,
    pub rms_jitter: f64,
    pub pk_pk_jitter: f64,
}

/// The histograms of `codes` (mV = (hw_offset − code) × `mv_per_code`) at
/// `samplerate`; None with fewer than 2 samples.
pub fn histograms(
    codes: &[u8],
    hw_offset: i32,
    mv_per_code: f64,
    samplerate: f64,
) -> Option<HistogramStats> {
    if codes.len() < 2 {
        return None;
    }
    let n = codes.len().min(MAX_SAMPLES);
    let c = &codes[..n];
    let volt = c
        .iter()
        .map(|&b| (hw_offset as f64 - b as f64) * mv_per_code);
    let value = histogram(volt, n);
    let dt_ns = if samplerate > 0.0 {
        1e9 / samplerate
    } else {
        0.0
    };
    let (rising, _) = detect_edges(n, |i| c[i] as f64);
    let mut out = HistogramStats {
        samples: n,
        value,
        edges: rising.len(),
        period: None,
        rms_jitter: 0.0,
        pk_pk_jitter: 0.0,
    };
    if rising.len() >= 3 && dt_ns > 0.0 {
        let periods: Vec<f64> = rising
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 * dt_ns)
            .collect();
        let h = histogram(periods.iter().copied(), periods.len());
        let var: f64 = periods.iter().map(|p| (p - h.mean) * (p - h.mean)).sum();
        out.rms_jitter = (var / periods.len() as f64).sqrt();
        out.pk_pk_jitter = h.max - h.min;
        out.period = Some(h);
    }
    Some(out)
}

/// `extract_edges`: rising and falling edge times (ns) and the mean period.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edges {
    pub rising: Vec<f64>,
    pub falling: Vec<f64>,
    pub period: f64,
}

pub fn edges(codes: &[u8], hw_offset: i32, mv_per_code: f64, dt_ns: f64) -> Option<Edges> {
    if codes.len() < 2 || dt_ns <= 0.0 {
        return None;
    }
    let n = codes.len().min(MAX_SAMPLES);
    let (r, f) = detect_edges(n, |i| (hw_offset as f64 - codes[i] as f64) * mv_per_code);
    let rising: Vec<f64> = r.iter().map(|&i| i as f64 * dt_ns).collect();
    let falling: Vec<f64> = f.iter().map(|&i| i as f64 * dt_ns).collect();
    let period = if rising.len() >= 2 {
        rising.windows(2).map(|w| w[1] - w[0]).sum::<f64>() / (rising.len() - 1) as f64
    } else {
        0.0
    };
    Some(Edges {
        rising,
        falling,
        period,
    })
}

/// `mean_delay`: the mean signed distance (ns) from each edge of `a` to
/// the nearest of `b`, wrapped into (−period/2, period/2].
pub fn mean_delay(a: &[f64], b: &[f64], period: f64) -> Option<f64> {
    if a.is_empty() || b.is_empty() || period <= 0.0 {
        return None;
    }
    let (mut sum, mut cnt) = (0.0, 0);
    for &ta in a {
        let i = b.partition_point(|&x| x < ta);
        let mut best = None;
        if i < b.len() {
            best = Some(b[i] - ta);
        }
        if i > 0 {
            let d = b[i - 1] - ta;
            if best.is_none_or(|x: f64| d.abs() < x.abs()) {
                best = Some(d);
            }
        }
        let Some(mut best) = best else { continue };
        while best > period / 2.0 {
            best -= period;
        }
        while best <= -period / 2.0 {
            best += period;
        }
        sum += best;
        cnt += 1;
    }
    (cnt > 0).then(|| sum / cnt as f64)
}

/// `DsoChannelMeasure::compute`'s text for sources named `na` and `nb`.
pub fn channel_measure(na: &str, a: &Edges, nb: &str, b: &Edges) -> String {
    let none = "--".to_string();
    let freq = |e: &Edges| {
        if e.period > 0.0 {
            fmt_freq_hz(1e9 / e.period)
        } else {
            none.clone()
        }
    };
    let mut t = format!("Freq {na}: {}\nFreq {nb}: {}\n\n", freq(a), freq(b));
    let period = a.period;
    let delay = mean_delay(&a.rising, &b.rising, period);
    let skew = mean_delay(&a.falling, &b.falling, period);
    t += &format!(
        "Delay (rise): {}\n",
        delay.map_or(none.clone(), fmt_time_ns)
    );
    t += &format!("Skew (fall): {}\n", skew.map_or(none.clone(), fmt_time_ns));
    let phase = match delay {
        Some(d) if period > 0.0 => {
            let mut deg = d / period * 360.0;
            while deg > 180.0 {
                deg -= 360.0;
            }
            while deg <= -180.0 {
                deg += 360.0;
            }
            format!("{deg:.2} °")
        }
        _ => none,
    };
    t += &format!("Phase: {phase}");
    t
}

/// `fmt_time_ns`
pub fn fmt_time_ns(t: f64) -> String {
    let a = t.abs();
    if a >= 1e9 {
        format!("{:.3} s", t / 1e9)
    } else if a >= 1e6 {
        format!("{:.3} ms", t / 1e6)
    } else if a >= 1e3 {
        format!("{:.3} us", t / 1e3)
    } else {
        format!("{t:.2} ns")
    }
}

/// `fmt_freq_hz`
pub fn fmt_freq_hz(f: f64) -> String {
    let a = f.abs();
    if a >= 1e6 {
        format!("{:.3} MHz", f / 1e6)
    } else if a >= 1e3 {
        format!("{:.3} kHz", f / 1e3)
    } else {
        format!("{f:.2} Hz")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(n: usize, period: usize, shift: usize) -> Vec<u8> {
        (0..n)
            .map(|i| {
                if ((i + period - shift) / (period / 2)).is_multiple_of(2) {
                    50
                } else {
                    200
                }
            })
            .collect()
    }

    #[test]
    fn histogram_and_jitter() {
        let c = square(10_000, 100, 0);
        let h = histograms(&c, 128, 10.0, 1e6).unwrap();
        // two levels: (128-200)·10 = −720 mV and (128-50)·10 = 780 mV
        assert_eq!((h.value.min, h.value.max), (-720.0, 780.0));
        assert_eq!(h.value.bins[0] + h.value.bins[127], 10_000.0);
        assert!((h.value.mean - 30.0).abs() < 1.0);
        // the raw codes rise once per period
        let p = h.period.unwrap();
        assert_eq!((p.min, p.max), (100_000.0, 100_000.0));
        assert_eq!((h.rms_jitter, h.pk_pk_jitter), (0.0, 0.0));
        assert!(histograms(&[1], 128, 1.0, 1.0).is_none());
    }

    #[test]
    fn delay_and_phase() {
        let a = edges(&square(10_000, 100, 0), 128, 10.0, 10.0).unwrap();
        let b = edges(&square(10_000, 100, 25), 128, 10.0, 10.0).unwrap();
        assert_eq!(a.period, 1000.0);
        let d = mean_delay(&a.rising, &b.rising, a.period).unwrap();
        assert!((d - 250.0).abs() < 1e-9, "{d}");
        let t = channel_measure("0", &a, "1", &b);
        assert!(t.contains("Freq 0: 1.000 MHz"), "{t}");
        assert!(t.contains("Delay (rise): 250.00 ns"), "{t}");
        assert!(t.contains("Phase: 90.00 °"), "{t}");
        assert_eq!(fmt_time_ns(1500.0), "1.500 us");
        assert_eq!(fmt_freq_hz(12.0), "12.00 Hz");
    }
}
