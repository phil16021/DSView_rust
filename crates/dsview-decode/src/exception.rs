//! `srd_exception_catch` (exception.c): turns the pending Python exception
//! into DSView's error text `"<msg>:<ExceptionType>:<str(value)>"`, logs the
//! formatted traceback and clears the exception.

use pyo3::prelude::*;

use crate::log::err;

/// Formats, logs and clears the pending exception; returns the message
/// DSView shows (`"<msg>:unknown error"` when none is pending).
pub(crate) fn exception_catch(py: Python<'_>, msg: &str) -> String {
    let Some(e) = PyErr::take(py) else {
        let text = format!("{msg}:unknown error");
        err(&format!("{text}."));
        return text;
    };
    let etype = e.get_type(py);
    let name = etype
        .getattr("__name__")
        .ok()
        .and_then(|n| n.extract::<String>().ok());
    if name.is_none() {
        err("Failed to get object attribute __name__.");
    }
    let value = e
        .value(py)
        .str()
        .ok()
        .and_then(|s| s.to_str().ok().map(str::to_owned));
    if value.is_none() {
        err("Failed to stringify object.");
    }
    let name = name.unwrap_or_else(|| "(unknown exception)".to_owned());
    let text = match value {
        Some(v) => format!("{msg}:{name}:{v}"),
        None => format!("{msg}:{name}"),
    };
    err(&format!("{text}."));

    if let Some(tb) = e.traceback(py)
        && let Ok(lines) = py
            .import("traceback")
            .and_then(|m| m.call_method1("format_exception", (etype, e.value(py), tb)))
    {
        let s: String = lines
            .try_iter()
            .into_iter()
            .flatten()
            .filter_map(|l| l.ok()?.extract::<String>().ok())
            .collect();
        err(&s);
    }
    // just in case
    PyErr::take(py);
    text
}
