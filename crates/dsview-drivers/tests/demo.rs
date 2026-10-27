//! The demo driver on its own: device list, the protocol pattern played back
//! completely, and inputs the C driver crashes on (DEVIATIONS.md, Phase 3).
//! The packet-by-packet comparison with the C driver is `drv-diff` in
//! dev/dsview-oracle.

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dsview_core::{Body, ConfValue, Mode, Packet, SrError, key};
use dsview_drivers::sys::{self, Clock};
use dsview_drivers::{Device, Step, demo};

/// The driver's process-wide state (clock, pattern list) is shared by the tests.
fn setup() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    struct Fast(AtomicI64);
    impl Clock for Fast {
        fn now_us(&self) -> i64 {
            self.0.fetch_add(10, Ordering::Relaxed)
        }
        fn sleep_us(&self, us: u64) {
            self.0.fetch_add(us as i64, Ordering::Relaxed);
        }
    }
    sys::set_clock(Arc::new(Fast(AtomicI64::new(0))));
    sys::set_user_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets"));
    g
}

fn logic_device() -> demo::Demo {
    demo::scan()
        .into_iter()
        .find(|d| d.info().model == "Demo Logic")
        .unwrap()
}

/// One source callback, as `dsv_drv_poll` runs it.
fn callback(d: &mut demo::Demo, abort: bool, out: &mut Vec<Packet>) -> bool {
    loop {
        match d.poll(abort, out) {
            Step::SleepThenPoll(_) => continue,
            Step::Remove => return false,
            Step::Continue | Step::SleepThenReturn(_) => return true,
        }
    }
}

/// `sr_session_run`: callbacks until the source is removed, at most `max`,
/// then one aborting callback, then `dev_acquisition_stop`.
fn capture(d: &mut demo::Demo, max: usize) -> Vec<Packet> {
    let mut out = Vec::new();
    d.start(&mut out).unwrap();
    let mut live = true;
    for _ in 0..max {
        live = callback(d, false, &mut out);
        if !live {
            break;
        }
    }
    if live {
        callback(d, true, &mut out);
    }
    d.stop(&mut out).unwrap();
    out
}

#[test]
fn devices_and_defaults() {
    let _g = setup();
    let devs = demo::scan();
    let names: Vec<_> = devs.iter().map(|d| d.info().model).collect();
    assert_eq!(names, ["Demo Oscilloscope", "Demo Logic"]);
    assert_eq!(devs[0].mode_list(), [Mode::Dso]);
    assert_eq!(devs[1].mode_list(), [Mode::Logic, Mode::Analog, Mode::Dso]);

    let mut d = logic_device();
    d.open().unwrap();
    assert_eq!(d.info().mode, Mode::Logic);
    assert_eq!(
        d.config_get(key::PATTERN_MODE, None).unwrap(),
        Some(ConfValue::Str("protocol".into()))
    );
    assert_eq!(d.channels().len(), 16);
    assert!(
        d.info()
            .path
            .unwrap()
            .ends_with("/demo/logic/protocol.demo")
    );
}

#[test]
fn protocol_pattern_plays_back_completely() {
    let _g = setup();
    let mut d = logic_device();
    d.open().unwrap();
    let total = d
        .config_get(key::LIMIT_SAMPLES, None)
        .unwrap()
        .unwrap()
        .get_u64();
    let packets = capture(&mut d, 1_000_000);
    let bytes: usize = packets
        .iter()
        .map(|p| match &p.body {
            Body::Logic { data, .. } => data.len(),
            _ => 0,
        })
        .sum();
    // 16 channels, whole 64-sample words
    assert_eq!(bytes as u64, total.div_ceil(64) * 8 * 16);
    assert_eq!(packets.last().unwrap().body, Body::End);
    assert_eq!(packets.iter().filter(|p| p.body == Body::End).count(), 1);
}

/// The LA_CROSS words of each of `n` channels, from a capture's packets.
fn channels_of(packets: &[Packet], n: usize) -> Vec<Vec<u64>> {
    let data: Vec<u8> = packets
        .iter()
        .flat_map(|p| match &p.body {
            Body::Logic { data, .. } => data.clone(),
            _ => Vec::new(),
        })
        .collect();
    let mut out = vec![Vec::new(); n];
    let (words, _) = data.as_chunks::<8>();
    for (k, w) in words.iter().enumerate() {
        out[k % n].push(u64::from_le_bytes(*w));
    }
    out
}

