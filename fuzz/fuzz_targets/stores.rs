#![no_main]
//! Arbitrary packet streams and queries into the sample stores: no panic,
//! and the documented output shapes hold (crates/dsview-store).

use dsview_store::analog::{AnalogChannel, AnalogStore};
use dsview_store::dso::{DsoChannel, DsoStore};
use dsview_store::logic::{LogicStore, Pattern};
use libfuzzer_sys::fuzz_target;

/// Reads the input front to back; zeros once it is used up.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn byte(&mut self) -> u8 {
        let (b, rest) = self.0.split_first().map_or((0, &[][..]), |(b, r)| (*b, r));
        self.0 = rest;
        b
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes([self.byte(), self.byte(), self.byte(), self.byte()])
    }
    fn bytes(&mut self, n: usize) -> &[u8] {
        let n = n.min(self.0.len());
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        a
    }
    fn done(&self) -> bool {
        self.0.is_empty()
    }
}

fuzz_target!(|data: &[u8]| {
    let mut inp = Input(data);
    match inp.byte() % 3 {
        0 => logic(&mut inp),
        1 => dso(&mut inp),
        _ => analog(&mut inp),
    }
});

fn logic(inp: &mut Input) {
    let n = 1 + inp.byte() % 8;
    let chans: Vec<u16> = (0..n as u16).map(|i| i * 3 % 32).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let total = (inp.u32() as u64) % (1 << 26) + 1;
    let mut s = LogicStore::new();
    s.set_loop(inp.byte() & 1 != 0);
    let able_free = inp.byte() & 1 != 0;
    let mut first = true;
    while !inp.done() {
        let op = inp.byte();
        let a = inp.u32() as u64;
        let sig = chans[(a % chans.len() as u64) as usize] as i32;
        let ring = s.ring_sample_count();
        match op % 10 {
            0..=3 => {
                let len = (inp.u32() % 4096) as usize;
                let pkt = inp.bytes(len).to_vec();
                if first {
                    s.first_payload(&pkt, total, &chans, able_free);
                    first = false;
                } else {
                    s.append_payload(&pkt);
                }
            }
            4 => {
                let _ = s.get_sample(a, sig);
                let mut i = a % (ring + 1);
                let _ = s.get_nxt_edge(&mut i, op & 16 != 0, a % (ring + 2), 1.0, sig);
                let mut i = a % (ring + 1);
                let _ = s.get_pre_edge(&mut i, op & 32 != 0, 1.0, sig);
            }
            5 if ring > 0 => {
                let start = a % ring;
                let end = start + (inp.u32() as u64) % (ring - start);
                let w = (inp.u32() % 2048) as u16;
                let (mut e, mut t) = (Vec::new(), Vec::new());
                let spp = 0.5 + (inp.byte() as f64) * 100.0;
                s.get_display_edges(&mut e, &mut t, start, end, w, w / 2, 0.0, spp, sig as u16);
                assert!(e.len() == w as usize || e.len() == w as usize + 1);
            }
            6 if ring > 0 => {
                let r = s.get_samples(a % ring, sig);
                if !r.data.is_null() {
                    assert!(!r.lbp.is_null());
                }
                s.free_decode_lpb(r.lbp);
            }
            7 if ring > 0 => {
                let start = (a % ring) as i64;
                let end = (start + (inp.u32() % 3000) as i64).min(ring as i64 - 1);
                let mut idx = start;
                let mut p = Pattern::new();
                p.insert(sig as u16, b"01XRFC"[(op % 6) as usize]);
                let _ = s.pattern_search(start, end, &mut idx, &p, op & 1 != 0);
            }
            8 => {
                for b in -1..s.get_block_num() + 1 {
                    let _ = s.get_block_size(b);
                    let _ = s.get_block_buf(b, sig);
                }
                let _ = s.get_block_index_with_sample(a);
            }
            _ => match op % 4 {
                0 => s.capture_ended(),
                1 => s.decode_end(),
                2 => {
                    s.clear();
                    first = true;
                }
                _ => s.init(),
            },
        }
    }
}

fn dso(inp: &mut Input) {
    let chans = [
        DsoChannel { index: 0, enabled: inp.byte() & 1 != 0 },
        DsoChannel { index: 1, enabled: inp.byte() & 1 != 0 },
    ];
    let total = (inp.u32() % 100_000) as u64;
    let (instant, file) = (inp.byte() & 1 != 0, inp.byte() & 1 != 0);
    let mut s = DsoStore::new();
    s.set_ref_range(inp.byte() as u32, inp.byte() as u32);
    let mut first = true;
    while !inp.done() {
        let n = (inp.u32() % 50_000) as u64;
        let len = (inp.u32() % 8192) as usize;
        let pkt = inp.bytes(len).to_vec();
        let tog = inp.byte() & 1 != 0;
        if first {
            s.first_payload(&pkt, n, total, &chans, instant, file, tog);
            first = false;
        } else {
            s.append_payload(&pkt, n, tog);
        }
        for ch in 0..3 {
            let _ = s.get_samples(n / 2, ch);
            let _ = s.get_max_min_value(ch);
        }
        for b in 0..s.get_block_num() {
            let _ = s.get_block_size(b);
        }
        if inp.byte() % 8 == 0 {
            s.clear();
            first = true;
        }
    }
}

fn analog(inp: &mut Input) {
    let n = 1 + inp.byte() % 4;
    let chans: Vec<AnalogChannel> = (0..n as u16).map(|i| AnalogChannel { index: i, enabled: i % 2 == 0 }).collect();
    let bits = inp.byte();
    let pitch = (inp.byte() % 9) as u16;
    let total = (inp.u32() % 200_000) as u64;
    let mut s = AnalogStore::new();
    let mut first = true;
    while !inp.done() {
        let samples = (inp.u32() % 100_000) as u64;
        let len = (inp.u32() % 8192) as usize;
        let pkt = inp.bytes(len).to_vec();
        if first {
            s.first_payload(&pkt, samples, bits, pitch, total, &chans);
            first = false;
        } else {
            s.append_payload(&pkt, samples, pitch);
        }
        let _ = s.get_samples(samples / 3);
        for probe in -1..5 {
            let e = s.get_envelope_section(samples, samples as i64, 1.0 + (bits as f32) * 7.0, probe);
            assert!(e.samples_num == 0 || !e.samples.is_null());
        }
        for b in 0..s.get_block_num() {
            let _ = s.get_block_size(b);
        }
        if inp.byte() % 8 == 0 {
            s.clear();
            first = true;
        }
    }
}
