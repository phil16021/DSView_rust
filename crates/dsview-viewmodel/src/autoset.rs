//! The DSO auto-set (`DsoSignal::auto_start`, `auto_set`, `autoV_end`,
//! `autoH_end`): while a capture runs it steps the timebase until 3 to 6
//! periods show and the V/div until the signal fills the grid without
//! clipping, then puts the trigger level in the middle. Each step waits for
//! `AUTO_LOCK` new frames; after `AUTO_TIME` ms it gives up.
//!
//! The state machine only decides; the caller carries out the actions.

/// `DsoSignal::AutoTime` (ms), `AutoLock` (frames)
pub const AUTO_TIME_MS: i64 = 10_000;
pub const AUTO_LOCK: u32 = 3;

/// What one frame of auto-set tells the caller to do, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `set_zero_ratio(0.5)`
    ZeroToMiddle,
    /// `_view->zoom(-1)`: one timebase step longer.
    ZoomOut,
    /// `_view->zoom(1)`: one timebase step shorter.
    ZoomIn,
    /// `go_vDialNext(false)`: one V/div step up (larger).
    VdivUp,
    /// `go_vDialPre(false)`: one V/div step down.
    VdivDown,
    /// `go_attenStep(dir)` (MSO-E8): to probe factor 1/10/100.
    Factor(u64),
    /// The trigger level to this code (`set_trig_vpos` of
    /// `(_min + _max) / 2`).
    TrigValue(u8),
    /// `auto_trig(index)`: trigger source auto on this channel.
    AutoTrig,
    /// `data_auto_lock(AutoLock)`: skip the next frames.
    Lock,
}

/// What auto-set reads each frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Input {
    pub running: bool,
    /// The frames to skip after a step (`get_data_auto_lock`).
    pub locked: bool,
    pub zero_ratio: f64,
    /// `_mValid`, `_level_valid`, `_pcount`, `_period` (ns), `_min`, `_max`
    pub valid: bool,
    pub level_valid: bool,
    pub pcount: u64,
    pub period: f64,
    pub min: u8,
    pub max: u8,
    /// `SR_CONF_ROLL`
    pub roll: bool,
    /// `get_hori_res()`: the timebase (ns per division).
    pub hori_res: f64,
    /// MSO-E8: steps the probe factor instead of the V/div.
    pub atten: bool,
    pub hw_offset: i32,
    pub factor: u64,
    /// The V/div dial at its first or last entry.
    pub vdiv_at_min: bool,
    pub vdiv_at_max: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AutoSet {
    pub v: bool,
    pub h: bool,
    v_over: bool,
    cnt: u32,
    /// The timeout (`_end_timer`), ms.
    deadline: Option<i64>,
}

impl AutoSet {
    pub fn active(&self) -> bool {
        self.v || self.h
    }

    /// `auto_start`: only while running and not already busy.
    pub fn start(&mut self, running: bool, now_ms: i64) -> Vec<Action> {
        if self.active() || !running {
            return Vec::new();
        }
        self.v = true;
        self.h = true;
        self.deadline = Some(now_ms + AUTO_TIME_MS);
        vec![Action::Lock, Action::AutoTrig]
    }

    /// `autoV_end`
    fn v_end(&mut self, i: &Input, out: &mut Vec<Action>) {
        self.v = false;
        self.v_over = false;
        out.push(Action::AutoTrig);
        out.push(Action::TrigValue(((i.min as u16 + i.max as u16) / 2) as u8));
    }

    /// `auto_end` (the timeout)
    fn end(&mut self, i: &Input, out: &mut Vec<Action>) {
        if self.v {
            self.v_end(i, out);
        }
        self.h = false;
        self.deadline = None;
    }

    /// `go_attenStep(dir)`: the next factor, if any.
    fn atten_step(i: &Input, dir: i32) -> Option<u64> {
        const F: [u64; 3] = [1, 10, 100];
        let mut k = 0usize;
        while k < F.len() - 1 && F[k] < i.factor {
            k += 1;
        }
        let next = k as i32 + if dir > 0 { 1 } else { -1 };
        (0..F.len() as i32)
            .contains(&next)
            .then(|| F[next as usize])
    }

    /// One V/div step (`go_vDialNext` / `go_vDialPre` / `go_attenStep`): at
    /// the end of the range auto-V ends unless it is backing off a clip.
    fn v_step(&mut self, i: &Input, up: bool, out: &mut Vec<Action>) {
        let ok = if i.atten {
            match Self::atten_step(i, if up { 1 } else { -1 }) {
                Some(f) => {
                    out.push(Action::Factor(f));
                    true
                }
                None => false,
            }
        } else if up && !i.vdiv_at_max {
            out.push(Action::VdivUp);
            true
        } else if !up && !i.vdiv_at_min {
            out.push(Action::VdivDown);
            true
        } else {
            false
        };
        if !ok && self.v && !self.v_over {
            self.v_end(i, out);
        }
    }

