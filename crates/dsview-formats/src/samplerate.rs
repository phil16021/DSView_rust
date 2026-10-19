//! Samplerate strings in `.dsl` headers ("25 MHz", "500 kHz").
//!
//! The C writer (`sr_si_string_u64`) prints a non-integral rate as
//! `<rate / unit>.<rate % unit>` with the remainder in Hz, unpadded: 1.05 MHz
//! becomes "1.50000 MHz", 1.5 MHz becomes "1.500000 MHz". The C reader
//! (`sr_parse_sizestring`) stops at the dot, so such files load with a wrong
//! rate (DEVIATIONS.md F3). This reader decodes the writer's actual format,
//! integer part times unit plus the digits after the dot as Hz, which recovers
//! every rate the C writer can produce. The Rust writer never emits a dot.

use crate::Error;

/// Parses a samplerate string into Hz.
pub fn parse(s: &str) -> Result<u64, Error> {
    let s = s.trim();
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, rest) = s.split_at(num_end);
    if num.is_empty() {
        return Err(Error::Header(format!("samplerate {s:?} has no number")));
    }
    let rest = rest.trim_start();
    let (mult, unit) = match rest.chars().next() {
        Some('k' | 'K') => (1_000u64, &rest[1..]),
        Some('m' | 'M') => (1_000_000, &rest[1..]),
        Some('g' | 'G') => (1_000_000_000, &rest[1..]),
        _ => (1, rest),
    };
    if !(unit.is_empty() || unit.eq_ignore_ascii_case("hz")) {
        return Err(Error::Header(format!(
            "samplerate {s:?} has an unknown unit"
        )));
    }

    let malformed = || Error::Header(format!("samplerate {s:?} is malformed"));
    let (int, rem) = num.split_once('.').unwrap_or((num, ""));
    let int: u64 = int.parse().map_err(|_| malformed())?;
    let hz = int
        .checked_mul(mult)
        .ok_or_else(|| Error::Header(format!("samplerate {s:?} overflows")))?;
    if rem.is_empty() {
        return Ok(hz);
    }
    // C writer remainder: Hz below one unit.
    let rem: u64 = rem.parse().map_err(|_| malformed())?;
    if mult == 1 || rem >= mult {
        return Err(malformed());
    }
    Ok(hz + rem)
}

/// Formats a samplerate so that both the C and the Rust reader recover it exactly.
pub fn format(hz: u64) -> String {
    for (div, unit) in [
        (1_000_000_000u64, "GHz"),
        (1_000_000, "MHz"),
        (1_000, "kHz"),
    ] {
        if hz >= div && hz.is_multiple_of(div) {
            return format!("{} {unit}", hz / div);
        }
    }
    format!("{hz} Hz")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn dsview_strings() {
        assert_eq!(parse("25 MHz").unwrap(), 25_000_000);
        assert_eq!(parse("1 GHz").unwrap(), 1_000_000_000);
        assert_eq!(parse("500 kHz").unwrap(), 500_000);
        assert_eq!(parse("100 Hz").unwrap(), 100);
        assert_eq!(parse("10").unwrap(), 10);
        assert!(parse("fast").is_err());
        assert!(parse("1 THz").is_err());
        assert!(parse("1.2.3 kHz").is_err());
        assert!(parse("1.5 Hz").is_err());
    }

    #[test]
    fn c_writer_fractions() {
        // sr_si_string_u64(1_050_000) == "1.50000 MHz"; (1_500_000) == "1.500000 MHz".
        assert_eq!(parse("1.50000 MHz").unwrap(), 1_050_000);
        assert_eq!(parse("1.500000 MHz").unwrap(), 1_500_000);
        assert_eq!(parse("2.500 kHz").unwrap(), 2_500);
        assert!(parse("1.1000 kHz").is_err());
    }

    #[test]
    fn format_never_uses_fractions() {
        assert_eq!(format(25_000_000), "25 MHz");
        assert_eq!(format(2_500_000), "2500 kHz");
        assert_eq!(format(1_050_000), "1050 kHz");
        assert_eq!(format(12_345), "12345 Hz");
        assert_eq!(format(0), "0 Hz");
    }

    /// The C writer, for checking that every string it can produce decodes back.
    fn c_format(x: u64) -> String {
        for (div, unit) in [
            (1_000_000_000u64, "GHz"),
            (1_000_000, "MHz"),
            (1_000, "kHz"),
        ] {
            if x >= div {
                return if x.is_multiple_of(div) {
                    format!("{} {unit}", x / div)
                } else {
                    format!("{}.{} {unit}", x / div, x % div)
                };
            }
        }
        format!("{x} Hz")
    }

    proptest! {
        #[test]
        fn round_trip(hz in 0u64..=10_000_000_000) {
            prop_assert_eq!(parse(&format(hz)).unwrap(), hz);
        }

        #[test]
        fn decodes_every_c_written_rate(hz in 0u64..=10_000_000_000) {
            prop_assert_eq!(parse(&c_format(hz)).unwrap(), hz);
        }
    }
}
