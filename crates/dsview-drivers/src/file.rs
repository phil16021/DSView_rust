//! The "virtual-session" driver, from libsigrok4DSL/session_driver.c: a
//! device per opened `.dsl` capture, replaying it through the session.
//!
//! Parsing and packetizing are dsview-formats' (verified against the C
//! loader in Phase 1a); this is the driver around them: `sr_new_virtual_device`,
//! the load into config values and channels, `config_get`/`set`/`list` and
//! acquisition. The packets are those of the C driver's Rust-reader path
//! (`receive_data_rust`), which equal its C path.

use std::path::Path;
use std::sync::Mutex;

use dsview_core::{
    Body, Channel, ConfValue, LA_CROSS_DATA, MQ_VOLTAGE, MQFLAG_AC, Mode, Packet, SrError,
    SrResult, Status, TriggerPos, UNIT_VOLT, channel_type, dev_status, dev_type, key, pkt_status,
};
use dsview_formats::dsl::{DslReader, Packet as DslPacket, StreamState};
use dsview_formats::header::Mode as FileMode;

use crate::sys::{sr_dbg, sr_err, sr_info};
use crate::{Device, DeviceInfo, Step};

const MAX_TIMEBASE: u64 = 10_000_000_000;
const MIN_TIMEBASE: u64 = 10;
const MAX_HEIGHTS: [&str; 5] = ["1X", "2X", "3X", "4X", "5X"];
const VDIVS: [u64; 8] = [10, 20, 50, 100, 200, 500, 1000, 2000];
const PROBE_MAP_UNITS: [&str; 7] = ["V", "A", "℃", "℉", "g", "m", "m/s"];

/// session_driver.c's `samplerates[1]` / `samplecounts[1]`: one pair for
/// all file devices, set by whichever loaded last.
static RATES: Mutex<(u64, u64)> = Mutex::new((0, 0));

fn mode_of(m: FileMode) -> Mode {
    match m {
        FileMode::Logic => Mode::Logic,
        FileMode::Dso => Mode::Dso,
        FileMode::Analog => Mode::Analog,
    }
}

/// struct session_vdev (allocated by dev_open, freed by dev_close).
struct Vdev {
    version: i16,
    samplerate: u64,
    total_samples: u64,
    trig_time: i64,
    trig_pos: u64,
    num_blocks: i32,
    num_probes: i32,
    enabled_probes: i32,
    timebase: u64,
    max_timebase: u64,
    min_timebase: u64,
    unit_bits: u8,
    ref_min: u32,
    ref_max: u32,
    max_height: u8,
    mstatus: Status,
    /// rs_reader: the capture being streamed
    reader: Option<(DslReader, StreamState)>,
}

impl Vdev {
    fn new() -> Vdev {
        Vdev {
            version: 0,
            samplerate: 0,
            total_samples: 0,
            trig_time: 0,
            trig_pos: 0,
            num_blocks: 0,
            num_probes: 0,
            enabled_probes: 0,
            timebase: 0,
            max_timebase: MAX_TIMEBASE,
            min_timebase: MIN_TIMEBASE,
            unit_bits: 1,
            ref_min: 0,
            ref_max: 0,
            max_height: 0,
            mstatus: Status {
                measure_valid: 1,
                ..Status::default()
            },
            reader: None,
        }
    }
}

pub struct FileDevice {
    path: String,
    name: String,
    mode: Mode,
    status: i32,
    channels: Vec<Channel>,
    generation: u64,
    vdev: Option<Vdev>,
}

/// get_file_short_name(): the last path component, at most 149 bytes.
fn short_name(path: &str) -> String {
    let start = path.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let raw = &path.as_bytes()[start..];
    String::from_utf8_lossy(&raw[..raw.len().min(149)]).into_owned()
}

impl FileDevice {
    /// sr_new_virtual_device(): reads the header for the work mode; the
    /// device stays closed until opened.
    pub fn new(path: &str) -> SrResult<FileDevice> {
        let mode = match DslReader::open(Path::new(path)) {
            Ok(r) => mode_of(r.header().mode),
            Err(e) => {
                sr_err!("sr_new_virtual_device: {e}");
                return Err(SrError::Err);
            }
        };
        Ok(FileDevice {
            path: path.to_owned(),
            name: short_name(path),
            mode,
            status: dev_status::INACTIVE,
            channels: Vec::new(),
            generation: 0,
            vdev: None,
        })
    }

