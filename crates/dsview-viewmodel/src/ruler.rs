//! The time ruler (`pv/view/ruler.cpp`): tick spacing, labels in time or
//! sample units relative to the trigger, and the number formats the view
//! uses elsewhere (cursor labels, measurements).
//!
//! Ported as Qt draws it. The spacing is a multiple of 10 samples that
//! leaves room for a label, not a 1-2-5 sequence (`ScaleUnits` is declared
//! and unused in C++), so labels need not be round numbers (DEVIATIONS F27).

/// `Ruler::SIPrefixes`, from femto.
pub const SI_PREFIXES: [&str; 9] = ["f", "p", "n", "\u{03BC}", "m", "", "k", "M", "G"];
/// `Ruler::FreqPrefixes`
pub const FREQ_PREFIXES: [&str; 9] = ["P", "T", "G", "M", "K", "", "", "", ""];
pub const FIRST_SI_PREFIX_POWER: i32 = -15;
/// `Ruler::MinPeriodScale`: minor ticks per major tick.
pub const MIN_PERIOD_SCALE: i32 = 10;
/// `Ruler::CursorHsbColorTable`: hue per cursor number.
pub const CURSOR_HUES: [u16; 22] = [
    120, 195, 270, 345, 60, 135, 210, 285, 15, 75, 150, 225, 300, 30, 90, 165, 240, 315, 45, 105,
    180, 255,
];

/// Text width and height in pixels of the ruler's font.
pub trait TextMeasure {
    fn width(&self, text: &str) -> f32;
    fn height(&self) -> f32;
}

/// `Ruler::format_time(t, prefix, precision)`: seconds with an SI prefix
/// and a sign (`+` for zero and up).
pub fn format_time(t: f64, prefix: i32, precision: usize) -> String {
    let multiplier = 10f64.powf(-(prefix as f64) * 3.0 - FIRST_SI_PREFIX_POWER as f64 + 6.0);
    let v = (t * multiplier) / 1_000_000.0;
    let sign = if v >= 0.0 { "+" } else { "" };
    let p = SI_PREFIXES
        .get(prefix.clamp(0, 8) as usize)
        .copied()
        .unwrap_or("");
    format!("{sign}{v:.precision$}{p}s")
}

/// `Ruler::format_freq(period, precision)`; `View::Unknown_Str` for no
/// period.
pub fn format_freq(period: f64, precision: usize) -> String {
    if period <= 0.0 {
        return crate::measure::UNKNOWN.into();
    }
    let order = (period as f32).log10().ceil() as i32;
    let prefix = ((order - FIRST_SI_PREFIX_POWER) as f32 / 3.0).ceil() as i32;
    let multiplier = 10f64.powf((-(prefix as f64) * 3.0 - FIRST_SI_PREFIX_POWER as f64).max(0.0));
    let unit = FREQ_PREFIXES
        .get(prefix.clamp(0, 8) as usize)
        .copied()
        .unwrap_or("");
    format!("{:.precision$}{unit}Hz", 1.0 / (period * multiplier))
}

/// `Ruler::format_real_time(delta_index, samplerate)`: a sample distance
/// as a time with just the digits it needs (computed in picoseconds).
pub fn format_real_time(delta_index: i64, samplerate: u64) -> String {
    let v1 = 1e12 / samplerate as f64;
    let dt = v1 * delta_index as f64;
    if dt > i64::MAX as f64 || dt < i64::MIN as f64 {
        return "INF".into();
    }
    let mut delta = dt as i64;
    if delta == 0 {
        return "0".into();
    }
    let prefix = (delta.unsigned_abs() as f64).log10().floor() as i32;
    let mut zero = 0;
    while delta == delta / 10 * 10 {
        delta /= 10;
        zero += 1;
    }
    let precision = if prefix / 3 * 3 > zero {
        prefix / 3 * 3 - zero
    } else {
        0
    };
    format_time(
        delta as f64 / 10f64.powi(12 - zero),
        prefix / 3 + 1,
        precision as usize,
    )
}

