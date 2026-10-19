//! The trigger dock's settings (`pv/dock/triggerdock.cpp`): simple or
//! advanced, the 16 stages (two value strings with inversion, and/or,
//! contiguous, counter) and the serial trigger (start, stop, clock, data
//! channel, value, bits), turned into `ds_trigger` as `commit_trigger`
//! does through the `ds_trigger_*` setters.

use dsview_core::{
    ADV_TRIGGER, S_TRIGGER_DATA_STAGE, SERIAL_TRIGGER, TRIGGER_PROBES, TRIGGER_STAGES, Trigger,
};
use serde_json::{Map, Value};

/// A value string as the dock's line edits hold it: 16 characters of
/// `X01RFC` separated by spaces, bit 15 first.
pub const ALL_X: &str = "X X X X X X X X X X X X X X X X";

/// One stage of the advanced trigger.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub value0: String,
    pub value1: String,
    /// Channels 31-16 (32-channel devices).
    pub value0_ext32: String,
    pub value1_ext32: String,
    /// 0 "==", 1 "!="
    pub inv0: u8,
    pub inv1: u8,
    /// 0 Or, 1 And (`_logic_comboBox` index)
    pub logic: u8,
    pub contiguous: bool,
    pub count: u32,
}

impl Default for Stage {
    fn default() -> Self {
        Stage {
            value0: ALL_X.into(),
            value1: ALL_X.into(),
            value0_ext32: ALL_X.into(),
            value1_ext32: ALL_X.into(),
            inv0: 0,
            inv1: 0,
            logic: 1,
            contiguous: false,
            count: 1,
        }
    }
}

/// The serial trigger's fields.
#[derive(Clone, Debug, PartialEq)]
pub struct Serial {
    pub start: String,
    pub stop: String,
    pub clock: String,
    pub start_ext32: String,
    pub stop_ext32: String,
    pub clock_ext32: String,
    /// The data channel (`_serial_data_comboBox` index).
    pub channel: usize,
    /// 16 characters of `X01`, spaced like the others.
    pub data: String,
    /// Bits − 1 (`_serial_bits_comboBox` index, 0..=15).
    pub bits: usize,
}

impl Default for Serial {
    fn default() -> Self {
        Serial {
            start: ALL_X.into(),
            stop: ALL_X.into(),
            clock: ALL_X.into(),
            start_ext32: ALL_X.into(),
            stop_ext32: ALL_X.into(),
            clock_ext32: ALL_X.into(),
            channel: 0,
            data: ALL_X.into(),
            bits: 0,
        }
    }
}

/// The dock's state.
#[derive(Clone, Debug, PartialEq)]
pub struct AdvTrigger {
    /// "Advanced Trigger" chosen (`_adv_radioButton`).
    pub advanced: bool,
    /// Stages in use − 1 (`stages_comboBox` index).
    pub stages: usize,
    /// 0 stage trigger, 1 serial trigger (`_adv_tabWidget`).
    pub tab: usize,
    pub stage: Vec<Stage>,
    pub serial: Serial,
}

impl Default for AdvTrigger {
    fn default() -> Self {
        AdvTrigger {
            advanced: false,
            stages: 0,
            tab: 0,
            stage: vec![Stage::default(); TRIGGER_STAGES],
            serial: Serial::default(),
        }
    }
}

/// `value_changed()`: an edited value string normalised: upper case,
/// missing or blank positions become X, spaced, 31 characters.
pub fn normalise(s: &str, allowed: &str) -> String {
    let chars: Vec<char> = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_uppercase())
        .map(|c| if allowed.contains(c) { c } else { 'X' })
        .take(TRIGGER_PROBES)
        .collect();
    let mut out = String::with_capacity(TRIGGER_PROBES * 2 - 1);
    for i in 0..TRIGGER_PROBES {
        if i > 0 {
            out.push(' ');
        }
        out.push(chars.get(i).copied().unwrap_or('X'));
    }
    out
}