    fn vdev(&self) -> SrResult<&Vdev> {
        // C dereferences sdi->priv unchecked before dev_open
        self.vdev.as_ref().ok_or(SrError::Err)
    }

    /// sr_load_virtual_device_session() (the Rust-reader variant)
    fn load(&mut self) -> SrResult<()> {
        self.channels.clear();
        self.generation += 1;
        let r = match DslReader::open(Path::new(&self.path)) {
            Ok(r) => r,
            Err(e) => {
                sr_err!("load_session_rust: {e}");
                return Err(SrError::Err);
            }
        };
        let h = r.header().clone();
        sr_info!("Loading '{}' with the Rust reader.", self.path);
        use ConfValue::*;
        let set = |d: &mut Self, k: i32, v: ConfValue, ch: Option<usize>| {
            let _ = d.config_set(k, &v, ch);
        };
        set(self, key::FILE_VERSION, I16(h.version as i16), None);
        set(self, key::SAMPLERATE, U64(h.samplerate), None);
        set(self, key::LIMIT_SAMPLES, U64(h.total_samples), None);
        if let Some(v) = h.hdiv {
            set(self, key::TIMEBASE, U64(v), None);
        }
        if let Some(v) = h.hdiv_min {
            set(self, key::MIN_TIMEBASE, U64(v), None);
        }
        if let Some(v) = h.hdiv_max {
            set(self, key::MAX_TIMEBASE, U64(v), None);
        }
        if let Some(v) = h.unit_bits {
            set(self, key::UNIT_BITS, Byte(v), None);
        }
        if let Some(v) = h.ref_min {
            set(self, key::REF_MIN, U32(v), None);
        }
        if let Some(v) = h.ref_max {
            set(self, key::REF_MAX, U32(v), None);
        }
        if let Some(v) = h.trigger_time {
            set(self, key::TRIGGER_TIME, I64(v), None);
        }
        set(self, key::TRIGGER_POS, U64(h.trigger_pos), None);
        set(self, key::NUM_BLOCKS, U64(h.total_blocks), None);
        set(self, key::CAPTURE_NUM_PROBES, U64(h.total_probes), None);

        for c in &h.channels {
            let ty = match c.mode.unwrap_or(FileMode::Logic) {
                FileMode::Logic => channel_type::LOGIC,
                FileMode::Dso => channel_type::DSO,
                FileMode::Analog => channel_type::ANALOG,
            };
            let name = c.name.clone().unwrap_or_default();
            self.channels
                .push(Channel::new(c.index, ty, c.enabled, &name));
        }
        self.generation += 1;

        for (i, c) in h.channels.iter().enumerate() {
            if let Some(t) = &c.trigger {
                // sr_dev_trigger_set(): the first channel with this index
                if let Some(p) = self.channels.iter_mut().find(|p| p.index == c.index) {
                    p.trigger = Some(t.clone());
                }
            }
            let ch = Some(i);
            if let Some(v) = c.coupling {
                set(self, key::PROBE_COUPLING, Byte(v), ch);
            }
            if let Some(v) = c.vdiv {
                set(self, key::PROBE_VDIV, U64(v), ch);
            }
            if let Some(v) = c.vfactor {
                set(self, key::PROBE_FACTOR, U64(v), ch);
            }
            if let Some(v) = c.voffset {
                set(self, key::PROBE_HW_OFFSET, U16(v), ch);
            }
            if let Some(v) = c.vtrig {
                set(self, key::TRIGGER_VALUE, Byte(v), ch);
            }
            for (name, v) in &c.status {
                let v = *v;
                let (k, val) = match name.as_str() {
                    "period" => (key::STATUS_PERIOD, U32(v as u32)),
                    "pcnt" => (key::STATUS_PCNT, U32(v as u32)),
                    "max" => (key::STATUS_MAX, Byte(v as u8)),
                    "min" => (key::STATUS_MIN, Byte(v as u8)),
                    "plen" => (key::STATUS_PLEN, U32(v as u32)),
                    "llen" => (key::STATUS_LLEN, U32(v as u32)),
                    "level" => (key::STATUS_LEVEL, Bool(v != 0)),
                    "plevel" => (key::STATUS_PLEVEL, Bool(v != 0)),
                    "low" => (key::STATUS_LOW, Byte(v as u8)),
                    "high" => (key::STATUS_HIGH, Byte(v as u8)),
                    "rlen" => (key::STATUS_RLEN, U32(v as u32)),
                    "flen" => (key::STATUS_FLEN, U32(v as u32)),
                    "rms" => (key::STATUS_RMS, U64(v)),
                    "mean" => (key::STATUS_MEAN, U32(v as u32)),
                    _ => continue,
                };
                set(self, k, val, ch);
            }
            if let Some(u) = &c.map_unit {
                set(self, key::PROBE_MAP_UNIT, Str(u.clone()), ch);
            }
            if let Some(v) = c.map_max {
                set(self, key::PROBE_MAP_MAX, F64(v), ch);
            }
            if let Some(v) = c.map_min {
                set(self, key::PROBE_MAP_MIN, F64(v), ch);
            }
        }
        Ok(())
    }

