//! dsview-formats' CSV/VCD exporters and C-style number formatting must be
//! byte-identical to libsigrok4DSL's output modules and glibc printf.

use std::ffi::{CStr, CString, c_char, c_int};

use dsview_formats::cfmt::{fmt_f, fmt_g};
use dsview_formats::export::{ChannelKind, ExportChannel, ExportPacket, Exporter, Format};
use dsview_oracle::ffi;
use proptest::prelude::*;

unsafe extern "C" {
    fn snprintf(s: *mut c_char, n: usize, format: *const c_char, ...) -> c_int;
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

fn c_printf_f64(fmt: &str, v: f64) -> String {
    let fmt = CString::new(fmt).unwrap();
    let mut buf = vec![0 as c_char; 512];
    unsafe { snprintf(buf.as_mut_ptr(), buf.len(), fmt.as_ptr(), v) };
    unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

#[derive(Debug, Clone)]
struct Case {
    format: Format,
    kind: ChannelKind,
    start_index: u64,
    channels: Vec<ExportChannel>,
    samplerate: u64,
    limit: u64,
    refs: Option<(u32, u32)>,
    /// (data, unitsize/num_samples, export_original)
    packets: Vec<(Vec<u8>, usize, bool)>,
}

fn kind_code(k: ChannelKind) -> c_int {
    match k {
        ChannelKind::Logic => 1,
        ChannelKind::Dso => 2,
        ChannelKind::Analog => 3,
        ChannelKind::Other => 0,
    }
}

fn run_c(case: &Case) -> (c_int, String) {
    let names: Vec<CString> = case
        .channels
        .iter()
        .map(|c| CString::new(c.name.clone()).unwrap())
        .collect();
    let units: Vec<CString> = case
        .channels
        .iter()
        .map(|c| CString::new(c.map_unit.clone()).unwrap())
        .collect();
    let chans: Vec<ffi::OracleExportChannel> = case
        .channels
        .iter()
        .enumerate()
        .map(|(i, c)| ffi::OracleExportChannel {
            index: c_int::from(c.index),
            kind: kind_code(c.kind),
            enabled: c_int::from(c.enabled),
            name: names[i].as_ptr(),
            vdiv: c.vdiv,
            vfactor: c.vfactor,
            hw_offset: c.hw_offset,
            map_max: c.map_max,
            map_min: c.map_min,
            map_unit: units[i].as_ptr(),
        })
        .collect();
    let mut pkts = vec![ffi::OracleExportPacket {
        kind: ffi::EXP_META,
        samplerate: case.samplerate,
        limit_samples: case.limit,
        has_ref: c_int::from(case.refs.is_some()),
        ref_min: case.refs.map_or(0, |r| r.0),
        ref_max: case.refs.map_or(0, |r| r.1),
        data: std::ptr::null(),
        len: 0,
        unitsize: 0,
        num_samples: 0,
        export_original: 0,
    }];
    for (data, n, orig) in &case.packets {
        let (kind, unitsize, num) = match case.kind {
            ChannelKind::Logic => (ffi::EXP_LOGIC, *n as u16, 0),
            ChannelKind::Dso => (ffi::EXP_DSO, 0, *n as c_int),
            _ => (ffi::EXP_ANALOG, 0, *n as c_int),
        };
        pkts.push(ffi::OracleExportPacket {
            kind,
            samplerate: 0,
            limit_samples: 0,
            has_ref: 0,
            ref_min: 0,
            ref_max: 0,
            data: data.as_ptr(),
            len: data.len() as u64,
            unitsize,
            num_samples: num,
            export_original: c_int::from(*orig),
        });
    }
    if case.format == Format::Vcd {
        pkts.push(ffi::OracleExportPacket {
            kind: ffi::EXP_END,
            ..pkts[0]
        });
    }
    let module = CString::new(if case.format == Format::Csv {
        "csv"
    } else {
        "vcd"
    })
    .unwrap();
    let time = CString::new("2026-10-17 20:20:26").unwrap();
    let mut out: *mut c_char = std::ptr::null_mut();
    let rc = unsafe {
        ffi::oracle_export_run(
            module.as_ptr(),
            kind_code(case.kind),
            case.start_index,
            time.as_ptr(),
            chans.as_ptr(),
            chans.len() as c_int,
            pkts.as_ptr(),
            pkts.len() as c_int,
            &mut out,
        )
    };
    let text = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::oracle_free(out.cast()) };
    (rc, mask_date(&text))
}

fn run_rust(case: &Case) -> String {
    let mut e = Exporter::new(
        case.format,
        case.kind,
        case.channels.clone(),
        case.start_index,
    )
    .unwrap();
    e.time_string = "2026-10-17 20:20:26".into();
    e.date_string = "X".into();
    let mut out = String::new();
    let meta = ExportPacket::Meta {
        samplerate: Some(case.samplerate),
        limit_samples: Some(case.limit),
        ref_min: case.refs.map(|r| r.0),
        ref_max: case.refs.map(|r| r.1),
    };
    out += &e.receive(&meta).unwrap().unwrap_or_default();
    for (data, n, orig) in &case.packets {
        let p = match case.kind {
            ChannelKind::Logic => ExportPacket::Logic {
                data,
                unitsize: *n,
                export_original: *orig,
            },
            ChannelKind::Dso => ExportPacket::Dso {
                data,
                num_samples: *n,
            },
            _ => ExportPacket::Analog {
                data,
                num_samples: *n,
            },
        };
        out += &e.receive(&p).unwrap().unwrap_or_default();
    }
    if case.format == Format::Vcd {
        out += &e.receive(&ExportPacket::End).unwrap().unwrap_or_default();
    }
    mask_date(&out)
}

/// VCD's $date is the wall clock; compare everything else.
fn mask_date(s: &str) -> String {
    match s.strip_prefix("$date ") {
        Some(rest) => format!("$date X{}", &rest[rest.find(" $end").unwrap_or(0)..]),
        None => s.to_string(),
    }
}

fn channel(index: u16, kind: ChannelKind, enabled: bool) -> ExportChannel {
    ExportChannel {
        index,
        kind,
        enabled,
        name: format!("CH{index}"),
        vdiv: 0,
        vfactor: 0,
        hw_offset: 0,
        map_max: 0.0,
        map_min: 0.0,
        map_unit: String::new(),
    }
}

/// Logic samples as runs, so the "skip unchanged samples" path is exercised.
fn logic_case(format: Format) -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(any::<bool>(), 1..=16),
        prop::sample::select(vec![
            1u64,
            10,
            1000,
            1_000_000,
            25_000_000,
            100_000_000,
            3_333_333,
        ]),
        0u64..5000,
        prop::collection::vec(
            (
                prop::collection::vec((any::<u16>(), 1usize..20), 1..15),
                any::<bool>(),
            ),
            1..4,
        ),
    )
        .prop_map(move |(enabled, samplerate, start_index, raw_packets)| {
            let channels: Vec<ExportChannel> = enabled
                .iter()
                .enumerate()
                .map(|(i, &e)| channel(i as u16, ChannelKind::Logic, e))
                .collect();
            let n = enabled.iter().filter(|&&e| e).count().max(1);
            let unitsize = n.div_ceil(8);
            let packets = raw_packets
                .into_iter()
                .map(|(runs, orig)| {
                    let mut data = Vec::new();
                    for (value, len) in runs {
                        for _ in 0..len {
                            let v = (u64::from(value) & ((1u64 << n) - 1)).to_le_bytes();
                            data.extend_from_slice(&v[..unitsize]);
                        }
                    }
                    (data, unitsize, orig)
                })
                .collect();
            let limit = 123_456;
            Case {
                format,
                kind: ChannelKind::Logic,
                start_index,
                channels,
                samplerate,
                limit,
                refs: None,
                packets,
            }
        })
}

