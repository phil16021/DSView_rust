//! DSO measurements as DSView shows them (`DsoSignal::get_measure`): from
//! the device's status block when it reports a valid measurement, else the
//! software fallback over the samples (`compute_soft_measure`,
//! `data::dso_detect_edges`).

use dsview_core::Status;
use serde_json::{Map, Value};

/// The status block of the last frame and what scales it.
pub struct HwStatus {
    pub status: Status,
    /// Position of the channel (0 or 1).
    pub index: usize,
    pub total_channels: u64,
    pub enabled_channels: u64,
    pub is_file: bool,
}

#[derive(Default)]
struct M {
    valid: bool,
    level_valid: bool,
    min: f64,
    max: f64,
    low: f64,
    high: f64,
    period: f64,
    high_time: f64,
    rise_time: f64,
    fall_time: f64,
    burst_time: f64,
    pcount: u64,
    rms: f64,
    mean: f64,
}

/// `dso_detect_edges`: crossings of the middle level with 5 % hysteresis.
fn detect_edges(n: usize, val: impl Fn(usize) -> f64) -> (Vec<usize>, Vec<usize>) {
    let (mut rising, mut falling) = (Vec::new(), Vec::new());
    if n < 2 {
        return (rising, falling);
    }
    let (mut vmin, mut vmax) = (f64::MAX, f64::MIN);
    for i in 0..n {
        vmin = vmin.min(val(i));
        vmax = vmax.max(val(i));
    }
    if vmax <= vmin {
        return (rising, falling);
    }
    let mid = (vmin + vmax) / 2.0;
    let hyst = (vmax - vmin) * 0.05;
    let mut high = val(0) >= mid;
    for i in 1..n {
        let v = val(i);
        if !high && v >= mid + hyst {
            high = true;
            rising.push(i);
        } else if high && v <= mid - hyst {
            high = false;
            falling.push(i);
        }
    }
    (rising, falling)
}

fn soft(m: &mut M, buf: &[u8], hw_offset: f64, rate: f64) {
    if buf.len() < 2 || rate <= 0.0 {
        return;
    }
    let tfactor = 1e9 / rate;
    let n = buf.len().min(1_000_000);
    let val = |i: usize| hw_offset - buf[i] as f64;
    let (mut rmin, mut rmax, mut sum, mut sumsq) = (255u8, 0u8, 0.0, 0.0);
    for &raw in &buf[..n] {
        rmin = rmin.min(raw);
        rmax = rmax.max(raw);
        let d = hw_offset - raw as f64;
        sum += d;
        sumsq += d * d;
    }
    m.min = rmin as f64;
    m.max = rmax as f64;
    m.rms = (sumsq / n as f64).sqrt();
    m.mean = sum / n as f64;
    m.valid = true;
    let (rising, falling) = detect_edges(n, val);
    if rising.len() < 2 {
        m.period = 0.0;
        m.high_time = 0.0;
        m.rise_time = 0.0;
        m.fall_time = 0.0;
        m.burst_time = 0.0;
        m.pcount = 0;
        m.level_valid = false;
        return;
    }
    let psum: f64 = rising.windows(2).map(|w| (w[1] - w[0]) as f64).sum();
    let period = psum / (rising.len() - 1) as f64;
    let (mut hsum, mut hn, mut fi) = (0.0, 0, 0);
    for &r in &rising {
        while fi < falling.len() && falling[fi] <= r {
            fi += 1;
        }
        if fi < falling.len() {
            hsum += (falling[fi] - r) as f64;
            hn += 1;
        }
    }
    let high = if hn > 0 { hsum / hn as f64 } else { 0.0 };
    m.period = period * tfactor;
    m.high_time = high * tfactor;
    m.pcount = rising.len() as u64;
    m.low = rmin as f64;
    m.high = rmax as f64;
    m.level_valid = true;
    m.rise_time = 0.0;
    m.fall_time = 0.0;
    m.burst_time = (rising[rising.len() - 1] - rising[0]) as f64 * tfactor;
}

