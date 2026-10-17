//! Rust LogicStore against the C++ LogicSnapshot on the hot paths.
//! Phase 1b gates: display edges ≤ 1.1× the C++ time; ingest ≥ 1 GB/s per core.
//!
//!     cargo bench -p dsview-oracle --bench store

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use dsview_oracle::ffi;
use dsview_oracle::store::CLogicStore;
use dsview_store::logic::LogicStore;

/// Deterministic LA_CROSS stream: `n` channels, `groups` × 64 samples.
/// Channel c toggles every `period[c]` samples, with random bursts.
fn stream(n: usize, groups: u64, periods: &[u64]) -> Vec<u8> {
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut out = Vec::with_capacity(groups as usize * n * 8);
    for g in 0..groups {
        for &p in periods.iter().take(n) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let s0 = g * 64;
            let w = if x.is_multiple_of(997) {
                x
            } else if s0 / p == (s0 + 63) / p {
                if (s0 / p) % 2 == 1 { !0 } else { 0 }
            } else {
                (0..64).fold(0u64, |w, i| w | ((((s0 + i) / p) % 2) << i))
            };
            out.extend_from_slice(&w.to_le_bytes());
        }
    }
    out
}

const PACKET: usize = 1 << 20;

fn ingest(c: &mut Criterion) {
    let n = 16;
    let periods: Vec<u64> = (0..n as u64).map(|i| 40 + i * 1_000_003).collect();
    let samples: u64 = 1 << 27;
    let data = stream(n, samples / 64, &periods);
    let chans: Vec<u16> = (0..n as u16).collect();
    let mut g = c.benchmark_group("ingest_16ch_128Msamples");
    g.sample_size(10);
    g.throughput(Throughput::Bytes(data.len() as u64));
    g.bench_function("cpp", |b| {
        b.iter(|| {
            let mut s = CLogicStore::new();
            let mut first = true;
            for p in data.chunks(PACKET) {
                if first {
                    s.first_payload(p, samples, &chans, true);
                    first = false;
                } else {
                    // whole groups: the C++ code stays inside the packet,
                    // no padded copy needed
                    unsafe { ffi::ostore_append(s.raw(), p.as_ptr(), p.len() as u64) };
                }
            }
            s.capture_ended();
            black_box(s.counts().sample_count)
        })
    });
    g.bench_function("rust", |b| {
        b.iter(|| {
            let mut s = LogicStore::new();
            let mut first = true;
            for p in data.chunks(PACKET) {
                if first {
                    s.first_payload(p, samples, &chans, true);
                    first = false;
                } else {
                    s.append_payload(p);
                }
            }
            s.capture_ended();
            black_box(s.sample_count())
        })
    });
    g.finish();
}

fn display(c: &mut Criterion) {
    let samples: u64 = 1 << 28;
    // ch 0: fast clock (every leaf has toggles), ch 1: slow clock (mostly
    // constant leaves)
    let data = stream(2, samples / 64, &[997, 9_000_001]);
    let mut cs = CLogicStore::new();
    let mut rs = LogicStore::new();
    for (k, p) in data.chunks(PACKET).enumerate() {
        if k == 0 {
            cs.first_payload(p, samples, &[0, 1], true);
            rs.first_payload(p, samples, &[0, 1], true);
        } else {
            unsafe { ffi::ostore_append(cs.raw(), p.as_ptr(), p.len() as u64) };
            rs.append_payload(p);
        }
    }
    cs.capture_ended();
    rs.capture_ended();

    let width: u16 = 1920;
    let mut g = c.benchmark_group("display_edges_256Msamples_1920px");
    for (name, start, spp) in [
        ("all", 0u64, samples as f64 / width as f64),
        ("mid_zoom", samples / 3, 4096.0),
        ("fine_zoom", samples / 2 + 12345, 2.5),
    ] {
        let end = (start + (spp * width as f64) as u64).min(samples - 1);
        for ch in [0u16, 1] {
            let px = (start as f64 / spp).floor();
            let id = format!("{name}/ch{ch}");
            let (mut e, mut t) = (Vec::new(), Vec::new());
            g.bench_with_input(BenchmarkId::new("cpp", &id), &ch, |b, &ch| {
                b.iter(|| cs.get_display_edges(&mut e, &mut t, start, end, width, 500, px, spp, ch))
            });
            g.bench_with_input(BenchmarkId::new("rust", &id), &ch, |b, &ch| {
                b.iter(|| rs.get_display_edges(&mut e, &mut t, start, end, width, 500, px, spp, ch))
            });
        }
    }
    g.finish();

    let mut g = c.benchmark_group("next_edge_scan_256Msamples");
    g.sample_size(20);
    for ch in [0i32, 1] {
        let scan = |f: &dyn Fn(&mut u64, bool) -> bool| {
            let (mut i, mut level, mut n) = (0u64, false, 0u32);
            while i < samples - 1 && n < 20_000 {
                if !f(&mut i, level) {
                    break;
                }
                level = !level;
                n += 1;
            }
            n
        };
        g.bench_function(BenchmarkId::new("cpp", ch), |b| {
            b.iter(|| scan(&|i, l| cs.get_nxt_edge(i, l, samples - 1, 1.0, ch)))
        });
        g.bench_function(BenchmarkId::new("rust", ch), |b| {
            b.iter(|| scan(&|i, l| rs.get_nxt_edge(i, l, samples - 1, 1.0, ch)))
        });
    }
    g.finish();
}

criterion_group!(benches, ingest, display);
criterion_main!(benches);
