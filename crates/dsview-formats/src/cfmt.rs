//! Number formatting that reproduces C `printf` and libsigrok4DSL's
//! string helpers byte for byte (exports must match the C output modules).
//! Verified against glibc `snprintf` in dev/dsview-oracle/tests.

/// C `printf("%.{prec}g", v)` (no `#` flag).
pub fn fmt_g(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() {
            "-nan".into()
        } else {
            "nan".into()
        };
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    let p = prec.max(1);
    if v == 0.0 {
        return if v.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    // Exponent after rounding to p significant digits decides the style.
    let e_form = format!("{:.*e}", p - 1, v);
    let (mantissa, exp) = e_form.split_once('e').expect("exponent");
    let x: i32 = exp.parse().expect("exponent digits");
    if x < -4 || x >= p as i32 {
        let m = strip_zeros(mantissa);
        let sign = if x < 0 { '-' } else { '+' };
        format!("{m}e{sign}{:02}", x.abs())
    } else {
        let decimals = (p as i32 - 1 - x).max(0) as usize;
        strip_zeros(&format!("{v:.decimals$}")).to_string()
    }
}

fn strip_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

/// C `printf("%.{prec}f", v)`.
pub fn fmt_f(v: f64, prec: usize) -> String {
    if v.is_nan() {
        return if v.is_sign_negative() {
            "-nan".into()
        } else {
            "nan".into()
        };
    }
    if v.is_infinite() {
        return if v < 0.0 { "-inf".into() } else { "inf".into() };
    }
    format!("{v:.prec$}")
}

/// libsigrok4DSL `sr_si_string_u64`, including its unpadded remainder
/// (131072 Samples -> "131.72 k Samples").
pub fn si_string_u64(x: u64, unit: &str) -> String {
    for (div, prefix) in [(1_000_000_000u64, "G"), (1_000_000, "M"), (1_000, "k")] {
        if x >= div {
            return if x.is_multiple_of(div) {
                format!("{} {prefix}{unit}", x / div)
            } else {
                format!("{}.{} {prefix}{unit}", x / div, x % div)
            };
        }
    }
    format!("{x} {unit}")
}

/// `sr_samplerate_string`.
pub fn samplerate_string(x: u64) -> String {
    si_string_u64(x, "Hz")
}

/// `sr_samplecount_string`.
pub fn samplecount_string(x: u64) -> String {
    si_string_u64(x, " Samples")
}

/// `sr_period_string`.
pub fn period_string(frequency: u64) -> String {
    if frequency >= 1_000_000_000 {
        format!("{} ns", frequency / 1_000_000_000)
    } else if frequency >= 1_000_000 {
        format!("{} us", frequency / 1_000_000)
    } else if frequency >= 1_000 {
        format!("{} ms", frequency / 1_000)
    } else {
        format!("{frequency} s")
    }
}

/// `sr_time_string`: a duration in ns as the sampling bar shows it
/// ("40.00 ms", "1.00 s", "500 ns").
pub fn time_string(ns: u64) -> String {
    const US: u64 = 1_000;
    const MS: u64 = 1_000_000;
    const SEC: u64 = 1_000_000_000;
    const MIN: u64 = 60 * SEC;
    const HOUR: u64 = 3600 * SEC;
    const DAY: u64 = 86_400 * SEC;
    let t = ns as f64;
    if ns >= DAY {
        format!("{} day", fmt_f(t / DAY as f64, 2))
    } else if ns >= HOUR {
        format!("{} hour", fmt_f(t / HOUR as f64, 2))
    } else if ns >= MIN {
        format!("{} min", fmt_f(t / MIN as f64, 2))
    } else if ns >= SEC {
        format!("{} s", fmt_f(t / SEC as f64, 2))
    } else if ns >= MS {
        format!("{} ms", fmt_f(t / MS as f64, 2))
    } else if ns >= US {
        format!("{} us", fmt_f(t / US as f64, 2))
    } else {
        format!("{ns} ns")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g_format_known_values() {
        assert_eq!(fmt_g(0.0, 15), "0");
        assert_eq!(fmt_g(1.536e-05, 15), "1.536e-05");
        assert_eq!(fmt_g(4.096e-05, 15), "4.096e-05");
        assert_eq!(fmt_g(0.5, 15), "0.5");
        assert_eq!(fmt_g(123456.0, 15), "123456");
        assert_eq!(fmt_g(1e15, 15), "1e+15");
        assert_eq!(fmt_g(0.0001, 15), "0.0001");
        assert_eq!(fmt_g(1.0 / 3.0, 15), "0.333333333333333");
    }

    #[test]
    fn si_strings() {
        assert_eq!(samplecount_string(131072), "131.72 k Samples");
        assert_eq!(samplerate_string(25_000_000), "25 MHz");
        assert_eq!(samplerate_string(100), "100 Hz");
        assert_eq!(period_string(1_000_000_000), "1 ns");
    }
}
