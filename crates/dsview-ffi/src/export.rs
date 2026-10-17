//! `dsv_export_*`: CSV/VCD exporters behind libsigrok4DSL's output-module
//! interface (adapter: libsigrok4DSL/output/rs_output.c).

use std::ffi::{CStr, CString, c_char};

use dsview_formats::export::{ChannelKind, ExportChannel, ExportPacket, Exporter, Format};

use crate::error::{DSV_ERR, DSV_ERR_ARG, DSV_OK, guard, set_error};

pub struct DsvExport {
    inner: Exporter,
}

/// One device channel (`struct sr_channel` fields the exporters use).
#[repr(C)]
pub struct DsvExportChannel {
    pub index: u16,
    /// 0 logic, 1 DSO, 2 analog, 3 other.
    pub kind: i32,
    pub enabled: i32,
    pub name: *const c_char,
    pub vdiv: u64,
    pub vfactor: u64,
    pub hw_offset: u16,
    pub map_max: f64,
    pub map_min: f64,
    pub map_unit: *const c_char,
}

pub const DSV_EXPORT_META: i32 = 0;
pub const DSV_EXPORT_LOGIC: i32 = 1;
pub const DSV_EXPORT_DSO: i32 = 2;
pub const DSV_EXPORT_ANALOG: i32 = 3;
pub const DSV_EXPORT_END: i32 = 4;

/// One datafeed packet. `data`/`len` are the payload bytes.
#[repr(C)]
pub struct DsvExportPacket {
    pub kind: i32,
    pub has_samplerate: i32,
    pub samplerate: u64,
    pub has_limit_samples: i32,
    pub limit_samples: u64,
    pub has_ref_min: i32,
    pub ref_min: u32,
    pub has_ref_max: i32,
    pub ref_max: u32,
    pub data: *const u8,
    pub len: u64,
    pub unitsize: u32,
    pub num_samples: u64,
    pub export_original: i32,
}

fn kind(k: i32) -> ChannelKind {
    match k {
        0 => ChannelKind::Logic,
        1 => ChannelKind::Dso,
        2 => ChannelKind::Analog,
        _ => ChannelKind::Other,
    }
}

fn string(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Creates a CSV ("csv") or VCD ("vcd") exporter for channel type `kind`.
///
/// # Safety
/// `format` NUL-terminated; `chans` points to `n` channels; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_export_new(
    format: *const c_char,
    kind_code: i32,
    start_index: u64,
    chans: *const DsvExportChannel,
    n: u32,
    out: *mut *mut DsvExport,
) -> i32 {
    guard(|| {
        if format.is_null() || out.is_null() || (chans.is_null() && n > 0) {
            set_error("dsv_export_new: NULL argument");
            return DSV_ERR_ARG;
        }
        let Some(fmt) = Format::from_id(&string(format)) else {
            set_error(format!("unknown export format {:?}", string(format)));
            return DSV_ERR_ARG;
        };
        let raw = if n == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(chans, n as usize) }
        };
        let channels = raw
            .iter()
            .map(|c| ExportChannel {
                index: c.index,
                kind: kind(c.kind),
                enabled: c.enabled != 0,
                name: string(c.name),
                vdiv: c.vdiv,
                vfactor: c.vfactor,
                hw_offset: c.hw_offset,
                map_max: c.map_max,
                map_min: c.map_min,
                map_unit: string(c.map_unit),
            })
            .collect();
        match Exporter::new(fmt, kind(kind_code), channels, start_index) {
            Ok(inner) => {
                unsafe { *out = Box::into_raw(Box::new(DsvExport { inner })) };
                DSV_OK
            }
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

/// Sets the "sampled on" time (CSV) and `$date` (VCD) strings.
///
/// # Safety
/// `h` from `dsv_export_new`; strings NUL-terminated or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_export_set_strings(
    h: *mut DsvExport,
    time_string: *const c_char,
    date_string: *const c_char,
) {
    if let Some(h) = unsafe { h.as_mut() } {
        h.inner.time_string = string(time_string);
        h.inner.date_string = string(date_string);
    }
}

/// Feeds one packet. `*out` receives the produced text (free with
/// `dsv_string_free`) or NULL when the packet produces none.
///
/// # Safety
/// `h` from `dsv_export_new`; `p` valid with `len` readable bytes at `data`; `out` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_export_receive(
    h: *mut DsvExport,
    p: *const DsvExportPacket,
    out: *mut *mut c_char,
) -> i32 {
    guard(|| {
        let (Some(h), Some(p), false) =
            (unsafe { h.as_mut() }, unsafe { p.as_ref() }, out.is_null())
        else {
            set_error("dsv_export_receive: NULL argument");
            return DSV_ERR_ARG;
        };
        unsafe { *out = std::ptr::null_mut() };
        let data = if p.data.is_null() || p.len == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(p.data, p.len as usize) }
        };
        let packet = match p.kind {
            DSV_EXPORT_META => ExportPacket::Meta {
                samplerate: (p.has_samplerate != 0).then_some(p.samplerate),
                limit_samples: (p.has_limit_samples != 0).then_some(p.limit_samples),
                ref_min: (p.has_ref_min != 0).then_some(p.ref_min),
                ref_max: (p.has_ref_max != 0).then_some(p.ref_max),
            },
            DSV_EXPORT_LOGIC => ExportPacket::Logic {
                data,
                unitsize: p.unitsize as usize,
                export_original: p.export_original != 0,
            },
            DSV_EXPORT_DSO => ExportPacket::Dso {
                data,
                num_samples: p.num_samples as usize,
            },
            DSV_EXPORT_ANALOG => ExportPacket::Analog {
                data,
                num_samples: p.num_samples as usize,
            },
            DSV_EXPORT_END => ExportPacket::End,
            k => {
                set_error(format!("dsv_export_receive: unknown packet kind {k}"));
                return DSV_ERR_ARG;
            }
        };
        match h.inner.receive(&packet) {
            Ok(Some(text)) => {
                let c = CString::new(text.replace('\0', "")).unwrap_or_default();
                unsafe { *out = c.into_raw() };
                DSV_OK
            }
            Ok(None) => DSV_OK,
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

/// # Safety
/// `h` from `dsv_export_new`, not used afterwards. NULL is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_export_free(h: *mut DsvExport) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h) });
    }
}

