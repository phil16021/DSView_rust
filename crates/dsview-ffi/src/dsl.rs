//! `dsv_dsl_*`: the Rust `.dsl` reader for libsigrok4DSL's session driver.

use std::ffi::{CStr, CString, c_char};
use std::path::Path;

use dsview_formats::dsl::{DslReader, Packet, StreamState};
use dsview_formats::header::Mode;

use crate::error::{DSV_ERR, DSV_ERR_ARG, DSV_OK, guard, set_error};

/// Opaque handle owned by C between `dsv_dsl_open` and `dsv_dsl_close`.
pub struct DsvDsl {
    reader: DslReader,
    stream: Option<StreamState>,
    strings: Vec<Vec<Option<CString>>>, // per channel: name, trigger, map_unit
    status_names: Vec<Vec<CString>>,
    last_entry: Vec<u8>,
}

impl DsvDsl {
    /// Reads a metadata entry into the handle's buffer.
    pub(crate) fn read_meta(&mut self, name: &str) -> Result<Option<&[u8]>, dsview_formats::Error> {
        match self.reader.meta_entry(name)? {
            Some(bytes) => {
                self.last_entry = bytes;
                Ok(Some(&self.last_entry))
            }
            None => Ok(None),
        }
    }
}

#[repr(C)]
pub struct DsvDslHeader {
    pub version: u32,
    /// 0 logic, 1 DSO, 2 analog.
    pub mode: i32,
    pub samplerate: u64,
    pub total_samples: u64,
    pub total_blocks: u64,
    pub total_probes: u64,
    pub has_trigger_time: i32,
    pub trigger_time: i64,
    pub trigger_pos: u64,
    pub has_hdiv: i32,
    pub hdiv: u64,
    pub has_hdiv_min: i32,
    pub hdiv_min: u64,
    pub has_hdiv_max: i32,
    pub hdiv_max: u64,
    pub has_unit_bits: i32,
    pub unit_bits: u8,
    pub has_ref_min: i32,
    pub ref_min: u32,
    pub has_ref_max: i32,
    pub ref_max: u32,
    pub n_channels: u32,
}

#[repr(C)]
pub struct DsvDslChannel {
    pub index: u16,
    /// Work mode in effect when the channel was created: 0 logic, 1 DSO, 2 analog.
    pub mode: i32,
    pub enabled: i32,
    /// NULL when absent or not decodable (GKeyFile escape error).
    pub name: *const c_char,
    pub trigger: *const c_char,
    pub has_coupling: i32,
    pub coupling: u8,
    pub has_vdiv: i32,
    pub vdiv: u64,
    pub has_vfactor: i32,
    pub vfactor: u64,
    pub has_voffset: i32,
    pub voffset: u16,
    pub has_vtrig: i32,
    pub vtrig: u8,
    pub map_unit: *const c_char,
    pub has_map_max: i32,
    pub map_max: f64,
    pub has_map_min: i32,
    pub map_min: f64,
    pub n_status: u32,
}

pub const DSV_PACKET_LOGIC: i32 = 1;
pub const DSV_PACKET_DSO: i32 = 2;
pub const DSV_PACKET_ANALOG: i32 = 3;

fn mode_code(m: Mode) -> i32 {
    m as i32
}

fn cstring(s: &Option<String>) -> Option<CString> {
    s.as_ref()
        .map(|s| CString::new(s.replace('\0', "")).unwrap_or_default())
}

fn ptr(s: &Option<CString>) -> *const c_char {
    s.as_ref().map_or(std::ptr::null(), |c| c.as_ptr())
}

/// Opens and validates a capture. Returns DSV_OK and a handle, or an error code.
///
/// # Safety
/// `path` must be a NUL-terminated string; `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dsl_open(path: *const c_char, out: *mut *mut DsvDsl) -> i32 {
    guard(|| {
        if path.is_null() || out.is_null() {
            set_error("dsv_dsl_open: NULL argument");
            return DSV_ERR_ARG;
        }
        let path = unsafe { CStr::from_ptr(path) };
        let Ok(path) = path.to_str() else {
            set_error("dsv_dsl_open: path is not UTF-8");
            return DSV_ERR_ARG;
        };
        match DslReader::open(Path::new(path)) {
            Ok(reader) => {
                let h = reader.header();
                let strings = h
                    .channels
                    .iter()
                    .map(|c| vec![cstring(&c.name), cstring(&c.trigger), cstring(&c.map_unit)])
                    .collect();
                let status_names = h
                    .channels
                    .iter()
                    .map(|c| {
                        c.status
                            .iter()
                            .map(|(n, _)| CString::new(n.as_str()).unwrap_or_default())
                            .collect()
                    })
                    .collect();
                let handle = Box::new(DsvDsl {
                    reader,
                    stream: None,
                    strings,
                    status_names,
                    last_entry: Vec::new(),
                });
                unsafe { *out = Box::into_raw(handle) };
                DSV_OK
            }
            Err(e) => {
                set_error(format!("{path}: {e}"));
                DSV_ERR
            }
        }
    })
}

