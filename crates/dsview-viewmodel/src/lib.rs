//! The logic of DSView's trace view (`pv/view`) without a UI toolkit, so the
//! Rust GUI only paints and forwards input (docs/migration/PLAN.md, Phase 5).
//!
//! Each module ports one part of the Qt view exactly first (parity); the
//! quirks found on the way are listed in DEVIATIONS.md and fixed later.
//!
//! * [`viewport`]: scale and offset, zoom, limits, index ↔ pixel (`View`)
//! * [`logic`]: a logic trace's lines and sample dots (`LogicSignal`)
//! * [`ruler`]: ticks, labels and number formats (`Ruler`)
//! * [`measure`]: the pulse under the mouse, edge counts, edge snapping
//! * [`cursors`]: time cursors
//! * [`analog`]: analog (DAQ) traces (`AnalogSignal`)
//! * [`dso`]: oscilloscope traces, grid, trigger level (`DsoSignal`)
//! * [`spectrum`]: the FFT pane (`SpectrumTrace`)
//! * [`decode`]: decoder rows, annotation shapes, colours, text fitting
//! * [`layout`]: trace heights and order (`View::signals_changed`)
//! * [`input`]: wheel actions, kinetic scroll, rubber-band zoom

pub mod analog;
pub mod cursors;
pub mod decode;
pub mod dso;
pub mod input;
pub mod layout;
pub mod logic;
pub mod measure;
pub mod ruler;
pub mod spectrum;
pub mod viewport;
