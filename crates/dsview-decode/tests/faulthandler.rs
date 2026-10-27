//! `DSVIEW_PY_FAULTHANDLER` turns on Python's faulthandler when the engine
//! starts (its own test binary: the variable is read once, at `init`).

use std::path::PathBuf;

use pyo3::prelude::*;

#[test]
fn faulthandler_follows_the_variable() {
    // SAFETY: the only thread of this test binary so far.
    unsafe { std::env::set_var("DSVIEW_PY_FAULTHANDLER", "30") };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/decoders");
    dsview_decode::init(&[dir.as_path()]).unwrap();
    Python::attach(|py| {
        let fh = py.import("faulthandler").unwrap();
        let on: bool = fh.call_method0("is_enabled").unwrap().extract().unwrap();
        assert!(on, "faulthandler not enabled");
        // the periodic dump is armed: cancelling it is what stops it
        fh.call_method0("cancel_dump_traceback_later").unwrap();
    });
}
