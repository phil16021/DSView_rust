//! Rust `LogicStore` against the original C++ `pv::data::LogicSnapshot`:
//! the same packet stream goes into both, then every query DSView makes
//! (counts, samples, decoder runs, block access for saving, display edges,
//! edge search, pattern search) must give the same answer — during the
//! capture and after it ended.
//!
//! PROPTEST_CASES scales the random scenarios (Phase 1b exit: 10^4).

use dsview_oracle::store::CLogicStore;
use dsview_store::logic::{LEAF_BLOCK_SAMPLES, LogicStore, Pattern};
use proptest::prelude::*;

/// SplitMix64: bulk data and query parameters from one proptest seed.
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
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize]
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
}

#[derive(Clone, Debug)]
enum Signal {
    Const(bool),
    /// A word gets random bits with this probability, else holds its level.
    Sparse(f64),
    Clock(u64),
    Random,
}

impl Signal {
    fn word(&self, group: u64, level: &mut bool, rng: &mut Rng) -> u64 {
        match *self {
            Signal::Const(b) => {
                if b {
                    !0
                } else {
                    0
                }
            }
            Signal::Sparse(p) => {
                if rng.chance(p) {
                    let w = rng.next() >> rng.below(64);
                    let w = if *level { !w } else { w };
                    *level = w >> 63 != 0;
                    w
                } else if *level {
                    !0
                } else {
                    0
                }
            }
            Signal::Clock(period) => {
                let s0 = group * 64;
                if (s0 / period) == ((s0 + 63) / period) {
                    if (s0 / period) % 2 == 1 { !0 } else { 0 }
                } else {
                    (0..64).fold(0u64, |w, i| w | ((((s0 + i) / period) % 2) << i))
                }
            }
            Signal::Random => rng.next(),
        }
    }
}

fn signal(rng: &mut Rng) -> Signal {
    match rng.below(10) {
        0 | 1 => Signal::Const(rng.chance(0.5)),
        2..=5 => Signal::Sparse(rng.pick(&[0.0005, 0.01, 0.2, 0.9])),
        6..=8 => {
            let max = rng.pick(&[3, 70, 5000, 300_000]);
            Signal::Clock(1 + rng.below(max))
        }
        _ => Signal::Random,
    }
}

/// LA_CROSS stream: per 64-sample group one u64 per channel.
fn cross_stream(signals: &[Signal], groups: u64, rng: &mut Rng) -> Vec<u8> {
    let mut levels = vec![false; signals.len()];
    let mut out = Vec::with_capacity((groups as usize) * signals.len() * 8);
    for g in 0..groups {
        for (s, l) in signals.iter().zip(levels.iter_mut()) {
            out.extend_from_slice(&s.word(g, l, rng).to_le_bytes());
        }
    }
    out
}

/// Packets of whole 64-sample groups: what every driver and the .dsl loader
/// send. The C++ store mis-assembles other packet sizes and then reads and
/// writes out of bounds (DEVIATIONS.md F9); the Rust store is checked on
/// those alone (crates/dsview-store/tests/robustness.rs).
fn split(len: usize, n: usize, max: usize, rng: &mut Rng) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    let mut at = 0;
    let unit = 8 * n;
    while at < len {
        let sz = (unit * (1 + rng.below((max / unit).max(1) as u64) as usize)).min(len - at);
        v.push((at, at + sz));
        at += sz;
    }
    v
}

/// A decoder's pinned leaf: C lbp, Rust lbp, and the leaf position.
type Pin = (*mut std::ffi::c_void, *const u8, Option<(u64, u64)>);

struct Pair {
    c: CLogicStore,
    r: LogicStore,
    chans: Vec<u16>,
    /// Decoder pins per channel, as DecoderStack keeps them.
    pins: Vec<Pin>,
    ctx: String,
}

impl Pair {
    fn new(chans: &[u16]) -> Pair {
        Pair {
            c: CLogicStore::new(),
            r: LogicStore::new(),
            chans: chans.to_vec(),
            pins: vec![(std::ptr::null_mut(), std::ptr::null(), None); chans.len()],
            ctx: String::new(),
        }
    }