    fn ch_mut(&mut self, ch: Option<usize>) -> SrResult<&mut Channel> {
        ch.and_then(|i| self.channels.get_mut(i))
            .ok_or(SrError::Arg)
    }

    /// The status block of the channel's index (C: index 0, else channel 1).
    fn status_slot(&mut self, ch: Option<usize>) -> SrResult<(bool, &mut Status)> {
        let first = self.ch_mut(ch)?.index == 0;
        let v = self.vdev.as_mut().ok_or(SrError::Err)?;
        Ok((first, &mut v.mstatus))
    }

    fn end(&mut self, status: u16, out: &mut Vec<Packet>) -> Step {
        out.push(Packet::end(status));
        if let Some(v) = self.vdev.as_mut() {
            v.reader = None;
        }
        Step::Remove
    }
}

impl Device for FileDevice {
    fn info(&self) -> DeviceInfo {
        DeviceInfo {
            vendor: String::new(),
            model: self.name.clone(),
            version: None,
            dev_type: dev_type::FILELOG,
            status: self.status,
            mode: self.mode,
            path: Some(self.path.clone()),
        }
    }

    fn set_status(&mut self, status: i32) {
        self.status = status;
    }

    fn mode_list(&self) -> Vec<Mode> {
        vec![self.mode]
    }

    fn channels(&self) -> &[Channel] {
        &self.channels
    }

    fn channels_mut(&mut self) -> &mut [Channel] {
        &mut self.channels
    }

    fn channel_generation(&self) -> u64 {
        self.generation
    }

    /// dev_open()
    fn open(&mut self) -> SrResult<()> {
        if self.status == dev_status::ACTIVE {
            return Ok(());
        }
        self.vdev = Some(Vdev::new());
        self.status = dev_status::ACTIVE;
        if let Err(e) = self.load() {
            sr_err!("Error!Load session file failed.");
            return Err(e);
        }
        Ok(())
    }

    /// dev_close()
    fn close(&mut self) -> SrResult<()> {
        if self.vdev.take().is_none() {
            return Err(SrError::CallStatus);
        }
        self.status = dev_status::INACTIVE;
        Ok(())
    }

