//! The `header` entry of a `.dsl`/`.demo` file, interpreted exactly like
//! `sr_new_virtual_device` + `sr_load_virtual_device_session` in
//! libsigrok4DSL/session_driver.c (including the hardening of commit
//! 5224e047). Quirks that matter for compatibility:
//!
//! * `[version]` and `device mode` only take effect for keys that come after
//!   them in the file (the C loader walks groups and keys in order).
//! * Keys are matched in the C if-chain order: exact names first, then prefixes
//!   (`probe`, `trigger`, `enable`, ... ). `trigger time`/`trigger pos` are exact.
//! * `probeN` creates channel N in version >= 2; in version 1 `total probes`
//!   creates the channels and `probeN` only names and enables them.
//! * `triggerN` addresses the channel with *index* N; every other per-channel
//!   key (`enableN`, `vDivN`, ...) addresses the N-th channel in list order.

use crate::Error;
use crate::keyfile::{Group, KeyFile};

/// Highest probe count / index accepted (logic data is unpacked through a u64 bit mask).
pub const MAX_PROBES: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Logic = 0,
    Dso = 1,
    Analog = 2,
}

impl Mode {
    fn from_c(v: u64) -> Mode {
        match v {
            1 => Mode::Dso,
            2 => Mode::Analog,
            _ => Mode::Logic,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Channel {
    pub index: u16,
    /// Channel type follows the work mode in effect when the channel was created.
    pub mode: Option<Mode>,
    pub name: Option<String>,
    pub enabled: bool,
    pub trigger: Option<String>,
    pub coupling: Option<u8>,
    pub vdiv: Option<u64>,
    pub vfactor: Option<u64>,
    pub voffset: Option<u16>,
    pub vtrig: Option<u8>,
    pub map_unit: Option<String>,
    pub map_max: Option<f64>,
    pub map_min: Option<f64>,
    /// DSO status values (`period`, `pcnt`, `max`, ...), raw.
    pub status: Vec<(String, u64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    /// Format version in effect when the header section was parsed.
    pub version: u64,
    /// Work mode of the device (`sr_new_virtual_device`).
    pub mode: Mode,
    pub driver: Option<String>,
    pub samplerate: u64,
    pub total_samples: u64,
    pub total_blocks: u64,
    pub total_probes: u64,
    pub trigger_time: Option<i64>,
    pub trigger_pos: u64,
    pub hdiv: Option<u64>,
    pub hdiv_min: Option<u64>,
    pub hdiv_max: Option<u64>,
    pub unit_bits: Option<u8>,
    pub ref_min: Option<u32>,
    pub ref_max: Option<u32>,
    pub channels: Vec<Channel>,
}

/// C `strtoull(s, NULL, 10)`: optional whitespace and sign, leading digits, 0 if none;
/// a minus sign negates modulo 2^64, overflow saturates.
pub fn strtoull(s: &str) -> u64 {
    let s = s.trim_start();
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: u64 = 0;
    for b in digits.bytes().take_while(u8::is_ascii_digit) {
        match v
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(b - b'0')))
        {
            Some(n) => v = n,
            None => return u64::MAX,
        }
    }
    if neg { v.wrapping_neg() } else { v }
}

/// C `strtoll(s, NULL, 10)` with saturation.
pub fn strtoll(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut v: i64 = 0;
    for b in digits.bytes().take_while(u8::is_ascii_digit) {
        let d = i64::from(b - b'0');
        v = match v.checked_mul(10).and_then(|v| {
            if neg {
                v.checked_sub(d)
            } else {
                v.checked_add(d)
            }
        }) {
            Some(n) => n,
            None => return if neg { i64::MIN } else { i64::MAX },
        };
    }
    v
}

/// C `strtod(s, NULL)` for the decimal forms DSView writes; 0.0 if no number.
pub fn strtod(s: &str) -> f64 {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .take_while(|&(i, c)| {
            c.is_ascii_digit()
                || c == '.'
                || ((c == '-' || c == '+') && (i == 0 || s[..i].ends_with(['e', 'E'])))
                || c == 'e'
                || c == 'E'
        })
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    // Back off until the prefix parses ("1e" -> "1").
    let mut prefix = &s[..end];
    while !prefix.is_empty() {
        if let Ok(v) = prefix.parse::<f64>() {
            return v;
        }
        prefix = &prefix[..prefix.len() - 1];
    }
    0.0
}

fn device_mode(kf: &KeyFile) -> Mode {
    // sr_new_virtual_device: the last "device mode" among header* groups.
    let mut mode = Mode::Logic;
    for g in kf.groups().iter().filter(|g| g.name.starts_with("header")) {
        if let Some(v) = g.get("device mode") {
            mode = Mode::from_c(strtoull(&v));
        }
    }
    mode
}

struct Parser {
    version: u64,
    type_mode: Mode,
    total_probes: u64,
    h: Header,
}

impl Parser {
    fn nth(&mut self, n: u64) -> Option<&mut Channel> {
        self.h.channels.get_mut(usize::try_from(n).ok()?)
    }

    fn header_key(&mut self, key: &str, val: Option<String>) -> Result<(), Error> {
        let v = val.clone().unwrap_or_default();
        let num = |prefix: &str| strtoull(&key[prefix.len()..]);
        match key {
            "device mode" => self.type_mode = Mode::from_c(strtoull(&v)),
            "capturefile" => {}
            // sr_parse_sizestring's result is used even when it fails; ours fails loudly.
            "samplerate" => self.h.samplerate = crate::samplerate::parse(&v)?,
            "total samples" => self.h.total_samples = strtoull(&v),
            "hDiv" => self.h.hdiv = Some(strtoull(&v)),
            "hDiv min" => self.h.hdiv_min = Some(strtoull(&v)),
            "hDiv max" => self.h.hdiv_max = Some(strtoull(&v)),
            "bits" => self.h.unit_bits = Some(strtoull(&v) as u8),
            "ref min" => self.h.ref_min = Some(strtoull(&v) as u32),
            "ref max" => self.h.ref_max = Some(strtoull(&v) as u32),
            "trigger time" => self.h.trigger_time = Some(strtoll(&v)),
            "trigger pos" => self.h.trigger_pos = strtoull(&v),
            "total blocks" => self.h.total_blocks = strtoull(&v),
            "total probes" => {
                let n = strtoull(&v);
                if n == 0 || n > MAX_PROBES {
                    return Err(Error::Header(format!("invalid probe count {n}")));
                }
                self.total_probes = n;
                if self.version == 1 {
                    for p in 0..n {
                        self.h.channels.push(Channel {
                            index: p as u16,
                            mode: Some(self.type_mode),
                            name: Some(p.to_string()),
                            ..Channel::default()
                        });
                    }
                }
            }
            k if k.starts_with("probe") => {
                let idx = num("probe");
                if idx >= MAX_PROBES {
                    return Err(Error::Header(format!("invalid probe index {idx}")));
                }
                if self.version == 1 {
                    if let Some(ch) = self
                        .h
                        .channels
                        .iter_mut()
                        .find(|c| u64::from(c.index) == idx)
                    {
                        ch.name = val;
                        ch.enabled = true;
                    }
                } else if self.version > 1 {
                    self.h.channels.push(Channel {
                        index: idx as u16,
                        mode: Some(self.type_mode),
                        name: val,
                        enabled: true,
                        ..Channel::default()
                    });
                }
            }
            k if k.starts_with("trigger") => {
                let idx = num("trigger");
                if let Some(ch) = self
                    .h
                    .channels
                    .iter_mut()
                    .find(|c| u64::from(c.index) == idx)
                {
                    ch.trigger = val;
                }
            }
            k if k.starts_with("enable") => {
                let on = strtoull(&v) != 0;
                if let Some(ch) = self.nth(num("enable")) {
                    ch.enabled = on;
                }
            }
            k if k.starts_with("coupling") => {
                let x = strtoull(&v) as u8;
                if let Some(ch) = self.nth(num("coupling")) {
                    ch.coupling = Some(x);
                }
            }
            k if k.starts_with("vDiv") => {
                let x = strtoull(&v);
                if let Some(ch) = self.nth(num("vDiv")) {
                    ch.vdiv = Some(x);
                }
            }
            k if k.starts_with("vFactor") => {
                let x = strtoull(&v);
                if let Some(ch) = self.nth(num("vFactor")) {
                    ch.vfactor = Some(x);
                }
            }
            k if k.starts_with("vOffset") => {
                let x = strtoull(&v) as u16;
                if let Some(ch) = self.nth(num("vOffset")) {
                    ch.voffset = Some(x);
                }
            }
            k if k.starts_with("vTrig") => {
                let x = strtoull(&v) as u8;
                if let Some(ch) = self.nth(num("vTrig")) {
                    ch.vtrig = Some(x);
                }
            }
            k if [
                "period", "pcnt", "max", "min", "plen", "llen", "level", "plevel", "low", "high",
                "rlen", "flen", "rms", "mean",
            ]
            .iter()
            .any(|p| k.starts_with(p)) =>
            {
                // First prefix in C order wins ("plevel" is checked after "plen"/"llen"/"level").
                let p = [
                    "period", "pcnt", "max", "min", "plen", "llen", "level", "plevel", "low",
                    "high", "rlen", "flen", "rms", "mean",
                ]
                .into_iter()
                .find(|p| k.starts_with(p))
                .unwrap();
                let x = strtoull(&v);
                if let Some(ch) = self.nth(num(p)) {
                    ch.status.push((p.to_string(), x));
                }
            }
            k if k.starts_with("mapUnit") => {
                if let Some(ch) = self.nth(num("mapUnit")) {
                    ch.map_unit = val;
                }
            }
            k if k.starts_with("mapMax") => {
                let x = strtod(&v);
                if let Some(ch) = self.nth(num("mapMax")) {
                    ch.map_max = Some(x);
                }
            }
            k if k.starts_with("mapMin") => {
                let x = strtod(&v);
                if let Some(ch) = self.nth(num("mapMin")) {
                    ch.map_min = Some(x);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn header_group(&mut self, g: &Group) -> Result<(), Error> {
        self.h.version = self.version;
        self.total_probes = 0;
        for (key, val) in g.iter() {
            self.header_key(key, val)?;
        }
        Ok(())
    }
}

impl Header {
    /// Parses the `header` entry text.
    pub fn parse(text: &str) -> Result<Header, Error> {
        let kf = KeyFile::parse(text)?;
        let mode = device_mode(&kf);
        let mut p = Parser {
            version: 1,
            type_mode: Mode::Logic,
            total_probes: 0,
            h: Header {
                version: 1,
                mode,
                driver: None,
                samplerate: 0,
                total_samples: 0,
                total_blocks: 0,
                total_probes: 0,
                trigger_time: None,
                trigger_pos: 0,
                hdiv: None,
                hdiv_min: None,
                hdiv_max: None,
                unit_bits: None,
                ref_min: None,
                ref_max: None,
                channels: Vec::new(),
            },
        };

        let mut header_found = false;
        for g in kf.groups() {
            if g.name == "version"
                && let Some(v) = g.get("version")
            {
                p.version = strtoull(&v);
            }
            if g.name.starts_with("header") {
                if header_found {
                    return Err(Error::Header("more than one header section".into()));
                }
                header_found = true;
                p.h.driver = g.get("driver");
                p.header_group(g)?;
            }
        }

        if !header_found || p.total_probes == 0 || (p.h.channels.len() as u64) < p.total_probes {
            return Err(Error::Header(
                "header does not describe the stored channels".into(),
            ));
        }
        p.h.total_probes = p.total_probes;
        Ok(p.h)
    }

    /// Bytes per sample for DSO/analog data.
    pub fn unit_bytes(&self) -> u64 {
        u64::from(self.unit_bits.unwrap_or(8)).div_ceil(8).max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOGIC_V3: &str = "[version]\nversion = 3\n[header]\ndriver = virtual-demo\ndevice mode = 0\ncapturefile = data\ntotal samples = 131072\ntotal probes = 2\ntotal blocks = 1\nsamplerate = 25 MHz\ntrigger time = -28800000\ntrigger pos = 0\nprobe0 = SDA\nprobe1 = SCL\n";

    #[test]
    fn logic_v3() {
        let h = Header::parse(LOGIC_V3).unwrap();
        assert_eq!(
            (h.version, h.mode, h.samplerate, h.total_samples),
            (3, Mode::Logic, 25_000_000, 131072)
        );
        assert_eq!(h.trigger_time, Some(-28_800_000));
        assert_eq!(h.channels.len(), 2);
        assert_eq!(h.channels[1].name.as_deref(), Some("SCL"));
        assert!(h.channels.iter().all(|c| c.enabled));
    }

    #[test]
    fn hardening_rules() {
        let twice = format!("{LOGIC_V3}[header1]\ntotal probes = 1\n");
        assert!(Header::parse(&twice).is_err());
        assert!(
            Header::parse(&LOGIC_V3.replace("total probes = 2", "total probes = 70000")).is_err()
        );
        assert!(Header::parse(&LOGIC_V3.replace("total probes = 2", "total probes = 3")).is_err());
        assert!(Header::parse(&LOGIC_V3.replace("probe1 = SCL", "probe64 = SCL")).is_err());
        assert!(Header::parse("[version]\nversion = 3\n").is_err());
    }

    #[test]
    fn version_after_header_is_too_late() {
        // Like the C loader, the header is parsed as version 1 here.
        let text =
            "[header]\ndevice mode = 0\ntotal probes = 2\nprobe1 = B\n[version]\nversion = 3\n";
        let h = Header::parse(text).unwrap();
        assert_eq!(h.version, 1);
        assert_eq!(h.channels.len(), 2);
        assert_eq!(h.channels[0].name.as_deref(), Some("0"));
        assert!(!h.channels[0].enabled);
        assert_eq!(h.channels[1].name.as_deref(), Some("B"));
        assert!(h.channels[1].enabled);
    }

    #[test]
    fn dso_positional_keys() {
        let text = "[version]\nversion = 3\n[header]\ndevice mode = 1\ntotal probes = 2\nsamplerate = 100 MHz\nprobe0 = 0\n enable0 = 1\n vDiv0 = 1000\n coupling0 = 1\nprobe1 = 1\n enable1 = 0\n vDiv1 = 500\n max1 = 7\n plevel1 = 1\n mapUnit1 = V\n";
        let h = Header::parse(text).unwrap();
        assert_eq!(h.mode, Mode::Dso);
        assert_eq!(h.channels[0].mode, Some(Mode::Dso));
        assert_eq!(
            (h.channels[0].vdiv, h.channels[0].coupling),
            (Some(1000), Some(1))
        );
        assert!(!h.channels[1].enabled);
        assert_eq!(
            h.channels[1].status,
            vec![("max".into(), 7), ("plevel".into(), 1)]
        );
        assert_eq!(h.channels[1].map_unit.as_deref(), Some("V"));
    }

    #[test]
    fn c_number_parsing() {
        assert_eq!(strtoull("  42abc"), 42);
        assert_eq!(strtoull("x"), 0);
        assert_eq!(strtoull("-1"), u64::MAX);
        assert_eq!(strtoull("99999999999999999999999"), u64::MAX);
        assert_eq!(strtoll("-28800000"), -28_800_000);
        assert_eq!(strtod("1.5e3V"), 1500.0);
        assert_eq!(strtod("abc"), 0.0);
        assert_eq!(strtod("-0.25"), -0.25);
    }
}