    fn check_counts(&self) {
        let c = self.c.counts();
        let r = self.r.counts();
        assert_eq!(
            (
                c.sample_count,
                c.ring_sample_count,
                c.total_sample_count,
                c.loop_offset,
                c.ring_start,
                c.ring_end,
                c.block_num,
                c.last_ended != 0,
                c.channel_num
            ),
            (
                r.sample_count,
                r.ring_sample_count,
                r.total_sample_count,
                r.loop_offset,
                r.ring_start,
                r.ring_end,
                r.block_num,
                r.last_ended,
                r.channel_num
            ),
            "counts differ {}",
            self.ctx
        );
    }

    /// Internal state: fractions, write position, mip-map progress, decoder
    /// pins, and every root node's bits and leaves (samples and mip-maps).
    fn check_state(&self) {
        let n = self.r.channel_num() as usize;
        let ci = self.c.internals(n);
        let ri = self.r.internals();
        let ri = dsview_oracle::store::Internals {
            byte_fraction: ri.byte_fraction,
            ch_fraction: ri.ch_fraction,
            dest: ri.dest,
            last_sample: ri.last_sample,
            last_calc_count: ri.last_calc_count,
            cur_ref: ri.cur_ref,
            is_loop: ri.is_loop,
            able_free: ri.able_free,
            kept_leaves: ri.kept_leaves,
            lst_free_block_index: ri.lst_free_block_index,
            orders: ri.orders,
        };
        assert_eq!(ci, ri, "internals differ {}", self.ctx);
        for order in 0..ci.orders {
            let roots = self.c.roots(order);
            assert_eq!(
                roots,
                self.r.roots(order),
                "root count, order {order} {}",
                self.ctx
            );
            for root in 0..roots {
                let c = self.c.root(order, root);
                let (rbits, rleaves) = self.r.root(order, root);
                assert_eq!(
                    c.bits, rbits,
                    "tog/first/last of order {order} root {root} {}",
                    self.ctx
                );
                for (pos, (cl, rl)) in c.leaves.iter().zip(&rleaves).enumerate() {
                    match (cl, rl) {
                        (None, None) => {}
                        (Some(a), Some(z)) => {
                            if a != z {
                                let at = a.iter().zip(z.iter()).position(|(x, y)| x != y).unwrap();
                                panic!(
                                    "leaf order {order} root {root} pos {pos} word {at}: C {:016x} Rust {:016x} {}",
                                    a[at], z[at], self.ctx
                                );
                            }
                        }
                        _ => panic!(
                            "leaf order {order} root {root} pos {pos}: C {} Rust {} {}",
                            if cl.is_some() { "present" } else { "freed" },
                            if rl.is_some() { "present" } else { "freed" },
                            self.ctx
                        ),
                    }
                }
            }
        }
    }

    fn sig(&self, rng: &mut Rng) -> i32 {
        self.chans[rng.below(self.chans.len() as u64) as usize] as i32
    }

