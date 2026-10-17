//! DSView file formats.
//!
//! * [`keyfile`]: the GKeyFile INI dialect of `.dsl` headers
//! * [`header`]: the `header` entry, interpreted like the C loader
//! * [`dsl`]: `.dsl`/`.demo` capture reader and writer
//! * [`samplerate`]: samplerate strings
//! * [`export`]: CSV/VCD export (byte-compatible with libsigrok4DSL's output modules)
//! * [`cfmt`]: C printf-compatible number formatting
//! * [`zipw`]: safe, atomic `.dsl` archive writing
//!
//! All parsing is bounded: malformed or hostile files produce an [`Error`],
//! never a panic or an unbounded allocation (fuzzed in `fuzz/`).

pub mod cfmt;
pub mod dsl;
pub mod export;
pub mod header;
pub mod keyfile;
pub mod samplerate;
pub mod zipw;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("header: {0}")]
    Header(String),
    #[error("missing {0}")]
    Missing(&'static str),
    #[error("missing zip entry {0}")]
    MissingEntry(String),
    #[error("corrupt file: {0}")]
    Corrupt(String),
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}