/// Frees a string returned by this library. NULL is ignored.
///
/// # Safety
/// `s` must come from this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trip_through_the_c_abi() {
        let name = CString::new("D0").unwrap();
        let unit = CString::new("").unwrap();
        let ch = DsvExportChannel {
            index: 0,
            kind: 0,
            enabled: 1,
            name: name.as_ptr(),
            vdiv: 0,
            vfactor: 0,
            hw_offset: 0,
            map_max: 0.0,
            map_min: 0.0,
            map_unit: unit.as_ptr(),
        };
        let fmt = CString::new("csv").unwrap();
        let mut h = std::ptr::null_mut();
        assert_eq!(
            unsafe { dsv_export_new(fmt.as_ptr(), 0, 0, &ch, 1, &mut h) },
            DSV_OK
        );
        let t = CString::new("T").unwrap();
        unsafe { dsv_export_set_strings(h, t.as_ptr(), t.as_ptr()) };

        let mut p: DsvExportPacket = unsafe { std::mem::zeroed() };
        p.kind = DSV_EXPORT_META;
        p.has_samplerate = 1;
        p.samplerate = 1000;
        let mut out = std::ptr::null_mut();
        assert_eq!(unsafe { dsv_export_receive(h, &p, &mut out) }, DSV_OK);
        assert!(out.is_null());

        let data = [1u8, 0];
        p = unsafe { std::mem::zeroed() };
        p.kind = DSV_EXPORT_LOGIC;
        p.data = data.as_ptr();
        p.len = 2;
        p.unitsize = 1;
        assert_eq!(unsafe { dsv_export_receive(h, &p, &mut out) }, DSV_OK);
        let text = unsafe { CStr::from_ptr(out) }.to_str().unwrap().to_string();
        assert!(text.ends_with("Time(s), D0\n0,1\n0.001,0\n"), "{text}");
        unsafe { dsv_string_free(out) };
        unsafe { dsv_export_free(h) };

        let bad = CString::new("gnuplot").unwrap();
        assert_eq!(
            unsafe { dsv_export_new(bad.as_ptr(), 0, 0, &ch, 1, &mut h) },
            DSV_ERR_ARG
        );
    }
}