    /// Every query DSView makes, with random parameters inside the C++
    /// preconditions.
    fn check_queries(&mut self, rng: &mut Rng, able_free: bool) {
        self.check_counts();
        self.check_state();
        let ring = self.r.ring_sample_count();
        let is_loop = self.r.is_loop();
        for ch in 0..20 {
            assert_eq!(
                self.c.has_data(ch),
                self.r.has_data(ch),
                "has_data({ch}) {}",
                self.ctx
            );
        }
        if ring == 0 {
            return;
        }

        // single samples
        for _ in 0..40 {
            let i = if rng.chance(0.2) {
                ring - 1 - rng.below(ring.min(70))
            } else {
                rng.below(ring + 70)
            };
            let sig = self.sig(rng);
            assert_eq!(
                self.c.get_sample(i, sig),
                self.r.get_sample(i, sig),
                "get_sample({i}, {sig}) {}",
                self.ctx
            );
        }

        // blocks (saving, export)
        let blocks = self.r.get_block_num();
        for b in 0..blocks {
            let size = self.c.get_block_size(b);
            assert_eq!(
                size,
                self.r.get_block_size(b),
                "get_block_size({b}) {}",
                self.ctx
            );
            for (k, &ch) in self.chans.iter().enumerate() {
                let (cp, cs) = self.c.get_block_buf(b, ch as i32);
                let (rp, rs) = self.r.get_block_buf(b, ch as i32);
                assert_eq!(
                    (cp.is_null(), cs),
                    (rp.is_null(), rs),
                    "get_block_buf({b}, {ch}) {}",
                    self.ctx
                );
                if !cp.is_null() && size > 0 {
                    // SAFETY: both point at `size` bytes inside a leaf.
                    let (a, z) = unsafe {
                        (
                            std::slice::from_raw_parts(cp, size as usize),
                            std::slice::from_raw_parts(rp, size as usize),
                        )
                    };
                    if a != z {
                        let at = a.iter().zip(z).position(|(x, y)| x != y).unwrap();
                        panic!(
                            "block {b} channel #{k} ({ch}) differs at byte {at}: C {:02x} Rust {:02x} {}",
                            a[at], z[at], self.ctx
                        );
                    }
                }
            }
        }
        for _ in 0..5 {
            let s = rng.below(ring);
            assert_eq!(
                self.c.get_block_index_with_sample(s),
                self.r.get_block_index_with_sample(s),
                "get_block_index_with_sample({s}) {}",
                self.ctx
            );
        }

        // decoder reads
        for _ in 0..6 {
            let start = rng.below(ring);
            let k = rng.below(self.chans.len() as u64) as usize;
            let sig = self.chans[k] as i32;
            let c = self.c.get_samples(start, sig);
            let r = self.r.get_samples(start, sig);
            assert_eq!(
                (c.data.is_null(), c.end_sample, c.lbp.is_null()),
                (r.data.is_null(), r.end_sample, r.lbp.is_null()),
                "get_samples({start}, {sig}) {}",
                self.ctx
            );
            if !c.data.is_null() && c.end_sample > start {
                let n = (c.end_sample - (start & !7)).div_ceil(8) as usize;
                // SAFETY: the run lies inside one leaf (mip-map space follows).
                let (a, z) = unsafe {
                    (
                        std::slice::from_raw_parts(c.data, n),
                        std::slice::from_raw_parts(r.data, n),
                    )
                };
                if a != z {
                    let at = a.iter().zip(z).position(|(x, y)| x != y).unwrap();
                    panic!(
                        "get_samples({start}, {sig}) byte {at} of {n}: C {:02x} Rust {:02x} {}",
                        a[at], z[at], self.ctx
                    );
                }
            }
            // DecoderStack: release the previous block once the decoder
            // moved on. It compares leaf addresses, and glibc may hand a
            // freed leaf's address to a new leaf, differently for the two
            // stores; the leaf position (what get_samples records as the
            // decoder's block) is the allocator-independent equivalent.
            if !able_free && !c.data.is_null() {
                let pos = Some(self.r.internals().cur_ref[k]);
                let (pc, pr, at) = self.pins[k];
                if at != pos {
                    if at.is_some() {
                        self.c.free_decode_lpb(pc);
                        self.r.free_decode_lpb(pr);
                    }
                    self.pins[k] = (c.lbp, r.lbp, pos);
                }
            }
        }

        // display edges (LogicSignal::paint_mid)
        for _ in 0..4 {
            let start = rng.below(ring);
            let end = start + rng.below(ring - start);
            let min_length = rng.pick(&[0.5, 1.0, 1.7, 3.0, 64.0, 977.3, 1048576.0, 1e8]);
            let pixels_offset =
                (start as f64 / min_length).floor() + rng.pick(&[0.0, 0.25, -1.5, 2.0]);
            let width = 1 + rng.below(1500) as u16;
            let max_togs = rng.below(300) as u16;
            let sig = self.sig(rng) as u16;
            let (mut ce, mut ct, mut re, mut rt) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let cf = self.c.get_display_edges(
                &mut ce,
                &mut ct,
                start,
                end,
                width,
                max_togs,
                pixels_offset,
                min_length,
                sig,
            );
            let rf = self.r.get_display_edges(
                &mut re,
                &mut rt,
                start,
                end,
                width,
                max_togs,
                pixels_offset,
                min_length,
                sig,
            );
            let what = format!(
                "get_display_edges(start {start}, end {end}, width {width}, togs {max_togs}, px {pixels_offset}, len {min_length}, sig {sig}) {}",
                self.ctx
            );
            assert_eq!(cf, rf, "first sample: {what}");
            assert_eq!(ce, re, "edges: {what}");
            assert_eq!(ct, rt, "togs: {what}");
        }

        // edge search (cursor snapping, measurements, MCP get_edges)
        for _ in 0..10 {
            let sig = self.sig(rng);
            let min_length = rng.pick(&[1.0, 1.0, 1.0, 0.0, 64.0, 4096.0, 4e7]);
            let mut ci = rng.below(ring);
            let end = (ci + rng.below(ring - ci + 2)).min(ring);
            let last = if rng.chance(0.8) {
                self.r.get_sample(ci, sig)
            } else {
                rng.chance(0.5)
            };
            let mut ri = ci;
            let i0 = ci;
            let ch = self.c.get_nxt_edge(&mut ci, last, end, min_length, sig);
            let rh = self.r.get_nxt_edge(&mut ri, last, end, min_length, sig);
            assert_eq!(
                (ch, ci),
                (rh, ri),
                "get_nxt_edge({i0}, {last}, {end}, {min_length}, {sig}) {}",
                self.ctx
            );

            let min_length = rng.pick(&[1.0, 1.0, 1.0, 64.0, 4096.0]);
            let mut ci = rng.below(ring);
            let last = if rng.chance(0.8) {
                self.r.get_sample(ci, sig)
            } else {
                rng.chance(0.5)
            };
            let mut ri = ci;
            let i0 = ci;
            let ch = self.c.get_pre_edge(&mut ci, last, min_length, sig);
            let rh = self.r.get_pre_edge(&mut ri, last, min_length, sig);
            assert_eq!(
                (ch, ci),
                (rh, ri),
                "get_pre_edge({i0}, {last}, {min_length}, {sig}) {}",
                self.ctx
            );
        }

        // pattern search (SearchDock), over bounded ranges: it scans per sample
        if !is_loop || rng.chance(0.5) {
            for _ in 0..2 {
                let start = rng.below(ring) as i64;
                let end = start + rng.below((ring - start as u64).min(20_000)) as i64;
                let idx = start + rng.below((end - start + 1) as u64) as i64;
                let mut pattern = Pattern::new();
                let mut cpat = Vec::new();
                for _ in 0..1 + rng.below(3) {
                    let ch = if rng.chance(0.9) {
                        self.sig(rng) as u16
                    } else {
                        19
                    };
                    let f = rng.pick(b"01XRFC0");
                    pattern.insert(ch, f);
                }
                cpat.extend(pattern.iter().map(|(&c, &f)| (c, f)));
                let is_next = rng.chance(0.5);
                let (mut ci, mut ri) = (idx, idx);
                let ch = self.c.pattern_search(start, end, &mut ci, &cpat, is_next);
                let rh = self
                    .r
                    .pattern_search(start, end, &mut ri, &pattern, is_next);
                assert_eq!(
                    (ch, ci),
                    (rh, ri),
                    "pattern_search({start}, {end}, {idx}, {cpat:?}, next {is_next}) {}",
                    self.ctx
                );
            }
        }
    }
}

