//! The `sigrokdecode` Python module: the `Decoder` base class the protocol
//! decoders subclass, and the output/config constants. Port of
//! libsigrokdecode4DSL `type_decoder.c` and `module_sigrokdecode.c`.

use std::sync::Arc;

use pyo3::exceptions::{PyBaseException, PyIndexError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};

use crate::exception::exception_catch;
use crate::instance::{
    Instance, Term, TermType, current_pinvalues, process_until_match, set_skip_condition, term_type,
};
use crate::log::{dbg, err};
use crate::session::{Annotation, OutputData, find_instance};

pub const OUTPUT_ANN: i32 = 0;
pub const OUTPUT_PYTHON: i32 = 1;
pub const OUTPUT_BINARY: i32 = 2;
pub const OUTPUT_META: i32 = 3;
/// libsigrokdecode's `SRD_OUTPUT_LOGIC`, which DSView's engine lacks:
/// `[channel, bytes]` of the decoder's `logic_output_channels`.
pub const OUTPUT_LOGIC: i32 = 4;
pub const SRD_CONF_SAMPLERATE: i32 = 10000;

/// Longest numeric annotation string (`DECODE_NUM_HEX_MAX_LEN`).
const DECODE_NUM_HEX_MAX_LEN: usize = 256;

pyo3::create_exception!(
    sigrokdecode,
    Terminate,
    PyBaseException,
    "Raised by wait() when the decoder stack is torn down."
);

/// `sigrokdecode.Decoder`, the base class of every protocol decoder.
#[pyclass(subclass, name = "Decoder", module = "sigrokdecode")]
pub struct Decoder;

fn instance_of(slf: &Bound<'_, PyAny>) -> PyResult<Arc<Instance>> {
    find_instance(slf.as_ptr() as usize)
        .ok_or_else(|| pyo3::exceptions::PyException::new_err("decoder instance not found"))
}

/// PyArg_ParseTuple "K": any int, converted modulo 2^64.
fn arg_k(obj: &Bound<'_, PyAny>, pos: usize) -> PyResult<u64> {
    if !obj.is_instance_of::<PyInt>() {
        return Err(PyTypeError::new_err(format!(
            "argument {pos} must be int, not {}",
            obj.get_type().name()?
        )));
    }
    // SAFETY: obj is an int.
    let v = unsafe { pyo3::ffi::PyLong_AsUnsignedLongLongMask(obj.as_ptr()) };
    if let Some(e) = PyErr::take(obj.py()) {
        return Err(e);
    }
    Ok(v)
}

/// PyArg_ParseTuple "i".
fn arg_i(obj: &Bound<'_, PyAny>) -> PyResult<i32> {
    let v: i64 = obj.extract()?;
    i32::try_from(v).map_err(|_| {
        pyo3::exceptions::PyOverflowError::new_err(if v > 0 {
            "signed integer is greater than maximum"
        } else {
            "signed integer is less than minimum"
        })
    })
}

fn nargs_err(name: &str, want: &str, got: usize) -> PyErr {
    PyTypeError::new_err(format!("{name}() takes {want} ({got} given)"))
}

#[pymethods]
impl Decoder {
    /// `PyType_GenericNew`: any arguments are accepted and ignored.
    #[new]
    #[pyo3(signature = (*_args, **_kwargs))]
    fn new(_args: &Bound<'_, PyTuple>, _kwargs: Option<&Bound<'_, PyDict>>) -> Self {
        Decoder
    }

