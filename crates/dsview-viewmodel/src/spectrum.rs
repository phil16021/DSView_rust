//! The FFT pane (`pv/view/spectrumtrace.cpp`): which part of the spectrum
//! shows (scale and offset as fractions of the Nyquist range), the trace as
//! a polyline in linear or dBV units, the frequency and level rulers, and
//! the value under the mouse.

/// `SpectrumTrace::RightMargin`
pub const RIGHT_MARGIN: i32 = 30;
/// `length_support`
pub const LENGTHS: [u64; 5] = [1024, 2048, 4096, 8192, 16384];
/// `DbvRanges`
pub const DBV_RANGES: [i32; 4] = [100, 120, 150, 200];
/// `FFT_ViewMode`, `windows_support`
pub const VIEW_MODES: [&str; 2] = ["Linear RMS", "DBV RMS"];
pub const WINDOWS: [&str; 5] = ["Rectangle", "Hann", "Hamming", "Blackman", "Flat_top"];
/// `FreqMinorDivNum`, `TickHeight`, `VolDivNum`, `VerticalRate`
pub const FREQ_MINOR_DIV_NUM: i32 = 10;
pub const TICK_HEIGHT: f64 = 15.0;
pub const VOL_DIV_NUM: i32 = 5;
pub const VERTICAL_RATE: f64 = 1.0 / 2000.0;
/// `DS_CONF_DSO_HDIVS`
const HDIVS: f64 = 10.0;

/// One channel's FFT settings and zoom (`SpectrumTrace` + `SpectrumStack`).
#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    pub enabled: bool,
    pub sample_num: u64,
    pub interval: u64,
    pub window: i32,
    pub dc_ignore: bool,
    /// 0 linear, 1 dBV
    pub view_mode: usize,
    pub dbv_range: i32,
    pub scale: f64,
    pub offset: f64,
}

impl Default for Spectrum {
    fn default() -> Self {
        Spectrum {
            enabled: false,
            sample_num: LENGTHS[0],
            interval: 1,
            window: 0,
            dc_ignore: true,
            view_mode: 0,
            dbv_range: DBV_RANGES[1],
            scale: 1.0,
            offset: 0.0,
        }
    }
}

impl Spectrum {
    /// `zoom(steps, offset)`: around pixel `px` of a `width` wide pane.
    pub fn zoom(&mut self, steps: f64, px: i32, width: i32) {
        let width = width.max(1) as f64;
        let pre = self.offset + self.scale * px as f64 / width;
        self.scale *= 1.5f64.powf(-steps);
        self.scale = self.scale.min(1.0).max(100.0 / self.sample_num as f64);
        self.offset = (pre - self.scale * px as f64 / width)
            .min(1.0 - self.scale)
            .max(0.0);
    }

    /// `set_offset(delta)`: pan by `delta` pixels.
    pub fn pan(&mut self, delta: f64, width: i32) {
        self.offset += delta * self.scale / width.max(1) as f64;
        self.offset = self.offset.min(1.0 - self.scale).max(0.0);
    }

    /// `_vmin`, `_vmax` for the channel's V/div and probe factor.
    pub fn range(&self, vdiv: f64, factor: f64) -> (f64, f64) {
        let top = vdiv * HDIVS * factor * VERTICAL_RATE;
        if self.view_mode == 0 {
            (0.0, top)
        } else {
            let vmax = 20.0 * top.log10();
            (vmax - self.dbv_range as f64, vmax)
        }
    }

    fn level(&self, mag: f64, vmin: f64) -> f64 {
        if self.view_mode == 0 {
            mag
        } else if mag < 10f64.powf(vmin / 20.0) {
            vmin
        } else {
            20.0 * mag.log10()
        }
    }

