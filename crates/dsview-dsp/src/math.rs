//! `pv/data/mathstack.cpp`: the math trace of the oscilloscope. Two
//! channels combined (+, −, ×, ÷) or one channel transformed (integral,
//! derivative, |x|, x², signed √x, moving-average low and high pass), the
//! V/div dial the result gets, and its min/max envelope.

/// `MathStack::MathType`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MathType {
    #[default]
    Add = 0,
    Sub,
    Mul,
    Div,
    Integ,
    Diff,
    Abs,
    Square,
    Sqrt,
    LowPass,
    HighPass,
}

impl MathType {
    pub const ALL: [MathType; 11] = [
        MathType::Add,
        MathType::Sub,
        MathType::Mul,
        MathType::Div,
        MathType::Integ,
        MathType::Diff,
        MathType::Abs,
        MathType::Square,
        MathType::Sqrt,
        MathType::LowPass,
        MathType::HighPass,
    ];

    /// `is_unary`: everything from the integral on uses one source.
    pub fn is_unary(self) -> bool {
        self as i32 >= MathType::Integ as i32
    }

    pub fn from_index(i: i32) -> MathType {
        Self::ALL
            .get(i.max(0) as usize)
            .copied()
            .unwrap_or_default()
    }

    /// The math options dialog's names.
    pub fn name(self) -> &'static str {
        match self {
            MathType::Add => "Add",
            MathType::Sub => "Subtract",
            MathType::Mul => "Multiply",
            MathType::Div => "Divide",
            MathType::Integ => "Integrate",
            MathType::Diff => "Differentiate",
            MathType::Abs => "Absolute",
            MathType::Square => "Square",
            MathType::Sqrt => "Square root",
            MathType::LowPass => "Low-pass",
            MathType::HighPass => "High-pass",
        }
    }

    /// `get_unit(level)`: mV, V, kV with the operator's suffix.
    pub fn unit(self, level: usize) -> &'static str {
        const ADD: [&str; 3] = ["mV", "V", "kV"];
        const MUL: [&str; 3] = ["mV*V", "V*V", "kV*V"];
        const DIV: [&str; 3] = ["mV/V", "V/V", "kV/V"];
        let t = match self {
            MathType::Mul => &MUL,
            MathType::Div => &DIV,
            _ => &ADD,
        };
        t.get(level).copied().unwrap_or(" ")
    }
}

/// `MathStack::vDialValue`
pub const V_DIAL_VALUES: [u64; 19] = [
    1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000, 50000, 100000, 200000,
    500000, 1000000,
];

/// `get_math_scale()`: one tenth for every operator.
pub const MATH_SCALE: f64 = 0.1;

/// A source channel's V/div dial (mV) and probe factor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dial {
    pub value: u64,
    pub factor: u64,
    /// The dial's first and last entries.
    pub min: u64,
    pub max: u64,
}

/// `default_vDialValue`: the first dial entry at or above the result's
/// natural V/div.
pub fn default_vdial(t: MathType, d1: Dial, d2: Dial) -> u64 {
    let (v1, v2) = (d1.factor * d1.value, d2.factor * d2.value);
    let value = match t {
        MathType::Add | MathType::Sub => {
            if v1 > v2 {
                d1.value
            } else {
                d2.value
            }
        }
        MathType::Mul => (d1.value as f64 * d2.value as f64 / 1000.0) as u64,
        MathType::Div => (d1.value as f64 * 1000.0 / d2.value as f64) as u64,
        _ => d1.value,
    };
    V_DIAL_VALUES
        .iter()
        .copied()
        .find(|&v| v >= value)
        .unwrap_or(V_DIAL_VALUES[V_DIAL_VALUES.len() - 1])
}

/// `default_factor`
pub fn default_factor(t: MathType, d1: Dial, d2: Dial) -> u64 {
    let (v1, v2) = (d1.value * d1.factor, d2.value * d2.factor);
    let v = match t {
        MathType::Add | MathType::Sub => {
            if v1 > v2 {
                d1.factor
            } else {
                d2.factor
            }
        }
        MathType::Mul => d1.factor * d2.factor,
        MathType::Div => d1.factor / d2.factor.max(1),
        _ => d1.factor,
    };
    v.max(1)
}