struct Scenario {
    chans: Vec<u16>,
    signals: Vec<Signal>,
    total: u64,
    /// Samples streamed (>= total in loop mode or to overrun the limit).
    stream_samples: u64,
    max_packet: usize,
    is_loop: bool,
    able_free: bool,
    queries_every: usize,
}

/// Runs a scenario; returns how often the loop window rotated a root node.
fn run(sc: &Scenario, rng: &mut Rng) -> usize {
    let mut rotations = 0;
    let n = sc.chans.len();
    let groups = sc.stream_samples.div_ceil(64);
    let mut p = Pair::new(&sc.chans);
    p.c.set_loop(sc.is_loop);
    p.r.set_loop(sc.is_loop);

    // repeat mode: the next capture into a cleared store (SigSession clears
    // before every capture; without that the C++ store reuses stale state,
    // DEVIATIONS.md F10)
    for capture in 0..if rng.chance(0.25) { 2 } else { 1 } {
        let data = cross_stream(&sc.signals, groups, rng);
        let packets = split(data.len(), n, sc.max_packet, rng);
        for (k, &(a, b)) in packets.iter().enumerate() {
            let pkt = &data[a..b];
            p.ctx = format!(
                "[capture {capture}, packet {k}/{} bytes {a}..{b}, {n} ch {:?}, total {}, loop {}, able_free {}]",
                packets.len(),
                sc.chans,
                sc.total,
                sc.is_loop,
                sc.able_free
            );
            let lo = p.r.loop_offset();
            if k == 0 {
                p.c.first_payload(pkt, sc.total, &sc.chans, sc.able_free);
                p.r.first_payload(pkt, sc.total, &sc.chans, sc.able_free);
            } else {
                p.c.append_payload(pkt);
                p.r.append_payload(pkt);
                if p.r.loop_offset() < lo {
                    rotations += 1;
                }
            }
            if sc.queries_every > 0 && k % sc.queries_every == sc.queries_every - 1 {
                p.check_queries(rng, sc.able_free);
            }
        }
        p.c.capture_ended();
        p.r.capture_ended();
        p.ctx = format!(
            "[capture {capture} ended, {n} ch {:?}, total {}, loop {}]",
            sc.chans, sc.total, sc.is_loop
        );
        p.check_queries(rng, sc.able_free);
        if rng.chance(0.3) {
            p.c.decode_end();
            p.r.decode_end();
            p.check_state();
        }
        p.c.clear();
        p.r.clear();
        // the next capture is decoded by a new decode run with no pins
        p.pins
            .iter_mut()
            .for_each(|x| *x = (std::ptr::null_mut(), std::ptr::null(), None));
        p.check_counts();
        p.check_state();
    }
    rotations
}