/// `ds_trigger_stage_set_value`: every second character, last probe first.
fn set_value(t: &mut Trigger, stage: usize, probes: usize, t0: &str, t1: &str) {
    let (a, b) = (t0.as_bytes(), t1.as_bytes());
    for j in 0..probes {
        // C reads past a short string; the dock's are always long enough
        t.trigger0[stage][probes - j - 1] = a.get(j * 2).copied().unwrap_or(0);
        t.trigger1[stage][probes - j - 1] = b.get(j * 2).copied().unwrap_or(0);
    }
}

impl AdvTrigger {
    /// `commit_trigger()` after `ds_trigger_reset()`: the position, and
    /// for the advanced trigger everything else. False for the simple
    /// trigger (the caller adds the channels' simple settings).
    pub fn commit(&self, t: &mut Trigger, pos: u16, ch_num: usize) -> bool {
        t.trigger_pos = pos;
        if !self.advanced {
            t.trigger_mode = dsview_core::SIMPLE_TRIGGER;
            return false;
        }
        t.trigger_en = 1;
        t.trigger_mode = if self.tab == 0 {
            ADV_TRIGGER
        } else {
            SERIAL_TRIGGER
        };
        let n = self.stages + 1;
        t.trigger_stages = self.stages as u16;
        let ext = ch_num == 32;
        let join = |hi: &str, lo: &str| {
            if ext {
                format!("{hi} {lo}")
            } else {
                lo.to_owned()
            }
        };
        if self.tab == 0 {
            for (i, s) in self.stage.iter().take(n).enumerate() {
                set_value(
                    t,
                    i,
                    ch_num,
                    &join(&s.value0_ext32, &s.value0),
                    &join(&s.value1_ext32, &s.value1),
                );
            }
        } else {
            let sr = &self.serial;
            let s1 = &self.stage[1];
            set_value(
                t,
                0,
                ch_num,
                &join(&sr.start_ext32, &sr.start),
                &join(&sr.stop_ext32, &sr.stop),
            );
            set_value(
                t,
                1,
                ch_num,
                &join(&sr.clock_ext32, &sr.clock),
                &join(&s1.value1_ext32, &s1.value1),
            );
            // the data channel as a '0' in an all-X string
            let mut channel: Vec<u8> = ALL_X.as_bytes().to_vec();
            let mut channel_ext32 = channel.clone();
            let d = sr.channel;
            if ext && d >= 16 {
                if let Some(c) = (30usize)
                    .checked_sub(2 * (d - 16))
                    .and_then(|i| channel_ext32.get_mut(i))
                {
                    *c = b'0';
                }
            } else if let Some(c) = (30usize)
                .checked_sub(2 * d)
                .and_then(|i| channel.get_mut(i))
            {
                *c = b'0';
            }
            let (channel, channel_ext32) = (
                String::from_utf8(channel).unwrap_or_default(),
                String::from_utf8(channel_ext32).unwrap_or_default(),
            );
            set_value(t, 2, TRIGGER_PROBES, &channel, &channel_ext32);
            set_value(
                t,
                S_TRIGGER_DATA_STAGE,
                TRIGGER_PROBES,
                &sr.data,
                &self.stage[3].value1,
            );
        }
        for (i, s) in self.stage.iter().take(n).enumerate() {
            t.trigger_logic[i] = ((s.contiguous as u8) << 1) + s.logic;
            t.trigger0_inv[i] = s.inv0;
            t.trigger1_inv[i] = s.inv1;
        }
        if self.tab == 0 {
            for (i, s) in self.stage.iter().take(n).enumerate() {
                t.trigger0_count[i] = s.count;
                t.trigger1_count[i] = 0;
            }
        } else {
            t.trigger0_count[1] = 1;
            t.trigger1_count[1] = 0;
            t.trigger0_count[3] = self.serial.bits as u32;
            t.trigger1_count[3] = 0;
        }
        true
    }

