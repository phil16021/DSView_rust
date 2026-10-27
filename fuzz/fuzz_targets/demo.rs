#![no_main]
//! The demo driver under any sequence of settings, then a capture.

use dsview_core::{ConfValue, key};
use dsview_drivers::{Device, Step};
use libfuzzer_sys::fuzz_target;

const KEYS: &[i32] = &[
    key::SAMPLERATE,
    key::LIMIT_SAMPLES,
    key::PATTERN_MODE,
    key::DEVICE_MODE,
    key::PROBE_EN,
    key::PROBE_VDIV,
    key::PROBE_OFFSET,
    key::PROBE_COUPLING,
    key::TIMEBASE,
    key::INSTANT,
    key::TRIGGER_SOURCE,
    key::TRIGGER_SLOPE,
    key::HORIZ_TRIGGERPOS,
];

fuzz_target!(|data: &[u8]| {
    let mut demos = dsview_drivers::demo::scan();
    let Some((&pick, ops)) = data.split_first() else {
        return;
    };
    let n = demos.len();
    let d = &mut demos[pick as usize % n];
    if d.open().is_err() {
        return;
    }
    for op in ops.chunks(10).take(32) {
        let [k, ch, kind, rest @ ..] = op else { break };
        let key = KEYS[*k as usize % KEYS.len()];
        let mut b = [0u8; 8];
        b[..rest.len().min(8)].copy_from_slice(&rest[..rest.len().min(8)]);
        let x = u64::from_le_bytes(b);
        let v = match kind % 6 {
            0 => ConfValue::Bool(x & 1 != 0),
            1 => ConfValue::I16(x as i16),
            2 => ConfValue::U64(x % 1_000_000_000_000),
            3 => ConfValue::F64(f64::from_bits(x)),
            4 => ConfValue::Byte(x as u8),
            _ => ConfValue::Str(format!("{x:x}")),
        };
        let ch = (*ch < 8).then_some(*ch as usize);
        let _ = d.config_set(key, &v, ch);
    }
    let mut out = Vec::new();
    if d.start(&mut out).is_ok() {
        for _ in 0..50 {
            if d.poll(false, &mut out) == Step::Remove {
                break;
            }
            out.clear();
        }
        let _ = d.poll(true, &mut out);
        let _ = d.stop(&mut out);
    }
    let _ = d.close();
});