    /// put(startsample, endsample, output_id, data)
    #[pyo3(signature = (*args))]
    fn put(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let Ok(di) = instance_of(slf.as_any()) else {
            dbg("put(): self instance not found.");
            return Err(pyo3::exceptions::PySystemError::new_err(
                "put(): self instance not found",
            ));
        };
        if args.len() != 4 {
            return Err(nargs_err("function", "exactly 4 arguments", args.len()));
        }
        let start = arg_k(&args.get_item(0)?, 1)?;
        let end = arg_k(&args.get_item(1)?, 2)?;
        let output_id = arg_i(&args.get_item(2)?)?;
        let data = args.get_item(3)?;

        let pdo = {
            let outs = di.pd_output.lock().unwrap_or_else(|e| e.into_inner());
            match usize::try_from(output_id).ok().and_then(|i| outs.get(i)) {
                Some(p) => p.clone(),
                None => {
                    err(&format!(
                        "Protocol decoder {} submitted invalid output ID {output_id}.",
                        di.decoder.name
                    ));
                    return Err(pyo3::exceptions::PySystemError::new_err(
                        "invalid output ID",
                    ));
                }
            }
        };
        let Some(sess) = di.session.upgrade() else {
            return Ok(py.None());
        };

        match pdo.output_type {
            OUTPUT_ANN => {
                if sess.has_callback(OUTPUT_ANN)
                    && let Some(ann) = convert_annotation(&di, &data)
                {
                    sess.emit(py, &di, &pdo, start, end, OutputData::Annotation(&ann));
                }
            }
            OUTPUT_PYTHON => {
                for next in di.stacked() {
                    if let Err(e) = next
                        .py_inst
                        .bind(py)
                        .call_method1("decode", (start, end, &data))
                    {
                        e.restore(py);
                        exception_catch(py, &format!("Calling {} decode() failed", next.inst_id));
                    }
                }
                if sess.has_callback(OUTPUT_PYTHON) {
                    sess.emit(py, &di, &pdo, start, end, OutputData::Python(&data));
                }
            }
            OUTPUT_BINARY => {
                if sess.has_callback(OUTPUT_BINARY)
                    && let Some((cls, bytes)) = convert_binary(&di, &data)
                {
                    sess.emit(
                        py,
                        &di,
                        &pdo,
                        start,
                        end,
                        OutputData::Binary {
                            class: cls,
                            data: &bytes,
                        },
                    );
                }
            }
            OUTPUT_META => {
                if sess.has_callback(OUTPUT_META) {
                    match convert_meta(&pdo, &data) {
                        Ok(v) => sess.emit(py, &di, &pdo, start, end, OutputData::Meta(v)),
                        // C sets the TypeError and still returns None, so
                        // Python reports "returned a result with an exception set"
                        Err(e) => e.restore(py),
                    }
                }
            }
            OUTPUT_LOGIC => {
                if sess.has_callback(OUTPUT_LOGIC)
                    && let Some((channel, bytes)) = convert_logic(&di, &data)
                {
                    sess.emit(
                        py,
                        &di,
                        &pdo,
                        start,
                        end,
                        OutputData::Logic {
                            channel,
                            data: &bytes,
                        },
                    );
                }
            }
            t => err(&format!(
                "Protocol decoder {} submitted invalid output type {t}.",
                di.decoder.name
            )),
        }
        Ok(py.None())
    }

    /// register(output_type, proto_id=None, meta=None) -> output id
    #[pyo3(signature = (*args, **kwargs))]
    fn register(
        slf: &Bound<'_, Self>,
        args: &Bound<'_, PyTuple>,
        kwargs: Option<&Bound<'_, PyDict>>,
    ) -> PyResult<i32> {
        let di = instance_of(slf.as_any())?;
        let get = |pos: usize, key: &str| -> PyResult<Option<Bound<'_, PyAny>>> {
            if pos < args.len() {
                return Ok(Some(args.get_item(pos)?));
            }
            Ok(kwargs.map(|k| k.get_item(key)).transpose()?.flatten())
        };
        if args.len() > 3 {
            return Err(nargs_err("function", "at most 3 arguments", args.len()));
        }
        let output_type = match get(0, "output_type")? {
            Some(v) => arg_i(&v)?,
            None => {
                return Err(PyTypeError::new_err(
                    "function missing required argument 'output_type' (pos 1)",
                ));
            }
        };
        let proto_id = match get(1, "proto_id")? {
            Some(v) => v
                .cast::<PyString>()
                .map_err(|_| PyTypeError::new_err("argument 2 must be str"))?
                .to_str()?
                .to_owned(),
            None => di.inst_id.clone(),
        };
        let meta = match get(2, "meta")? {
            Some(v) => {
                let t = v
                    .cast::<PyTuple>()
                    .map_err(|_| PyTypeError::new_err("argument 3 must be 3-item sequence"))?;
                if t.len() != 3 {
                    return Err(PyTypeError::new_err(format!(
                        "argument 3 must be sequence of length 3, not {}",
                        t.len()
                    )));
                }
                let ty = t.get_item(0)?;
                let name: String = t.get_item(1)?.extract()?;
                let descr: String = t.get_item(2)?.extract()?;
                Some((ty, name, descr))
            }
            None => None,
        };