    /// `get_session()` with the position `pos`.
    pub fn session_json(&self, pos: i32, ch_num: usize) -> Value {
        let mut t = Map::new();
        t.insert("advTriggerMode".into(), self.advanced.into());
        t.insert("triggerPos".into(), pos.into());
        t.insert("triggerStages".into(), self.stages.into());
        t.insert("triggerTab".into(), self.tab.into());
        for (i, s) in self.stage.iter().enumerate() {
            t.insert(format!("stageTriggerValue0{i}"), s.value0.clone().into());
            t.insert(format!("stageTriggerValue1{i}"), s.value1.clone().into());
            t.insert(format!("stageTriggerInv0{i}"), s.inv0.into());
            t.insert(format!("stageTriggerInv1{i}"), s.inv1.into());
            t.insert(format!("stageTriggerLogic{i}"), s.logic.into());
            t.insert(format!("stageTriggerCount{i}"), s.count.into());
            t.insert(format!("stageTriggerContiguous{i}"), s.contiguous.into());
            if ch_num == 32 {
                t.insert(
                    format!("stageTriggerExt32Value0{i}"),
                    s.value0_ext32.clone().into(),
                );
                t.insert(
                    format!("stageTriggerExt32Value1{i}"),
                    s.value1_ext32.clone().into(),
                );
            }
        }
        let sr = &self.serial;
        t.insert("serialTriggerStart".into(), sr.start.clone().into());
        t.insert("serialTriggerStop".into(), sr.stop.clone().into());
        t.insert("serialTriggerClock".into(), sr.clock.clone().into());
        t.insert("serialTriggerChannel".into(), sr.channel.into());
        t.insert("serialTriggerData".into(), sr.data.clone().into());
        t.insert("serialTriggerBits".into(), sr.bits.into());
        if ch_num == 32 {
            t.insert(
                "serialTriggerExt32Start".into(),
                sr.start_ext32.clone().into(),
            );
            t.insert(
                "serialTriggerExt32Stop".into(),
                sr.stop_ext32.clone().into(),
            );
            t.insert(
                "serialTriggerExt32Clock".into(),
                sr.clock_ext32.clone().into(),
            );
        }
        Value::Object(t)
    }

    /// `set_session()`: missing keys read as Qt's `toDouble()`/`toString()`
    /// of nothing (0, "", false), as Qt sets them.
    pub fn load_json(&mut self, o: &Map<String, Value>, ch_num: usize) {
        let num = |k: &str| o.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let text = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let flag = |k: &str| o.get(k).and_then(Value::as_bool).unwrap_or(false);
        self.stages = (num("triggerStages") as usize).min(TRIGGER_STAGES - 1);
        self.tab = (num("triggerTab") as usize).min(1);
        self.advanced = flag("advTriggerMode");
        for (i, s) in self.stage.iter_mut().enumerate() {
            s.value0 = text(&format!("stageTriggerValue0{i}"));
            s.value1 = text(&format!("stageTriggerValue1{i}"));
            s.inv0 = (num(&format!("stageTriggerInv0{i}")) as u8).min(1);
            s.inv1 = (num(&format!("stageTriggerInv1{i}")) as u8).min(1);
            s.logic = (num(&format!("stageTriggerLogic{i}")) as u8).min(1);
            s.count = num(&format!("stageTriggerCount{i}")).clamp(1.0, i32::MAX as f64) as u32;
            s.contiguous = flag(&format!("stageTriggerContiguous{i}"));
            if ch_num == 32 {
                if let Some(v) = o
                    .get(&format!("stageTriggerExt32Value0{i}"))
                    .and_then(Value::as_str)
                {
                    s.value0_ext32 = v.to_owned();
                }
                if let Some(v) = o
                    .get(&format!("stageTriggerExt32Value1{i}"))
                    .and_then(Value::as_str)
                {
                    s.value1_ext32 = v.to_owned();
                }
            }
        }
        let sr = &mut self.serial;
        sr.start = text("serialTriggerStart");
        sr.stop = text("serialTriggerStop");
        sr.clock = text("serialTriggerClock");
        sr.channel = num("serialTriggerChannel") as usize;
        sr.data = text("serialTriggerData");
        sr.bits = (num("serialTriggerBits") as usize).min(15);
        if ch_num == 32 {
            for (k, f) in [
                ("serialTriggerExt32Start", &mut sr.start_ext32),
                ("serialTriggerExt32Stop", &mut sr.stop_ext32),
                ("serialTriggerExt32Clock", &mut sr.clock_ext32),
            ] {
                if let Some(v) = o.get(k).and_then(Value::as_str) {
                    *f = v.to_owned();
                }
            }
        }
    }
}

