//! Shared test infrastructure for the Rust port: the capture corpus, golden
//! files (produced by the C oracle up to tag `c-final`, checked against Rust
//! since), a JSONL diff that points at the first divergence, and the Rust
//! decoder engine's runner (`rust-decode`).

pub mod capture;
pub mod control;
pub mod corpus;
pub mod dsl_summary;
pub mod golden;
pub mod rust_run;
pub mod watchdog;

use std::path::PathBuf;

/// Repository root (this crate lives in dev/dsview-testkit).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// DSViewR's data directory in the source tree (`res/`, `demo/`, `lang/`,
/// `decoders/`, icons); installed as `<prefix>/share/dsview`.
pub fn data_dir() -> PathBuf {
    repo_root().join("assets")
}

/// The Python protocol decoders.
pub fn decoder_dir() -> PathBuf {
    data_dir().join("decoders")
}
