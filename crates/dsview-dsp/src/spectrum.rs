//! `pv/data/spectrumstack.cpp`: a DSO channel's power spectrum. FFTW's
//! real-to-halfcomplex transform is replaced by realfft's forward
//! transform; both give the same unnormalised bins.

use realfft::RealFftPlanner;

/// `PI` as spectrumstack.cpp defines it (DEVIATIONS.md: kept for parity).
#[allow(clippy::approx_constant)]
pub const PI: f64 = 3.1415;

/// The FFT windows of the FFT options dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Rectangle = 0,
    Hann = 1,
    Hamming = 2,
    Blackman = 3,
    FlatTop = 4,
}

impl Window {
    /// `SpectrumStack::window(i, type)` for an `n`-sample transform.
    pub fn at(self, i: u64, n: usize) -> f64 {
        let n_m_1 = n as f64 - 1.0;
        let x = i as f64;
        match self {
            Window::Hann => 0.5 * (1.0 - (2.0 * PI * x / n_m_1).cos()),
            Window::Hamming => 0.54 - 0.46 * (2.0 * PI * x / n_m_1).cos(),
            Window::Blackman => {
                0.42659 - 0.49656 * (2.0 * PI * x / n_m_1).cos()
                    + 0.076849 * (4.0 * PI * x / n_m_1).cos()
            }
            Window::FlatTop => {
                1.0 - 1.93 * (2.0 * PI * x / n_m_1).cos() + 1.29 * (4.0 * PI * x / n_m_1).cos()
                    - 0.388 * (6.0 * PI * x / n_m_1).cos()
                    + 0.028 * (8.0 * PI * x / n_m_1).cos()
            }
            Window::Rectangle => 1.0,
        }
    }

    pub fn from_index(i: i32) -> Window {
        match i {
            1 => Window::Hann,
            2 => Window::Hamming,
            3 => Window::Blackman,
            4 => Window::FlatTop,
            _ => Window::Rectangle,
        }
    }
}

/// The input scaling of `calc_fft`: volts per code from the V/div (mV) and
/// the probe factor. C divides by 255, not by the reference range.
pub fn volts_per_code(vdiv_mv: u64, factor: u64) -> f64 {
    vdiv_mv as f64 * factor as f64 * 10.0 / (1000.0 * 255.0)
}

/// `calc_fft`: `n` samples taken every `step`-th code of `samples`, centred
/// on `hw_offset`, scaled and windowed. Returns the `n / 2 + 1` power
/// spectrum bins (RMS amplitude; bin 0 the DC level), as C fills
/// `_power_spectrum`. None when `samples` holds fewer than `n * step`.
pub fn power_spectrum(
    samples: &[u8],
    hw_offset: i32,
    vscale: f64,
    n: usize,
    step: usize,
    window: Window,
) -> Option<Vec<f64>> {
    if n < 2 || step == 0 || samples.len() < (n - 1) * step + 1 {
        return None;
    }
    let mut xn = Vec::with_capacity(n);
    let mut wsum = 0.0;
    for i in 0..n {
        let w = window.at(i as u64, n);
        xn.push((samples[i * step] as i32 - hw_offset) as f64 * vscale * w);
        wsum += w;
    }
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(n);
    let mut xk = fft.make_output_vec();
    fft.process(&mut xn, &mut xk).ok()?;
    let mut ps = vec![0.0; n / 2 + 1];
    ps[0] = xk[0].re.abs() / wsum;
    for k in 1..n.div_ceil(2) {
        ps[k] = ((xk[k].re * xk[k].re + xk[k].im * xk[k].im) * 2.0).sqrt() / wsum;
    }
    if n.is_multiple_of(2) {
        ps[n / 2] = xk[n / 2].re.abs() / wsum;
    }
    Some(ps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_shows_up_in_its_bin() {
        // 1 V amplitude at bin 64 of 1024, codes around 128
        let n = 1024;
        let vscale = volts_per_code(1000, 1); // 39.2 mV per code
        let amp_codes = 1.0 / vscale;
        let samples: Vec<u8> = (0..n)
            .map(|i| {
                let v = 128.0
                    + amp_codes * (2.0 * std::f64::consts::PI * 64.0 * i as f64 / n as f64).sin();
                v.round() as u8
            })
            .collect();
        let ps = power_spectrum(&samples, 128, vscale, n, 1, Window::Rectangle).unwrap();
        let peak = (1..ps.len())
            .max_by(|&a, &b| ps[a].total_cmp(&ps[b]))
            .unwrap();
        assert_eq!(peak, 64);
        // RMS of a 1 V sine, within the 8-bit quantisation
        assert!((ps[64] - 1.0 / 2f64.sqrt()).abs() < 0.01, "{}", ps[64]);
        assert!(ps[0].abs() < 0.01);
    }

    #[test]
    fn windows_match_their_formulas() {
        assert_eq!(Window::Rectangle.at(3, 16), 1.0);
        // Qt's PI = 3.1415 makes the Hann window not quite 0 at the end
        let h = Window::Hann.at(15, 16);
        assert!(h > 0.0 && h < 1e-6, "{h}");
        assert!((Window::Hamming.at(0, 16) - 0.08).abs() < 1e-12);
    }
}
