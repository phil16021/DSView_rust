//! The horizontal view (`pv/view/view.cpp`): seconds per pixel (`scale`)
//! and the view's left edge as whole pixels from the capture start
//! (`x_offset`), their limits, zooming around a point, `show_region`, and
//! the sample ↔ pixel mapping.
//!
//! As in Qt the offset is an integer pixel count, so zooming loses the
//! sub-pixel position (kept for parity; see DEVIATIONS.md F27).

/// `View::MaxViewRate`: the whole capture fits the width at most zoom-out.
pub const MAX_VIEW_RATE: f64 = 1.0;
/// `View::MaxPixelsPerSample`
pub const MAX_PIXELS_PER_SAMPLE: f64 = 100.0;
/// `View::WellSamplesPerPixel`: a virtual device's first zoom.
pub const WELL_SAMPLES_PER_PIXEL: f64 = 2048.0;
/// Zoom step factor per wheel step (`std::pow(3.0/2.0, -steps)`).
pub const ZOOM_STEP: f64 = 1.5;

/// What the view needs from the session (`SigSession`'s `cur_*` values).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timebase {
    /// `cur_snap_samplerate()`: the rate of the data shown.
    pub snap_samplerate: f64,
    /// `cur_samplerate()`: the device's rate now.
    pub samplerate: f64,
    /// `cur_samplelimits()`
    pub sample_limit: u64,
    /// DSO: `cur_view_time()` (timebase × divisions, seconds).
    pub dso_view_time: Option<f64>,
}

