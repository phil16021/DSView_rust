//! Input of the time view (`Viewport` event handlers): what the wheel does,
//! the kinetic scroll after a swipe, and rubber-band zoom.

use crate::viewport::{Timebase, Viewport};

/// `Viewport::DragTimerIntervalMs`: kinetic scroll tick.
pub const DRAG_TIMER_INTERVAL_MS: u64 = 8;
/// `Viewport::DragDamping`, `MinorDragRateUp`, `MinorDragOffsetUp`.
pub const DRAG_DAMPING: f64 = 1.05;
pub const MINOR_DRAG_RATE_UP: i32 = 15;
pub const MINOR_DRAG_OFFSET_UP: i32 = 100;
/// `Viewport::DragVelocityMeasureIntervalMs`
pub const DRAG_VELOCITY_MEASURE_INTERVAL_MS: u64 = 10;

/// What a wheel event does in the time view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WheelAction {
    /// Zoom by `steps` around pixel `x`.
    Zoom {
        steps: f64,
        x: i32,
    },
    /// Scroll the trace list by `dy` (pixels, Qt's `delta`).
    ScrollVertical {
        dy: i32,
    },
    /// Change the trace height factor (Ctrl+Shift+wheel).
    VerticalZoom {
        steps: f64,
    },
    /// Pan by `dx` pixels.
    Pan {
        dx: i32,
    },
    None,
}

/// Modifier keys of an input event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
}

/// `Viewport::wheelEvent` (time view): `angle_x`/`angle_y` in Qt's eighths
/// of a degree (120 per notch); `inverted` as the platform reports it.
pub fn wheel(
    angle_x: i32,
    angle_y: i32,
    x: i32,
    mods: Mods,
    inverted: bool,
    vertical_scroll_is_zoom: bool,
) -> WheelAction {
    let vertical = angle_x == 0 || angle_y.abs() >= angle_x.abs();
    let delta = if vertical { angle_y } else { angle_x };
    let mut steps = delta as f64 / 120.0;
    if inverted {
        steps = -steps;
    }
    if vertical {
        let do_vscroll = if vertical_scroll_is_zoom {
            mods.ctrl
        } else {
            !mods.ctrl
        };
        if mods.ctrl && mods.shift {
            WheelAction::VerticalZoom { steps }
        } else if do_vscroll {
            WheelAction::ScrollVertical { dy: -delta }
        } else {
            WheelAction::Zoom { steps, x }
        }
    } else if !mods.shift {
        WheelAction::Pan { dx: -delta }
    } else {
        WheelAction::None
    }
}

/// The swipe that keeps scrolling after the button is released
/// (`_drag_strength`, `updateDragVelocity`, `on_drag_timer`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Kinetic {
    /// Pixels per tick; an integer as in Qt (so it stops abruptly, F27).
    pub strength: i32,
    delta_t: u64,
    delta_x: i32,
}

impl Kinetic {
    /// A drag step: `dx` pixels moved (old minus new x) after `dt_ms`.
    pub fn track(&mut self, dx: i32, dt_ms: u64) {
        self.delta_t += dt_ms;
        self.delta_x += dx;
        if self.delta_t > DRAG_VELOCITY_MEASURE_INTERVAL_MS {
            let velocity = self.delta_x as f64 / self.delta_t as f64;
            self.strength = (velocity * 12.0) as i32;
            self.delta_t = 0;
            self.delta_x = 0;
        }
    }

    /// The button was released: true if the view keeps scrolling.
    pub fn release(&mut self) -> bool {
        if self.strength.abs() > MINOR_DRAG_RATE_UP {
            if self.strength.abs() >= MINOR_DRAG_OFFSET_UP {
                self.strength *= 5;
            }
            true
        } else {
            self.strength = 0;
            false
        }
    }

    /// One timer tick: moves the view; false when it stopped.
    pub fn tick(&mut self, vp: &mut Viewport, tb: &Timebase) -> bool {
        let off = vp.x_offset;
        if self.strength != 0 && off < vp.max_offset(tb) && off > vp.min_offset() {
            vp.set_scale_offset(vp.scale, off + self.strength as i64, tb);
            self.strength = (self.strength as f64 / DRAG_DAMPING) as i32;
            self.strength != 0
        } else {
            self.strength = 0;
            false
        }
    }
}

