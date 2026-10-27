//! Properties of the horizontal view for any capture, width and zoom:
//! the sample ↔ pixel mapping round-trips and keeps order, and zooming
//! stays inside the limits and keeps the sample under the pointer.

use dsview_viewmodel::viewport::{Timebase, Viewport};
use proptest::prelude::*;

fn timebase() -> impl Strategy<Value = Timebase> {
    (
        prop::sample::select(vec![10e3, 1e6, 25e6, 100e6, 400e6, 1e9]),
        1024u64..2_000_000_000,
    )
        .prop_map(|(rate, limit)| Timebase {
            snap_samplerate: rate,
            samplerate: rate,
            sample_limit: limit,
            dso_view_time: None,
        })
}

/// A view on `tb` after `steps` zoom steps at `px`.
fn view(tb: &Timebase, width: i32, steps: &[(i8, i32)]) -> Viewport {
    let mut v = Viewport {
        width,
        ..Viewport::default()
    };
    v.update_limits(tb);
    for &(s, px) in steps {
        v.zoom(f64::from(s), px.rem_euclid(width), tb);
    }
    v
}

proptest! {
    #[test]
    fn index_pixel_round_trip(
        tb in timebase(),
        width in 100i32..4000,
        steps in prop::collection::vec((-5i8..12, 0i32..4000), 0..12),
        at in 0.0f64..1.0,
    ) {
        let v = view(&tb, width, &steps);
        let i = (at * tb.sample_limit as f64) as u64;
        let px = v.index_to_pixel(i, false, &tb);
        let back = v.pixel_to_index(px, &tb);
        prop_assert!(back.abs_diff(i) <= 1, "{i} -> {px} -> {back}");
        let j = i + 1 + i / 7;
        prop_assert!(v.index_to_pixel(j, false, &tb) >= px);
    }

    #[test]
    fn zoom_stays_inside_the_limits(
        tb in timebase(),
        width in 100i32..4000,
        steps in prop::collection::vec((-8i8..16, 0i32..4000), 1..20),
    ) {
        let v = view(&tb, width, &steps);
        prop_assert!(v.scale >= v.min_scale && v.scale <= v.max_scale, "{v:?}");
        prop_assert!(v.x_offset >= v.min_offset(), "{v:?}");
        prop_assert!(v.x_offset <= v.max_offset(&tb).max(v.min_offset()), "{v:?}");
    }

    #[test]
    fn zoom_keeps_the_sample_under_the_pointer(
        tb in timebase(),
        width in 100i32..4000,
        steps in prop::collection::vec((-5i8..12, 0i32..4000), 0..8),
        step in -3i8..4,
        px in 0i32..4000,
    ) {
        let mut v = view(&tb, width, &steps);
        let px = px % width;
        let before = v.pixel_to_index(f64::from(px), &tb);
        let spp0 = v.samples_per_pixel(&tb);
        v.zoom(f64::from(step), px, &tb);
        let clamped = v.x_offset == v.min_offset() || v.x_offset == v.max_offset(&tb);
        prop_assume!(!clamped);
        let after = v.pixel_to_index(f64::from(px), &tb);
        let spp1 = v.samples_per_pixel(&tb);
        // the integer offset loses up to a pixel at either scale (F27)
        let slack = (spp0 + spp1).ceil() as u64 + 1;
        prop_assert!(after.abs_diff(before) <= slack, "{before} -> {after}, slack {slack}");
    }

    /// F27: zooming in step by step at a pixel keeps the sample that was
    /// under it there. Qt's integer offset rounded down by up to a pixel per
    /// step, and earlier errors grow with every further zoom-in, so the view
    /// drifted away from the pointer (up to about 3 px).
    #[test]
    fn zooming_in_does_not_drift(
        tb in timebase(),
        width in 200i32..4000,
        n in 4usize..20,
        px in 0i32..4000,
    ) {
        let px = px % width;
        let mut v = view(&tb, width, &[(2, width / 2)]);
        // the exact sample under the pixel (the offset with its fraction)
        let start = (v.x_offset as f64 + v.x_frac + f64::from(px)) * v.samples_per_pixel(&tb);
        for _ in 0..n {
            v.zoom(1.0, px, &tb);
            let clamped = v.x_offset <= v.min_offset()
                || v.x_offset >= v.max_offset(&tb)
                || v.scale <= v.min_scale
                || v.scale >= v.max_scale;
            prop_assume!(!clamped);
        }
        // where that sample is drawn now: within the pixel the offset rounds off
        let at = start / v.samples_per_pixel(&tb) - v.x_offset as f64;
        prop_assert!(
            (at - f64::from(px)).abs() <= 1.0,
            "the sample under pixel {px} moved to {at} after {n} steps"
        );
    }
}