impl Timebase {
    /// `cur_sampletime()`
    pub fn sampletime(&self) -> f64 {
        self.sample_limit as f64 / self.samplerate
    }
    /// `cur_snap_sampletime()`
    pub fn snap_sampletime(&self) -> f64 {
        self.sample_limit as f64 / self.snap_samplerate
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Viewport {
    /// Seconds per pixel.
    pub scale: f64,
    /// Pixels from the capture start to the view's left edge.
    pub x_offset: i64,
    /// The previous scale and offset (`_preScale`, `_preOffset`).
    pub pre_scale: f64,
    pub pre_offset: i64,
    pub min_scale: f64,
    pub max_scale: f64,
    /// The trace area's width in pixels.
    pub width: i32,
    /// DSO: the trigger's sub-sample offset in samples (`trig_hoff`).
    pub trig_hoff: f64,
}

impl Default for Viewport {
    fn default() -> Self {
        Viewport {
            scale: 1e-8,
            x_offset: 0,
            pre_scale: 1e-8,
            pre_offset: 0,
            min_scale: 1e-15,
            max_scale: 1e9,
            width: 1,
            trig_hoff: 0.0,
        }
    }
}

impl Viewport {
    fn clamp_scale(&self, s: f64) -> f64 {
        s.min(self.max_scale).max(self.min_scale)
    }

    /// `get_min_offset()`
    pub fn min_offset(&self) -> i64 {
        if MAX_VIEW_RATE > 1.0 {
            (self.width as f64 * (1.0 - MAX_VIEW_RATE)).floor() as i64
        } else {
            0
        }
    }

    /// `get_max_offset()`
    pub fn max_offset(&self, tb: &Timebase) -> i64 {
        (tb.snap_sampletime() / self.scale - self.width as f64 * MAX_VIEW_RATE).ceil() as i64
    }

    fn clamp_offset(&self, off: i64, tb: &Timebase) -> i64 {
        off.min(self.max_offset(tb)).max(self.min_offset())
    }

    /// `update_scale_offset()`: the limits for the current data and width.
    pub fn update_limits(&mut self, tb: &Timebase) {
        if self.width == 0 {
            return;
        }
        match tb.dso_view_time {
            None => {
                self.max_scale = tb.sampletime() / (self.width as f64 * MAX_VIEW_RATE);
                self.min_scale = (1.0 / tb.snap_samplerate) / MAX_PIXELS_PER_SAMPLE;
            }
            Some(t) => {
                self.scale = t / self.width as f64;
                self.max_scale = 1e9;
                self.min_scale = 1e-15;
            }
        }
        self.scale = self.clamp_scale(self.scale);
        self.x_offset = self.clamp_offset(self.x_offset, tb);
        self.pre_scale = self.scale;
        self.pre_offset = self.x_offset;
    }

    /// `mode_changed()` for a virtual device: the first zoom shows 2048
    /// samples per pixel.
    pub fn reset_for_virtual_device(&mut self, tb: &Timebase) {
        self.scale = self.clamp_scale(WELL_SAMPLES_PER_PIXEL / tb.snap_samplerate);
    }

    /// `set_scale_offset()`: true if anything changed.
    pub fn set_scale_offset(&mut self, scale: f64, offset: i64, tb: &Timebase) -> bool {
        self.pre_scale = self.scale;
        self.pre_offset = self.x_offset;
        self.scale = self.clamp_scale(scale);
        self.x_offset = self.clamp_offset(offset, tb);
        self.scale != self.pre_scale || self.x_offset != self.pre_offset
    }

    /// `set_preScale_preOffset()`: back to the previous view.
    pub fn restore_previous(&mut self, tb: &Timebase) -> bool {
        let (s, o) = (self.pre_scale, self.pre_offset);
        self.set_scale_offset(s, o, tb)
    }

    /// `zoom(steps, offset)` outside DSO mode: `steps` wheel steps (positive
    /// zooms in) around pixel `px`. True if anything changed. (DSO zooms
    /// by stepping the timebase knob instead.)
    pub fn zoom(&mut self, steps: f64, px: i32, tb: &Timebase) -> bool {
        if self.width == 0 {
            return false;
        }
        self.pre_scale = self.scale;
        self.pre_offset = self.x_offset;
        self.scale = self.clamp_scale(self.scale * ZOOM_STEP.powf(-steps));
        self.zoom_offset(px, tb)
    }

    /// The offset after a scale change around pixel `px` (also after a DSO
    /// timebase step, which sets `scale` itself).
    pub fn zoom_offset(&mut self, px: i32, tb: &Timebase) -> bool {
        let off = ((self.x_offset + px as i64) as f64 * (self.pre_scale / self.scale) - px as f64)
            .floor() as i64;
        self.x_offset = self.clamp_offset(off, tb);
        self.scale != self.pre_scale || self.x_offset != self.pre_offset
    }

    /// `show_region(start, end, false)`: the region fills half the width,
    /// centred; with `map_zoom` the scale stays and the region is centred.
    pub fn show_region(&mut self, start: u64, end: u64, map_zoom: bool, tb: &Timebase) -> bool {
        if self.width == 0 {
            return false;
        }
        let w = self.width as f64;
        let rate = tb.snap_samplerate;
        let scale = if map_zoom {
            self.scale
        } else {
            self.clamp_scale((end - start) as f64 * 2.0 / rate / w)
        };
        let off = (start + end) as f64 * 0.5 / (rate * scale) - w / 2.0;
        // C++ passes the double to an int64_t parameter: truncation
        self.set_scale_offset(scale, off as i64, tb)
    }

    /// `scroll_to_logic_last_data_time()`: the newest samples at the right
    /// edge (auto-scroll while streaming).
    pub fn scroll_to_last(&mut self, ring_count: u64, tb: &Timebase) -> bool {
        let off = (ring_count as f64 / tb.snap_samplerate / self.scale
            - self.width as f64 * MAX_VIEW_RATE)
            .ceil() as i64;
        self.set_scale_offset(self.scale, off + 10, tb)
    }

    /// Samples per pixel.
    pub fn samples_per_pixel(&self, tb: &Timebase) -> f64 {
        tb.snap_samplerate * self.scale
    }

    /// `index2pixel(index, has_hoff)`
    pub fn index_to_pixel(&self, index: u64, with_hoff: bool, tb: &Timebase) -> f64 {
        let spp = self.samples_per_pixel(tb);
        if spp == 0.0 {
            return 0.0;
        }
        let px = index as f64 / spp - self.x_offset as f64;
        if with_hoff {
            px + self.trig_hoff / spp
        } else {
            px
        }
    }

    /// `pixel2index(pixel)`
    pub fn pixel_to_index(&self, pixel: f64, tb: &Timebase) -> u64 {
        let spp = self.samples_per_pixel(tb);
        ((pixel + self.x_offset as f64) * spp - self.trig_hoff).round() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Timebase {
        // protocol.demo: 131072 samples at 25 MHz
        Timebase {
            snap_samplerate: 25e6,
            samplerate: 25e6,
            sample_limit: 131_072,
            dso_view_time: None,
        }
    }

    fn view(width: i32, tb: &Timebase) -> Viewport {
        let mut v = Viewport {
            width,
            ..Viewport::default()
        };
        v.update_limits(tb);
        v
    }

    #[test]
    fn limits_follow_capture_and_width() {
        let tb = demo();
        let v = view(1000, &tb);
        assert_eq!(v.max_scale, 131_072.0 / 25e6 / 1000.0);
        assert_eq!(v.min_scale, 1.0 / 25e6 / 100.0);
        // the default 10 ns/px lies inside [400 ps, 5.24 us]
        assert_eq!(v.scale, 1e-8);
    }

    #[test]
    fn zoom_keeps_the_point_under_the_mouse() {
        let tb = demo();
        let mut v = view(1000, &tb);
        v.set_scale_offset(4e-8, 1000, &tb);
        let before = v.pixel_to_index(300.0, &tb);
        assert!(v.zoom(2.0, 300, &tb));
        let after = v.pixel_to_index(300.0, &tb);
        // integer offsets lose up to one pixel
        let spp = v.samples_per_pixel(&tb);
        assert!(
            (before as f64 - after as f64).abs() <= spp.max(1.0),
            "{before} {after}"
        );
        assert!((v.scale - 4e-8 / 2.25).abs() < 1e-20);
        // the previous view comes back
        assert!(v.restore_previous(&tb));
        assert_eq!((v.scale, v.x_offset), (4e-8, 1000));
    }

    #[test]
    fn show_region_fills_half_the_width() {
        let tb = demo();
        let mut v = view(1000, &tb);
        v.show_region(40_000, 60_000, false, &tb);
        let (a, b) = (
            v.index_to_pixel(40_000, false, &tb),
            v.index_to_pixel(60_000, false, &tb),
        );
        assert!((b - a - 500.0).abs() < 1e-6, "{a} {b}");
        assert!(((a + b) / 2.0 - 500.0).abs() <= 1.0, "{a} {b}");
    }

    #[test]
    fn offsets_stay_inside_the_capture() {
        let tb = demo();
        let mut v = view(1000, &tb);
        v.set_scale_offset(v.max_scale, 5000, &tb);
        assert_eq!(v.x_offset, 0);
        v.set_scale_offset(v.min_scale, i64::MAX / 2, &tb);
        assert_eq!(v.x_offset, v.max_offset(&tb));
        assert_eq!(v.index_to_pixel(131_072, false, &tb).round() as i64, 1000);
    }

    #[test]
    fn index_pixel_round_trip() {
        let tb = demo();
        let mut v = view(1280, &tb);
        v.set_scale_offset(2e-9, 333, &tb);
        for i in [0u64, 1, 999, 50_000, 131_071] {
            let px = v.index_to_pixel(i, false, &tb);
            assert_eq!(v.pixel_to_index(px, &tb), i);
        }
    }
}