/// # Safety
/// `h` must come from `dsv_dsl_open` and not be used afterwards. NULL is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dsl_close(h: *mut DsvDsl) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

/// # Safety
/// `h` from `dsv_dsl_open`; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dsl_header(h: *const DsvDsl, out: *mut DsvDslHeader) -> i32 {
    guard(|| {
        let (Some(h), false) = (unsafe { h.as_ref() }, out.is_null()) else {
            set_error("dsv_dsl_header: NULL argument");
            return DSV_ERR_ARG;
        };
        let x = h.reader.header();
        let opt64 = |v: Option<u64>| (i32::from(v.is_some()), v.unwrap_or(0));
        let (has_hdiv, hdiv) = opt64(x.hdiv);
        let (has_hdiv_min, hdiv_min) = opt64(x.hdiv_min);
        let (has_hdiv_max, hdiv_max) = opt64(x.hdiv_max);
        unsafe {
            *out = DsvDslHeader {
                version: x.version.min(u64::from(u32::MAX)) as u32,
                mode: mode_code(x.mode),
                samplerate: x.samplerate,
                total_samples: x.total_samples,
                total_blocks: x.total_blocks,
                total_probes: x.total_probes,
                has_trigger_time: i32::from(x.trigger_time.is_some()),
                trigger_time: x.trigger_time.unwrap_or(0),
                trigger_pos: x.trigger_pos,
                has_hdiv,
                hdiv,
                has_hdiv_min,
                hdiv_min,
                has_hdiv_max,
                hdiv_max,
                has_unit_bits: i32::from(x.unit_bits.is_some()),
                unit_bits: x.unit_bits.unwrap_or(0),
                has_ref_min: i32::from(x.ref_min.is_some()),
                ref_min: x.ref_min.unwrap_or(0),
                has_ref_max: i32::from(x.ref_max.is_some()),
                ref_max: x.ref_max.unwrap_or(0),
                n_channels: x.channels.len() as u32,
            }
        };
        DSV_OK
    })
}

/// Channel `i` in list order. Strings stay valid until `dsv_dsl_close`.
///
/// # Safety
/// `h` from `dsv_dsl_open`; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dsl_channel(h: *const DsvDsl, i: u32, out: *mut DsvDslChannel) -> i32 {
    guard(|| {
        let (Some(h), false) = (unsafe { h.as_ref() }, out.is_null()) else {
            set_error("dsv_dsl_channel: NULL argument");
            return DSV_ERR_ARG;
        };
        let Some(c) = h.reader.header().channels.get(i as usize) else {
            set_error(format!("dsv_dsl_channel: no channel {i}"));
            return DSV_ERR_ARG;
        };
        let s = &h.strings[i as usize];
        unsafe {
            *out = DsvDslChannel {
                index: c.index,
                mode: mode_code(c.mode.unwrap_or(Mode::Logic)),
                enabled: i32::from(c.enabled),
                name: ptr(&s[0]),
                trigger: ptr(&s[1]),
                has_coupling: i32::from(c.coupling.is_some()),
                coupling: c.coupling.unwrap_or(0),
                has_vdiv: i32::from(c.vdiv.is_some()),
                vdiv: c.vdiv.unwrap_or(0),
                has_vfactor: i32::from(c.vfactor.is_some()),
                vfactor: c.vfactor.unwrap_or(0),
                has_voffset: i32::from(c.voffset.is_some()),
                voffset: c.voffset.unwrap_or(0),
                has_vtrig: i32::from(c.vtrig.is_some()),
                vtrig: c.vtrig.unwrap_or(0),
                map_unit: ptr(&s[2]),
                has_map_max: i32::from(c.map_max.is_some()),
                map_max: c.map_max.unwrap_or(0.0),
                has_map_min: i32::from(c.map_min.is_some()),
                map_min: c.map_min.unwrap_or(0.0),
                n_status: c.status.len() as u32,
            }
        };
        DSV_OK
    })
}

