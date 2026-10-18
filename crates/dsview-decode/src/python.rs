//! The interpreter's home in a package. Packages carry their own Python in
//! `<prefix>/lib/dsview`: libpython (found through the binaries' RUNPATH
//! `$ORIGIN/../lib/dsview`), the standard library in `python3.X/` and its
//! extension modules in `python3.X/lib-dynload/`. The Qt packages pointed
//! the interpreter there with a wrapper script setting `PYTHONHOME` and
//! `PYTHONPATH`; here the binary finds the layout itself.

use std::ffi::{CStr, CString, c_char};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use pyo3::ffi;

/// The linked libpython's `X.Y`, read without starting it.
pub fn version() -> String {
    // SAFETY: a static string, valid before the interpreter starts.
    let v = unsafe { CStr::from_ptr(ffi::Py_GetVersion()) }.to_string_lossy();
    v.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// The bundled home for a binary in `bin_dir`: `../lib/dsview` when it
/// holds the standard library of Python `ver`.
pub fn bundled_home(bin_dir: &Path, ver: &str) -> Option<PathBuf> {
    let home = bin_dir.parent()?.join("lib/dsview");
    home.join(format!("python{ver}/encodings"))
        .is_dir()
        .then_some(home)
}

/// The module search path of a bundled home, as the wrapper's `PYTHONPATH`.
pub fn search_paths(home: &Path, ver: &str) -> Vec<PathBuf> {
    let lib = home.join(format!("python{ver}"));
    vec![lib.clone(), lib.join("lib-dynload")]
}

/// The bundled home of the running binary, unless `PYTHONHOME` is set (the
/// user's choice wins).
pub fn find_home() -> Option<PathBuf> {
    if std::env::var_os("PYTHONHOME").is_some() {
        return None;
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    bundled_home(exe.parent()?, &version())
}

fn check(status: ffi::PyStatus) -> Result<(), String> {
    // SAFETY: reads the status only.
    if unsafe { ffi::PyStatus_Exception(status) } == 0 {
        return Ok(());
    }
    let msg = if status.err_msg.is_null() {
        "unknown error".to_owned()
    } else {
        // SAFETY: CPython's static message.
        unsafe { CStr::from_ptr(status.err_msg) }
            .to_string_lossy()
            .into_owned()
    };
    Err(format!(
        "Python start-up with the bundled home failed: {msg}"
    ))
}

fn c_path(p: &Path) -> Result<CString, String> {
    CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string())
}

/// Starts the interpreter with `home` and the bundled search path, then
/// releases the GIL as PyO3's own start does.
///
/// # Safety
/// Only before the interpreter runs, from one thread.
pub unsafe fn initialize(home: &Path, ver: &str) -> Result<(), String> {
    let mut config = std::mem::MaybeUninit::<ffi::PyConfig>::uninit();
    // SAFETY: initialises `config`, which is cleared on every path below.
    unsafe { ffi::PyConfig_InitPythonConfig(config.as_mut_ptr()) };
    // SAFETY: initialised above.
    let config = unsafe { config.assume_init_mut() };
    let result = (|| {
        let h = c_path(home)?;
        // SAFETY: a field of `config`, with a NUL-terminated string.
        check(unsafe {
            ffi::PyConfig_SetBytesString(config, &mut config.home, h.as_ptr() as *const c_char)
        })?;
        config.module_search_paths_set = 1;
        for p in search_paths(home, ver) {
            let c = c_path(&p)?;
            // SAFETY: decodes before start-up as documented; freed below.
            let w = unsafe { ffi::Py_DecodeLocale(c.as_ptr(), std::ptr::null_mut()) };
            if w.is_null() {
                return Err(format!("cannot decode {}", p.display()));
            }
            // SAFETY: `w` is a valid wide string; the list copies it.
            let s = unsafe { ffi::PyWideStringList_Append(&mut config.module_search_paths, w) };
            // SAFETY: allocated by Py_DecodeLocale.
            unsafe { ffi::PyMem_RawFree(w.cast()) };
            check(s)?;
        }
        // SAFETY: the interpreter is not running (the caller's contract).
        check(unsafe { ffi::Py_InitializeFromConfig(config) })
    })();
    // SAFETY: frees what `config` owns.
    unsafe { ffi::PyConfig_Clear(config) };
    result?;
    // SAFETY: this thread holds the GIL right after start-up.
    unsafe { ffi::PyEval_SaveThread() };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_major_minor() {
        let v = version();
        let (major, minor) = v.split_once('.').unwrap();
        assert_eq!(major, "3");
        assert!(minor.parse::<u32>().is_ok(), "{v}");
    }

    #[test]
    fn the_bundled_layout_is_found_next_to_bin() {
        let root = std::env::temp_dir().join(format!("dsview-pyhome-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        assert_eq!(bundled_home(&root.join("bin"), "3.12"), None);
        std::fs::create_dir_all(root.join("lib/dsview/python3.12/encodings")).unwrap();
        let home = bundled_home(&root.join("bin"), "3.12").unwrap();
        assert_eq!(home, root.join("lib/dsview"));
        // another version's library does not count
        assert_eq!(bundled_home(&root.join("bin"), "3.10"), None);
        assert_eq!(
            search_paths(&home, "3.12"),
            [
                root.join("lib/dsview/python3.12"),
                root.join("lib/dsview/python3.12/lib-dynload")
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }
}
