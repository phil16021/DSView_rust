//! Time cursors (`View` cursor list, `Cursor`, the drag in `Viewport`):
//! numbered by position, coloured by number, dragged with snapping to a
//! nearby edge.
//!
//! As in Qt, numbers (and with them colours) are positions in the list, so
//! deleting a cursor renumbers the ones after it (DEVIATIONS F27).

use crate::ruler::CURSOR_HUES;

/// `Viewport::SnapMinSpace`: snap to an edge within 10 px.
pub const SNAP_MIN_SPACE: f64 = 10.0;
/// Grab distance in pixels.
pub const GRAB_RADIUS: f64 = 10.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cursors {
    /// Sample positions; cursor `i` has number `i + 1`.
    pub samples: Vec<u64>,
}

impl Cursors {
    /// `add_cursor`: a new cursor at the end of the list; its number.
    pub fn add(&mut self, sample: u64) -> usize {
        self.samples.push(sample);
        self.samples.len()
    }

    /// `del_cursor`: later cursors move up a number.
    pub fn remove(&mut self, number: usize) {
        if (1..=self.samples.len()).contains(&number) {
            self.samples.remove(number - 1);
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// The cursor whose x (from `to_px`) is within the grab radius of `x`,
    /// nearest first.
    pub fn grab(&self, x: f64, to_px: impl Fn(u64) -> f64) -> Option<usize> {
        self.samples
            .iter()
            .enumerate()
            .map(|(i, &s)| (i + 1, (to_px(s) - x).abs()))
            .filter(|&(_, d)| d <= GRAB_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(n, _)| n)
    }

    /// Moves cursor `number` for a drag to `pos` (fractional sample under
    /// the pointer). `pulse` is the edge pair around the pointer on the
    /// logic trace under it, with their pixel distances: the cursor snaps to
    /// the nearer edge within [`SNAP_MIN_SPACE`], else rounds to a sample.
    pub fn drag(&mut self, number: usize, pos: f64, pulse: Option<((u64, f64), (u64, f64))>) {
        let Some(slot) = self.samples.get_mut(number.wrapping_sub(1)) else {
            return;
        };
        *slot =
            match pulse {
                Some(((i0, d0), (i1, d1))) if d0 < SNAP_MIN_SPACE || d1 < SNAP_MIN_SPACE => {
                    if d0 < d1 { i0 } else { i1 }
                }
                _ => {
                    if pos - pos.trunc() < 0.5 {
                        pos.floor() as u64
                    } else {
                        pos.ceil() as u64
                    }
                }
            };
    }
}

/// `Ruler::GetColorByCursorOrder`: HSV (hue, 200, 200), alpha 180.
pub fn cursor_hsva(number: usize) -> (u16, u8, u8, u8) {
    let hue = CURSOR_HUES[(number.max(1) - 1) % CURSOR_HUES.len()];
    (hue, 200, 200, 180)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_follow_positions() {
        let mut c = Cursors::default();
        assert_eq!(c.add(100), 1);
        assert_eq!(c.add(50), 2);
        assert_eq!(c.add(300), 3);
        c.remove(1);
        assert_eq!(c.samples, [50, 300]);
        assert_eq!(cursor_hsva(1).0, 120);
        assert_eq!(cursor_hsva(23).0, 120);
    }

    #[test]
    fn drag_snaps_to_near_edges() {
        let mut c = Cursors::default();
        c.add(0);
        c.drag(1, 104.4, Some(((100, 4.4), (200, 95.6))));
        assert_eq!(c.samples[0], 100);
        c.drag(1, 150.6, Some(((100, 50.6), (200, 49.4))));
        assert_eq!(c.samples[0], 151);
        assert_eq!(c.grab(149.0, |s| s as f64), Some(1));
        assert_eq!(c.grab(170.0, |s| s as f64), None);
    }
}
