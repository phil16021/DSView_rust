//! DSView's signal computations without a UI (docs/migration/PLAN.md,
//! Phase 5): what `DsoSignal`, `SpectrumStack` and `MathStack` compute,
//! ported so that their results can be compared against the C++.
//!
//! * [`measure`]: the DSO measurements (`DsoSignal::get_measure`)
//! * [`spectrum`]: windows and the power spectrum (`SpectrumStack`), on
//!   realfft instead of FFTW

pub mod measure;
pub mod spectrum;