    fn config_get(&self, id: i32, ch: Option<usize>) -> SrResult<Option<ConfValue>> {
        use ConfValue::*;
        let v = self.vdev()?;
        let c = ch.and_then(|i| self.channels.get(i));
        let chan = || c.ok_or(SrError::Err);
        Ok(Some(match id {
            key::SAMPLERATE => U64(v.samplerate),
            key::LIMIT_SAMPLES => U64(v.total_samples),
            key::TRIGGER_TIME => I64(v.trig_time),
            key::TIMEBASE => U64(v.timebase),
            key::MAX_TIMEBASE => U64(v.max_timebase),
            key::MIN_TIMEBASE => U64(v.min_timebase),
            key::UNIT_BITS => Byte(v.unit_bits),
            key::REF_MIN if v.ref_min == 0 => return Err(SrError::Err),
            key::REF_MIN => U32(v.ref_min),
            key::REF_MAX if v.ref_max == 0 => return Err(SrError::Err),
            key::REF_MAX => U32(v.ref_max),
            key::PROBE_EN => Bool(chan()?.enabled),
            key::PROBE_COUPLING => Byte(chan()?.coupling),
            key::PROBE_VDIV => U64(chan()?.vdiv),
            key::PROBE_FACTOR => U64(chan()?.vfactor),
            key::PROBE_OFFSET => U16(chan()?.offset),
            key::PROBE_HW_OFFSET => U16(chan()?.hw_offset),
            // C: g_variant_new_string(NULL) when unset, i.e. SR_OK without a value
            key::PROBE_MAP_UNIT => return Ok(chan()?.map_unit.clone().map(Str)),
            key::PROBE_MAP_MIN => F64(chan()?.map_min),
            key::PROBE_MAP_MAX => F64(chan()?.map_max),
            key::TRIGGER_VALUE => Byte(chan()?.trig_value),
            key::MAX_DSO_SAMPLERATE => U64(v.samplerate),
            key::MAX_DSO_SAMPLELIMITS | key::HW_DEPTH => U64(v.total_samples),
            key::MAX_HEIGHT => Str(MAX_HEIGHTS[v.max_height as usize].to_owned()),
            key::MAX_HEIGHT_VALUE => Byte(v.max_height),
            key::VLD_CH_NUM => I16(v.num_probes as i16),
            key::FILE_VERSION => I16(v.version),
            _ => return Err(SrError::Na),
        }))
    }

