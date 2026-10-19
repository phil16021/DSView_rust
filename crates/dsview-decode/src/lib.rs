//! DSView's protocol decoder engine: runs the sigrok Python decoders
//! (`assets/decoders`) in an embedded CPython through PyO3.
//!
//! A port of libsigrokdecode4DSL with the same behaviour, quirks included:
//! decoder loading and validation ([`decoder`]), the `sigrokdecode` module
//! the decoders import ([`module`]), instances and the `wait()` condition
//! matching ([`instance`]), and sessions with one worker thread per decoder
//! stack ([`session`]). Checked against the C engine output by
//! `dev/dsview-oracle` (`rust-decode` vs `oracle-decode`).
//!
//! The process may embed one Python interpreter only: call [`init`] once,
//! before anything else in the process starts Python.

pub mod decoder;
mod exception;
pub mod instance;
pub mod log;
pub mod module;
pub mod python;
pub mod session;

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use pyo3::prelude::*;

pub use decoder::{DecoderInfo, OptValue};
pub use instance::Instance;
pub use module::MetaValue;
pub use session::{Annotation, Output, OutputData, SendError, Session};

static DECODERS: RwLock<Vec<Arc<DecoderInfo>>> = RwLock::new(Vec::new());
static SEARCHPATHS: RwLock<Vec<PathBuf>> = RwLock::new(Vec::new());
static INITIALIZED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `srd_init`: starts Python with the `sigrokdecode` module and puts the
/// decoder directories first on `sys.path` (the last one given ends up
/// first, as in C).
pub fn init(paths: &[&Path]) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        let m = "libsigrokdecode is already initialized.".to_owned();
        log::err(&m);
        return Err(m);
    }
    log::dbg("Initializing libsigrokdecode.");
    // SAFETY: only checks a flag.
    if unsafe { pyo3::ffi::Py_IsInitialized() } == 0 {
        // SAFETY: before Python starts, as CPython requires.
        unsafe {
            pyo3::ffi::PyImport_AppendInittab(c"sigrokdecode".as_ptr(), Some(module::init_module));
        }
        match python::find_home() {
            Some(home) => {
                log::dbg(&format!("Python home: {}", home.display()));
                // SAFETY: Python is not running yet.
                if let Err(e) = unsafe { python::initialize(&home, &python::version()) } {
                    log::err(&e);
                    INITIALIZED.store(false, Ordering::SeqCst);
                    return Err(e);
                }
            }
            None => Python::initialize(),
        }
    } else {
        // someone else started Python: install the module by hand
        Python::attach(|py| -> PyResult<()> {
            let m = PyModule::new(py, "sigrokdecode")?;
            module::populate(&m)?;
            py.import("sys")?
                .getattr("modules")?
                .set_item("sigrokdecode", m)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    }
    // decoders are read, never compiled into the (possibly shared or
    // root-owned) decoder directories
    Python::attach(|py| {
        py.import("sys")
            .and_then(|s| s.setattr("dont_write_bytecode", true))
    })
    .map_err(|e| e.to_string())?;
    for p in paths {
        searchpath_add(p)?;
    }
    for p in SEARCHPATHS.read().unwrap_or_else(|e| e.into_inner()).iter() {
        log::dbg(&format!("Protocol decoder search path: {}", p.display()));
    }
    Ok(())
}

/// `srd_decoder_searchpath_add`.
pub fn searchpath_add(path: &Path) -> Result<(), String> {
    log::dbg(&format!("Adding '{}' to module path.", path.display()));
    Python::attach(|py| -> PyResult<()> {
        let sys_path = py.import("sys")?.getattr("path")?;
        sys_path.call_method1("insert", (0, path.to_string_lossy().into_owned()))?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;
    SEARCHPATHS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(0, path.to_path_buf());
    Ok(())
}

/// `srd_searchpaths_get`.
pub fn searchpaths() -> Vec<PathBuf> {
    SEARCHPATHS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// `srd_decoder_load`: imports one decoder module; false when it failed
/// (logged). A module imported before is not loaded again.
pub fn load(module_name: &str) -> bool {
    match Python::attach(|py| decoder::load(py, module_name)) {
        Ok(Some(info)) => {
            DECODERS
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push(Arc::new(info));
            true
        }
        Ok(None) => true,
        Err(()) => false,
    }
}

/// `srd_decoder_load_all`: every directory entry of every search path;
/// failures are logged and skipped. Entries load in name order: C takes
/// the directory's order, which on ext4 follows a per-directory hash, so the
/// decoder list and the row order of decoder stacks (rows follow load
/// order) differed between two installs of the same files.
pub fn load_all() {
    for dir in searchpaths() {
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
                names.sort();
                for n in names {
                    load(&n.to_string_lossy());
                }
            }
            // C then tries the path as a zip archive (zipimport); not supported
            Err(_) => log::err(&format!("Cannot read decoder directory {}.", dir.display())),
        }
    }
}

/// `srd_decoder_list`: loaded decoders in load order.
pub fn decoders() -> Vec<Arc<DecoderInfo>> {
    DECODERS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// `srd_decoder_get_by_id`.
pub fn decoder_by_id(id: &str) -> Option<Arc<DecoderInfo>> {
    DECODERS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|d| d.id == id)
        .cloned()
}

/// `srd_decoder_unload_all`.
pub fn unload_all() {
    DECODERS.write().unwrap_or_else(|e| e.into_inner()).clear();
}
