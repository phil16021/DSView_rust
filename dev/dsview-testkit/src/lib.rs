//! Shared test infrastructure for the Rust port: the capture corpus, golden
//! files produced by the C oracle, and a JSONL diff that points at the first
//! divergence.

pub mod control;
pub mod corpus;
pub mod dsl_summary;
pub mod golden;

use std::path::PathBuf;

/// Repository root (this crate lives in dev/dsview-testkit).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}
