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

/// C's `trigger` is one global: the tests take turns.
static C_TRIGGER: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn trigger_masks_match_c() {
    let _g = C_TRIGGER.lock().unwrap_or_else(|e| e.into_inner());
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

/// The trigger dock: `dsview_session::trigger::AdvTrigger::commit` against
/// Qt's `TriggerDock::commit_trigger` call sequence on the C setters, for
/// random dock states (stage and serial, 16 and 32 channels).
#[test]
fn dock_commit_matches_c() {
    let _g = C_TRIGGER.lock().unwrap_or_else(|e| e.into_inner());
    use dsview_session::trigger::{ALL_X, AdvTrigger, normalise};
    use std::ffi::CString;
    let mut r = Rng(0x0dd_b1a5_e5ee_d123);
    let text = |r: &mut Rng, sym: &[u8]| -> String {
        let s: String = (0..16)
            .map(|_| sym[r.below(sym.len() as u64) as usize] as char)
            .collect();
        normalise(&s, "X01RFC")
    };
    for round in 0..3000 {
        let ch_num = if r.below(4) == 0 { 32usize } else { 16 };
        let mut a = AdvTrigger {
            advanced: r.below(5) != 0,
            stages: r.below(16) as usize,
            tab: r.below(2) as usize,
            ..AdvTrigger::default()
        };
        for s in &mut a.stage {
            s.value0 = text(&mut r, SYMBOLS);
            s.value1 = text(&mut r, SYMBOLS);
            s.value0_ext32 = text(&mut r, SYMBOLS);
            s.value1_ext32 = text(&mut r, SYMBOLS);
            s.inv0 = r.below(2) as u8;
            s.inv1 = r.below(2) as u8;
            s.logic = r.below(2) as u8;
            s.contiguous = r.below(2) == 1;
            s.count = 1 + r.below(100_000) as u32;
        }
        let sr = &mut a.serial;
        sr.start = text(&mut r, SYMBOLS);
        sr.stop = text(&mut r, SYMBOLS);
        sr.clock = text(&mut r, SYMBOLS);
        sr.start_ext32 = text(&mut r, SYMBOLS);
        sr.stop_ext32 = text(&mut r, SYMBOLS);
        sr.clock_ext32 = text(&mut r, SYMBOLS);
        sr.channel = r.below(ch_num as u64) as usize;
        sr.data = text(&mut r, b"X01");
        sr.bits = r.below(16) as usize;
        let pos = 1 + r.below(90) as u16;

        // Rust
        let mut rust = Trigger::default();
        let adv = a.commit(&mut rust, pos, ch_num);

        // C, as commit_trigger() calls it
        let cs = |s: String| CString::new(s).unwrap();
        let join = |hi: &str, lo: &str| {
            if ch_num == 32 {
                format!("{hi} {lo}")
            } else {
                lo.to_owned()
            }
        };
        unsafe {
            ds_trigger_init();
            ds_trigger_set_pos(pos);
            if !a.advanced {
                ds_trigger_set_mode(dsview_core::SIMPLE_TRIGGER);
            } else {
                ds_trigger_set_en(1);
                ds_trigger_set_mode(if a.tab == 0 {
                    dsview_core::ADV_TRIGGER
                } else {
                    dsview_core::SERIAL_TRIGGER
                });
                ds_trigger_set_stage(a.stages as u16);
                let n = a.stages + 1;
                if a.tab == 0 {
                    for i in 0..n {
                        let s = &a.stage[i];
                        let (v0, v1) = (
                            cs(join(&s.value0_ext32, &s.value0)),
                            cs(join(&s.value1_ext32, &s.value1)),
                        );
                        ds_trigger_stage_set_value(
                            i as u16,
                            ch_num as u16,
                            v0.as_ptr(),
                            v1.as_ptr(),
                        );
                    }
                } else {
                    let sr = &a.serial;
                    let s1 = &a.stage[1];
                    let (st, sp) = (
                        cs(join(&sr.start_ext32, &sr.start)),
                        cs(join(&sr.stop_ext32, &sr.stop)),
                    );
                    let (ed, cp) = (
                        cs(join(&sr.clock_ext32, &sr.clock)),
                        cs(join(&s1.value1_ext32, &s1.value1)),
                    );
                    ds_trigger_stage_set_value(0, ch_num as u16, st.as_ptr(), sp.as_ptr());
                    ds_trigger_stage_set_value(1, ch_num as u16, ed.as_ptr(), cp.as_ptr());
                    let (mut ch, mut ch32) = (ALL_X.to_owned(), ALL_X.to_owned());
                    let d = sr.channel;
                    if ch_num == 32 && d >= 16 {
                        ch32.replace_range(30 - 2 * (d - 16)..31 - 2 * (d - 16), "0");
                    } else {
                        ch.replace_range(30 - 2 * d..31 - 2 * d, "0");
                    }
                    let (ch, ch32) = (cs(ch), cs(ch32));
                    ds_trigger_stage_set_value(2, 16, ch.as_ptr(), ch32.as_ptr());
                    let (dv, v13) = (cs(sr.data.clone()), cs(a.stage[3].value1.clone()));
                    ds_trigger_stage_set_value(3, 16, dv.as_ptr(), v13.as_ptr());
                }
                for i in 0..n {
                    let s = &a.stage[i];
                    ds_trigger_stage_set_logic(i as u16, 16, ((s.contiguous as u8) << 1) + s.logic);
                }
                for i in 0..n {
                    let s = &a.stage[i];
                    ds_trigger_stage_set_inv(i as u16, 16, s.inv0, s.inv1);
                }
                if a.tab == 0 {
                    for i in 0..n {
                        ds_trigger_stage_set_count(i as u16, 16, a.stage[i].count, 0);
                    }
                } else {
                    ds_trigger_stage_set_count(1, 16, 1, 0);
                    ds_trigger_stage_set_count(3, 16, a.serial.bits as u32, 0);
                }
            }
        }
        let c = unsafe { *trigger };
        assert_eq!(adv, a.advanced, "round {round}");
        assert_eq!(rust, c, "round {round}: {a:?}");
    }
}