    /// `auto_set` for one frame (`paint_fore`).
    pub fn step(&mut self, i: &Input, now_ms: i64) -> Vec<Action> {
        let mut out = Vec::new();
        if !self.active() {
            return out;
        }
        if self.deadline.is_some_and(|d| now_ms >= d) {
            self.end(i, &mut out);
            return out;
        }
        if !i.running {
            if self.v {
                self.v_end(i, &mut out);
            }
            self.h = false;
            return out;
        }
        if self.h && self.v && i.zero_ratio != 0.5 {
            out.push(Action::ZeroToMiddle);
        }
        if !i.valid || i.locked {
            return out;
        }
        if self.h {
            if i.level_valid && ((!i.roll && i.pcount < 3) || i.period > 4.0 * i.hori_res) {
                out.push(Action::ZoomOut);
            } else if i.level_valid && i.pcount > 6 && i.period < 1.5 * i.hori_res {
                out.push(Action::ZoomIn);
            } else if i.level_valid {
                self.h = false;
            }
        }
        if self.v {
            let over = i.max == 0xff || i.min == 0;
            let outside = i.max >= 0xE0 || i.min <= 0x20;
            let mut under = i.max <= 0xA0 && i.min >= 0x60;
            if i.atten {
                // one attenuator step is ×10: only step down if that still fits
                let swing = (i.max as i32 - i.hw_offset).max(i.hw_offset - i.min as i32);
                under = swing * 10 < (0xE0 - i.hw_offset).min(i.hw_offset - 0x20);
            }
            if over {
                if !self.v_over {
                    self.cnt = 0;
                }
                self.v_over = true;
                self.v_step(i, true, &mut out);
            } else if outside {
                self.v_step(i, true, &mut out);
            } else if !self.v_over && under {
                self.v_step(i, false, &mut out);
            } else if !self.h {
                self.v_end(i, &mut out);
            }
            if self.v_over && under {
                self.cnt += 1;
                if self.cnt > 17 {
                    self.v_over = false;
                }
            } else {
                self.cnt = 0;
            }
            // when auto-V just ended, `v_end` has set it already; C set it
            // a second time from the same values (F28)
            if self.v && i.level_valid {
                out.push(Action::TrigValue(((i.min as u16 + i.max as u16) / 2) as u8));
            }
        }
        if self.active() {
            out.push(Action::Lock);
        } else {
            self.deadline = None;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> Input {
        Input {
            running: true,
            zero_ratio: 0.5,
            valid: true,
            level_valid: true,
            pcount: 4,
            period: 1000.0,
            min: 0x70,
            max: 0x90,
            hori_res: 500.0,
            hw_offset: 0x80,
            factor: 1,
            ..Input::default()
        }
    }

    #[test]
    fn starts_only_while_running() {
        let mut a = AutoSet::default();
        assert!(a.start(false, 0).is_empty());
        assert_eq!(a.start(true, 0), vec![Action::Lock, Action::AutoTrig]);
        assert!(a.start(true, 0).is_empty());
    }

    #[test]
    fn steps_until_it_fits() {
        let mut a = AutoSet::default();
        a.start(true, 0);
        // too few periods and a small signal: zoom out, V/div down
        let mut i = input();
        i.pcount = 2;
        assert_eq!(
            a.step(&i, 1),
            vec![
                Action::ZoomOut,
                Action::VdivDown,
                Action::TrigValue(0x80),
                Action::Lock
            ]
        );
        // locked frames do nothing but keep the zero centred
        i.locked = true;
        i.zero_ratio = 0.4;
        assert_eq!(a.step(&i, 2), vec![Action::ZeroToMiddle]);
        // clipping: V/div up
        let mut i = input();
        i.max = 0xff;
        assert_eq!(
            a.step(&i, 3),
            vec![Action::VdivUp, Action::TrigValue(0xb7), Action::Lock]
        );
        // in range with 4 periods: horizontal done, vertical done
        let mut i = input();
        i.min = 0x40;
        i.max = 0xc0;
        let acts = a.step(&i, 4);
        assert!(acts.contains(&Action::AutoTrig), "{acts:?}");
        // the level once (F28: C set it a second time in the same pass)
        let levels = acts
            .iter()
            .filter(|x| matches!(x, Action::TrigValue(_)))
            .count();
        assert_eq!(levels, 1, "{acts:?}");
        assert!(!a.active());
    }

    #[test]
    fn ends_on_timeout_and_stop() {
        let mut a = AutoSet::default();
        a.start(true, 0);
        let acts = a.step(&input(), AUTO_TIME_MS);
        assert_eq!(acts, vec![Action::AutoTrig, Action::TrigValue(0x80)]);
        assert!(!a.active());
        a.start(true, 0);
        let mut i = input();
        i.running = false;
        a.step(&i, 1);
        assert!(!a.active());
    }

    #[test]
    fn attenuator_steps_the_factor() {
        let mut a = AutoSet::default();
        a.start(true, 0);
        let mut i = input();
        i.atten = true;
        i.max = 0xff;
        assert_eq!(a.step(&i, 1)[0], Action::Factor(10));
        i.factor = 100;
        // at ×100 there is no step up left; the clip back-off keeps auto-V
        assert!(!a.step(&i, 2).contains(&Action::Factor(100)));
    }
}