/// `on_serial_value_changed`: the hex shown for a 16-bit 0/1 value, or
/// empty while it holds an X.
pub fn serial_hex(value: &str) -> String {
    let s: String = value
        .chars()
        .filter(|c| *c != ' ')
        .collect::<String>()
        .to_lowercase();
    if s.is_empty() || s.contains('x') || s.len() != 16 {
        return String::new();
    }
    let v = s.chars().fold(0u32, |a, c| (a << 1) | u32::from(c == '1'));
    format!("{v:02X}")
}

/// `on_serial_hex_changed`: the value string for up to four hex digits.
pub fn serial_from_hex(hex: &str) -> Option<String> {
    if hex.len() > 4 {
        return None;
    }
    // strtoul stops at the first non-hex digit
    let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    let v = u32::from_str_radix(if digits.is_empty() { "0" } else { &digits }, 16).ok()?;
    Some(
        (0..16)
            .rev()
            .map(|b| if v >> b & 1 == 1 { "1" } else { "0" })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_trigger() {
        let mut a = AdvTrigger {
            advanced: true,
            stages: 1,
            ..AdvTrigger::default()
        };
        a.stage[0].value0 = normalise("r x 1", "X01RFC");
        a.stage[0].count = 5;
        a.stage[1].contiguous = true;
        a.stage[1].logic = 0;
        let mut t = Trigger::default();
        assert!(a.commit(&mut t, 30, 16));
        assert_eq!(
            (
                t.trigger_en,
                t.trigger_mode,
                t.trigger_pos,
                t.trigger_stages
            ),
            (1, ADV_TRIGGER, 30, 1)
        );
        // bit 15 first in the string: probe 15 = R, probe 13 = 1
        assert_eq!(t.trigger0[0][15], b'R');
        assert_eq!(t.trigger0[0][13], b'1');
        assert_eq!(t.trigger0[0][0], b'X');
        assert_eq!(t.trigger0_count[0], 5);
        assert_eq!(t.trigger_logic[1], 2);
        assert_eq!(t.trigger_logic[0], 1);
        // stage 2 untouched
        assert_eq!(t.trigger_logic[2], Trigger::default().trigger_logic[2]);
    }

    #[test]
    fn serial_trigger() {
        let mut a = AdvTrigger {
            advanced: true,
            tab: 1,
            ..AdvTrigger::default()
        };
        a.serial.channel = 3;
        a.serial.bits = 7;
        a.serial.data = serial_from_hex("a5").unwrap();
        let mut t = Trigger::default();
        assert!(a.commit(&mut t, 1, 16));
        assert_eq!(t.trigger_mode, SERIAL_TRIGGER);
        // the data channel's position holds a '0'
        assert_eq!(t.trigger0[2][3], b'0');
        assert_eq!(t.trigger0[2][4], b'X');
        assert_eq!(t.trigger0_count[3], 7);
        assert_eq!(t.trigger0_count[1], 1);
        assert_eq!(serial_hex(&a.serial.data), "A5");
        assert_eq!(t.trigger0[3][0], b'1');
        assert_eq!(t.trigger0[3][1], b'0');
    }

    #[test]
    fn json_round_trip() {
        let mut a = AdvTrigger {
            advanced: true,
            stages: 3,
            tab: 1,
            ..AdvTrigger::default()
        };
        a.stage[2].count = 9;
        a.serial.channel = 5;
        let j = a.session_json(40, 16);
        let mut b = AdvTrigger::default();
        b.load_json(j.as_object().unwrap(), 16);
        assert_eq!(a, b);
        assert_eq!(normalise("10", "X01"), "1 0 X X X X X X X X X X X X X X");
    }
}