    fn config_set(&mut self, id: i32, data: &ConfValue, ch: Option<usize>) -> SrResult<()> {
        let mode = self.mode;
        let v = self.vdev.as_mut().ok_or(SrError::Err)?;
        match id {
            key::SAMPLERATE => {
                v.samplerate = data.get_u64();
                RATES.lock().unwrap().0 = v.samplerate;
                sr_dbg!("Setting samplerate to {}.", v.samplerate);
            }
            key::TIMEBASE => v.timebase = data.get_u64(),
            key::MAX_TIMEBASE => v.max_timebase = data.get_u64(),
            key::MIN_TIMEBASE => v.min_timebase = data.get_u64(),
            key::UNIT_BITS => v.unit_bits = data.get_byte(),
            key::REF_MIN => v.ref_min = data.get_u32(),
            key::REF_MAX => v.ref_max = data.get_u32(),
            key::FILE_VERSION => v.version = data.get_i16(),
            key::LIMIT_SAMPLES => {
                v.total_samples = data.get_u64();
                RATES.lock().unwrap().1 = v.total_samples;
            }
            key::TRIGGER_TIME => v.trig_time = data.get_i64(),
            key::TRIGGER_POS => v.trig_pos = data.get_u64(),
            key::NUM_BLOCKS => v.num_blocks = data.get_u64() as i32,
            // C also sizes a version-1 unpack buffer here
            key::CAPTURE_NUM_PROBES => v.num_probes = data.get_u64() as i32,
            key::PROBE_EN => self.ch_mut(ch)?.enabled = data.get_bool(),
            key::PROBE_COUPLING => self.ch_mut(ch)?.coupling = data.get_byte(),
            key::PROBE_VDIV => self.ch_mut(ch)?.vdiv = data.get_u64(),
            key::PROBE_FACTOR => self.ch_mut(ch)?.vfactor = data.get_u64(),
            key::PROBE_OFFSET => self.ch_mut(ch)?.offset = data.get_u16(),
            key::PROBE_HW_OFFSET => {
                let c = self.ch_mut(ch)?;
                c.hw_offset = data.get_u16();
                c.offset = c.hw_offset;
            }
            key::PROBE_MAP_UNIT => self.ch_mut(ch)?.map_unit = data.get_str().map(str::to_owned),
            key::PROBE_MAP_MIN => self.ch_mut(ch)?.map_min = data.get_f64(),
            key::PROBE_MAP_MAX => self.ch_mut(ch)?.map_max = data.get_f64(),
            key::TRIGGER_VALUE => self.ch_mut(ch)?.trig_value = data.get_byte(),
            key::STATUS_PERIOD
            | key::STATUS_PCNT
            | key::STATUS_MAX
            | key::STATUS_MIN
            | key::STATUS_PLEN
            | key::STATUS_LLEN
            | key::STATUS_LEVEL
            | key::STATUS_PLEVEL
            | key::STATUS_LOW
            | key::STATUS_HIGH
            | key::STATUS_RLEN
            | key::STATUS_FLEN
            | key::STATUS_RMS
            | key::STATUS_MEAN => {
                let (first, st) = self.status_slot(ch)?;
                macro_rules! pick {
                    ($a:ident, $b:ident, $val:expr) => {
                        if first { st.$a = $val } else { st.$b = $val }
                    };
                }
                match id {
                    key::STATUS_PERIOD => pick!(ch0_cyc_tlen, ch1_cyc_tlen, data.get_u32()),
                    key::STATUS_PCNT => pick!(ch0_cyc_cnt, ch1_cyc_cnt, data.get_u32()),
                    key::STATUS_MAX => pick!(ch0_max, ch1_max, data.get_byte()),
                    key::STATUS_MIN => pick!(ch0_min, ch1_min, data.get_byte()),
                    key::STATUS_PLEN => pick!(ch0_cyc_plen, ch1_cyc_plen, data.get_u32()),
                    key::STATUS_LLEN => pick!(ch0_cyc_llen, ch1_cyc_llen, data.get_u32()),
                    key::STATUS_LEVEL => {
                        pick!(ch0_level_valid, ch1_level_valid, data.get_bool() as i32)
                    }
                    key::STATUS_PLEVEL => pick!(ch0_plevel, ch1_plevel, data.get_bool() as i32),
                    key::STATUS_LOW => pick!(ch0_low_level, ch1_low_level, data.get_byte()),
                    key::STATUS_HIGH => pick!(ch0_high_level, ch1_high_level, data.get_byte()),
                    key::STATUS_RLEN => pick!(ch0_cyc_rlen, ch1_cyc_rlen, data.get_u32()),
                    key::STATUS_FLEN => pick!(ch0_cyc_flen, ch1_cyc_flen, data.get_u32()),
                    key::STATUS_RMS => pick!(ch0_acc_square, ch1_acc_square, data.get_u64()),
                    _ => pick!(ch0_acc_mean, ch1_acc_mean, data.get_u32()),
                }
            }
            key::MAX_HEIGHT => {
                let Some(s) = data.get_str() else {
                    return Err(SrError::Arg);
                };
                if let Some(i) = MAX_HEIGHTS.iter().position(|h| *h == s) {
                    v.max_height = i as u8;
                }
            }
            key::INSTANT | key::RLE => {}
            _ => {
                sr_err!("Unknown capability: {id}.");
                return Err(SrError::Na);
            }
        }
        let _ = mode;
        Ok(())
    }

    fn config_list(&self, id: i32) -> SrResult<ConfValue> {
        use ConfValue::*;
        let (rate, count) = *RATES.lock().unwrap();
        Ok(match id {
            key::DEVICE_OPTIONS | key::DEVICE_SESSIONS => I32s(vec![key::MAX_HEIGHT]),
            key::SAMPLERATE => Dict(vec![("samplerates".into(), U64s(vec![rate]))]),
            key::LIMIT_SAMPLES => Dict(vec![("samplecounts".into(), U64s(vec![count]))]),
            key::MAX_HEIGHT => StrV(MAX_HEIGHTS.iter().map(|s| s.to_string()).collect()),
            key::PROBE_CONFIGS => I32s(vec![
                key::PROBE_MAP_UNIT,
                key::PROBE_MAP_MIN,
                key::PROBE_MAP_MAX,
            ]),
            key::PROBE_VDIV => Dict(vec![("vdivs".into(), U64s(VDIVS.to_vec()))]),
            key::PROBE_MAP_UNIT => StrV(PROBE_MAP_UNITS.iter().map(|s| s.to_string()).collect()),
            _ => return Err(SrError::Arg),
        })
    }

