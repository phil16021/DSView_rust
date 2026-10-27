//! Decoder traces (`pv/view/decodetrace.cpp`): which rows are shown, when a
//! row is detailed enough to draw, the annotation shapes, their colours,
//! and the text that fits.

use crate::ruler::TextMeasure;

/// `DecodeTrace::EndCapWidth`, `DrawPadding`.
pub const END_CAP_WIDTH: f64 = 5.0;
pub const DRAW_PADDING: f64 = 100.0;

/// `DecodeTrace::Colours`: row base colours (dark and light themes).
pub const COLOURS: [[u8; 3]; 16] = [
    [0xEF, 0x29, 0x29],
    [0xF6, 0x6A, 0x32],
    [0xFC, 0xAE, 0x3E],
    [0xFB, 0xCA, 0x47],
    [0xFC, 0xE9, 0x4F],
    [0xCD, 0xF0, 0x40],
    [0x8A, 0xE2, 0x34],
    [0x4E, 0xDC, 0x44],
    [0x55, 0xD7, 0x95],
    [0x64, 0xD1, 0xD2],
    [0x72, 0x9F, 0xCF],
    [0xD4, 0x76, 0xC4],
    [0x9D, 0x79, 0xB9],
    [0xAD, 0x7F, 0xA8],
    [0xC2, 0x62, 0x9B],
    [0xD7, 0x47, 0x6F],
];

/// `get_row_base_colour(local_row)` for the dark/light themes.
pub fn row_base_colour(local_row: usize) -> [u8; 3] {
    COLOURS[local_row % COLOURS.len()]
}

/// `compute_colour_luminance`
pub fn luminance([r, g, b]: [u8; 3]) -> i32 {
    (0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64) as i32
}

/// `get_text_colour`: black on light fills, white on dark.
pub fn text_colour(fill: [u8; 3]) -> [u8; 3] {
    if luminance(fill) > 128 {
        [0, 0, 0]
    } else {
        [255, 255, 255]
    }
}

/// QColor's HSL (h 0..359 or -1, s, l 0..255) from RGB.
fn to_hsl([r, g, b]: [u8; 3]) -> (i32, i32, i32) {
    let (r, g, b) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if max == min {
        return (-1, 0, (l * 255.0).round() as i32);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    } * 60.0;
    (
        h.round() as i32 % 360,
        (s * 255.0).round() as i32,
        (l * 255.0).round() as i32,
    )
}

fn from_hsl(h: i32, s: i32, l: i32) -> [u8; 3] {
    let (s, l) = (s as f64 / 255.0, l as f64 / 255.0);
    if h < 0 || s == 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h as f64 / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let f = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [f(r), f(g), f(b)]
}

/// `generate_annotation_colours`: (fill, outline) for an annotation of
/// `ann_type` in local row `local_row` on base colour `base`.
pub fn annotation_colours(base: [u8; 3], local_row: usize, ann_type: i32) -> ([u8; 3], [u8; 3]) {
    let v = (ann_type % 3) - 1;
    let hue_offset = (15.0 * if local_row != 0 { 4.0 } else { 1.0 } * v as f64) as i32;
    let lightness_offset = 10 * (local_row as i32 % 6) * v;
    let (h, s, l) = to_hsl(base);
    let h = if h < 0 {
        h
    } else {
        (360 + ((h + hue_offset) % 360)) % 360
    };
    let l = (l + lightness_offset).clamp(0, 255);
    let fill = from_hsl(h, s, l);
    let lo = if luminance(fill) > 128 { -128 } else { 80 };
    let (h2, s2, l2) = to_hsl(fill);
    let outline = from_hsl(h2, s2, (l2 + lo).clamp(0, 255));
    (fill, outline)
}

/// The detail gate of `paint_mid`: whether a row with annotations from
/// `min_len` (shortest non-zero) to `max_len` samples long is drawn at
/// `spp` samples per pixel, or replaced by "Zoom in for details".
pub fn row_detailed(min_len: u64, max_len: u64, spp: f64) -> bool {
    let min_w = min_len as f64 / spp;
    let max_w = max_len as f64 / spp;
    max_w > 100.0 || (max_w > 10.0 && (min_w > 1.0 || spp < 50.0)) || (max_w == 0.0 && spp < 10.0)
}