        let is_meta = output_type == OUTPUT_META;
        let meta_info = if is_meta {
            let py = slf.py();
            let Some((ty, name, descr)) = meta else {
                return Err(PyTypeError::new_err("Unsupported type."));
            };
            let is_float = if ty.is(py.get_type::<PyInt>()) {
                false
            } else if ty.is(py.get_type::<PyFloat>()) {
                true
            } else {
                return Err(PyTypeError::new_err("Unsupported type."));
            };
            Some((is_float, name, descr))
        } else {
            None
        };

        let mut outs = di.pd_output.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = outs.iter().find(|p| {
            p.output_type == output_type
                && p.proto_id == proto_id
                && (!is_meta || p.meta == meta_info)
        }) {
            return Ok(p.pdo_id);
        }
        let pdo_id = outs.len() as i32;
        outs.push(crate::instance::PdOutput {
            pdo_id,
            output_type,
            proto_id: proto_id.clone(),
            meta: meta_info,
        });
        dbg(&format!(
            "Instance {} creating new output type {} as oid {pdo_id} ({proto_id}).",
            di.inst_id,
            output_type_name(output_type)
        ));
        Ok(pdo_id)
    }

    /// wait(conditions=None) -> pin values at the matching sample
    #[pyo3(signature = (*args))]
    fn wait(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>) -> PyResult<Py<PyAny>> {
        let py = slf.py();
        let di = instance_of(slf.as_any())?;
        if args.len() > 1 {
            return Err(nargs_err("function", "at most 1 argument", args.len()));
        }
        let conds = if args.is_empty() {
            None
        } else {
            Some(args.get_item(0)?)
        };

        match set_new_condition_list(&di, conds.as_ref())? {
            ConditionList::Set => {}
            ConditionList::Empty => {
                // no conditions: return the next sample, but not past sample 0
                let mut st = di.lock();
                let skip = if !st.first_pos && st.abs_cur != 0 {
                    1
                } else if st.conditions.is_none() {
                    0
                } else {
                    1
                };
                set_skip_condition(&mut st, skip);
            }
        }

        let n = di.num_channels();
        let channelmap = di.channelmap();
        loop {
            let step = py.detach(|| {
                let mut st = di.lock();
                while !st.got_new_samples && !st.want_wait_terminate {
                    st = di
                        .got_new_samples_cond
                        .wait(st)
                        .unwrap_or_else(|e| e.into_inner());
                }
                if process_until_match(&mut st, n) {
                    return Step::Matched(
                        st.abs_cur,
                        st.match_array,
                        current_pinvalues(&st, &channelmap),
                    );
                }
                // no match: reset for the next chunk, hand back to send()
                st.got_new_samples = false;
                st.handled_all_samples = true;
                st.abs_start = 0;
                st.abs_end = 0;
                st.inbuf.clear();
                st.inbuflen = 0;
                di.handled_all_samples_cond.notify_one();
                if st.want_wait_terminate {
                    dbg(&format!("{}: wait: Will return from wait().", di.inst_id));
                    return Step::Terminate;
                }
                Step::Again
            });
            match step {
                Step::Again => continue,
                Step::Terminate => return Err(Terminate::new_err("decoder terminated")),
                Step::Matched(samplenum, matched, pins) => {
                    let inst = di.py_inst.bind(py);
                    // An error left pending by the conditions (F14) must
                    // survive the stores: on a type-cache miss CPython's
                    // _PyType_Lookup clears live exceptions, so whether it
                    // survives would depend on memory layout (F17).
                    let pending = PyErr::take(py);
                    inst.setattr(pyo3::intern!(py, "samplenum"), samplenum)?;
                    inst.setattr(pyo3::intern!(py, "matched"), matched)?;
                    if let Some(e) = pending {
                        e.restore(py);
                    }
                    return Ok(fill_pin_tuple(py, &di, &pins));
                }
            }
        }
    }

    /// has_channel(index) -> whether the frontend supplied the channel
    #[pyo3(signature = (*args))]
    fn has_channel(slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>) -> PyResult<bool> {
        let di = instance_of(slf.as_any())?;
        if args.len() != 1 {
            return Err(nargs_err("function", "exactly one argument", args.len()));
        }
        let idx = arg_i(&args.get_item(0)?)?;
        let count = di.num_channels() as i32;
        if idx < 0 || idx >= count {
            err(&format!("Invalid index {idx}, PD channel count {count}."));
            return Err(PyIndexError::new_err("invalid channel index"));
        }
        Ok(di.channelmap()[idx as usize] != -1)
    }

    /// printlog(text): logs the string (as an error, like DSView).
    #[pyo3(signature = (*args))]
    fn printlog(_slf: &Bound<'_, Self>, args: &Bound<'_, PyTuple>) -> PyResult<()> {
        let s = match args.get_item(0) {
            Ok(v) if args.len() == 1 && v.is_instance_of::<PyString>() => v,
            _ => {
                err("printlog() read param error!");
                return Err(PyTypeError::new_err("printlog() param type must be string"));
            }
        };
        err(s.cast::<PyString>()?.to_str()?);
        Ok(())
    }
}