fn channels(rng: &mut Rng, max_n: u64) -> Vec<u16> {
    let n = 1 + rng.below(max_n);
    let mut v: Vec<u16> = Vec::new();
    while (v.len() as u64) < n {
        let c = rng.below(16) as u16;
        if !v.contains(&c) {
            v.push(c);
        }
    }
    v.sort();
    v
}

fn cases(default: u32) -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Captures within one leaf: packetization, bit alignment, mip-map
/// levels, limit overrun, decoder pins, queries mid-capture.
fn small_case(seed: u64) {
    let mut rng = Rng(seed);
    let chans = channels(&mut rng, 6);
    let signals = chans.iter().map(|_| signal(&mut rng)).collect();
    let max_total = rng.pick(&[200, 5000, 300_000]);
    let total = 1 + rng.below(max_total);
    let stream_samples = match rng.below(4) {
        0 => total / 2 + 1,
        1 => total + rng.below(500),
        _ => total,
    };
    let sc = Scenario {
        chans,
        signals,
        total,
        stream_samples,
        max_packet: rng.pick(&[16, 256, 4096, 65536]),
        is_loop: false,
        able_free: rng.chance(0.7),
        queries_every: rng.pick(&[0, 1, 7]),
    };
    run(&sc, &mut rng);
}

/// Loop mode within a few leaves: the window slides, head blocks are
/// freed.
fn small_loop_case(seed: u64) {
    let mut rng = Rng(seed);
    let chans = channels(&mut rng, 3);
    let signals = chans.iter().map(|_| signal(&mut rng)).collect();
    let total = 64 * (1 + rng.below(4000));
    let sc = Scenario {
        chans,
        signals,
        total,
        stream_samples: total + rng.below(3 * total),
        max_packet: rng.pick(&[512, 8192]),
        is_loop: true,
        able_free: true,
        queries_every: 5,
    };
    run(&sc, &mut rng);
}

/// Captures over several leaves: leaf completion, constant leaves freed
/// (or kept for a decoder), root first/last/tog bits, edge search across
/// leaves.
fn multi_leaf_case(seed: u64) {
    let mut rng = Rng(seed);
    let chans = channels(&mut rng, 3);
    let signals = chans
        .iter()
        .map(|_| match rng.below(4) {
            0 => Signal::Const(rng.chance(0.5)),
            1 => Signal::Sparse(0.0002),
            // leaf-aligned levels: edges exactly at leaf boundaries
            2 => Signal::Clock(LEAF_BLOCK_SAMPLES * (1 + rng.below(2)) / rng.pick(&[1, 2])),
            _ => Signal::Clock(1 + rng.below(20_000_000)),
        })
        .collect();
    let total =
        LEAF_BLOCK_SAMPLES * (1 + rng.below(3)) + rng.below(LEAF_BLOCK_SAMPLES) - rng.below(2000);
    let sc = Scenario {
        chans,
        signals,
        total,
        stream_samples: total + rng.below(1000),
        max_packet: 512 * 1024,
        is_loop: false,
        able_free: rng.chance(0.5),
        queries_every: 13,
    };
    run(&sc, &mut rng);
}

