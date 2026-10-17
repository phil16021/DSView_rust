//! DSView sample stores.
//!
//! * [`logic`]: bit-packed logic captures with toggle mip-maps, edge search
//!   and loop (ring) mode, a port of `pv::data::LogicSnapshot`
//! * [`dso`]: oscilloscope frames, a port of `pv::data::DsoSnapshot`
//! * [`analog`]: analog sample ring with min/max envelopes, a port of
//!   `pv::data::AnalogSnapshot`
//!
//! The ports keep the C++ behaviour, quirks included, so the Qt GUI can switch
//! between implementations (`DSVIEW_IMPL_STORE`). They are checked call by
//! call against the original code in `dev/dsview-oracle/tests/store_diff.rs`.

pub mod analog;
pub mod dso;
pub mod logic;

/// A zero-filled vector, or None when the allocation fails: the C++ stores
/// report `memory_failed` instead of aborting.
#[allow(clippy::slow_vector_initialization)] // `vec![]` would abort on failure
pub(crate) fn try_zeroed<T: Clone + Default>(n: usize) -> Option<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, T::default());
    Some(v)
}