/// Right-drag zoom (`LOGIC_ZOOM` release): the dragged span from `x0` to
/// `x1` fills the width.
pub fn rubber_band_zoom(vp: &mut Viewport, x0: i32, x1: i32, tb: &Timebase) -> bool {
    if x0 == x1 || vp.width <= 0 {
        return false;
    }
    let new_offset = vp.x_offset + x0.min(x1) as i64;
    let new_scale = (vp.scale * (x1 - x0).abs() as f64 / vp.width as f64)
        .min(vp.max_scale)
        .max(vp.min_scale);
    if new_scale == vp.scale {
        return false;
    }
    let off = (new_offset as f64 * (vp.scale / new_scale)).floor() as i64;
    vp.set_scale_offset(new_scale, off, tb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wheel_mapping() {
        let none = Mods::default();
        let ctrl = Mods {
            ctrl: true,
            shift: false,
        };
        let both = Mods {
            ctrl: true,
            shift: true,
        };
        assert_eq!(
            wheel(0, 120, 50, none, false, true),
            WheelAction::Zoom { steps: 1.0, x: 50 }
        );
        assert_eq!(
            wheel(0, 120, 50, ctrl, false, true),
            WheelAction::ScrollVertical { dy: -120 }
        );
        assert_eq!(
            wheel(0, 120, 50, ctrl, false, false),
            WheelAction::Zoom { steps: 1.0, x: 50 }
        );
        assert_eq!(
            wheel(0, -240, 0, both, false, true),
            WheelAction::VerticalZoom { steps: -2.0 }
        );
        assert_eq!(
            wheel(-60, 10, 0, none, false, true),
            WheelAction::Pan { dx: 60 }
        );
        assert_eq!(
            wheel(0, 120, 7, none, true, true),
            WheelAction::Zoom { steps: -1.0, x: 7 }
        );
    }

    #[test]
    fn swipe_decays_to_a_stop() {
        let tb = Timebase {
            snap_samplerate: 1e6,
            samplerate: 1e6,
            sample_limit: 10_000_000,
            dso_view_time: None,
        };
        let mut vp = Viewport {
            width: 1000,
            ..Viewport::default()
        };
        vp.update_limits(&tb);
        vp.set_scale_offset(1e-6, 5000, &tb);
        let mut k = Kinetic::default();
        k.track(30, 12); // 2.5 px/ms -> strength 30
        assert_eq!(k.strength, 30);
        assert!(k.release());
        let mut ticks = 0;
        while k.tick(&mut vp, &tb) {
            ticks += 1;
        }
        assert!(
            ticks > 10 && vp.x_offset > 5000 + 300,
            "{ticks} {}",
            vp.x_offset
        );
        assert!(!Kinetic::default().release());
    }

    #[test]
    fn rubber_band_fills_the_width() {
        let tb = Timebase {
            snap_samplerate: 1e6,
            samplerate: 1e6,
            sample_limit: 100_000,
            dso_view_time: None,
        };
        let mut vp = Viewport {
            width: 1000,
            ..Viewport::default()
        };
        vp.update_limits(&tb);
        vp.set_scale_offset(1e-5, 0, &tb);
        let first = vp.pixel_to_index(200.0, &tb);
        assert!(rubber_band_zoom(&mut vp, 400, 200, &tb));
        assert!((vp.scale - 2e-6).abs() < 1e-18);
        assert_eq!(vp.pixel_to_index(0.0, &tb), first);
    }
}

/// `Viewport::YScaleBadgeDurationMs`, `YScaleBadgeFadeMs`: the Y-scale
/// badge shows 4 s after a vertical zoom, fading over the last 0.8 s.
pub const YSCALE_BADGE_MS: u64 = 4000;
pub const YSCALE_BADGE_FADE_MS: u64 = 800;

/// `paintYScaleBadge`: the badge's opacity `elapsed_ms` after it was
/// flashed, None once it is gone.
pub fn yscale_badge_opacity(elapsed_ms: u64) -> Option<f64> {
    if elapsed_ms >= YSCALE_BADGE_MS {
        return None;
    }
    let fade_start = YSCALE_BADGE_MS - YSCALE_BADGE_FADE_MS;
    Some(if elapsed_ms > fade_start {
        (1.0 - (elapsed_ms - fade_start) as f64 / YSCALE_BADGE_FADE_MS as f64).max(0.0)
    } else {
        1.0
    })
}

/// The badge's text (`"Y-scale %1x"` with two decimals).
pub fn yscale_badge_text(factor: f64) -> String {
    format!("Y-scale {factor:.2}x")
}

#[cfg(test)]
mod badge_tests {
    use super::*;

    #[test]
    fn badge_fades() {
        assert_eq!(yscale_badge_opacity(0), Some(1.0));
        assert_eq!(yscale_badge_opacity(3200), Some(1.0));
        assert_eq!(yscale_badge_opacity(3600), Some(0.5));
        assert_eq!(yscale_badge_opacity(4000), None);
        assert_eq!(yscale_badge_text(1.5), "Y-scale 1.50x");
    }
}

/// `Viewport::gestureEvent`: pinch zoom. Each `ZoomNativeGesture` adds twice
/// its value (the scale change, factor − 1) to a total that is tried as zoom
/// steps; the total restarts once a zoom took effect or the gesture ends.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pinch {
    total: f64,
}

