//! Golden-reference runner for the Rust port: links the original C
//! libsigrok4DSL / libsigrokdecode4DSL and exposes them to the oracle
//! binaries. Never shipped; see docs/migration/PLAN.md (Phase 0).

pub mod capture;
pub mod ffi;

use std::path::PathBuf;

/// Repository root (this crate lives in dev/dsview-oracle).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// The Python decoders the C engine loads.
pub fn decoder_dir() -> PathBuf {
    repo_root().join("libsigrokdecode4DSL/decoders")
}