/// How one annotation is drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum AnnShape {
    /// Skipped: off-screen, or under half a pixel after the previous one.
    None,
    /// A range narrower than 2 px: one vertical line.
    Line { x: f64 },
    /// A range: the hexagon with caps, and the text that fits (with its
    /// left-aligned fade when even the shortest form does not fit).
    Range {
        start: f64,
        end: f64,
        cap: f64,
        text: Option<(String, bool)>,
    },
    /// A zero-length annotation: a rounded box `w` wide around `x`.
    Instant { x: f64, w: f64, text: String },
}

/// `draw_annotation` / `draw_range` / `draw_instant` without the painting:
/// `start`/`end` in samples, `left`/`right` the visible pixel range, `h`
/// the row height. `last_x` carries over between the annotations of a row.
#[allow(clippy::too_many_arguments)]
pub fn annotation_shape(
    start: u64,
    end: u64,
    lines: &[String],
    spp: f64,
    x_offset: i64,
    left: f64,
    right: f64,
    h: f64,
    min_ann_width: f64,
    last_x: &mut f64,
    font: &dyn TextMeasure,
) -> AnnShape {
    let s = (start as f64 / spp - x_offset as f64).max(left);
    let e = (end as f64 / spp - x_offset as f64).min(right);
    if s > right + DRAW_PADDING || e < left - DRAW_PADDING {
        return AnnShape::None;
    }
    if e - *last_x <= 0.5 && e - s < 1.0 {
        return AnnShape::None;
    }
    *last_x = e;
    if start == end {
        return AnnShape::Instant {
            x: s,
            w: min_ann_width.min(h),
            text: lines.last().cloned().unwrap_or_default(),
        };
    }
    if s + 2.0 > e {
        return AnnShape::Line { x: s };
    }
    let cap = ((e - s) / 4.0).min(END_CAP_WIDTH);
    let room = e - s - cap;
    let text = if lines.is_empty() || room <= 6.0 {
        None
    } else {
        // the first form that fits; else the last one, faded at the right
        Some(
            lines
                .iter()
                .find(|t| font.width(t) as f64 <= room + 1.0)
                .map(|t| (t.clone(), false))
                .unwrap_or_else(|| (lines.last().cloned().unwrap_or_default(), true)),
        )
    };
    AnnShape::Range {
        start: s,
        end: e,
        cap,
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Mono;
    impl TextMeasure for Mono {
        fn width(&self, t: &str) -> f32 {
            7.0 * t.chars().count() as f32
        }
        fn height(&self) -> f32 {
            14.0
        }
    }

    #[test]
    fn colours_follow_qt() {
        // type % 3 == 1: no offset, the base colour itself
        let (fill, _) = annotation_colours(COLOURS[0], 0, 1);
        assert_eq!(fill, COLOURS[0]);
        assert_eq!(text_colour([0xFC, 0xE9, 0x4F]), [0, 0, 0]);
        assert_eq!(text_colour([0x30, 0x30, 0x30]), [255, 255, 255]);
        // hue shifts for the other types
        let (f2, _) = annotation_colours(COLOURS[10], 1, 2);
        assert_ne!(f2, COLOURS[10]);
    }

    #[test]
    fn detail_gate() {
        assert!(row_detailed(100, 2000, 10.0));
        assert!(!row_detailed(5, 50, 100.0));
        assert!(row_detailed(0, 0, 5.0));
    }

    #[test]
    fn text_fits_or_fades() {
        let lines = vec!["Address write: 62".to_owned(), "AW: 62".into(), "A".into()];
        let mut last = -1.0;
        let s = annotation_shape(
            0, 100, &lines, 1.0, 0, 0.0, 1000.0, 20.0, 1.0, &mut last, &Mono,
        );
        match s {
            AnnShape::Range {
                text: Some((t, false)),
                ..
            } => assert_eq!(t, "AW: 62"),
            other => panic!("{other:?}"),
        }
        let mut last = -1.0;
        let s = annotation_shape(
            0, 14, &lines, 1.0, 0, 0.0, 1000.0, 20.0, 1.0, &mut last, &Mono,
        );
        match s {
            AnnShape::Range {
                text: Some((t, true)),
                ..
            } => assert_eq!(t, "A"),
            AnnShape::Range {
                text: Some((t, false)),
                ..
            } => assert_eq!(t, "A"),
            other => panic!("{other:?}"),
        }
    }
}
