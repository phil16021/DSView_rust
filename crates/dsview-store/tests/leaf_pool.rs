//! The logic store's leaf pool (one test: the pool is process-wide).
//! Freed leaves are reused instead of reallocated, a pre-fault fills the
//! pool ahead of a capture, and a reused leaf reads as zero.

use dsview_store::logic::{LEAF_BLOCK_SAMPLES, LogicStore, leaf_pool};

/// 16 channels of a slow square wave (mostly constant leaves) and a fast one.
fn capture(s: &mut LogicStore, samples: u64) {
    let n = 4usize;
    let mut data = Vec::with_capacity((samples / 64) as usize * n * 8);
    for g in 0..samples / 64 {
        for c in 0..n as u64 {
            let w = if c == 0 {
                0xAAAA_AAAA_AAAA_AAAAu64
            } else if (g / (1 << 18)) % 2 == 1 {
                !0
            } else {
                0
            };
            data.extend_from_slice(&w.to_le_bytes());
        }
    }
    let chans: Vec<u16> = (0..n as u16).collect();
    let mut first = true;
    for p in data.chunks(1 << 20) {
        if first {
            s.first_payload(p, samples, &chans, true);
            first = false;
        } else {
            s.append_payload(p);
        }
    }
    s.capture_ended();
}

#[test]
fn leaves_are_reused_and_prefaulted() {
    let samples = 8 * LEAF_BLOCK_SAMPLES;
    // a first capture allocates
    let mut a = LogicStore::new();
    capture(&mut a, samples);
    assert!(leaf_pool::fresh_allocations() > 0);
    // a second store, after the first one is gone: its leaves come back
    drop(a);
    let before = leaf_pool::fresh_allocations();
    let mut b = LogicStore::new();
    capture(&mut b, samples);
    assert_eq!(
        leaf_pool::fresh_allocations(),
        before,
        "the second capture took its leaves from the pool"
    );
    // a reused leaf starts clean: a constant capture into it shows no
    // edges (a stale mip-map from its last use would)
    drop(b);
    let mut c = LogicStore::new();
    c.first_payload(&[0xffu8; 8 * 4], 64 * 4, &[0], true);
    c.capture_ended();
    assert_eq!(leaf_pool::fresh_allocations(), before, "c reused a leaf");
    for i in 0..256 {
        assert!(c.get_sample(i, 0), "sample {i}");
    }
    let mut i = 0;
    assert!(
        !c.get_nxt_edge(&mut i, true, 255, 1.0, 0),
        "phantom edge at {i}"
    );
    drop(c);
    // a pre-fault fills the pool up to the request (bounded by the cap)
    leaf_pool::release();
    assert_eq!(leaf_pool::pooled(), 0);
    leaf_pool::prefault(6).join().unwrap();
    assert_eq!(leaf_pool::pooled(), 6);
    let before = leaf_pool::fresh_allocations();
    let mut d = LogicStore::new();
    d.first_payload(&[0u8; 8], LEAF_BLOCK_SAMPLES * 2, &[0, 1, 2], true);
    assert_eq!(
        leaf_pool::fresh_allocations(),
        before,
        "taken from the pre-fault"
    );
    // idle: the pool keeps at most its idle capacity
    drop(d);
    leaf_pool::trim_idle();
    assert!(leaf_pool::pooled() <= leaf_pool::IDLE_LEAVES);
    leaf_pool::release();
}
