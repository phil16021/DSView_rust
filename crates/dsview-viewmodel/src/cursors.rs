//! Time cursors (`View` cursor list, `Cursor`, the drag in `Viewport`):
//! numbered by position, coloured by number, dragged with snapping to a
//! nearby edge.
//!
//! A cursor keeps its number, and with it its colour, until it is deleted; a
//! new cursor takes the lowest free number. Qt numbered by list position, so
//! deleting a cursor renumbered and recoloured the ones after it (F27).
//! Methods taking a cursor `number`-like `usize` other than [`index_of`]
//! take its position from 1, as the views address them.
//!
//! [`index_of`]: Cursors::index_of

use crate::ruler::CURSOR_HUES;

/// `Viewport::SnapMinSpace`: snap to an edge within 10 px.
pub const SNAP_MIN_SPACE: f64 = 10.0;
/// Grab distance in pixels.
pub const GRAB_RADIUS: f64 = 10.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cursors {
    /// Sample positions, in the order the cursors were added.
    pub samples: Vec<u64>,
    /// The cursors' numbers, parallel to `samples`. When `samples` is set
    /// from outside to a different length, cursors number by position again.
    pub numbers: Vec<usize>,
}

impl Cursors {
    /// Re-numbers by position when `samples` was replaced from outside.
    fn sync(&mut self) {
        if self.numbers.len() != self.samples.len() {
            self.numbers = (1..=self.samples.len()).collect();
        }
    }

    /// The number of the cursor at position `i` (from 0).
    pub fn number(&self, i: usize) -> usize {
        if self.numbers.len() == self.samples.len() {
            self.numbers.get(i).copied().unwrap_or(i + 1)
        } else {
            i + 1
        }
    }

    /// Every cursor's number, in list order.
    pub fn numbers(&self) -> Vec<usize> {
        (0..self.samples.len()).map(|i| self.number(i)).collect()
    }

    /// The position (from 0) of the cursor numbered `number`.
    pub fn index_of(&self, number: usize) -> Option<usize> {
        (0..self.samples.len()).find(|&i| self.number(i) == number)
    }

    /// `add_cursor`: a new cursor at the end of the list; its number.
    pub fn add(&mut self, sample: u64) -> usize {
        self.sync();
        let n = (1..).find(|n| !self.numbers.contains(n)).unwrap_or(1);
        self.samples.push(sample);
        self.numbers.push(n);
        n
    }

    /// `del_cursor`: the cursor at position `pos` (from 1); the others keep
    /// their numbers.
    pub fn remove(&mut self, pos: usize) {
        self.sync();
        if (1..=self.samples.len()).contains(&pos) {
            self.samples.remove(pos - 1);
            self.numbers.remove(pos - 1);
        }
    }

    pub fn clear(&mut self) {
        self.samples.clear();
        self.numbers.clear();
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

    /// F27: a cursor keeps its number and colour when another is deleted; a
    /// new cursor takes the lowest free number.
    #[test]
    fn numbers_are_stable() {
        let mut c = Cursors::default();
        assert_eq!(c.add(100), 1);
        assert_eq!(c.add(50), 2);
        assert_eq!(c.add(300), 3);
        c.remove(1);
        assert_eq!(c.samples, [50, 300]);
        assert_eq!((c.number(0), c.number(1)), (2, 3));
        assert_eq!(c.index_of(3), Some(1));
        assert_eq!(c.index_of(1), None);
        assert_eq!(c.add(7), 1);
        assert_eq!(c.numbers(), [2, 3, 1]);
        // positions set from outside number by position again
        c.samples = vec![1, 2];
        assert_eq!(c.numbers(), [1, 2]);
        assert_eq!(c.add(3), 3);
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