enum Step {
    Again,
    Terminate,
    Matched(u64, u64, Vec<u8>),
}

/// A fresh tuple of the pin values per `wait()` (F13). C refilled one tuple
/// per instance with `PyTuple_SetItem`, which CPython refuses while the
/// decoder still holds the previous result: decoders written as
/// `pins = self.wait(...)` (`pjdl`, `ieee488`) then failed on their next
/// `wait()` with a SystemError.
fn fill_pin_tuple(py: Python<'_>, di: &Instance, pins: &[u8]) -> Py<PyAny> {
    if di.py_pinvalues.is_none() {
        return PyTuple::empty(py).into_any().unbind();
    }
    PyTuple::new(py, pins.iter().map(|&v| v as i64))
        .expect("int tuple")
        .into_any()
        .unbind()
}

enum ConditionList {
    Set,
    /// None, [] or {}: match the next available sample.
    Empty,
}

/// `set_new_condition_list` + `create_term_list`.
fn set_new_condition_list(
    di: &Instance,
    conds: Option<&Bound<'_, PyAny>>,
) -> PyResult<ConditionList> {
    if di.lock().want_wait_terminate {
        dbg(&format!(
            "{}: set_new_condition_list: Skip (want_term).",
            di.inst_id
        ));
        return Err(Terminate::new_err("decoder terminated"));
    }
    let list: Vec<Bound<'_, PyAny>> = match conds {
        None => return Ok(ConditionList::Empty),
        Some(c) if c.is_none() => return Ok(ConditionList::Empty),
        Some(c) => {
            if let Ok(l) = c.cast::<PyList>() {
                if l.is_empty() {
                    return Ok(ConditionList::Empty);
                }
                l.iter().collect()
            } else if let Ok(d) = c.cast::<PyDict>() {
                if d.is_empty() {
                    return Ok(ConditionList::Empty);
                }
                vec![c.clone()]
            } else {
                err("Condition list is neither a list nor a dict.");
                return Err(pyo3::exceptions::PySystemError::new_err(
                    "Condition list is neither a list nor a dict.",
                ));
            }
        }
    };

    let cur_matched = di.lock().abs_cur_matched;
    let mut out: Vec<Option<Vec<Term>>> = Vec::new();
    let mut failed = None;
    for c in &list {
        let Ok(d) = c.cast::<PyDict>() else {
            err("Condition is not a dict.");
            failed = Some("Condition is not a dict.");
            break;
        };
        let mut terms = Vec::new();
        let mut bad = None;
        for (k, v) in d.iter() {
            if k.is_instance_of::<PyInt>() {
                let Some(s) = crate::decoder::py_str_as_str(&v) else {
                    err("Failed to get the value.");
                    bad = Some("Failed to get the value.");
                    break;
                };
                terms.push(Term {
                    ty: term_type(&s),
                    channel: k.extract::<i64>().unwrap_or(-1),
                    num_samples_to_skip: 0,
                    num_samples_already_skipped: 0,
                });
            } else if k.is_instance_of::<PyString>() {
                if !v.is_instance_of::<PyInt>() {
                    err("Failed to get number of samples to skip.");
                    bad = Some("Failed to get number of samples to skip.");
                    break;
                }
                // a negative count is `skip: 0`: the sample asked for is
                // already behind (`flexray` waits for `pos - samplenum`).
                // C's PyLong_AsUnsignedLongLong left an OverflowError pending
                // instead, and a later wait() failed with a SystemError (F14)
                let n = match v.extract::<i64>() {
                    Ok(n) => n.max(0) as u64,
                    Err(_) => match v.extract::<u64>() {
                        Ok(n) => n,
                        Err(_) => {
                            err("Failed to get number of samples to skip.");
                            bad = Some("Failed to get number of samples to skip.");
                            break;
                        }
                    },
                };
                terms.push(Term {
                    ty: TermType::Skip,
                    channel: 0,
                    num_samples_to_skip: n,
                    num_samples_already_skipped: if cur_matched { u64::from(n != 0) } else { 0 },
                });
            } else {
                err("Term key is neither a string nor a number.");
                bad = Some("Term key is neither a string nor a number.");
                break;
            }
        }
        if let Some(b) = bad {
            failed = Some(b);
            break;
        }
        out.push(if terms.is_empty() { None } else { Some(terms) });
    }
    // C keeps the conditions parsed before an error
    di.lock().conditions = Some(out);
    match failed {
        Some(msg) => Err(pyo3::exceptions::PySystemError::new_err(msg)),
        None => Ok(ConditionList::Set),
    }
}