/// F21: a disabled channel takes its own data out of the playback; the
/// others keep theirs (C read `L-0` .. `L-<n-1>` for the n enabled ones,
/// so channel 4 showed channel 3's data).
#[test]
fn protocol_playback_skips_disabled_channels() {
    let _g = setup();
    let mut d = logic_device();
    d.open().unwrap();
    let all = channels_of(&capture(&mut d, 1_000_000), 16);
    assert!(all[3] != all[4], "channels 3 and 4 differ in protocol.demo");
    d.config_set(key::PROBE_EN, &ConfValue::Bool(false), Some(3))
        .unwrap();
    let some = channels_of(&capture(&mut d, 1_000_000), 15);
    let enabled: Vec<usize> = (0..16).filter(|&i| i != 3).collect();
    for (k, &ch) in enabled.iter().enumerate() {
        assert!(some[k] == all[ch], "channel {ch} plays its own data");
    }
}

#[test]
fn channel_keys_without_a_channel_are_rejected() {
    let _g = setup();
    let mut d = logic_device();
    d.open().unwrap();
    // C dereferences the NULL channel
    for k in [key::PROBE_EN, key::PROBE_VDIV, key::PROBE_MAP_MIN] {
        assert_eq!(d.config_set(k, &ConfValue::U64(1), None), Err(SrError::Arg));
    }
    // getters answer "nothing" or SR_ERR, as in C
    assert_eq!(d.config_get(key::PROBE_VDIV, None), Ok(None));
    assert_eq!(d.config_get(key::PROBE_MAP_MIN, None), Err(SrError::Err));
}

#[test]
fn analog_packets_longer_than_the_pattern_are_zero_padded() {
    let _g = setup();
    let mut d = logic_device();
    d.open().unwrap();
    d.config_set(key::DEVICE_MODE, &ConfValue::I16(Mode::Analog as i16), None)
        .unwrap();
    // C reads past its 1000-byte pattern buffer here and crashes (F18)
    d.config_set(key::LIMIT_SAMPLES, &ConfValue::U64(10_000), None)
        .unwrap();
    d.config_set(key::SAMPLERATE, &ConfValue::U64(2_000_000_000), None)
        .unwrap();
    let packets = capture(&mut d, 5);
    let data: Vec<_> = packets
        .iter()
        .filter_map(|p| match &p.body {
            Body::Analog { data, .. } => Some(data.len()),
            _ => None,
        })
        .collect();
    assert_eq!(data, [20_000_000; 6]);
    assert_eq!(packets.last().unwrap().body, Body::End);
}

/// F19: an analog capture under 2048 samples delivers data. C's buffer is
/// 103/2048 of the samples in whole 206-byte cycles, which is none there:
/// every callback failed and the session spun at 100% CPU with no data.
#[test]
fn short_analog_capture_delivers_data() {
    let _g = setup();
    let mut d = logic_device();
    d.open().unwrap();
    d.config_set(key::DEVICE_MODE, &ConfValue::I16(Mode::Analog as i16), None)
        .unwrap();
    d.config_set(key::LIMIT_SAMPLES, &ConfValue::U64(1000), None)
        .unwrap();
    let packets = capture(&mut d, 20);
    let bytes: usize = packets
        .iter()
        .map(|p| match &p.body {
            Body::Analog { data, .. } => data.len(),
            _ => 0,
        })
        .sum();
    assert!(
        bytes > 0,
        "no analog data: {:?}",
        packets.iter().map(|p| &p.body).collect::<Vec<_>>()
    );
}

#[test]
fn dso_runs_until_stopped_and_ends_twice_like_c() {
    let _g = setup();
    let mut d = demo::scan().remove(0);
    d.open().unwrap();
    let packets = capture(&mut d, 20);
    let ends = packets.iter().filter(|p| p.body == Body::End).count();
    let dso = packets
        .iter()
        .filter(|p| matches!(p.body, Body::Dso { .. }))
        .count();
    assert!(dso > 0);
    // the source callback and dev_acquisition_stop both send END in C
    assert_eq!(ends, 2);
}

#[test]
fn zero_vdiv_is_refused() {
    // found by the `demo` fuzz target: C stores V/div 0 and divides by it
    // when it scales the DSO data
    let _g = setup();
    let mut d = demo::scan().remove(0);
    d.open().unwrap();
    assert_eq!(
        d.config_set(key::PROBE_VDIV, &ConfValue::U64(0), Some(0)),
        Err(SrError::Arg)
    );
    assert_eq!(
        d.config_get(key::PROBE_VDIV, Some(0)).unwrap(),
        Some(ConfValue::U64(1000))
    );
    assert!(
        capture(&mut d, 5)
            .iter()
            .any(|p| matches!(p.body, Body::Dso { .. }))
    );
}
