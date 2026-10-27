//! The logic capture's progress screen (`Viewport::paintProgress`): a
//! circle with the DreamSourceLab logo, an arc for the samples received
//! and, until the first data arrives, three dots blinking in turn and an
//! arc for the hardware's capture progress.

/// `TriggerTimer`: the dots move on every 333 ms.
pub const TRIGGER_TIMER_MS: u64 = 333;

/// What the time view shows for the logic mode (`Viewport::paintEvent`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Signals,
    Progress,
}

/// `paintEvent`'s choice: stopped or repeating captures show the signals;
/// a capture followed in real time shows them once there is data; any
/// other capture shows the progress screen.
pub fn screen(working: bool, repeat: bool, realtime: bool, have_data: bool) -> Screen {
    if !working || repeat || (realtime && have_data) {
        Screen::Signals
    } else {
        Screen::Progress
    }
}

/// `get_captured_progress`: the samples received in percent, rounded up.
pub fn progress100(received: u64, limit: u64) -> i32 {
    let p = received.min(limit) as f64 / limit.max(1) as f64 * 360.0 * 16.0;
    (p / 3.6 / 16.0).ceil() as i32
}

/// The screen's geometry for a view of `w` × `h` pixels (`cenPos`,
/// `radius`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geom {
    pub cx: f32,
    pub cy: f32,
    pub radius: f32,
}

impl Geom {
    pub fn new(w: f32, h: f32) -> Geom {
        Geom {
            cx: (w / 2.0).trunc(),
            cy: (h / 2.0).trunc(),
            radius: (0.3 * w).min(0.3 * h).trunc(),
        }
    }

    /// `logoPoints`: ten vertical strokes, as (x, y0, y1); each ends in a
    /// circle of radius 5 below (the left five) or above (the right five).
    pub fn logo(&self) -> [(f32, f32, f32); 10] {
        const STEPS: [(f32, f32); 10] = [
            (-0.75, 0.15),
            (-0.6, 0.3),
            (-0.45, 0.45),
            (-0.3, 0.3),
            (-0.15, 0.15),
            (0.15, -0.15),
            (0.3, -0.3),
            (0.45, -0.45),
            (0.6, -0.3),
            (0.75, -0.15),
        ];
        STEPS.map(|(dx, dy)| {
            // QPoint rounds each coordinate
            let x = (self.cx + dx * self.radius).round();
            (x, self.cy, (self.cy + dy * self.radius).round())
        })
    }

    /// The circles at the strokes' ends: centre and radius.
    pub fn logo_dots(&self) -> [(f32, f32, f32); 10] {
        const R: f32 = 10.0 / 2.0;
        self.logo().map(|(x, _, y1)| {
            // drawEllipse(x - r, y, 2r, 2r) below, (x - r, y - 2r, ...) above
            let cy = if y1 >= self.cy { y1 + R } else { y1 - R };
            (x, cy, R)
        })
    }

    /// The three trigger-wait dots (`cenLeftPos`, `cenPos`, `cenRightPos`)
    /// and their radius.
    pub fn wait_dots(&self, w: f32, h: f32) -> ([(f32, f32); 3], f32) {
        let r = (0.02 * w).min(0.02 * h).trunc();
        let (l, c, rt) = (
            (w / 2.0 - 0.05 * w).trunc(),
            (w / 2.0).trunc(),
            (w / 2.0 + 0.05 * w).trunc(),
        );
        let y = (h / 2.0).trunc();
        ([(l, y), (c, y), (rt, y)], r)
    }

    /// Points on an arc of radius `r` starting at nine o'clock and running
    /// clockwise over `percent` of the circle (`drawArc(180 * 16, -span)`).
    pub fn arc(&self, r: f32, percent: f64) -> Vec<(f32, f32)> {
        let frac = (percent / 100.0).clamp(0.0, 1.0);
        if frac <= 0.0 {
            return Vec::new();
        }
        let n = ((frac * 128.0).ceil() as usize).max(2);
        (0..=n)
            .map(|i| {
                let a = std::f64::consts::PI * (1.0 + 2.0 * frac * i as f64 / n as f64);
                (self.cx + r * a.cos() as f32, self.cy + r * a.sin() as f32)
            })
            .collect()
    }
}

/// `_timer_cnt % 3`: the dot drawn in the foreground colour.
pub fn lit_dot(elapsed_ms: u64) -> usize {
    (elapsed_ms / TRIGGER_TIMER_MS % 3) as usize
}

/// The status line under the logo while waiting for the transfer.
pub fn status_text(triggered: bool, captured: i32) -> String {
    if triggered {
        format!("Triggered! {captured}% Captured")
    } else {
        format!("Waiting for Trigger! {captured}% Captured")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice() {
        assert_eq!(screen(false, false, false, false), Screen::Signals);
        assert_eq!(screen(true, true, false, false), Screen::Signals);
        assert_eq!(screen(true, false, true, true), Screen::Signals);
        assert_eq!(screen(true, false, true, false), Screen::Progress);
        assert_eq!(screen(true, false, false, true), Screen::Progress);
    }

    #[test]
    fn percent() {
        assert_eq!(progress100(0, 1000), 0);
        assert_eq!(progress100(1, 1000), 1);
        assert_eq!(progress100(500, 1000), 50);
        assert_eq!(progress100(2000, 1000), 100);
        assert_eq!(progress100(5, 0), 0);
    }

    #[test]
    fn geometry() {
        let g = Geom::new(1000.0, 600.0);
        assert_eq!((g.cx, g.cy, g.radius), (500.0, 300.0, 180.0));
        let logo = g.logo();
        assert_eq!(logo[0], (365.0, 300.0, 327.0));
        assert_eq!(logo[9], (635.0, 300.0, 273.0));
        let dots = g.logo_dots();
        assert_eq!(dots[0], (365.0, 332.0, 5.0));
        assert_eq!(dots[9], (635.0, 268.0, 5.0));
        // a quarter: from nine to twelve o'clock
        let a = g.arc(100.0, 25.0);
        let (s, e) = (a[0], *a.last().unwrap());
        assert!((s.0 - 400.0).abs() < 1e-3 && (s.1 - 300.0).abs() < 1e-3);
        assert!((e.0 - 500.0).abs() < 1e-3 && (e.1 - 200.0).abs() < 1e-3);
        assert!(g.arc(100.0, 0.0).is_empty());
        assert_eq!(lit_dot(0), 0);
        assert_eq!(lit_dot(340), 1);
        assert_eq!(lit_dot(1000), 0);
    }
}