    /// `paint_mid`: the polyline over `width` × `height` pixels.
    pub fn points(
        &self,
        bins: &[f64],
        vdiv: f64,
        factor: f64,
        width: f64,
        height: f64,
    ) -> Vec<(f32, f32)> {
        if bins.is_empty() {
            return Vec::new();
        }
        let (vmin, vmax) = self.range(vdiv, factor);
        let full = (self.sample_num / 2) as f64;
        let view_off = full * self.offset;
        let view_start = view_off.floor() as i64;
        let view_size = (full * self.scale) as i64;
        let pps = width / view_size.max(1) as f64;
        let scale = height / (vmax - vmin);
        let mut x = (view_start as f64 - view_off) * pps;
        let mut sample = view_start.max(0) as usize;
        if self.dc_ignore && sample == 0 {
            sample += 1;
            x += pps;
        }
        let mut out = Vec::new();
        // a do-while in C: the first point is always drawn
        while let Some(&mag) = bins.get(sample) {
            let y = height - scale * (self.level(mag, vmin) - vmin);
            out.push((x as f32, y as f32));
            x += pps;
            sample += 1;
            if x >= width || x.is_nan() {
                break;
            }
        }
        out
    }

    /// `measure(p)`: the bin under pixel `x`, if inside the spectrum.
    pub fn hover_bin(&self, x: f64, width: f64) -> Option<usize> {
        let full = (self.sample_num / 2) as f64;
        let spp = full * self.scale / width.max(1.0);
        let i = (x * spp + full * self.offset).round();
        (i >= 0.0 && i < full).then_some(i as usize)
    }

    /// The hover point and text (`paint_fore`): x, y, value, "value@freq".
    #[allow(clippy::too_many_arguments)]
    pub fn hover(
        &self,
        bins: &[f64],
        bin: usize,
        vdiv: f64,
        factor: f64,
        width: f64,
        height: f64,
        delta_freq: f64,
    ) -> Option<(f64, f64, String)> {
        let &mag = bins.get(bin)?;
        let (vmin, vmax) = self.range(vdiv, factor);
        let full = (self.sample_num / 2) as f64;
        let view_size = (full * self.scale) as i64;
        let pps = width / view_size.max(1) as f64;
        let x = (bin as f64 - full * self.offset) * pps;
        let v = self.level(mag, vmin);
        let y = height - height / (vmax - vmin) * (v - vmin);
        let unit = if self.view_mode == 0 { "" } else { "dbv" };
        Some((
            x,
            y,
            format!("{v:.4}{unit}@{}", format_freq(delta_freq * bin as f64, 4)),
        ))
    }

    /// The frequency ruler: (x, major, label) from `paint_fore`.
    pub fn freq_ticks(&self, snap_rate: f64, width: f64) -> Vec<(f64, bool, Option<String>)> {
        let ny = snap_rate / (2.0 * self.interval as f64);
        let range = ny * self.scale;
        let off = ny * self.offset;
        let order = range.log10().floor() as i32;
        let multiplier = if 10f64.powi(order) == range {
            range / 10.0
        } else {
            10f64.powi(order)
        };
        let fpp = range / width;
        let mut tick = multiplier * (off / multiplier).floor() as i64 as f64;
        let mut division = (tick * FREQ_MINOR_DIV_NUM as f64 / multiplier).round() as i64;
        let mut x = (tick - off) / fpp;
        let mut out = Vec::new();
        loop {
            if division % FREQ_MINOR_DIV_NUM as i64 == 0 {
                out.push((x, true, Some(format_freq(tick, 2))));
            } else {
                out.push((x, false, None));
            }
            tick += multiplier / FREQ_MINOR_DIV_NUM as f64;
            division += 1;
            x = (tick - off) / fpp;
            if x >= width || x.is_nan() || out.len() > 10_000 {
                break;
            }
        }
        out
    }

    /// The level ruler: (y, label) from `paint_fore`, bottom up.
    pub fn level_ticks(&self, vdiv: f64, factor: f64, height: f64) -> Vec<(f64, String)> {
        let (vmin, vmax) = self.range(vdiv, factor);
        let per = (vmax - vmin) / VOL_DIV_NUM as f64;
        let unit = if self.view_mode == 0 { "" } else { "dbv" };
        let mut v = per + vmin;
        let mut y = height - height / VOL_DIV_NUM as f64;
        let mut out = Vec::new();
        while y > 0.0 {
            out.push((y, format!("{v:.2}{unit}")));
            v += per;
            y -= height / VOL_DIV_NUM as f64;
        }
        out
    }
}

/// The bin spacing: `cur_snap_samplerate / (sample_num × interval)`.
pub fn delta_freq(snap_rate: f64, sample_num: u64, interval: u64) -> f64 {
    snap_rate / (sample_num * interval) as f64
}

