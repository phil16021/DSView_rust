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

fn c_spectrum(
    samples: &[u8],
    n: usize,
    step: u16,
    offset: i32,
    vdiv: u64,
    factor: u64,
    window: i32,
) -> Vec<f64> {
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
    let lengths = [
        1024usize, 2048, 4096, 8192, 16384, 32768, 65536, 1000, 1023, 3,
    ];
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
                let peak = c
                    .iter()
                    .fold(0.0f64, |a, &b| a.max(b.abs()))
                    .max(f64::MIN_POSITIVE);
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

use dsview_dsp::math::{self, Dial, MathType, Source};

#[test]
fn math_matches_mathstack() {
    let mut r = Rng(0xa11_c0de);
    let mut cases = 0;
    for t in MathType::ALL {
        for _ in 0..20 {
            let n = 1 + r.below(70_000) as usize;
            let a = waveform(&mut r, n);
            let b = waveform(&mut r, n);
            let (s1, s2) = (
                0.001 + r.below(1000) as f64 / 1000.0,
                0.001 + r.below(1000) as f64 / 1000.0,
            );
            let (d1, d2) = (
                s1 * (100 + r.below(56)) as f64,
                s2 * (100 + r.below(56)) as f64,
            );
            let rate = [0.0, 1e3, 1e6, 1e8][r.below(4) as usize];
            let width = r.below(40) as i32 - 2;
            let k = 1 + r.below(100);
            let mut c = vec![0.0; n];
            unsafe {
                dsview_oracle::ffi::osdsp_math(
                    t as i32,
                    a.as_ptr(),
                    b.as_ptr(),
                    n as u64,
                    s1,
                    d1,
                    s2,
                    d2,
                    rate,
                    width,
                    k,
                    c.as_mut_ptr(),
                )
            };
            let rust = math::calc(
                t,
                Source {
                    codes: &a,
                    scale: s1,
                    delta: d1,
                },
                Some(Source {
                    codes: &b,
                    scale: s2,
                    delta: d2,
                }),
                n,
                rate,
                width,
                k,
            )
            .unwrap();
            for (i, (x, y)) in rust.iter().zip(&c).enumerate() {
                assert!(
                    x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()),
                    "{t:?} n {n} sample {i}: rust {x} c {y}"
                );
            }
            // the envelope levels of the result
            let levels = math::envelope(&rust);
            for (level, l) in levels.iter().enumerate().take(3) {
                let cap = l.len().max(1);
                let (mut lo, mut hi) = (vec![0.0; cap], vec![0.0; cap]);
                let len = unsafe {
                    dsview_oracle::ffi::osdsp_math_envelope(
                        c.as_ptr(),
                        n as u64,
                        level as i32,
                        lo.as_mut_ptr(),
                        hi.as_mut_ptr(),
                        cap as u64,
                    )
                };
                assert_eq!(len as usize, l.len(), "{t:?} level {level}");
                for (i, &(mn, mx)) in l.iter().enumerate() {
                    let same =
                        |p: f64, q: f64| p.to_bits() == q.to_bits() || (p.is_nan() && q.is_nan());
                    assert!(
                        same(mn, lo[i]) && same(mx, hi[i]),
                        "{t:?} level {level} entry {i}"
                    );
                }
            }
            cases += 1;
        }
    }
    eprintln!("{cases} math results bit-identical, with their envelopes");
}

#[test]
fn math_dials_match() {
    let vdivs = [10u64, 20, 50, 100, 200, 500, 1000, 2000, 5000];
    let mut cases = 0;
    for t in MathType::ALL {
        for &v1 in &vdivs {
            for &v2 in &vdivs {
                for &(f1, f2) in &[(1u64, 1u64), (10, 1), (1, 10), (100, 10)] {
                    let d1 = Dial {
                        value: v1,
                        factor: f1,
                        min: 10,
                        max: 5000,
                    };
                    let d2 = Dial {
                        value: v2,
                        factor: f2,
                        min: 20,
                        max: 2000,
                    };
                    let (mut cv, mut cf, mut n) = (0u64, 0u64, 0i32);
                    let mut vals = [0u64; 32];
                    unsafe {
                        dsview_oracle::ffi::osdsp_math_dial(
                            t as i32,
                            v1,
                            f1,
                            10,
                            5000,
                            v2,
                            f2,
                            20,
                            2000,
                            &mut cv,
                            &mut cf,
                            vals.as_mut_ptr(),
                            &mut n,
                        )
                    };
                    assert_eq!(math::default_vdial(t, d1, d2), cv, "{t:?} {v1} {v2}");
                    assert_eq!(math::default_factor(t, d1, d2), cf, "{t:?} {f1} {f2}");
                    assert_eq!(
                        math::dial_values(t, d1, d2),
                        vals[..n as usize].to_vec(),
                        "{t:?}"
                    );
                    cases += 1;
                }
            }
        }
    }
    eprintln!("{cases} dial cases");
}