impl Pinch {
    /// One frame's zoom factors: the steps to try, if there were any.
    pub fn feed(&mut self, factors: &[f32]) -> Option<f64> {
        if factors.is_empty() {
            // no gesture this frame: it ended
            self.total = 0.0;
            return None;
        }
        for &f in factors {
            self.total += (f as f64 - 1.0) * 2.0;
        }
        Some(self.total)
    }

    /// The zoom took effect.
    pub fn applied(&mut self) {
        self.total = 0.0;
    }
}

#[cfg(test)]
mod pinch_tests {
    use super::*;

    #[test]
    fn pinch_accumulates() {
        let mut p = Pinch::default();
        assert_eq!(p.feed(&[]), None);
        let a = p.feed(&[1.05]).unwrap();
        assert!((a - 0.1).abs() < 1e-6);
        let b = p.feed(&[1.05]).unwrap();
        assert!((b - 0.2).abs() < 1e-6);
        p.applied();
        let c = p.feed(&[0.9]).unwrap();
        assert!((c + 0.2).abs() < 1e-6);
        assert_eq!(p.feed(&[]), None);
        assert!((p.feed(&[1.1]).unwrap() - 0.2).abs() < 1e-6);
    }
}

/// `MainWindow::eventFilter`'s keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    /// S, I: run/stop, instant.
    RunStop,
    Instant,
    /// T, D, M, R: the trigger, protocol, measure and search docks.
    Trigger,
    Protocol,
    Measure,
    Search,
    /// O: the device options.
    DeviceOptions,
    /// PgUp/PgDn: one view width to the left/right.
    PageLeft,
    PageRight,
    /// ←/→: zoom in/out around the middle.
    ZoomIn,
    ZoomOut,
    /// 0/1: makes DSO channel N's V/div dial the active one (or none).
    Dial(u16),
    /// ↑/↓: the active dial's next/previous V/div.
    DialUp,
    DialDown,
}

/// The shortcut of a key, by its name (egui's `Key::name()`).
pub fn shortcut(key: &str) -> Option<Shortcut> {
    Some(match key {
        "S" => Shortcut::RunStop,
        "I" => Shortcut::Instant,
        "T" => Shortcut::Trigger,
        "D" => Shortcut::Protocol,
        "M" => Shortcut::Measure,
        "R" => Shortcut::Search,
        "O" => Shortcut::DeviceOptions,
        "PageUp" => Shortcut::PageLeft,
        "PageDown" => Shortcut::PageRight,
        "Left" | "ArrowLeft" => Shortcut::ZoomIn,
        "Right" | "ArrowRight" => Shortcut::ZoomOut,
        "0" => Shortcut::Dial(0),
        "1" => Shortcut::Dial(1),
        "Up" | "ArrowUp" => Shortcut::DialUp,
        "Down" | "ArrowDown" => Shortcut::DialDown,
        _ => return None,
    })
}

/// The filter in front of the shortcuts: keys with Ctrl, Shift or Alt are
/// dropped, and so is a key less than 200 ms after the last one taken.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KeyGate {
    last_ms: Option<u64>,
}

impl KeyGate {
    pub const DEBOUNCE_MS: u64 = 200;

    pub fn accept(&mut self, now_ms: u64, with_modifier: bool) -> bool {
        if with_modifier {
            return false;
        }
        if self
            .last_ms
            .is_some_and(|l| now_ms.saturating_sub(l) < Self::DEBOUNCE_MS)
        {
            return false;
        }
        self.last_ms = Some(now_ms);
        true
    }
}

/// The docks a work mode offers (`TrigBar::reload`): logic all four,
/// analog the measure dock, DSO trigger and measure. Mode: 0 logic,
/// 1 DSO, 2 analog.
pub fn dock_offered(mode: i32, dock: Shortcut) -> bool {
    matches!(
        (mode, dock),
        (
            0,
            Shortcut::Trigger | Shortcut::Protocol | Shortcut::Measure | Shortcut::Search
        ) | (2, Shortcut::Measure)
            | (1, Shortcut::Trigger | Shortcut::Measure)
    )
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn keys_gate_and_docks() {
        assert_eq!(shortcut("S"), Some(Shortcut::RunStop));
        assert_eq!(shortcut("1"), Some(Shortcut::Dial(1)));
        assert_eq!(shortcut("ArrowDown"), Some(Shortcut::DialDown));
        assert_eq!(shortcut("Q"), None);
        let mut g = KeyGate::default();
        assert!(g.accept(1000, false));
        assert!(!g.accept(1100, false));
        // a dropped key does not restart the 200 ms
        assert!(g.accept(1200, false));
        assert!(!g.accept(1500, true));
        assert!(g.accept(1500, false));
        assert!(dock_offered(0, Shortcut::Search));
        assert!(!dock_offered(1, Shortcut::Protocol));
        assert!(dock_offered(1, Shortcut::Trigger));
        assert!(!dock_offered(2, Shortcut::Trigger));
        assert!(dock_offered(2, Shortcut::Measure));
    }
}
