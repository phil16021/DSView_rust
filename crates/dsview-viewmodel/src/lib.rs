//! The logic of DSView's trace view (`pv/view`) without a UI toolkit, so the
//! Rust GUI only paints and forwards input (docs/migration/PLAN.md, Phase 5).
//!
//! Each module ports one part of the Qt view exactly first (parity); the
//! quirks found on the way are listed in DEVIATIONS.md and fixed later.
//!
//! * [`viewport`]: scale and offset, zoom, limits, index ↔ pixel (`View`)

pub mod viewport;