/// Loop mode sliding over several leaves.
fn multi_leaf_loop_case(seed: u64) {
    let mut rng = Rng(seed);
    let chans = channels(&mut rng, 2);
    let signals = chans
        .iter()
        .map(|_| match rng.below(3) {
            0 => Signal::Sparse(0.0001),
            1 => Signal::Clock(LEAF_BLOCK_SAMPLES),
            _ => Signal::Clock(1 + rng.below(5_000_000)),
        })
        .collect();
    let total = 64 * (1 + rng.below(2 * LEAF_BLOCK_SAMPLES / 64));
    let sc = Scenario {
        chans,
        signals,
        total,
        stream_samples: total + rng.below(4 * LEAF_BLOCK_SAMPLES),
        max_packet: 1024 * 1024,
        is_loop: true,
        able_free: true,
        queries_every: 17,
    };
    run(&sc, &mut rng);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(300), failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn small_captures_match(seed in any::<u64>()) {
        small_case(seed);
    }

    #[test]
    fn small_loop_captures_match(seed in any::<u64>()) {
        small_loop_case(seed);
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(300).div_ceil(25), failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn multi_leaf_captures_match(seed in any::<u64>()) {
        multi_leaf_case(seed);
    }

    #[test]
    fn multi_leaf_loop_captures_match(seed in any::<u64>()) {
        multi_leaf_loop_case(seed);
    }
}

/// Seeds that once failed.
#[test]
fn regressions() {
    // the harness released a decoder pin on one side only: glibc reused a
    // freed leaf's address (pins are now tracked by leaf position)
    multi_leaf_case(12_916_737_274_779_255_734);
    multi_leaf_case(11_728_970_440_024_474_940);
    multi_leaf_case(8_874_298_677_451_347_520);
}

/// Loop mode past a whole root node (2^30 samples): root nodes rotate.
#[test]
fn loop_rotates_root_nodes() {
    let mut rng = Rng(7);
    let sc = Scenario {
        chans: vec![3],
        signals: vec![Signal::Sparse(0.00002)],
        total: 3 * LEAF_BLOCK_SAMPLES + 4096,
        stream_samples: 64 * LEAF_BLOCK_SAMPLES + 5 * LEAF_BLOCK_SAMPLES + 777,
        max_packet: 4 * 1024 * 1024,
        is_loop: true,
        able_free: true,
        queries_every: 9,
    };
    assert!(
        run(&sc, &mut rng) >= 1,
        "the window never rotated a root node"
    );
}

/// DEVIATIONS.md F11: a 1000-sample capture arrives as 16 whole groups; both
/// stores keep the 24-sample overrun as loop offset and read shifted.
#[test]
fn f11_odd_length_capture_reads_shifted() {
    let mut words = [0u64; 16];
    for i in 500..1000 {
        words[i / 64] |= 1 << (i % 64);
    }
    let data: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut p = Pair::new(&[0]);
    p.c.first_payload(&data[..64], 1000, &[0], true);
    p.r.first_payload(&data[..64], 1000, &[0], true);
    p.c.append_payload(&data[64..]);
    p.r.append_payload(&data[64..]);
    p.c.capture_ended();
    p.r.capture_ended();
    p.check_counts();
    p.check_state();
    for (name, first_high) in [
        ("C++", (0..1000).find(|&i| p.c.get_sample(i, 0))),
        ("Rust", (0..1000).find(|&i| p.r.get_sample(i, 0))),
    ] {
        assert_eq!(
            first_high,
            Some(476),
            "{name}: the edge at sample 500 reads at 476"
        );
    }
    assert_eq!(p.r.loop_offset(), 24);
}
