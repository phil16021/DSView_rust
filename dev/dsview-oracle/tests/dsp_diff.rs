//! `dsview-dsp` against the original kernels on FFTW (`csrc/oracle_dsp.cpp`):
//! the power spectrum of random waveforms for every window, length and
//! sample interval the FFT options offer, within 1e-9 of the spectrum's peak
//! (PLAN.md M6d: "DSP vs FFTW ≤ 1e-9 relative").

use dsview_dsp::spectrum::{Window, power_spectrum, volts_per_code};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn c_spectrum(samples: &[u8], n: usize, step: u16, offset: i32, vdiv: u64, factor: u64, window: i32) -> Vec<f64> {
    let mut out = vec![0.0; n / 2 + 1];
    let r = unsafe {
        dsview_oracle::ffi::osdsp_power_spectrum(
            samples.as_ptr(),
            n as u64,
            step,
            offset,
            vdiv,
            factor,
            window,
            out.as_mut_ptr(),
        )
    };
    assert_eq!(r, 0);
    out
}

/// A sine or square plus noise around `mid`, or pure noise.
fn waveform(r: &mut Rng, len: usize) -> Vec<u8> {
    let kind = r.below(4);
    let mid = 64.0 + r.below(128) as f64;
    let amp = r.below(100) as f64;
    let period = 2.0 + r.below(500) as f64;
    (0..len)
        .map(|i| {
            let ph = i as f64 / period * std::f64::consts::TAU;
            let noise = r.below(9) as f64 - 4.0;
            let v = match kind {
                0 => mid + amp * ph.sin() + noise,
                1 => mid + if ph.sin() >= 0.0 { amp } else { -amp } + noise,
                2 => mid + noise * 10.0,
                _ => mid,
            };
            v.clamp(0.0, 255.0) as u8
        })
        .collect()
}

#[test]
fn power_spectrum_matches_fftw() {
    let mut r = Rng(0x5eed_f00d);
    // the FFT dialog's lengths (and odd ones, which C handles too)
    let lengths = [1024usize, 2048, 4096, 8192, 16384, 32768, 65536, 1000, 1023, 3];
    let mut cases = 0;
    let mut worst = 0.0f64;
    for &n in &lengths {
        for window in 0..5 {
            for _ in 0..4 {
                let step = 1 + r.below(4) as u16;
                let samples = waveform(&mut r, n * step as usize);
                let offset = 100 + r.below(56) as i32;
                let vdiv = [10u64, 20, 50, 100, 200, 500, 1000, 2000, 5000][r.below(9) as usize];
                let factor = [1u64, 10, 100][r.below(3) as usize];
                let c = c_spectrum(&samples, n, step, offset, vdiv, factor, window);
                let rust = power_spectrum(
                    &samples,
                    offset,
                    volts_per_code(vdiv, factor),
                    n,
                    step as usize,
                    Window::from_index(window),
                )
                .expect("enough samples");
                assert_eq!(rust.len(), c.len());
                let peak = c.iter().fold(0.0f64, |a, &b| a.max(b.abs())).max(f64::MIN_POSITIVE);
                for (k, (a, b)) in rust.iter().zip(&c).enumerate() {
                    let err = (a - b).abs() / peak;
                    worst = worst.max(err);
                    assert!(
                        err <= 1e-9,
                        "n {n} window {window} step {step} bin {k}: rust {a} c {b} (peak {peak})"
                    );
                }
                cases += 1;
            }
        }
    }
    eprintln!("{cases} spectra, worst error {worst:e} of the peak");
}