/// `SpectrumTrace::format_freq`
pub fn format_freq(freq: f64, precision: usize) -> String {
    const PREFIXES: [&str; 9] = ["", "", "", "", "K", "M", "G", "T", "P"];
    const FIRST: i32 = -9;
    if freq <= 0.0 {
        return "0Hz".into();
    }
    let order = (freq as f32).log10().floor() as i32;
    let prefix = ((order - FIRST) as f32 / 3.0).floor() as i32;
    let divider = 10f64.powf((prefix as f64 * 3.0 + FIRST as f64).max(0.0));
    let p = PREFIXES
        .get(prefix.clamp(0, 8) as usize)
        .copied()
        .unwrap_or("");
    format!("{:.*}{p}Hz", precision, freq / divider)
}

/// `FftOptions`: the lengths below half the device's DSO depth, and the
/// intervals 1, 2, 4, … that fit `limit / 2 / length`.
pub fn lengths_and_intervals(max_dso_samplelimits: u64, length: u64) -> (Vec<u64>, Vec<u64>) {
    let limit = (max_dso_samplelimits as f64 * 0.5) as u64;
    let lengths: Vec<u64> = LENGTHS.iter().copied().filter(|&l| l < limit).collect();
    let mut intervals = Vec::new();
    let max_interval = limit / length.max(1);
    let mut i = 1;
    while i <= max_interval {
        intervals.push(i);
        i *= 2;
    }
    (lengths, intervals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_and_pan_stay_inside() {
        let mut s = Spectrum::default();
        s.zoom(2.0, 500, 1000);
        assert!((s.scale - 1.0 / 2.25).abs() < 1e-12);
        // the frequency under the mouse stays
        assert!((s.offset + s.scale * 0.5 - 0.5).abs() < 1e-12);
        s.pan(1e6, 1000);
        assert_eq!(s.offset, 1.0 - s.scale);
        for _ in 0..50 {
            s.zoom(1.0, 0, 1000);
        }
        assert_eq!(s.scale, 100.0 / 1024.0);
    }

    #[test]
    fn polyline_and_hover() {
        let s = Spectrum::default();
        let mut bins = vec![0.0; 513];
        bins[64] = 0.5;
        let p = s.points(&bins, 1000.0, 1.0, 1024.0, 200.0);
        // DC skipped; 2 px per bin; vmax = 1000 × 10 / 2000 = 5
        assert_eq!(p[0], (2.0, 200.0));
        assert_eq!(p[63], (128.0, (200.0 - 200.0 / 5.0 * 0.5) as f32));
        assert_eq!(s.hover_bin(128.4, 1024.0), Some(64));
        let (_, _, t) = s
            .hover(&bins, 64, 1000.0, 1.0, 1024.0, 200.0, 1e6 / 1024.0)
            .unwrap();
        assert_eq!(t, "0.5000@62.5000KHz");
        let mut d = s.clone();
        d.view_mode = 1;
        assert_eq!(
            d.range(1000.0, 1.0),
            (20.0 * 5f64.log10() - 120.0, 20.0 * 5f64.log10())
        );
    }

    #[test]
    fn rulers_and_texts() {
        assert_eq!(format_freq(0.0, 2), "0Hz");
        assert_eq!(format_freq(1500.0, 2), "1.50KHz");
        assert_eq!(format_freq(2.5e6, 4), "2.5000MHz");
        assert_eq!(format_freq(12.0, 2), "12.00Hz");
        let s = Spectrum::default();
        // 1 MHz rate: 500 kHz span, ticks every 10 kHz, labels every 100 kHz
        let t = s.freq_ticks(1e6, 1000.0);
        assert_eq!(t.len(), 50);
        assert_eq!(t[0], (0.0, true, Some("0Hz".into())));
        assert_eq!(t[10].2.as_deref(), Some("100.00KHz"));
        let l = s.level_ticks(1000.0, 1.0, 200.0);
        assert_eq!(l.len(), 4);
        assert_eq!(l[0].1, "1.00");
        let (lens, ints) = lengths_and_intervals(20_000, 1024);
        assert_eq!(lens, vec![1024, 2048, 4096, 8192]);
        assert_eq!(ints, vec![1, 2, 4, 8]);
    }
}
