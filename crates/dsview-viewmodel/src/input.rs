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