    fn status(&self, _prg: bool) -> SrResult<Status> {
        Ok(self.vdev()?.mstatus)
    }

    /// dev_acquisition_start(): SR_DF_HEADER, SR_DF_TRIGGER when the capture
    /// has a trigger position, then the stream.
    fn start(&mut self, out: &mut Vec<Packet>) -> SrResult<()> {
        let mode = self.mode;
        let enabled = self.channels.iter().filter(|c| c.enabled).count() as i32;
        let v = self.vdev.as_mut().ok_or(SrError::Err)?;
        v.enabled_probes = 0;
        v.reader = None;
        let r = match DslReader::open(Path::new(&self.path)) {
            Ok(r) => r,
            Err(e) => {
                sr_err!("dev_acquisition_start: {e}");
                return Err(SrError::Err);
            }
        };
        let mut r = r;
        for (i, c) in self.channels.iter().enumerate() {
            r.set_channel_enabled(i, c.enabled);
        }
        let st = r.start_stream();
        let v = self.vdev.as_mut().ok_or(SrError::Err)?;
        v.reader = Some((r, st));
        v.enabled_probes = enabled;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        out.push(Packet::ok(Body::Header {
            feed_version: 1,
            start_sec: now.as_secs() as i64,
            start_usec: i64::from(now.subsec_micros()),
        }));
        if v.trig_pos != 0 {
            let real_pos = if mode == Mode::Dso {
                // C divides by num_probes unchecked
                (v.trig_pos * v.enabled_probes as u64)
                    .checked_div(v.num_probes as u64)
                    .unwrap_or(0)
            } else {
                v.trig_pos
            };
            out.push(Packet::ok(Body::Trigger(TriggerPos {
                real_pos: real_pos as u32,
                ..TriggerPos::default()
            })));
        }
        Ok(())
    }

    /// receive_data_rust()
    fn poll(&mut self, abort: bool, out: &mut Vec<Packet>) -> Step {
        let Some(v) = self.vdev.as_mut() else {
            return Step::Remove;
        };
        let unit_bits = v.unit_bits;
        // receive_data() marks version-1 logic packets with its channel
        // cursor, which starts at the last channel
        let order = if v.version == 1 {
            (v.num_probes - 1) as u16
        } else {
            0
        };
        let Some((reader, st)) = v.reader.as_mut() else {
            return self.end(pkt_status::OK, out);
        };
        if abort {
            return self.end(pkt_status::OK, out);
        }
        let body = match reader.next_packet(st) {
            Ok(Some(DslPacket::Logic(data))) => Body::Logic {
                format: LA_CROSS_DATA,
                index: 0,
                order,
                unitsize: 0,
                data_error: 0,
                error_pattern: 0,
                data: data.to_vec(),
            },
            Ok(Some(DslPacket::Dso { data, num_samples })) => Body::Dso {
                num_samples: num_samples as i32,
                mq: MQ_VOLTAGE,
                unit: UNIT_VOLT,
                mqflags: MQFLAG_AC,
                samplerate_tog: false,
                trig_flag: false,
                trig_ch: 0,
                data: data.to_vec(),
            },
            Ok(Some(DslPacket::Analog { data, num_samples })) => Body::Analog {
                num_samples: num_samples as i32,
                unit_bits,
                unit_pitch: 0,
                mq: MQ_VOLTAGE,
                unit: UNIT_VOLT,
                mqflags: MQFLAG_AC,
                data: data.to_vec(),
            },
            Ok(None) => return self.end(pkt_status::OK, out),
            Err(e) => {
                sr_err!("receive_data_rust: {e}");
                return self.end(pkt_status::SOURCE_ERROR, out);
            }
        };
        out.push(Packet::ok(body));
        Step::Continue
    }

    /// The C driver has no dev_acquisition_stop.
    fn stop(&mut self, _out: &mut Vec<Packet>) -> SrResult<()> {
        Ok(())
    }
}