/// `Ruler::format_samples`
pub fn format_samples(delta: i64) -> String {
    if delta > 0 {
        format!("+{delta}")
    } else {
        delta.to_string()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TickKind {
    Major,
    Minor,
}

/// Where a label sits (Qt: centred on the tick, top at `2 * ValueMargin`).
#[derive(Clone, Debug, PartialEq)]
pub enum LabelSlot {
    /// The major-tick row.
    Top,
    /// A relative label of a minor tick, drawn in the box down to the minor
    /// tick's top.
    Relative,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    pub x: i32,
    pub kind: TickKind,
    pub label: Option<(String, LabelSlot)>,
}

/// The ruler's tick layout for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct RulerTicks {
    pub ticks: Vec<Tick>,
    /// The SI prefix the cursor labels use (`_cur_prefix`).
    pub prefix: i32,
    /// Smallest tick period (`_min_period`), for hover snapping.
    pub min_period: f64,
    /// y of the major ticks' top, of the minor ticks' top.
    pub major_y1: i32,
    pub minor_y1: i32,
}

/// `Ruler::draw_logic_tick_mark` without the drawing: the ticks of a view
/// `view_width` px wide at `scale` s/px, `offset` px from the start, over a
/// ruler `height` px high. Times are relative to `trigger_pos`.
#[allow(clippy::too_many_arguments)]
pub fn logic_ticks(
    scale: f64,
    offset: i64,
    view_width: i32,
    ruler_right: i32,
    height: i32,
    samplerate: u64,
    trigger_pos: u64,
    show_samples: bool,
    font: &dyn TextMeasure,
) -> RulerTicks {
    const SPACING_INCREMENT: f64 = 32.0;
    const MIN_VALUE_SPACING: f64 = 16.0;
    const VALUE_MARGIN: i32 = 5;
    let rate = samplerate as f64;
    let abs_min_period = 10.0 / rate;
    let cur_period_scale = ((scale * SPACING_INCREMENT) / abs_min_period).ceil();
    let min_period = cur_period_scale * abs_min_period;
    let order = ((scale * view_width as f64) as f32).log10().floor() as i32;
    let prefix = (order - FIRST_SI_PREFIX_POWER) / 3;
    let trigger_time = trigger_pos as f64 / rate;
    let time_prep = offset as f64 * scale - trigger_pos as f64 / rate;

    let digit = font.width("8") as f64;
    let typical_width = digit
        * (if show_samples {
            format_samples(((offset + view_width as i64) as f64 * scale * rate) as i64)
                .chars()
                .count()
        } else {
            format_time(time_prep, prefix, 2).chars().count()
        }) as f64
        + MIN_VALUE_SPACING;
    let mut tick_period = 0.0;
    loop {
        tick_period += min_period;
        // C's do-while: until the period leaves room for a label (NaN ends it)
        let room = typical_width.partial_cmp(&(tick_period / scale));
        if room != Some(std::cmp::Ordering::Greater) || min_period <= 0.0 {
            break;
        }
    }
    let text_height = font.height().round() as i32;
    let minor_tick_period = tick_period / MIN_PERIOD_SCALE as f64;
    let minor_order = (minor_tick_period as f32).log10().floor() as i32;
    let minor_prefix = (minor_order - FIRST_SI_PREFIX_POWER) / 3;
    let first_major = (time_prep / tick_period).floor();
    let first_minor = (time_prep / minor_tick_period + 1.0).floor();
    let t0 = first_major * tick_period;
    let mut division = (first_minor - first_major * MIN_PERIOD_SCALE as f64).round() as i32 - 1;
    let major_y1 = text_height + VALUE_MARGIN * 3;
    let minor_y1 = (major_y1 + height) / 2;
    let inc_text_width = if show_samples {
        digit
    } else {
        font.width(&format_time(minor_tick_period, minor_prefix, 2)) as f64
    } + MIN_VALUE_SPACING;

    let label = |t: f64, p: i32| {
        if show_samples {
            format_samples((t * rate).round() as i64)
        } else {
            format_time(t, p, 2)
        }
    };
    let mut ticks = Vec::new();
    loop {
        let t = t0 + division as f64 * minor_tick_period;
        let major_t = t0 + (division as f64 / MIN_PERIOD_SCALE as f64).floor() * tick_period;
        let x = ((t + trigger_time) / scale - offset as f64) as i32;
        if division % MIN_PERIOD_SCALE == 0 {
            ticks.push(Tick {
                x,
                kind: TickKind::Major,
                label: Some((label(t, prefix), LabelSlot::Top)),
            });
        } else {
            let text = if minor_tick_period / scale > 2.0 * typical_width {
                Some((label(t, prefix), LabelSlot::Top))
            } else if minor_tick_period / scale > 1.1 * inc_text_width
                || tick_period / scale > view_width as f64
            {
                Some((label(t - major_t, minor_prefix), LabelSlot::Relative))
            } else {
                None
            };
            ticks.push(Tick {
                x,
                kind: TickKind::Minor,
                label: text,
            });
        }
        division += 1;
        if x >= ruler_right || ticks.len() > 100_000 {
            break;
        }
    }
    RulerTicks {
        ticks,
        prefix,
        min_period,
        major_y1,
        minor_y1,
    }
}

/// `Cursor::Offset`, `Cursor::ArrowSize`, `Cursor::CloseSize`,
/// `View::LabelPadding`, `Ruler::CursorSelWidth`.
pub const LABEL_OFFSET: f64 = 1.0;
pub const ARROW_SIZE: f64 = 10.0;
pub const CLOSE_SIZE: f64 = 10.0;
pub const LABEL_PADDING: f64 = 4.0;
pub const CURSOR_SEL_WIDTH: f64 = 20.0;

/// A rectangle as Qt's `QRect`/`QRectF` gives it: left, top, width, height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bx {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Bx {
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// `Cursor::get_label_rect`: the label of a cursor at pixel `x` whose text
/// measures `text_w` × `text_h`, in a ruler `ruler_h` high (integers, as
/// `QRect`).
pub fn cursor_label_rect(x: i64, text_w: f64, text_h: f64, ruler_h: f64) -> Bx {
    let (w, h) = (
        (text_w + LABEL_PADDING * 2.0).trunc(),
        (text_h + LABEL_PADDING * 2.0).trunc(),
    );
    let top = (ruler_h - h - LABEL_OFFSET - ARROW_SIZE - 0.5).trunc();
    Bx {
        x: x as f64 - (w as i64 / 2) as f64,
        y: top,
        w,
        h,
    }
}

/// `Cursor::get_close_rect`: the delete box at the label's top right
/// (`QRect::right()` is the last pixel column).
pub fn cursor_close_rect(label: Bx) -> Bx {
    Bx {
        x: label.x + label.w - 1.0 - CLOSE_SIZE,
        y: label.y,
        w: CLOSE_SIZE,
        h: CLOSE_SIZE,
    }
}

/// `Ruler::get_cursor_sel_rect`: box `index` of the strip opened at
/// `sel_x` (0: the new-cursor box, then one per cursor).
pub fn cursor_sel_rect(sel_x: f64, index: usize, ruler_h: f64) -> Bx {
    Bx {
        x: sel_x - (0.5 - index as f64) * CURSOR_SEL_WIDTH,
        y: ruler_h - 1.3 * CURSOR_SEL_WIDTH,
        w: CURSOR_SEL_WIDTH,
        h: CURSOR_SEL_WIDTH,
    }
}

/// What a click on the ruler does to the cursors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RulerAction {
    None,
    /// A new cursor at the strip's pixel.
    Add(f64),
    /// Cursor n (from 1) to the strip's pixel.
    Move(usize, f64),
    Delete(usize),
    /// `set_cursor_middle`: cursor n to the middle of the view.
    GoTo(usize),
}