/// Status value `j` of channel `i` ("period", "pcnt", "max", ...).
///
/// # Safety
/// `h` from `dsv_dsl_open`; `name` and `value` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dsl_channel_status(
    h: *const DsvDsl,
    i: u32,
    j: u32,
    name: *mut *const c_char,
    value: *mut u64,
) -> i32 {
    guard(|| {
        let (Some(h), false, false) = (unsafe { h.as_ref() }, name.is_null(), value.is_null())
        else {
            set_error("dsv_dsl_channel_status: NULL argument");
            return DSV_ERR_ARG;
        };
        let Some((n, v)) = h
            .reader
            .header()
            .channels
            .get(i as usize)
            .and_then(|c| c.status.get(j as usize))
        else {
            set_error(format!(
                "dsv_dsl_channel_status: no status {j} on channel {i}"
            ));
            return DSV_ERR_ARG;
        };
        let _ = n;
        unsafe {
            *name = h.status_names[i as usize][j as usize].as_ptr();
            *value = *v;
        }
        DSV_OK
    })
}

/// Next data packet, with the C driver's packet boundaries.
/// Returns 1 with a packet, 0 at the end, a negative code on error.
/// `data` stays valid until the next call or `dsv_dsl_close`.
///
/// # Safety
/// `h` from `dsv_dsl_open`; output pointers writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_dsl_next_packet(
    h: *mut DsvDsl,
    kind: *mut i32,
    data: *mut *const u8,
    len: *mut u64,
    num_samples: *mut u64,
) -> i32 {
    guard(|| {
        let Some(h) = (unsafe { h.as_mut() }) else {
            set_error("dsv_dsl_next_packet: NULL handle");
            return DSV_ERR_ARG;
        };
        if kind.is_null() || data.is_null() || len.is_null() || num_samples.is_null() {
            set_error("dsv_dsl_next_packet: NULL argument");
            return DSV_ERR_ARG;
        }
        if h.stream.is_none() {
            h.stream = Some(h.reader.start_stream());
        }
        let st = h.stream.as_mut().unwrap();
        match h.reader.next_packet(st) {
            Ok(None) => 0,
            Ok(Some(p)) => {
                let (k, d, n) = match p {
                    Packet::Logic(d) => (DSV_PACKET_LOGIC, d, 0),
                    Packet::Dso { data, num_samples } => (DSV_PACKET_DSO, data, num_samples),
                    Packet::Analog { data, num_samples } => (DSV_PACKET_ANALOG, data, num_samples),
                };
                unsafe {
                    *kind = k;
                    *data = d.as_ptr();
                    *len = d.len() as u64;
                    *num_samples = n;
                }
                1
            }
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo(rel: &str) -> CString {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        CString::new(p.to_str().unwrap()).unwrap()
    }

    #[test]
    fn reads_protocol_demo_through_the_c_abi() {
        let path = demo("DSView/demo/logic/protocol.demo");
        let mut h: *mut DsvDsl = std::ptr::null_mut();
        assert_eq!(unsafe { dsv_dsl_open(path.as_ptr(), &mut h) }, DSV_OK);

        let mut hdr: DsvDslHeader = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { dsv_dsl_header(h, &mut hdr) }, DSV_OK);
        assert_eq!(
            (hdr.mode, hdr.samplerate, hdr.total_samples, hdr.n_channels),
            (0, 25_000_000, 131072, 16)
        );

        let mut ch: DsvDslChannel = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { dsv_dsl_channel(h, 0, &mut ch) }, DSV_OK);
        assert_eq!(unsafe { CStr::from_ptr(ch.name) }.to_str().unwrap(), "SDA");
        assert_eq!(unsafe { dsv_dsl_channel(h, 99, &mut ch) }, DSV_ERR_ARG);

        let (mut kind, mut data, mut len, mut n) = (0, std::ptr::null(), 0u64, 0u64);
        let mut total = 0;
        loop {
            match unsafe { dsv_dsl_next_packet(h, &mut kind, &mut data, &mut len, &mut n) } {
                1 => {
                    assert_eq!(kind, DSV_PACKET_LOGIC);
                    total += len;
                }
                0 => break,
                e => panic!("error {e}"),
            }
        }
        // 16 channels x 131072 samples / 8 bits.
        assert_eq!(total, 16 * 131072 / 8);
        unsafe { dsv_dsl_close(h) };
    }

    #[test]
    fn open_errors_are_reported() {
        let path = CString::new("/nonexistent.dsl").unwrap();
        let mut h: *mut DsvDsl = std::ptr::null_mut();
        assert_eq!(unsafe { dsv_dsl_open(path.as_ptr(), &mut h) }, DSV_ERR);
        let msg = unsafe { CStr::from_ptr(crate::error::dsv_last_error()) }
            .to_str()
            .unwrap();
        assert!(msg.contains("nonexistent"), "{msg}");
        assert_eq!(
            unsafe { dsv_dsl_open(std::ptr::null(), &mut h) },
            DSV_ERR_ARG
        );
    }
}