/// `convert_annotation` + `py_parse_ann_data`.
fn convert_annotation(di: &Instance, obj: &Bound<'_, PyAny>) -> Option<Annotation> {
    let name = &di.decoder.name;
    let Ok(list) = obj.cast::<PyList>() else {
        err(&format!(
            "Protocol decoder {name} submitted an annotation that is not a list"
        ));
        return None;
    };
    if list.len() != 2 {
        err(&format!(
            "Protocol decoder {name} submitted annotation list with {} elements instead of 2",
            list.len()
        ));
        return None;
    }
    let first = list.get_item(0).ok()?;
    if !first.is_instance_of::<PyInt>() {
        err(&format!(
            "Protocol decoder {name} submitted annotation list, but first element was not an integer."
        ));
        return None;
    }
    // PyLong_AsLong into an int
    let class = first.extract::<i64>().unwrap_or(-1) as i32;
    if class < 0 || class as usize >= di.decoder.ann_types.len() {
        err(&format!(
            "Protocol decoder {name} submitted data to unregistered annotation class {class}."
        ));
        return None;
    }
    let ann_type = di.decoder.ann_types[class as usize];
    let second = list.get_item(1).ok()?;
    let Ok(items) = second.cast::<PyList>() else {
        err(&format!(
            "Protocol decoder {name} submitted annotation list, but second element was not a list."
        ));
        return None;
    };
    if items.is_empty() {
        err(&format!(
            "Protocol decoder {name}, put() param, the annotation list is empty."
        ));
        return None;
    }

    let mut hex = String::new();
    let mut num: i64 = 0;
    let mut texts_obj = Vec::new();
    let mut numobj = None;
    for it in items.iter() {
        if it.is_instance_of::<PyString>() {
            texts_obj.push(it);
        } else if it.is_instance_of::<PyInt>() {
            numobj = Some(it);
        }
    }
    let malformed = || {
        err(&format!(
            "Protocol decoder {name} submitted annotation list, but second element was malformed."
        ));
    };
    if numobj.is_none() && texts_obj.is_empty() {
        err("list element type must be string or numberical");
        exception_catch(obj.py(), "Failed to obtain string item");
        malformed();
        return None;
    }
    if let Some(n) = numobj {
        // SAFETY: n is an int.
        let lv = unsafe { pyo3::ffi::PyLong_AsLongLong(n.as_ptr()) };
        PyErr::take(obj.py());
        hex = format!("{:02X}", lv as u64);
        num = lv;
    }
    let mut texts = Vec::with_capacity(texts_obj.len());
    for t in &texts_obj {
        let Ok(s) = t.cast::<PyString>().map(|s| s.to_str().map(str::to_owned)) else {
            exception_catch(obj.py(), "Failed to obtain string item");
            malformed();
            return None;
        };
        let Ok(s) = s else {
            exception_catch(obj.py(), "Failed to obtain string item");
            malformed();
            return None;
        };
        // g_strdup(PyBytes_AsString(...)): cut at a NUL
        let mut s = crate::decoder::c_string(&s);
        // "@<hex>": the numeric field; the text itself becomes "\n"
        if let Some(rest) = s.strip_prefix('@') {
            let nstr = rest.len();
            if nstr > 0 && nstr < DECODE_NUM_HEX_MAX_LEN {
                // copied byte-wise, only 'a'..'f' upper-cased
                let up: Vec<u8> = rest
                    .bytes()
                    .map(|c| {
                        if (b'a'..=b'f').contains(&c) {
                            c - 32
                        } else {
                            c
                        }
                    })
                    .collect();
                hex = String::from_utf8_lossy(&up).into_owned();
                s = "\n".to_owned();
            } else if nstr > 0 {
                s = rest.to_owned();
            }
        }
        texts.push(s);
    }
    Some(Annotation {
        class,
        ann_type,
        hex,
        num,
        texts,
    })
}