fn dso_case() -> impl Strategy<Value = Case> {
    (
        prop::sample::select(vec![(true, true), (true, false), (false, true)]),
        prop::sample::select(vec![10u64, 20, 50, 100, 200, 500, 1000, 2000, 5000]),
        prop::sample::select(vec![1u64, 10, 100]),
        (any::<u8>(), any::<u8>()),
        (0u32..128, 128u32..=255),
        prop::collection::vec(any::<u8>(), 0..120),
    )
        .prop_map(|((e0, e1), vdiv, vfactor, (off0, off1), refs, raw)| {
            let mut channels = vec![
                channel(0, ChannelKind::Dso, e0),
                channel(1, ChannelKind::Dso, e1),
            ];
            for (c, off) in channels.iter_mut().zip([off0, off1]) {
                c.vdiv = vdiv;
                c.vfactor = vfactor;
                c.hw_offset = u16::from(off);
            }
            let n = usize::from(e0) + usize::from(e1);
            let samples = raw.len() / n;
            let data = raw[..samples * n].to_vec();
            Case {
                format: Format::Csv,
                kind: ChannelKind::Dso,
                start_index: 0,
                channels,
                samplerate: 100_000_000,
                limit: samples as u64,
                refs: Some(refs),
                packets: vec![(data, samples, false)],
            }
        })
}