/// The ruler's cursor strips (`_cursor_sel_visible`, `_cursor_go_visible`,
/// `_cursor_sel_x`): a left click opens the selector (new cursor or move
/// one here), a right click the go-to strip; in the selector a right click
/// deletes a cursor.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RulerStrips {
    pub sel_x: Option<f64>,
    pub sel_visible: bool,
    pub go_visible: bool,
}

impl RulerStrips {
    /// `in_cursor_sel_rect`: the strip box under the pointer.
    pub fn hit(&self, px: f64, py: f64, cursors: usize, ruler_h: f64) -> Option<usize> {
        let x = self.sel_x?;
        (0..=cursors).find(|&i| cursor_sel_rect(x, i, ruler_h).contains(px, py))
    }

    pub fn visible(&self) -> bool {
        self.sel_visible || self.go_visible
    }

    /// `mouseReleaseEvent`, left button, when no cursor label was hit.
    pub fn left_release(&mut self, px: f64, py: f64, cursors: usize, ruler_h: f64) -> RulerAction {
        if self.go_visible {
            let over = self.hit(px, py, cursors, ruler_h);
            self.go_visible = false;
            return match over {
                Some(n) if n > 0 => RulerAction::GoTo(n),
                _ => RulerAction::None,
            };
        }
        if !self.sel_visible {
            self.sel_x = Some(px);
            self.sel_visible = true;
            return RulerAction::None;
        }
        self.sel_visible = false;
        let x = self.sel_x.unwrap_or(px);
        match self.hit(px, py, cursors, ruler_h) {
            Some(0) => RulerAction::Add(x),
            Some(n) => RulerAction::Move(n, x),
            None => RulerAction::None,
        }
    }

    /// `mouseReleaseEvent`, right button.
    pub fn right_release(&mut self, px: f64, py: f64, cursors: usize, ruler_h: f64) -> RulerAction {
        if !self.sel_visible {
            if !self.go_visible {
                self.sel_x = Some(px);
                self.go_visible = true;
            }
            return RulerAction::None;
        }
        match self.hit(px, py, cursors, ruler_h) {
            Some(n) if n > 0 => {
                if cursors == 1 {
                    self.sel_visible = false;
                }
                RulerAction::Delete(n)
            }
            _ => RulerAction::None,
        }
    }