/// `convert_binary`.
fn convert_binary(di: &Instance, obj: &Bound<'_, PyAny>) -> Option<(i32, Vec<u8>)> {
    let name = &di.decoder.name;
    let Ok(list) = obj.cast::<PyList>() else {
        err(&format!(
            "Protocol decoder {name} submitted non-list for SRD_OUTPUT_BINARY."
        ));
        return None;
    };
    if list.len() != 2 {
        err(&format!(
            "Protocol decoder {name} submitted SRD_OUTPUT_BINARY list with {} elements instead of 2",
            list.len()
        ));
        return None;
    }
    let first = list.get_item(0).ok()?;
    if !first.is_instance_of::<PyInt>() {
        err(&format!(
            "Protocol decoder {name} submitted SRD_OUTPUT_BINARY list, but first element was not an integer."
        ));
        return None;
    }
    let cls = first.extract::<i64>().unwrap_or(-1) as i32;
    if cls < 0 || cls as usize >= di.decoder.binary.len() {
        err(&format!(
            "Protocol decoder {name} submitted SRD_OUTPUT_BINARY with unregistered binary class {cls}."
        ));
        return None;
    }
    let second = list.get_item(1).ok()?;
    let Ok(bytes) = second.cast::<PyBytes>() else {
        err(&format!(
            "Protocol decoder {name} submitted SRD_OUTPUT_BINARY list, but second element was not bytes."
        ));
        return None;
    };
    if bytes.as_bytes().is_empty() {
        err(&format!(
            "Protocol decoder {name} submitted SRD_OUTPUT_BINARY with empty data set."
        ));
        return None;
    }
    Some((cls, bytes.as_bytes().to_vec()))
}

/// `convert_logic` (libsigrokdecode): `[channel, bytes]`, the channel one of
/// the decoder's `logic_output_channels`.
fn convert_logic(di: &Instance, obj: &Bound<'_, PyAny>) -> Option<(i32, Vec<u8>)> {
    let name = &di.decoder.name;
    let bad = |what: &str| {
        err(&format!(
            "Protocol decoder {name} submitted SRD_OUTPUT_LOGIC {what}."
        ));
        None
    };
    let Ok(list) = obj.cast::<PyList>() else {
        return bad("that is not a list");
    };
    if list.len() != 2 {
        return bad(&format!("list with {} elements instead of 2", list.len()));
    }
    let Ok(channel) = list.get_item(0).ok()?.extract::<i64>() else {
        return bad("list, but first element was not an integer");
    };
    let channels = di
        .py_inst
        .bind(obj.py())
        .getattr("logic_output_channels")
        .ok()
        .and_then(|c| c.len().ok())
        .unwrap_or(0) as i64;
    if !(0..channels).contains(&channel) {
        return bad(&format!("with unknown logic output channel {channel}"));
    }
    let second = list.get_item(1).ok()?;
    let Ok(bytes) = second.cast::<PyBytes>() else {
        return bad("list, but second element was not bytes");
    };
    if bytes.as_bytes().is_empty() {
        return bad("with empty data set");
    }
    Some((channel as i32, bytes.as_bytes().to_vec()))
}

