//! The protocol list's number formats (`AnnotationResTable::format_numberic`
//! with `DecoderDataFormat`): an annotation's hex number shown in hex, dec,
//! oct, bin or ASCII. Groups of hex digits separated by other characters
//! are converted one by one.

/// `DecoderDataFormat`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Format {
    #[default]
    Hex = 0,
    Dec = 1,
    Oct = 2,
    Bin = 3,
    Ascii = 4,
}

impl Format {
    pub const ALL: [Format; 5] = [
        Format::Hex,
        Format::Dec,
        Format::Oct,
        Format::Bin,
        Format::Ascii,
    ];

    /// `DecoderDataFormat::Parse`: unknown names are hex.
    pub fn parse(name: &str) -> Format {
        match name {
            "dec" => Format::Dec,
            "oct" => Format::Oct,
            "bin" => Format::Bin,
            "ascii" => Format::Ascii,
            _ => Format::Hex,
        }
    }

    pub fn name(self) -> &'static str {
        ["hex", "dec", "oct", "bin", "ascii"][self as usize]
    }

    pub fn from_index(i: u8) -> Format {
        Self::ALL.get(i as usize).copied().unwrap_or_default()
    }
}

/// `DECODER_MAX_DATA_BLOCK_LEN`, `CONVERT_STR_MAX_LEN`
pub const MAX_DATA_BLOCK_LEN: usize = 256;
const CONVERT_STR_MAX_LEN: usize = 150;

fn hex_digit(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

/// `bin2oct_string`: groups of three bits from the right.
fn bin2oct(bin: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = bin.len();
    while i > 0 {
        let lo = i.saturating_sub(3);
        let mut v = 0;
        for &b in &bin[lo..i] {
            v = v * 2 + u32::from(b == b'1');
        }
        out.push(b'0' + v as u8);
        i = lo;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// `bin2long_string`: `1 << dex` on a C `int`, so bit 31 is negative and
/// higher bits wrap (x86 masks the shift to 5 bits).
fn bin2long(bin: &[u8]) -> i64 {
    let mut value: i64 = 0;
    for (dex, &b) in bin.iter().rev().enumerate() {
        if b == b'1' {
            value = value.wrapping_add(1i32.wrapping_shl(dex as u32) as i64);
        }
    }
    value
}

/// `format_to_string`: one group of hex digits.
fn format_to_string(hex: &str, fmt: Format) -> String {
    if hex.is_empty() || fmt == Format::Hex || hex.len() > MAX_DATA_BLOCK_LEN {
        return hex.to_owned();
    }
    let mut bin = Vec::with_capacity(hex.len() * 4);
    for c in hex.bytes() {
        let Some(d) = hex_digit(c) else {
            // C asserts; the callers only pass hex digits
            return hex.to_owned();
        };
        for k in (0..4).rev() {
            bin.push(if d >> k & 1 == 1 { b'1' } else { b'0' });
        }
    }
    match fmt {
        Format::Bin => String::from_utf8(bin).unwrap_or_default(),
        Format::Oct => bin2oct(&bin),
        Format::Dec if hex.len() * 4 <= 64 => bin2long(&bin).to_string(),
        Format::Ascii if hex.len() < 30 - 3 => {
            if hex.len() == 2 {
                let v = bin2long(&bin);
                if (33..=126).contains(&v) {
                    return (v as u8 as char).to_string();
                }
            }
            format!("[{hex}]")
        }
        _ => hex.to_owned(),
    }
}

/// `format_numberic`: `hex` in `fmt`, each run of hex digits converted and
/// the separators kept; the hex text itself when the result would not fit.
pub fn format_numberic(hex: &str, fmt: Format) -> String {
    if hex.is_empty() || fmt == Format::Hex {
        return hex.to_owned();
    }
    if hex.bytes().all(|c| hex_digit(c).is_some()) {
        return format_to_string(hex, fmt);
    }
    let mut out = String::new();
    let mut sub = String::new();
    for c in hex.chars() {
        if c.is_ascii_hexdigit() {
            if sub.len() == MAX_DATA_BLOCK_LEN {
                return hex.to_owned();
            }
            sub.push(c);
            continue;
        }
        if !sub.is_empty() {
            let s = format_to_string(&sub, fmt);
            if out.len() + s.len() > CONVERT_STR_MAX_LEN {
                return hex.to_owned();
            }
            out.push_str(&s);
            sub.clear();
        }
        if out.len() + 1 > CONVERT_STR_MAX_LEN {
            return hex.to_owned();
        }
        out.push(c);
    }
    if !sub.is_empty() {
        let s = format_to_string(&sub, fmt);
        if out.len() + s.len() > CONVERT_STR_MAX_LEN {
            return hex.to_owned();
        }
        out.push_str(&s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_numberic("4F", Format::Hex), "4F");
        assert_eq!(format_numberic("4F", Format::Dec), "79");
        assert_eq!(format_numberic("4F", Format::Oct), "117");
        assert_eq!(format_numberic("4F", Format::Bin), "01001111");
        assert_eq!(format_numberic("4F", Format::Ascii), "O");
        assert_eq!(format_numberic("0A", Format::Ascii), "[0A]");
        assert_eq!(format_numberic("12 AB", Format::Dec), "18 171");
        // bit 31 through a C int
        assert_eq!(format_numberic("80000000", Format::Dec), "-2147483648");
        assert_eq!(format_numberic("100000000", Format::Dec), "1");
        assert_eq!(Format::parse("oct"), Format::Oct);
        assert_eq!(Format::parse("nope"), Format::Hex);
    }
}