fn analog_case() -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(
            (any::<bool>(), any::<u8>(), -10.0f64..10.0, 0.0f64..20.0),
            1..=8,
        ),
        (0u32..128, 128u32..=255),
        1usize..40,
    )
        .prop_flat_map(|(chs, refs, samples)| {
            let n = chs.len();
            (
                Just(chs),
                Just(refs),
                prop::collection::vec(any::<u8>(), samples * n..=samples * n),
                Just(samples),
            )
        })
        .prop_map(|(chs, refs, data, samples)| {
            let channels = chs
                .iter()
                .enumerate()
                .map(|(i, &(en, off, mmin, span))| {
                    let mut c = channel(i as u16, ChannelKind::Analog, en);
                    c.hw_offset = u16::from(off);
                    c.map_min = mmin;
                    c.map_max = mmin + span;
                    c.map_unit = "V".into();
                    c
                })
                .collect();
            Case {
                format: Format::Csv,
                kind: ChannelKind::Analog,
                start_index: 0,
                channels,
                samplerate: 10,
                limit: samples as u64,
                refs: Some(refs),
                packets: vec![(data, samples, false)],
            }
        })
}

fn check(case: &Case) -> Result<(), TestCaseError> {
    let (rc, c) = run_c(case);
    prop_assume!(rc == 0);
    let r = run_rust(case);
    prop_assert_eq!(r, c);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn csv_logic_matches_c(case in logic_case(Format::Csv)) { check(&case)?; }

    #[test]
    fn vcd_logic_matches_c(case in logic_case(Format::Vcd)) { check(&case)?; }

    #[test]
    fn csv_dso_matches_c(case in dso_case()) { check(&case)?; }

    #[test]
    fn csv_analog_matches_c(case in analog_case()) { check(&case)?; }

    #[test]
    fn g15_matches_printf(v in prop_oneof![any::<f64>(), -1e6f64..1e6, 0.0f64..1e-3, (0u64..10_000_000).prop_map(|i| i as f64 / 3_333_333.0)]) {
        prop_assert_eq!(fmt_g(v, 15), c_printf_f64("%0.15g", v));
    }

    #[test]
    fn f5_and_f0_match_printf(v in prop_oneof![any::<f64>(), -1e9f64..1e9, (0i64..1_000_000).prop_map(|i| i as f64 / 8.0)]) {
        prop_assert_eq!(fmt_f(v, 5), c_printf_f64("%0.5f", v));
        prop_assert_eq!(fmt_f(v, 0), c_printf_f64("%.0f", v));
    }
}

/// Finding F5: with a decimal-comma locale the C module writes DSO values
/// as "1,23456" inside a comma-separated file. (Qt applications set the
/// process locale from the environment; the Rust exporter always writes '.'.)
#[test]
fn c_csv_uses_locale_decimal_comma() {
    const LC_NUMERIC: c_int = 1;
    let de = CString::new("de_AT.utf8").unwrap();
    if unsafe { setlocale(LC_NUMERIC, de.as_ptr()) }.is_null() {
        eprintln!("de_AT.utf8 locale not installed; skipping");
        return;
    }
    let mut ch = channel(0, ChannelKind::Dso, true);
    ch.vdiv = 1000;
    ch.vfactor = 1;
    ch.hw_offset = 128;
    let case = Case {
        format: Format::Csv,
        kind: ChannelKind::Dso,
        start_index: 0,
        channels: vec![ch],
        samplerate: 1000,
        limit: 1,
        refs: Some((0, 255)),
        packets: vec![(vec![100], 1, false)],
    };
    let (_, c) = run_c(&case);
    let c_locale = CString::new("C").unwrap();
    unsafe { setlocale(LC_NUMERIC, c_locale.as_ptr()) };
    let last = c.lines().last().unwrap().to_string();
    assert!(
        last.contains(','),
        "expected a decimal comma from the C module, got {last:?}"
    );
    assert!(!run_rust(&case).lines().last().unwrap().contains(','));
}

/// Finding F6: an empty logic packet crashes the C CSV/VCD modules
/// (`length - unitsize` underflows). Run with --ignored to reproduce.
#[test]
#[ignore = "crashes the process (C bug); documents finding F6"]
fn c_crashes_on_empty_logic_packet() {
    let case = Case {
        format: Format::Csv,
        kind: ChannelKind::Logic,
        start_index: 0,
        channels: vec![channel(0, ChannelKind::Logic, true)],
        samplerate: 1000,
        limit: 0,
        refs: None,
        packets: vec![(vec![], 1, false)],
    };
    run_c(&case);
}