/// Meta value of an OUTPUT_META put.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MetaValue {
    Int(i64),
    Float(f64),
}

/// `convert_meta`.
fn convert_meta(pdo: &crate::instance::PdOutput, obj: &Bound<'_, PyAny>) -> PyResult<MetaValue> {
    match &pdo.meta {
        Some((false, _, _)) => {
            if !obj.is_instance_of::<PyInt>() {
                return Err(PyTypeError::new_err(
                    "This output was registered as 'int', but something else was passed.",
                ));
            }
            Ok(MetaValue::Int(obj.extract::<i64>()?))
        }
        Some((true, _, _)) => {
            if !obj.is_instance_of::<PyFloat>() {
                return Err(PyTypeError::new_err(
                    "This output was registered as 'float', but something else was passed.",
                ));
            }
            Ok(MetaValue::Float(obj.extract::<f64>()?))
        }
        None => Err(PyTypeError::new_err("not a meta output")),
    }
}

pub fn output_type_name(t: i32) -> &'static str {
    match t {
        OUTPUT_ANN => "OUTPUT_ANN",
        OUTPUT_PYTHON => "OUTPUT_PYTHON",
        OUTPUT_BINARY => "OUTPUT_BINARY",
        OUTPUT_META => "OUTPUT_META",
        OUTPUT_LOGIC => "OUTPUT_LOGIC",
        _ => "(invalid)",
    }
}

/// Fills the `sigrokdecode` module.
pub(crate) fn populate(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Decoder>()?;
    m.add("OUTPUT_ANN", OUTPUT_ANN)?;
    m.add("OUTPUT_PYTHON", OUTPUT_PYTHON)?;
    m.add("OUTPUT_BINARY", OUTPUT_BINARY)?;
    m.add("OUTPUT_META", OUTPUT_META)?;
    m.add("OUTPUT_LOGIC", OUTPUT_LOGIC)?;
    m.add("SRD_CONF_SAMPLERATE", SRD_CONF_SAMPLERATE)?;
    m.add("Terminate", m.py().get_type::<Terminate>())?;
    Ok(())
}

// A hand-written single-phase module (as the C engine has) instead of
// #[pymodule], which would export a `PyInit_sigrokdecode` symbol clashing
// with the C engine's, linked into the same DSView binary.
static mut MODULE_DEF: pyo3::ffi::PyModuleDef = pyo3::ffi::PyModuleDef {
    m_base: pyo3::ffi::PyModuleDef_HEAD_INIT,
    m_name: c"sigrokdecode".as_ptr(),
    m_doc: c"sigrokdecode module".as_ptr(),
    m_size: -1,
    m_methods: std::ptr::null_mut(),
    m_slots: std::ptr::null_mut(),
    m_traverse: None,
    m_clear: None,
    m_free: None,
};

/// The inittab entry for `sigrokdecode`.
pub(crate) unsafe extern "C" fn init_module() -> *mut pyo3::ffi::PyObject {
    // SAFETY: the import system calls this with the GIL held; MODULE_DEF is
    // only used by CPython from here on.
    unsafe {
        let m = pyo3::ffi::PyModule_Create(&raw mut MODULE_DEF);
        if m.is_null() {
            return m;
        }
        let py = Python::assume_attached();
        let module = Bound::from_borrowed_ptr(py, m).cast_into_unchecked::<PyModule>();
        if let Err(e) = populate(&module) {
            e.restore(py);
            exception_catch(py, "Failed to initialize module");
            pyo3::ffi::Py_DECREF(m);
            return std::ptr::null_mut();
        }
        m
    }
}

/// The `sigrokdecode.Decoder` type object.
pub(crate) fn decoder_type(py: Python<'_>) -> Bound<'_, PyType> {
    py.get_type::<Decoder>()
}
