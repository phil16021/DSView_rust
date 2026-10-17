//! Rust `DsoStore`/`AnalogStore` against the original C++ `DsoSnapshot` /
//! `AnalogSnapshot`: same frames in, same buffers, envelopes and answers out.
//!
//! The C++ classes leave their malloc'ed buffers uninitialised, so each
//! capture starts with a first payload without samples, after which the
//! oracle zero-fills them (the Rust stores allocate zeroed); the real data
//! follows as appended packets. `SigSession` clears the stores before every
//! capture, so the tests do too.

use dsview_oracle::store::{CAnalogStore, CDsoStore};
use dsview_store::analog::{AnalogChannel, AnalogStore};
use dsview_store::dso::{DsoChannel, DsoStore};
use proptest::prelude::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize]
    }
    /// Waveform-like bytes (a slow ramp with noise), sometimes pure noise.
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        let noise = self.chance(0.3);
        let (mut level, step) = (self.next() as u8, 1 + self.below(5) as u8);
        (0..n)
            .map(|_| {
                if noise {
                    self.next() as u8
                } else {
                    level = level
                        .wrapping_add(step)
                        .wrapping_add((self.below(3) as u8).wrapping_sub(1));
                    level
                }
            })
            .collect()
    }
}

fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

// ------------------------------------------------------------------ DSO

fn check_dso(c: &CDsoStore, r: &DsoStore, ctx: &str) {
    let cc = c.counts();
    assert_eq!(
        (
            cc.sample_count,
            cc.total_sample_count,
            cc.channel_num,
            cc.last_ended != 0,
            cc.memory_failed != 0
        ),
        (
            r.sample_count(),
            r.total_sample_count(),
            r.channel_num(),
            r.last_ended(),
            r.memory_failed()
        ),
        "dso counts {ctx}"
    );
    assert_eq!(
        (cc.is_file != 0, cc.instant != 0, cc.out_of_range != 0),
        (r.is_file(), r.is_instant(), r.data_out_off_range()),
        "dso flags {ctx}"
    );
    if r.sample_count() > 0 {
        assert_eq!(cc.block_num, r.get_block_num(), "dso block_num {ctx}");
        for b in 0..r.get_block_num() {
            assert_eq!(
                c.get_block_size(b),
                r.get_block_size(b),
                "dso block {b} {ctx}"
            );
        }
    }
    for sig in 0..4 {
        assert_eq!(
            c.has_data(sig),
            r.has_data(sig),
            "dso has_data({sig}) {ctx}"
        );
    }
    for (order, &idx) in r.channel_indexes().iter().enumerate() {
        let cb = c.buffer(order).expect("C buffer");
        let rb = r.get_samples(0, idx as i32).expect("Rust buffer");
        assert_eq!(cb, rb, "dso buffer of channel {idx} {ctx}");
        assert_eq!(
            c.get_max_min_value(order as i32),
            r.get_max_min_value(order as i32),
            "max/min {order} {ctx}"
        );
        if r.sample_count() > 0 {
            let start = r.sample_count() / 2;
            let p = c.get_samples(start, idx as i32);
            // SAFETY: inside the channel buffer.
            let cs = unsafe { std::slice::from_raw_parts(p, (r.sample_count() - start) as usize) };
            assert_eq!(
                cs,
                &r.get_samples(start, idx as i32).unwrap()[..cs.len()],
                "dso get_samples {ctx}"
            );
        }
    }
}

fn dso_case(seed: u64) {
    let mut rng = Rng(seed);
    let mut c = CDsoStore::new();
    let mut r = DsoStore::new();
    for capture in 0..1 + rng.below(3) {
        let channels: Vec<(u16, bool)> = (0..2).map(|i| (i, rng.chance(0.7))).collect();
        let is_file = rng.chance(0.3);
        let instant = rng.chance(0.4);
        let used = channels.iter().filter(|c| c.1 || is_file).count();
        let total = if rng.chance(0.1) {
            1_100_000 + rng.below(50_000)
        } else {
            1 + rng.below(20_000)
        };
        let (ref_max, ref_min) = (rng.below(256) as u32, rng.below(128) as u32);
        let rch: Vec<DsoChannel> = channels
            .iter()
            .map(|&(index, enabled)| DsoChannel { index, enabled })
            .collect();
        c.set_ref_range(ref_max, ref_min);
        r.set_ref_range(ref_max, ref_min);

        c.first_payload(&[], 0, total, &channels, instant, is_file);
        c.zero();
        r.first_payload(&[], 0, total, &rch, instant, is_file);
        let ctx = format!(
            "[seed {seed}, capture {capture}, channels {channels:?}, file {is_file}, instant {instant}, total {total}]"
        );
        check_dso(&c, &r, &ctx);
        for frame in 0..1 + rng.below(4) {
            let n = rng.below(total + total / 4 + 2) as usize;
            let data = rng.bytes(n * used.max(1));
            if rng.chance(0.3) {
                // the reference range exactly at the frame's extremes: the
                // out-of-range flag depends on the strict comparisons
                let mx = data.iter().copied().max().unwrap_or(0) as u32;
                let mn = data.iter().copied().min().unwrap_or(0) as u32;
                c.set_ref_range(mx, mn);
                r.set_ref_range(mx, mn);
            }
            c.append_payload(&data, n as i32);
            r.append_payload(&data, n as u64);
            check_dso(&c, &r, &format!("{ctx} frame {frame} of {n} samples"));
        }
        c.capture_ended();
        r.capture_ended();
        check_dso(&c, &r, &ctx);
        if rng.chance(0.3) {
            c.init();
            r.init();
            check_dso(&c, &r, &ctx);
        }
        c.clear();
        r.clear();
        check_dso(&c, &r, &ctx);
    }
}

