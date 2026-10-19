//! The Rust store on input the C++ class mishandles: packets of any byte
//! length (not whole 64-sample groups), new captures without `clear()`,
//! queries outside the C++ preconditions. The C++ code reads and writes out
//! of bounds there (DEVIATIONS.md F9, F10); the port must not panic or
//! misbehave, and must stay correct on whole groups afterwards.

use dsview_store::logic::{LogicStore, Pattern};
use proptest::prelude::*;

fn cross(words: &[u64]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn any_packets_and_queries_never_panic(
        n in 1usize..5,
        total in 1u64..20_000,
        is_loop in any::<bool>(),
        able_free in any::<bool>(),
        captures in proptest::collection::vec(
            proptest::collection::vec((1usize..3000, any::<u64>()), 1..12), 1..3),
        queries in proptest::collection::vec((any::<u64>(), any::<u64>(), 0u16..400), 1..20),
    ) {
        let chans: Vec<u16> = (0..n as u16).map(|i| i * 2).collect();
        let mut s = LogicStore::new();
        s.set_loop(is_loop);
        for packets in &captures {
            for (k, &(len, seed)) in packets.iter().enumerate() {
                let data: Vec<u8> = (0..len).map(|i| (seed >> (i % 57)) as u8 ^ i as u8).collect();
                if k == 0 {
                    s.first_payload(&data, total, &chans, able_free);
                } else {
                    s.append_payload(&data);
                }
                for &(a, b, w) in &queries {
                    let sig = chans[(a % n as u64) as usize] as i32;
                    let ring = s.ring_sample_count();
                    let _ = s.get_sample(a, sig);
                    let _ = s.get_sample(a % (ring + 1), sig);
                    let mut i = a % (ring + 1);
                    let _ = s.get_nxt_edge(&mut i, b & 1 != 0, b % (ring + 2), [1.0, 0.0, 1e9][(b % 3) as usize], sig);
                    let mut i = a % (ring + 1);
                    let _ = s.get_pre_edge(&mut i, b & 1 != 0, 1.0, sig);
                    if ring > 0 {
                        let start = a % ring;
                        let end = start + b % (ring - start);
                        let (mut e, mut t) = (Vec::new(), Vec::new());
                        s.get_display_edges(&mut e, &mut t, start, end, w, w / 3, 0.0, 1.0 + (b % 100) as f64, sig as u16);
                        // C++ can append one edge past the last pixel
                        prop_assert!(e.len() == w as usize || e.len() == w as usize + 1);
                        prop_assert!(t.len() <= (w / 3).max(1) as usize);
                        let r = s.get_samples(start, sig);
                        prop_assert!(r.end_sample <= ring.max(start + 1) || is_loop);
                        let mut idx = start as i64;
                        let mut pat = Pattern::new();
                        pat.insert(sig as u16, b"01RFC"[(b % 5) as usize]);
                        let _ = s.pattern_search(start as i64, (start + (b % 2000)).min(ring - 1) as i64, &mut idx, &pat, b & 2 != 0);
                    }
                    for blk in -1..s.get_block_num() + 1 {
                        let _ = s.get_block_size(blk);
                        let _ = s.get_block_buf(blk, sig);
                    }
                    let _ = s.get_block_index_with_sample(a);
                }
            }
            s.capture_ended();
        }
    }
}

/// After garbage, a clear() store behaves exactly like a fresh one.
#[test]
fn clear_recovers_from_misaligned_packets() {
    let mut s = LogicStore::new();
    s.first_payload(&[0xff; 13], 1000, &[0, 1, 2], true);
    s.append_payload(&[0x0f; 5]);
    s.capture_ended();
    s.clear();

    let mut words = vec![0u64; 3 * 4];
    words[0] = 0xf0;
    s.first_payload(&cross(&words), 256, &[0, 1, 2], true);
    s.capture_ended();
    let mut fresh = LogicStore::new();
    fresh.first_payload(&cross(&words), 256, &[0, 1, 2], true);
    fresh.capture_ended();
    assert_eq!(s.counts(), fresh.counts());
    for i in 0..256 {
        for ch in 0..3 {
            assert_eq!(s.get_sample(i, ch), fresh.get_sample(i, ch));
        }
    }
    assert!(s.get_sample(4, 0) && !s.get_sample(3, 0));
}