/// `get_vDial()`'s entries: the values that cover the sources' range.
pub fn dial_values(t: MathType, d1: Dial, d2: Dial) -> Vec<u64> {
    let (lo, hi) = match t {
        MathType::Add | MathType::Sub => (d1.min.min(d2.min) as f64, d1.max.max(d2.max) as f64),
        MathType::Mul => (
            d1.min as f64 * d2.min as f64 / 1000.0,
            d1.max as f64 * d2.max as f64 / 1000.0,
        ),
        MathType::Div => (
            (d1.min as f64 * 1000.0 / d2.max as f64).min(d2.min as f64 * 1000.0 / d1.max as f64),
            (d1.max as f64 * 1000.0 / d2.min as f64).max(d2.max as f64 * 1000.0 / d1.min as f64),
        ),
        _ => (d1.min as f64, d1.max as f64),
    };
    let mut out = Vec::new();
    for &v in &V_DIAL_VALUES {
        if (v as f64) < lo {
            continue;
        }
        out.push(v);
        if v as f64 > hi {
            break;
        }
    }
    out
}

/// One source: its codes and their volts (`delta - scale × code`).
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    pub codes: &'a [u8],
    /// `vdiv / 1000 × factor × VDIVS × trace scale / view height`
    pub scale: f64,
    /// `hw_offset × scale`
    pub delta: f64,
}

impl Source<'_> {
    /// `scale1` of `calc_math`: V/div (mV), probe factor, and the trace's
    /// pixels per code over its grid height.
    pub fn scale_for(vdiv_mv: u64, factor: u64, trace_scale: f64, height: i32) -> f64 {
        vdiv_mv as f64 / 1000.0 * factor as f64 * 10.0 * trace_scale / height.max(1) as f64
    }
    fn v(&self, i: usize) -> f64 {
        self.delta - self.scale * self.codes[i] as f64
    }
}

/// `calc_math(mathFactor)` over `n` samples; None when a binary operator
/// lacks its second source.
pub fn calc(
    t: MathType,
    s1: Source<'_>,
    s2: Option<Source<'_>>,
    n: usize,
    samplerate: f64,
    filter_width: i32,
    math_factor: u64,
) -> Option<Vec<f64>> {
    let n = n.min(s1.codes.len());
    let k = math_factor.max(1) as f64;
    let mut out = vec![0.0; n];
    if t.is_unary() {
        let dt = if samplerate > 0.0 {
            1.0 / samplerate
        } else {
            1.0
        };
        let win = filter_width.max(1) as usize;
        let (mut integ, mut acc) = (0.0, 0.0);
        for (i, o) in out.iter_mut().enumerate() {
            let v = s1.v(i);
            let r = match t {
                MathType::Integ => {
                    integ += v * dt;
                    integ
                }
                MathType::Diff => (v - s1.v(i.saturating_sub(1))) / dt,
                MathType::Abs => v.abs(),
                MathType::Square => v * v,
                MathType::Sqrt => {
                    if v < 0.0 {
                        -(-v).sqrt()
                    } else {
                        v.sqrt()
                    }
                }
                MathType::LowPass | MathType::HighPass => {
                    acc += v;
                    if i >= win {
                        acc -= s1.v(i - win);
                    }
                    let avg = acc / (i + 1).min(win) as f64;
                    if t == MathType::LowPass { avg } else { v - avg }
                }
                _ => v,
            };
            *o = r / k;
        }
    } else {
        let s2 = s2?;
        let n = n.min(s2.codes.len());
        out.truncate(n);
        for (i, o) in out.iter_mut().enumerate() {
            let (a, b) = (s1.v(i), s2.v(i));
            *o = match t {
                MathType::Add => (a + b) / k,
                MathType::Sub => (a - b) / k,
                MathType::Mul => a * b / k,
                _ => a / b / k,
            };
        }
    }
    Some(out)
}

/// `MathStack::EnvelopeScalePower`, `ScaleStepCount`
pub const ENVELOPE_SCALE_POWER: u32 = 8;
pub const SCALE_STEP_COUNT: usize = 10;

/// `append_to_envelope_level(true)`: min/max per 256 values, then per 256
/// entries of the level below.
pub fn envelope(math: &[f64]) -> Vec<Vec<(f64, f64)>> {
    let f = 1usize << ENVELOPE_SCALE_POWER;
    let mut levels: Vec<Vec<(f64, f64)>> = Vec::with_capacity(SCALE_STEP_COUNT);
    let l0: Vec<(f64, f64)> = math
        .chunks_exact(f)
        .map(|c| {
            c.iter()
                .fold((c[0], c[0]), |(lo, hi), &v| (lo.min(v), hi.max(v)))
        })
        .collect();
    levels.push(l0);
    for level in 1..SCALE_STEP_COUNT {
        let below = &levels[level - 1];
        let next: Vec<(f64, f64)> = below
            .chunks_exact(f)
            .map(|c| {
                c.iter()
                    .fold(c[0], |(lo, hi), &(a, b)| (lo.min(a), hi.max(b)))
            })
            .collect();
        levels.push(next);
    }
    levels
}