    /// `leaveEvent`
    pub fn leave(&mut self) {
        self.sel_visible = false;
        self.go_visible = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A monospace font: 7 px per character, 14 px high.
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
    fn formats() {
        assert_eq!(format_time(1.5e-6, 3, 2), "+1.50\u{03BC}s");
        assert_eq!(format_time(-2e-3, 4, 2), "-2.00ms");
        assert_eq!(format_samples(5), "+5");
        assert_eq!(format_samples(0), "0");
        assert_eq!(format_samples(-3), "-3");
        // 1000 samples at 25 MHz = 40 us
        assert_eq!(format_real_time(1000, 25_000_000), "+40\u{03BC}s");
        assert_eq!(format_real_time(1, 25_000_000), "+40ns");
        assert_eq!(format_real_time(0, 25_000_000), "0");
        assert_eq!(format_freq(1e-6, 2), "1.00MHz");
        assert_eq!(format_freq(0.0, 2), "########");
    }

    #[test]
    fn ticks_cover_the_ruler_and_major_every_ten() {
        // protocol.demo at 20 samples per pixel on 1830 px
        let r = logic_ticks(
            20.0 / 25e6,
            1500,
            1830,
            1830,
            50,
            25_000_000,
            0,
            false,
            &Mono,
        );
        assert!(r.ticks.first().unwrap().x <= 0);
        assert!(r.ticks.last().unwrap().x >= 1830);
        let majors: Vec<&Tick> = r
            .ticks
            .iter()
            .filter(|t| t.kind == TickKind::Major)
            .collect();
        assert!(majors.len() >= 2);
        let gaps: Vec<i32> = majors.windows(2).map(|w| w[1].x - w[0].x).collect();
        // equal spacing within a pixel of rounding, wide enough for a label
        assert!(gaps.iter().all(|g| (g - gaps[0]).abs() <= 1), "{gaps:?}");
        assert!(gaps[0] as f64 >= 7.0 * 7.0, "{gaps:?}");
        // the prefix follows the whole view's span: 1830 px x 0.8 us = 1.46 ms
        assert_eq!(r.prefix, 4);
        assert!(majors[0].label.as_ref().unwrap().0.ends_with("ms"));
    }

    #[test]
    fn cursor_labels() {
        // "+1.2ms"-sized text in a 50 px ruler (Qt's)
        let r = cursor_label_rect(100, 41.0, 14.0, 50.0);
        assert_eq!(
            r,
            Bx {
                x: 76.0,
                y: 16.0,
                w: 49.0,
                h: 22.0
            }
        );
        let c = cursor_close_rect(r);
        assert_eq!(
            c,
            Bx {
                x: 114.0,
                y: 16.0,
                w: 10.0,
                h: 10.0
            }
        );
        assert_eq!(
            cursor_sel_rect(200.0, 0, 50.0),
            Bx {
                x: 190.0,
                y: 24.0,
                w: 20.0,
                h: 20.0
            }
        );
        assert_eq!(cursor_sel_rect(200.0, 2, 50.0).x, 230.0);
    }

    #[test]
    fn strips() {
        let h = 50.0;
        let mut s = RulerStrips::default();
        // left click opens the selector at x; its box 0 adds a cursor there
        assert_eq!(s.left_release(300.0, 10.0, 2, h), RulerAction::None);
        assert!(s.sel_visible);
        assert_eq!(s.left_release(300.0, 30.0, 2, h), RulerAction::Add(300.0));
        assert!(!s.sel_visible);
        // box 2 moves cursor 2 to the selector's x
        s.left_release(300.0, 10.0, 2, h);
        assert_eq!(
            s.left_release(345.0, 30.0, 2, h),
            RulerAction::Move(2, 300.0)
        );
        // a click beside the boxes closes it
        s.left_release(300.0, 10.0, 2, h);
        assert_eq!(s.left_release(10.0, 30.0, 2, h), RulerAction::None);
        assert!(!s.visible());
        // right click deletes in the selector, stays open while cursors remain
        s.left_release(300.0, 10.0, 2, h);
        assert_eq!(s.right_release(325.0, 30.0, 2, h), RulerAction::Delete(1));
        assert!(s.sel_visible);
        assert_eq!(s.right_release(325.0, 30.0, 1, h), RulerAction::Delete(1));
        assert!(!s.sel_visible);
        // right click opens the go-to strip; a left click on box 1 goes there
        assert_eq!(s.right_release(500.0, 10.0, 1, h), RulerAction::None);
        assert!(s.go_visible);
        assert_eq!(s.right_release(400.0, 10.0, 1, h), RulerAction::None);
        assert_eq!(s.left_release(525.0, 30.0, 1, h), RulerAction::GoTo(1));
        assert!(!s.visible());
        s.right_release(500.0, 10.0, 1, h);
        s.leave();
        assert!(!s.visible());
    }
}