/// The hardware path of `paint_mid`; false when the status has no valid
/// measurement.
fn hardware(m: &mut M, hw: &HwStatus, samples: u64, hw_offset: f64, rate: f64) -> bool {
    let s = &hw.status;
    m.valid = true;
    if s.measure_valid == 0 {
        return false;
    }
    let c0 = hw.index == 0;
    let pick = |a: u32, b: u32| f64::from(if c0 { a } else { b });
    m.min = f64::from(if c0 { s.ch0_min } else { s.ch1_min });
    m.max = f64::from(if c0 { s.ch0_max } else { s.ch1_max });
    m.level_valid = (if c0 {
        s.ch0_level_valid
    } else {
        s.ch1_level_valid
    }) != 0;
    m.low = f64::from(if c0 { s.ch0_low_level } else { s.ch1_low_level });
    m.high = f64::from(if c0 {
        s.ch0_high_level
    } else {
        s.ch1_high_level
    });
    let count = if c0 { s.ch0_cyc_cnt } else { s.ch1_cyc_cnt } as f64;
    let plevel = (if c0 { s.ch0_plevel } else { s.ch1_plevel }) != 0;
    let start_xor_end = (if c0 { s.ch0_cyc_llen } else { s.ch1_cyc_llen }) == 0;
    let mut total = hw.total_channels;
    if total == 1 && hw.is_file {
        total += 1;
    }
    let tfactor = (total / hw.enabled_channels.max(1)) as f64 * 1e9 / rate;
    let per = |x: f64, d: f64| if count == 0.0 { 0.0 } else { x / d };
    m.period = per(pick(s.ch0_cyc_tlen, s.ch1_cyc_tlen), count) * tfactor;
    let rise_div = if plevel && start_xor_end {
        count
    } else {
        count + 1.0
    };
    m.rise_time = per(pick(s.ch0_cyc_flen, s.ch1_cyc_flen), rise_div) * tfactor;
    let fall_div = if !plevel && start_xor_end {
        count
    } else {
        count + 1.0
    };
    m.fall_time = per(pick(s.ch0_cyc_rlen, s.ch1_cyc_rlen), fall_div) * tfactor;
    let high = if c0 {
        if s.ch0_plevel != 0 {
            s.ch0_cyc_plen.wrapping_sub(s.ch0_cyc_llen)
        } else {
            s.ch0_cyc_tlen
                .wrapping_sub(s.ch0_cyc_plen)
                .wrapping_add(s.ch0_cyc_llen)
        }
    } else if s.ch1_plevel != 0 {
        s.ch1_cyc_plen.wrapping_sub(s.ch1_cyc_llen)
    } else {
        s.ch1_cyc_tlen
            .wrapping_sub(s.ch1_cyc_plen)
            .wrapping_add(s.ch1_cyc_llen)
    };
    m.high_time = per(f64::from(high), count) * tfactor;
    // C sums ch0's tlen for channel 0 but ch1's flen for channel 1
    let burst = if c0 {
        s.ch0_cyc_tlen.wrapping_add(s.ch0_cyc_llen)
    } else {
        s.ch1_cyc_flen.wrapping_add(s.ch1_cyc_llen)
    };
    m.burst_time = f64::from(burst) * tfactor;
    m.pcount = count as u64 + u64::from(plevel && !start_xor_end);
    let sq = if c0 {
        s.ch0_acc_square
    } else {
        s.ch1_acc_square
    } as f64;
    m.rms = (sq / samples.max(1) as f64).sqrt();
    let mean = if c0 { s.ch0_acc_mean } else { s.ch1_acc_mean } as f64;
    m.mean = hw_offset - mean / samples.max(1) as f64;
    true
}

/// `DsoSignal::get_voltage`: `v` in sample codes.
fn voltage(v: f64, mv_per_code: f64) -> String {
    let v = v * mv_per_code;
    if v.abs() >= 1000.0 {
        format!("{:.2}V", v / 1000.0)
    } else {
        format!("{v:.2}mV")
    }
}

/// `DsoSignal::get_time`: `t` in ns.
fn time(t: f64) -> String {
    if t.abs() > 1e9 {
        format!("{:.2}S", t / 1e9)
    } else if t.abs() > 1e6 {
        format!("{:.2}mS", t / 1e6)
    } else if t.abs() > 1e3 {
        format!("{:.2}uS", t / 1e3)
    } else {
        format!("{t:.2}nS")
    }
}