/// `get_math_envelope_section`: (start sample, scale, entries).
pub fn envelope_section(
    levels: &[Vec<(f64, f64)>],
    start: u64,
    end: u64,
    min_length: f32,
) -> (u64, u64, &[(f64, f64)]) {
    let log_factor = ((1u32 << ENVELOPE_SCALE_POWER) as f32).ln();
    let min_level = (((min_length.ln() / log_factor).floor() as i32) - 1).max(0) as usize;
    let power = (min_level as u32 + 1) * ENVELOPE_SCALE_POWER;
    let (s, e) = (start >> power, end >> power);
    let Some(l) = levels.get(min_level).filter(|l| !l.is_empty()) else {
        return (s << power, 1 << power, &[]);
    };
    let from = (s as usize).min(l.len());
    let to = (e as usize).min(l.len()).max(from);
    (s << power, 1 << power, &l[from..to])
}

/// `MathTrace::get_voltage(v, p)`: `v` in volts.
pub fn voltage_text(t: MathType, v: f64, precision: usize) -> String {
    if v.abs() >= 1000.0 {
        format!("{:.*}{}", precision, v / 1000.0, t.unit(2))
    } else if v.abs() >= 1.0 {
        format!("{:.*}{}", precision, v, t.unit(1))
    } else {
        format!("{:.*}{}", precision, v * 1000.0, t.unit(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dial(value: u64, factor: u64) -> Dial {
        Dial {
            value,
            factor,
            min: 10,
            max: 10000,
        }
    }

    #[test]
    fn dials() {
        assert_eq!(
            default_vdial(MathType::Add, dial(500, 1), dial(1000, 1)),
            1000
        );
        assert_eq!(
            default_vdial(MathType::Mul, dial(500, 1), dial(1000, 1)),
            500
        );
        assert_eq!(
            default_vdial(MathType::Div, dial(500, 1), dial(1000, 1)),
            500
        );
        assert_eq!(
            default_vdial(MathType::Div, dial(300, 1), dial(1000, 1)),
            500
        );
        assert_eq!(
            default_factor(MathType::Mul, dial(500, 10), dial(1000, 10)),
            100
        );
        assert_eq!(
            default_factor(MathType::Div, dial(500, 1), dial(1000, 10)),
            1
        );
        let v = dial_values(MathType::Add, dial(500, 1), dial(1000, 1));
        assert_eq!(v.first(), Some(&10));
        assert_eq!(v.last(), Some(&20000));
        assert_eq!(MathType::Div.unit(1), "V/V");
        assert_eq!(voltage_text(MathType::Add, 0.5, 2), "500.00mV");
        assert_eq!(voltage_text(MathType::Mul, 1500.0, 2), "1.50kV*V");
    }

    #[test]
    fn operators() {
        let a = [128u8, 138, 118, 128];
        let b = [128u8, 128, 148, 108];
        let s = |c: &'static [u8]| Source {
            codes: c,
            scale: 0.1,
            delta: 12.8,
        };
        let a: &'static [u8] = Box::leak(Box::new(a));
        let b: &'static [u8] = Box::leak(Box::new(b));
        let add = calc(MathType::Add, s(a), Some(s(b)), 4, 1.0, 10, 1).unwrap();
        let close = |x: &[f64], y: &[f64]| x.iter().zip(y).all(|(p, q)| (p - q).abs() < 1e-9);
        assert!(close(&add, &[0.0, -1.0, -1.0, 2.0]));
        assert!(calc(MathType::Mul, s(a), None, 4, 1.0, 10, 1).is_none());
        let integ = calc(MathType::Integ, s(a), None, 4, 2.0, 10, 1).unwrap();
        assert!(close(&integ, &[0.0, -0.5, 0.0, 0.0]));
        let diff = calc(MathType::Diff, s(a), None, 4, 1.0, 10, 1).unwrap();
        assert!(close(&diff, &[0.0, -1.0, 2.0, -1.0]));
        let lp = calc(MathType::LowPass, s(a), None, 4, 1.0, 2, 1).unwrap();
        assert!(close(&lp, &[0.0, -0.5, 0.0, 0.5]));
        let sq = calc(MathType::Sqrt, s(a), None, 4, 1.0, 2, 1).unwrap();
        assert!(close(&sq, &[0.0, -1.0, 1.0, 0.0]));
    }

    #[test]
    fn envelope_levels() {
        let m: Vec<f64> = (0..70_000).map(|i| (i % 1000) as f64).collect();
        let l = envelope(&m);
        assert_eq!(l[0].len(), 70_000 / 256);
        assert_eq!(l[1].len(), 1);
        assert_eq!(l[0][0], (0.0, 255.0));
        let (start, scale, e) = envelope_section(&l, 512, 65_536, 300.0);
        assert_eq!((start, scale, e.len()), (512, 256, 254));
    }
}
