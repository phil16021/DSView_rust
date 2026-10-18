//! The DSLogic trigger masks: dsview_drivers::dsl::trigger_bits against
//! libsigrok4DSL's ds_trigger_get_mask0 … get_edge1, on random triggers set
//! up through the C `ds_trigger_*` API (the GUI's way). Also checks that the
//! C `struct ds_trigger` reads as dsview_core::Trigger.

use std::ffi::{c_char, c_int, c_uchar};

use dsview_core::Trigger;
use dsview_drivers::dsl::{TrigField, trigger_bits};

unsafe extern "C" {
    static mut trigger: *mut Trigger;
    fn ds_trigger_init() -> c_int;
    fn ds_trigger_stage_set_value(
        stage: u16,
        probes: u16,
        t0: *const c_char,
        t1: *const c_char,
    ) -> c_int;
    fn ds_trigger_stage_set_logic(stage: u16, probes: u16, logic: c_uchar) -> c_int;
    fn ds_trigger_stage_set_inv(stage: u16, probes: u16, inv0: c_uchar, inv1: c_uchar) -> c_int;
    fn ds_trigger_stage_set_count(stage: u16, probes: u16, c0: u32, c1: u32) -> c_int;
    fn ds_trigger_probe_set(probe: u16, t0: c_uchar, t1: c_uchar) -> c_int;
    fn ds_trigger_set_stage(stages: u16) -> c_int;
    fn ds_trigger_set_pos(pos: u16) -> c_int;
    fn ds_trigger_set_en(en: u16) -> c_int;
    fn ds_trigger_set_mode(mode: u16) -> c_int;
    fn ds_trigger_get_mask0(stage: u16, msc: u16, lsc: u16, q: c_int, h: c_int) -> u16;
    fn ds_trigger_get_mask1(stage: u16, msc: u16, lsc: u16, q: c_int, h: c_int) -> u16;
    fn ds_trigger_get_value0(stage: u16, msc: u16, lsc: u16, q: c_int, h: c_int) -> u16;
    fn ds_trigger_get_value1(stage: u16, msc: u16, lsc: u16, q: c_int, h: c_int) -> u16;
    fn ds_trigger_get_edge0(stage: u16, msc: u16, lsc: u16, q: c_int, h: c_int) -> u16;
    fn ds_trigger_get_edge1(stage: u16, msc: u16, lsc: u16, q: c_int, h: c_int) -> u16;
}

// the oracle links the C libraries and the Rust core
use dsview_oracle as _;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const SYMBOLS: &[u8] = b"X01RFC";

#[test]
fn trigger_masks_match_c() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    let mut compared = 0u64;
    for round in 0..2000 {
        unsafe { ds_trigger_init() };
        // advanced stages: strings of `probes` symbols separated by spaces, as
        // the GUI passes them (ds_trigger_stage_set_value reads every other char)
        for stage in 0..16u16 {
            if r.below(3) == 0 {
                continue;
            }
            let probes = 1 + r.below(32) as u16;
            let mk = |r: &mut Rng| -> Vec<c_char> {
                let mut v = Vec::new();
                for _ in 0..probes {
                    v.push(SYMBOLS[r.below(SYMBOLS.len() as u64) as usize] as c_char);
                    v.push(b' ' as c_char);
                }
                v.push(0);
                v
            };
            let (t0, t1) = (mk(&mut r), mk(&mut r));
            unsafe {
                ds_trigger_stage_set_value(stage, probes, t0.as_ptr(), t1.as_ptr());
                ds_trigger_stage_set_logic(stage, probes, r.below(4) as u8);
                ds_trigger_stage_set_inv(stage, probes, r.below(2) as u8, r.below(2) as u8);
                ds_trigger_stage_set_count(stage, probes, r.next() as u32, r.next() as u32);
            }
        }
        // the simple trigger (stage 16)
        for p in 0..32u16 {
            if r.below(4) == 0 {
                let c = |r: &mut Rng| SYMBOLS[r.below(SYMBOLS.len() as u64) as usize];
                unsafe { ds_trigger_probe_set(p, c(&mut r), c(&mut r)) };
            }
        }
        unsafe {
            ds_trigger_set_stage(r.below(17) as u16);
            ds_trigger_set_pos(r.below(101) as u16);
            ds_trigger_set_en(r.below(2) as u16);
            ds_trigger_set_mode(r.below(3) as u16);
        }
        let t: Trigger = unsafe { *trigger };

        for stage in 0..=16usize {
            for (msc, lsc) in [(15, 0), (31, 16)] {
                for (q, h) in [(false, false), (true, false), (false, true), (true, true)] {
                    let args = (stage as u16, msc as u16, lsc as u16, q as c_int, h as c_int);
                    let c = unsafe {
                        [
                            ds_trigger_get_mask0(args.0, args.1, args.2, args.3, args.4),
                            ds_trigger_get_mask1(args.0, args.1, args.2, args.3, args.4),
                            ds_trigger_get_value0(args.0, args.1, args.2, args.3, args.4),
                            ds_trigger_get_value1(args.0, args.1, args.2, args.3, args.4),
                            ds_trigger_get_edge0(args.0, args.1, args.2, args.3, args.4),
                            ds_trigger_get_edge1(args.0, args.1, args.2, args.3, args.4),
                        ]
                    };
                    let fields = [
                        TrigField::Mask0,
                        TrigField::Mask1,
                        TrigField::Value0,
                        TrigField::Value1,
                        TrigField::Edge0,
                        TrigField::Edge1,
                    ];
                    for (i, f) in fields.into_iter().enumerate() {
                        let rust = trigger_bits(&t, f, stage, msc, lsc, q, h);
                        assert_eq!(
                            rust, c[i],
                            "round {round} stage {stage} probes {lsc}-{msc} q={q} h={h} field {i}"
                        );
                        compared += 1;
                    }
                }
            }
        }
    }
    assert!(compared > 1_000_000);
}