/// The twenty measurements of channel data `buf` (None: no data), keyed by
/// the control API's names.
pub fn measure(
    buf: Option<&[u8]>,
    hw_offset: f64,
    mv_per_code: f64,
    rate: f64,
    hw: Option<&HwStatus>,
) -> Value {
    let mut m = M::default();
    let empty = buf.is_none_or(|b| b.is_empty());
    if !empty {
        let samples = buf.map_or(0, |b| b.len() as u64);
        let hw_ok = hw.is_some_and(|h| hardware(&mut m, h, samples, hw_offset, rate));
        if !hw_ok || !m.level_valid || m.period == 0.0 {
            soft(&mut m, buf.unwrap_or(&[]), hw_offset, rate);
        }
    }
    let none = || "--".to_string();
    let lv = m.level_valid;
    let pct = |x: f64| format!("{x:.2}%");
    let get = |name: &str| -> String {
        if empty || !m.valid {
            return none();
        }
        match name {
            "amplitude" if lv => voltage(m.high - m.low, mv_per_code),
            "high" if lv => voltage(hw_offset - m.low, mv_per_code),
            "low" if lv => voltage(hw_offset - m.high, mv_per_code),
            "peak_to_peak" => voltage(m.max - m.min, mv_per_code),
            "max" => voltage(hw_offset - m.min, mv_per_code),
            "min" => voltage(hw_offset - m.max, mv_per_code),
            "period" => time(m.period),
            "frequency" => {
                if m.period == 0.0 {
                    none()
                } else if m.period.abs() > 1e6 {
                    format!("{:.2}Hz", 1e9 / m.period)
                } else if m.period.abs() > 1e3 {
                    format!("{:.2}kHz", 1e6 / m.period)
                } else {
                    format!("{:.2}MHz", 1e3 / m.period)
                }
            }
            "rms" => voltage(m.rms, mv_per_code),
            "mean" => voltage(m.mean, mv_per_code),
            "negative_overshoot" if lv && m.high != m.low => {
                pct((m.max - m.high) * 100.0 / (m.high - m.low))
            }
            "positive_overshoot" if lv && m.high != m.low => {
                pct((m.low - m.min) * 100.0 / (m.high - m.low))
            }
            "positive_duty" if lv && m.period != 0.0 => pct(m.high_time / m.period * 100.0),
            "negative_duty" if lv && m.period != 0.0 => pct(100.0 - m.high_time / m.period * 100.0),
            "positive_width" if lv => time(m.high_time),
            "negative_width" if lv => time(m.period - m.high_time),
            "rise_time" if lv => time(m.rise_time),
            "fall_time" if lv => time(m.fall_time),
            "burst_width" if lv => time(m.burst_time),
            "pulse_count" if lv => {
                let c = m.pcount as f64;
                if m.pcount > 1_000_000 {
                    format!("{:.6}M", c / 1e6)
                } else if m.pcount > 1000 {
                    format!("{:.3}K", c / 1e3)
                } else {
                    format!("{c:.0}")
                }
            }
            _ => none(),
        }
    };
    let mut o = Map::new();
    for name in [
        "frequency",
        "period",
        "positive_duty",
        "negative_duty",
        "pulse_count",
        "rise_time",
        "fall_time",
        "positive_width",
        "negative_width",
        "burst_width",
        "amplitude",
        "high",
        "low",
        "rms",
        "mean",
        "peak_to_peak",
        "max",
        "min",
        "positive_overshoot",
        "negative_overshoot",
    ] {
        o.insert(name.into(), get(name).into());
    }
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_wave_soft_measure() {
        // 1 MHz square wave at 100 MS/s: 51 samples high, 49 low
        let buf: Vec<u8> = (0..10_000)
            .map(|i| if i % 100 < 51 { 78 } else { 178 })
            .collect();
        let m = measure(Some(&buf), 128.0, 39.37, 100e6, None);
        assert_eq!(m["frequency"], "1.00MHz");
        assert_eq!(m["positive_duty"], "51.00%");
        assert_eq!(m["max"], "1.97V");
        assert_eq!(measure(None, 128.0, 1.0, 1.0, None)["max"], "--");
    }
}