// ------------------------------------------------------------------ analog

fn check_analog(c: &CAnalogStore, r: &AnalogStore, ctx: &str, rng: &mut Rng) {
    let cc = c.counts();
    assert_eq!(
        (
            cc.sample_count,
            cc.total_sample_count,
            cc.ring_sample_count,
            cc.channel_num,
            cc.unit_bytes
        ),
        (
            r.sample_count(),
            r.total_sample_count(),
            r.ring_sample_count(),
            r.channel_num(),
            r.unit_bytes()
        ),
        "analog counts {ctx}"
    );
    assert_eq!(
        (cc.last_ended != 0, cc.memory_failed != 0),
        (r.last_ended(), r.memory_failed()),
        "analog flags {ctx}"
    );
    assert_eq!(c.data(), r.data(), "analog sample ring {ctx}");
    if r.sample_count() > 0 {
        assert_eq!(cc.block_num, r.get_block_num(), "analog block_num {ctx}");
        for b in 0..r.get_block_num() {
            assert_eq!(
                c.get_block_size(b),
                r.get_block_size(b),
                "analog block {b} {ctx}"
            );
        }
    }
    for sig in 0..5 {
        assert_eq!(
            c.has_data(sig),
            r.has_data(sig),
            "analog has_data({sig}) {ctx}"
        );
        assert_eq!(
            c.has_enabled_channel(sig),
            r.has_enabled_channel(sig),
            "has_enabled_channel({sig}) {ctx}"
        );
        assert_eq!(c.ch_order(sig), r.ch_order(sig), "ch_order({sig}) {ctx}");
    }
    let probes = (r.channel_num() as usize).min(4);
    for probe in 0..probes {
        // C++ reads one entry past a ring when building the next level (F12);
        // without allocator slack to make that entry neutral, the levels
        // above are undefined in C++ and not compared
        let mut defined = true;
        for level in 0..10 {
            let (cl, cr, cn, cs) = c.envelope(probe, level);
            let (rl, rr, rn, rs) = r.envelope(probe, level);
            let rs: Vec<u8> = rs.iter().flat_map(|e| [e.min, e.max]).collect();
            assert_eq!(
                (cl, cr, cn),
                (rl, rr, rn),
                "envelope {probe}/{level} lengths {ctx}"
            );
            if defined {
                assert_eq!(cs, &rs[..], "envelope {probe}/{level} samples {ctx}");
            }
            if cn > 0 && c.envelope_slack(probe, level) < 2 {
                defined = false;
            }
        }
        for _ in 0..3 {
            let start = rng.below(r.total_sample_count() + 1);
            let count = rng.below(r.total_sample_count() + 1) as i64;
            let min_length = rng.pick(&[0.3f32, 1.0, 7.9, 16.0, 17.0, 300.0, 4096.0, 1e6]);
            let cs = c.get_envelope_section(start, count, min_length, probe as i32);
            let rs = r.get_envelope_section(start, count, min_length, probe as i32);
            let what =
                format!("get_envelope_section({start}, {count}, {min_length}, {probe}) {ctx}");
            assert_eq!(
                (
                    cs.start,
                    cs.scale,
                    cs.length,
                    cs.samples_num,
                    cs.samples.is_null()
                ),
                (
                    rs.start,
                    rs.scale,
                    rs.length,
                    rs.samples_num,
                    rs.samples.is_null()
                ),
                "{what}"
            );
        }
    }
}

fn analog_case(seed: u64) {
    let mut rng = Rng(seed);
    let mut c = CAnalogStore::new();
    let mut r = AnalogStore::new();
    for capture in 0..1 + rng.below(3) {
        let n_ch = 1 + rng.below(2) as u16;
        let channels: Vec<(u16, bool)> = (0..n_ch).map(|i| (i, rng.chance(0.7))).collect();
        let rch: Vec<AnalogChannel> = channels
            .iter()
            .map(|&(index, enabled)| AnalogChannel { index, enabled })
            .collect();
        let unit_bits = rng.pick(&[8u8, 8, 12, 16]);
        let pitch = rng.pick(&[0u16, 1, 1, 2, 3, 7]);
        let totals = [
            1 + rng.below(40),
            16 * (1 + rng.below(300)),
            1 + rng.below(70_000),
        ];
        let total = rng.pick(&totals);
        let bps = n_ch as usize * unit_bits.div_ceil(8) as usize;

        c.first_payload(&[], 0, unit_bits, pitch, total, &channels);
        c.zero();
        r.first_payload(&[], 0, unit_bits, pitch, total, &rch);
        let ctx = format!(
            "[seed {seed}, capture {capture}, {n_ch} ch, {unit_bits} bits, pitch {pitch}, total {total}]"
        );
        check_analog(&c, &r, &ctx, &mut rng);
        for k in 0..1 + rng.below(12) {
            let n = match rng.below(4) {
                0 => rng.below(20),
                1 => rng.below(total * 3 + 1),
                _ => rng.below(total / 3 + 2),
            } as usize;
            let data = rng.bytes(n * bps);
            c.append_payload(&data, n as i32, pitch);
            r.append_payload(&data, n as u64, pitch);
            check_analog(
                &c,
                &r,
                &format!("{ctx} packet {k} of {n} samples"),
                &mut rng,
            );
        }
        c.capture_ended();
        r.capture_ended();
        if rng.chance(0.3) {
            c.init();
            r.init();
        }
        check_analog(&c, &r, &ctx, &mut rng);
        c.clear();
        r.clear();
        check_analog(&c, &r, &ctx, &mut rng);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(300), failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn dso_captures_match(seed in any::<u64>()) {
        dso_case(seed);
    }

    #[test]
    fn analog_captures_match(seed in any::<u64>()) {
        analog_case(seed);
    }
}
