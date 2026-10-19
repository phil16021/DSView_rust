//! Loading the Python protocol decoders: port of libsigrokdecode4DSL
//! `decoder.c` (validation, metadata) and the conversion helpers of `util.c`.
//!
//! Metadata keeps the decoders' own order. The C library builds its
//! `annotations` GSList by prepending in forward order, i.e. reversed; the C
//! seam rebuilds that list the same way.

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};

use crate::exception::exception_catch;
use crate::log::{dbg, err};

/// `srd_channel_type`: SRD_CHANNEL_COMMON.
pub const CHANNEL_COMMON: i32 = -1;

/// A decoder option value (`GVariant` of type s, x or d in C).
#[derive(Clone, Debug, PartialEq)]
pub enum OptValue {
    Str(String),
    Int(i64),
    Float(f64),
}

impl OptValue {
    /// Same variant type (C compares `GVariantType`s).
    pub fn same_type(&self, other: &OptValue) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

#[derive(Clone, Debug)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    pub desc: String,
    /// Index among required + optional channels.
    pub order: i32,
    /// `srd_channel_type`, -1 when absent.
    pub ty: i32,
    pub idn: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OptionInfo {
    pub id: String,
    pub idn: Option<String>,
    pub desc: Option<String>,
    pub def: Option<OptValue>,
    pub values: Vec<OptValue>,
}

#[derive(Clone, Debug)]
pub struct AnnotationRow {
    pub id: String,
    pub desc: String,
    pub classes: Vec<usize>,
}

/// A loaded decoder (`struct srd_decoder`).
pub struct DecoderInfo {
    pub id: String,
    pub name: String,
    pub longname: String,
    pub desc: String,
    pub license: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub tags: Vec<String>,
    pub channels: Vec<ChannelInfo>,
    pub opt_channels: Vec<ChannelInfo>,
    pub options: Vec<OptionInfo>,
    /// Annotation classes as declared (2 or 3 strings each).
    pub annotations: Vec<Vec<String>>,
    /// `ann_types`, one per annotation class.
    pub ann_types: Vec<i32>,
    pub annotation_rows: Vec<AnnotationRow>,
    pub binary: Vec<Vec<String>>,
    pub(crate) py_mod: Py<PyAny>,
    pub(crate) py_dec: Py<PyAny>,
}

impl DecoderInfo {
    pub fn num_channels(&self) -> usize {
        self.channels.len() + self.opt_channels.len()
    }

    /// The module docstring (`srd_decoder_doc_get`).
    pub fn doc(&self) -> Option<String> {
        Python::attach(|py| {
            let m = self.py_mod.bind(py);
            if !m.hasattr("__doc__").unwrap_or(false) {
                return None;
            }
            match m.getattr("__doc__") {
                Ok(d) if d.is_none() => None,
                Ok(d) => py_str_as_str(&d),
                Err(e) => {
                    e.restore(py);
                    exception_catch(py, "Failed to get docstring");
                    None
                }
            }
        })
    }
}

// ------------------------------------------------------------------ util.c

/// A Python str as C sees it after `g_strdup(PyBytes_AsString(...))`:
/// UTF-8, cut at the first NUL.
pub(crate) fn c_string(s: &str) -> String {
    match s.find('\0') {
        Some(i) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

/// `py_str_as_str`: a str object as UTF-8 (cut at a NUL, like C).
pub(crate) fn py_str_as_str(obj: &Bound<'_, PyAny>) -> Option<String> {
    if !obj.is_instance_of::<PyString>() {
        dbg("Object is not a string object.");
        return None;
    }
    match obj.cast::<PyString>().ok()?.to_str() {
        Ok(s) => Some(c_string(s)),
        Err(e) => {
            e.restore(obj.py());
            exception_catch(obj.py(), "Failed to extract string");
            None
        }
    }
}

/// `py_attr_as_str`.
fn py_attr_as_str(obj: &Bound<'_, PyAny>, attr: &str) -> Option<String> {
    if !obj.hasattr(attr).unwrap_or(false) {
        dbg(&format!("Object has no attribute '{attr}'."));
        return None;
    }
    match obj.getattr(attr) {
        Ok(v) => py_str_as_str(&v),
        Err(e) => {
            e.restore(obj.py());
            exception_catch(obj.py(), &format!("Failed to get attribute '{attr}'"));
            None
        }
    }
}

/// `py_attr_as_strlist`: the attribute must be a *list* of str.
fn py_attr_as_strlist(obj: &Bound<'_, PyAny>, attr: &str) -> Option<Vec<String>> {
    if !obj.hasattr(attr).unwrap_or(false) {
        dbg(&format!("Object has no attribute '{attr}'."));
        return None;
    }
    let list = match obj.getattr(attr) {
        Ok(v) => v,
        Err(e) => {
            e.restore(obj.py());
            exception_catch(obj.py(), &format!("Failed to get attribute '{attr}'"));
            return None;
        }
    };
    let Ok(list) = list.cast::<PyList>() else {
        dbg("Object is not a list.");
        return None;
    };
    let mut out = Vec::new();
    for (i, item) in list.iter().enumerate() {
        match py_str_as_str(&item) {
            Some(s) => out.push(s),
            None => {
                dbg(&format!("Couldn't get item {i}."));
                return None;
            }
        }
    }
    Some(out)
}

/// `py_dictitem_as_str`.
fn py_dictitem_as_str(dict: &Bound<'_, PyDict>, key: &str) -> Option<String> {
    let v = dict.get_item(key).ok()??;
    py_str_as_str(&v)
}

/// `py_strseq_to_char`: strings, and ints formatted as `%d` (C `int`).
pub(crate) fn py_strseq_to_char(seq: &Bound<'_, PyAny>) -> Option<Vec<String>> {
    let py = seq.py();
    let fail = || {
        exception_catch(py, "Failed to obtain string item");
        None
    };
    let Ok(n) = seq.len() else {
        return fail();
    };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let Ok(item) = seq.get_item(i) else {
            return fail();
        };
        if item.is_instance_of::<PyString>() {
            match item.cast::<PyString>().map(|s| s.to_str().map(c_string)) {
                Ok(Ok(s)) => out.push(s),
                _ => return fail(),
            }
        } else if item.is_instance_of::<PyInt>() {
            // PyLong_AsLong into an int, printed with %d
            let v = item.extract::<i64>().unwrap_or(-1) as i32;
            out.push(v.to_string());
        } else {
            return fail();
        }
    }
    Some(out)
}

/// `py_obj_to_variant`.
fn py_obj_to_variant(obj: &Bound<'_, PyAny>) -> Option<OptValue> {
    let py = obj.py();
    if obj.is_instance_of::<PyString>() {
        match obj
            .cast::<PyString>()
            .map(|s| s.to_str().map(str::to_owned))
        {
            Ok(Ok(s)) => Some(OptValue::Str(s)),
            _ => {
                exception_catch(py, "Failed to extract string value");
                None
            }
        }
    } else if obj.is_instance_of::<PyInt>() {
        match obj.extract::<i64>() {
            Ok(v) => Some(OptValue::Int(v)),
            Err(e) => {
                e.restore(py);
                exception_catch(py, "Failed to extract integer value");
                None
            }
        }
    } else if obj.is_instance_of::<PyFloat>() {
        match obj.extract::<f64>() {
            Ok(v) => Some(OptValue::Float(v)),
            Err(e) => {
                e.restore(py);
                exception_catch(py, "Failed to extract float value");
                None
            }
        }
    } else {
        err("Failed to extract value of unsupported type.");
        None
    }
}

// ------------------------------------------------------------------ decoder.c

fn get_channels(
    py_dec: &Bound<'_, PyAny>,
    name: &str,
    attr: &str,
    offset: usize,
) -> Result<Vec<ChannelInfo>, ()> {
    let py = py_dec.py();
    if !py_dec.hasattr(attr).unwrap_or(false) {
        return Ok(Vec::new());
    }
    let list = match py_dec.getattr(attr) {
        Ok(v) => v,
        Err(e) => {
            e.restore(py);
            exception_catch(py, &format!("Failed to get {attr} list of {name} decoder"));
            return Err(());
        }
    };
    let Ok(tuple) = list.cast::<PyTuple>() else {
        err(&format!(
            "Protocol decoder {name} {attr} attribute is not a tuple."
        ));
        return Err(());
    };
    // C walks the tuple backwards and prepends: the result is in order
    let mut out = Vec::with_capacity(tuple.len());
    for (i, entry) in tuple.iter().enumerate().rev() {
        let Ok(d) = entry.cast::<PyDict>() else {
            err(&format!(
                "Protocol decoder {name} {attr} attribute is not a list of dict elements."
            ));
            return Err(());
        };
        let (Some(id), Some(cname), Some(desc)) = (
            py_dictitem_as_str(d, "id"),
            py_dictitem_as_str(d, "name"),
            py_dictitem_as_str(d, "desc"),
        ) else {
            return Err(());
        };
        let idn = py_dictitem_as_str(d, "idn");
        // py_dictitem_to_int: SRD_ERR (-1) unless an int
        let ty = match d.get_item("type").ok().flatten() {
            Some(v) if v.is_instance_of::<PyInt>() => v.extract::<i64>().unwrap_or(-1) as i32,
            _ => -1,
        };
        out.push(ChannelInfo {
            id,
            name: cname,
            desc,
            order: (offset + i) as i32,
            ty: if ty < 0 { CHANNEL_COMMON } else { ty },
            idn,
        });
    }
    out.reverse();
    Ok(out)
}

fn get_options(py_dec: &Bound<'_, PyAny>, id: &str, name: &str) -> Result<Vec<OptionInfo>, ()> {
    let py = py_dec.py();
    if !py_dec.hasattr("options").unwrap_or(false) {
        return Ok(Vec::new());
    }
    let opts = match py_dec.getattr("options") {
        Ok(v) => v,
        Err(e) => {
            e.restore(py);
            exception_catch(py, &format!("Failed to get {name} decoder options"));
            return Err(());
        }
    };
    let Ok(tuple) = opts.cast::<PyTuple>() else {
        err(&format!(
            "Protocol decoder {id}: options attribute is not a tuple."
        ));
        return Err(());
    };
    let mut out = Vec::with_capacity(tuple.len());
    for (opt_i, opt) in tuple.iter().enumerate().rev() {
        let Ok(d) = opt.cast::<PyDict>() else {
            err(&format!(
                "Protocol decoder {name} options: each option must consist of a dictionary."
            ));
            return Err(());
        };
        let Some(py_id) = d.get_item("id").ok().flatten() else {
            err(&format!(
                "Protocol decoder {name} option {opt_i} has no ID."
            ));
            return Err(());
        };
        let Some(oid) = py_str_as_str(&py_id) else {
            return Err(());
        };
        let desc = match d.get_item("desc").ok().flatten() {
            Some(v) => Some(py_str_as_str(&v).ok_or(())?),
            None => None,
        };
        let idn = d
            .get_item("idn")
            .ok()
            .flatten()
            .and_then(|v| py_str_as_str(&v));
        let py_default = d.get_item("default").ok().flatten();
        let def = match &py_default {
            Some(v) => match py_obj_to_variant(v) {
                Some(x) => Some(x),
                None => {
                    err(&format!(
                        "Protocol decoder {name} option 'default' has invalid default value."
                    ));
                    return Err(());
                }
            },
            None => None,
        };
        let mut values = Vec::new();
        if let Some(py_values) = d.get_item("values").ok().flatten() {
            if def.is_none() {
                err(&format!("No default for option '{oid}'."));
                return Err(());
            }
            let Ok(vt) = py_values.cast::<PyTuple>() else {
                err(&format!("Option '{oid}' values should be a tuple."));
                return Err(());
            };
            for item in vt.iter().rev() {
                if let Some(pd) = &py_default
                    && !pd.get_type().is(item.get_type())
                {
                    err(&format!(
                        "All values for option '{oid}' must be of the same type as the default."
                    ));
                    return Err(());
                }
                match py_obj_to_variant(&item) {
                    Some(v) => values.push(v),
                    None => {
                        err(&format!(
                            "Protocol decoder {name} option 'values' contains invalid value."
                        ));
                        return Err(());
                    }
                }
            }
            values.reverse();
        }
        out.push(OptionInfo {
            id: oid,
            idn,
            desc,
            def,
            values,
        });
    }
    out.reverse();
    Ok(out)
}

/// Annotation classes and their `ann_types` (DSView extension: a 3-tuple's
/// first element is the type number; 2-tuples count up from 7).
fn get_annotations(
    py_dec: &Bound<'_, PyAny>,
    name: &str,
) -> Result<(Vec<Vec<String>>, Vec<i32>), ()> {
    let py = py_dec.py();
    if !py_dec.hasattr("annotations").unwrap_or(false) {
        return Ok((Vec::new(), Vec::new()));
    }
    let list = match py_dec.getattr("annotations") {
        Ok(v) => v,
        Err(e) => {
            e.restore(py);
            exception_catch(py, &format!("Failed to get {name} decoder annotations"));
            return Err(());
        }
    };
    let Ok(tuple) = list.cast::<PyTuple>() else {
        err(&format!(
            "Protocol decoder {name} annotations should be a tuple."
        ));
        return Err(());
    };
    let mut anns = Vec::new();
    let mut types = Vec::new();
    let mut ann_type: i32 = 7;
    for (i, ann) in tuple.iter().enumerate() {
        let len = ann.cast::<PyTuple>().map(|t| t.len()).ok();
        if !matches!(len, Some(2) | Some(3)) {
            err(&format!(
                "Protocol decoder {name} annotation {} should be a tuple with two or three elements.",
                i + 1
            ));
            return Err(());
        }
        let Some(pair) = py_strseq_to_char(&ann) else {
            return Err(());
        };
        if len == Some(3) {
            // C: ann_type = ann_type * 10 + (c - '0') over the bytes
            ann_type = pair[0].bytes().fold(0i32, |t, c| {
                t.wrapping_mul(10).wrapping_add(c as i32 - b'0' as i32)
            });
            types.push(ann_type);
        } else {
            types.push(ann_type);
            ann_type = ann_type.wrapping_add(1);
        }
        anns.push(pair);
    }
    Ok((anns, types))
}

fn get_annotation_rows(py_dec: &Bound<'_, PyAny>, name: &str) -> Result<Vec<AnnotationRow>, ()> {
    let py = py_dec.py();
    if !py_dec.hasattr("annotation_rows").unwrap_or(false) {
        return Ok(Vec::new());
    }
    let catch = |e: PyErr| {
        e.restore(py);
        exception_catch(py, &format!("Failed to get {name} decoder annotation rows"));
    };
    let rows = py_dec.getattr("annotation_rows").map_err(catch)?;
    let Ok(tuple) = rows.cast::<PyTuple>() else {
        err(&format!(
            "Protocol decoder {name} annotation_rows must be a tuple."
        ));
        return Err(());
    };
    let mut out = Vec::new();
    for row in tuple.iter().rev() {
        let Some(row) = row.cast::<PyTuple>().ok().filter(|t| t.len() == 3) else {
            err(&format!(
                "Protocol decoder {name} annotation_rows must contain only tuples of 3 elements."
            ));
            return Err(());
        };
        let id = py_str_as_str(&row.get_item(0).map_err(catch)?).ok_or(())?;
        let desc = py_str_as_str(&row.get_item(1).map_err(catch)?).ok_or(())?;
        let classes = row.get_item(2).map_err(catch)?;
        let Ok(classes) = classes.cast::<PyTuple>() else {
            err(&format!(
                "Protocol decoder {name} annotation_rows tuples must have a tuple of numbers as 3rd element."
            ));
            return Err(());
        };
        let mut cls = Vec::new();
        for item in classes.iter().rev() {
            if !item.is_instance_of::<PyInt>() {
                err(&format!(
                    "Protocol decoder {name} annotation row class tuple must only contain numbers."
                ));
                return Err(());
            }
            cls.push(item.extract::<usize>().map_err(catch)?);
        }
        cls.reverse();
        out.push(AnnotationRow {
            id,
            desc,
            classes: cls,
        });
    }
    out.reverse();
    Ok(out)
}

fn get_binary_classes(py_dec: &Bound<'_, PyAny>, name: &str) -> Result<Vec<Vec<String>>, ()> {
    let py = py_dec.py();
    if !py_dec.hasattr("binary").unwrap_or(false) {
        return Ok(Vec::new());
    }
    let list = match py_dec.getattr("binary") {
        Ok(v) => v,
        Err(e) => {
            e.restore(py);
            exception_catch(py, &format!("Failed to get {name} decoder binary classes"));
            return Err(());
        }
    };
    let Ok(tuple) = list.cast::<PyTuple>() else {
        err(&format!(
            "Protocol decoder {name} binary classes should be a tuple."
        ));
        return Err(());
    };
    let mut out = Vec::new();
    for b in tuple.iter().rev() {
        if !b.cast::<PyTuple>().is_ok_and(|t| t.len() == 2) {
            err(&format!(
                "Protocol decoder {name} binary classes should consist only of tuples of 2 elements."
            ));
            return Err(());
        }
        out.push(py_strseq_to_char(&b).ok_or(())?);
    }
    out.reverse();
    Ok(out)
}

fn check_method(py_dec: &Bound<'_, PyAny>, mod_name: &str, method: &str) -> bool {
    let py = py_dec.py();
    match py_dec.getattr(method) {
        Ok(m) => {
            if m.is_callable() {
                true
            } else {
                err(&format!(
                    "Protocol decoder {mod_name} Decoder class attribute '{method}' is not a method."
                ));
                false
            }
        }
        Err(e) => {
            e.restore(py);
            exception_catch(
                py,
                &format!("Protocol decoder {mod_name} Decoder class has no {method}() method"),
            );
            false
        }
    }
}

/// `srd_decoder_load`. Ok(None): the module was imported before (C returns
/// SRD_OK without adding it again).
pub(crate) fn load(py: Python<'_>, module_name: &str) -> Result<Option<DecoderInfo>, ()> {
    let sys_modules = py
        .import("sys")
        .and_then(|s| s.getattr("modules"))
        .map_err(|_| ())?;
    if sys_modules.get_item(module_name).is_ok() {
        return Ok(None);
    }

    let fail = |txt: &str| {
        err(&format!("Failed to load decoder {module_name}: {txt}"));
    };
    let except = |py: Python<'_>, txt: &str| {
        // the "common" directory is not a decoder
        if module_name != "common" {
            exception_catch(py, &format!("Failed to load decoder {module_name}: {txt}"));
        } else {
            PyErr::take(py);
        }
    };

    let py_mod = match py.import(module_name) {
        Ok(m) => m,
        Err(e) => {
            e.restore(py);
            except(py, "import by name failed");
            return Err(());
        }
    };
    let py_dec = match py_mod.getattr("Decoder") {
        Ok(d) => d,
        Err(e) => {
            e.restore(py);
            except(py, "no 'Decoder' attribute in imported module");
            return Err(());
        }
    };
    let base = crate::module::decoder_type(py);
    let is_subclass = py_dec
        .cast::<pyo3::types::PyType>()
        .map(|t| t.is_subclass(&base).unwrap_or(false))
        .unwrap_or(false);
    if !is_subclass {
        err(&format!(
            "Decoder class in protocol decoder module {module_name} is not a subclass of sigrokdecode.Decoder."
        ));
        fail("not a subclass of sigrokdecode.Decoder");
        return Err(());
    }

    let apiver = py_dec
        .getattr("api_version")
        .ok()
        .filter(|v| v.is_instance_of::<PyInt>())
        .and_then(|v| v.extract::<i64>().ok())
        .unwrap_or(0);
    if apiver != 3 {
        exception_catch(
            py,
            &format!(
                "Only PD API version 3 is supported, decoder {module_name} has version {apiver}"
            ),
        );
        fail("API version mismatch");
        return Err(());
    }

    for (m, txt) in [
        ("reset", "no 'reset()' method"),
        ("start", "no 'start()' method"),
        ("decode", "no 'decode()' method"),
    ] {
        if !check_method(&py_dec, module_name, m) {
            fail(txt);
            return Err(());
        }
    }

    let attr = |a: &str, txt: &str| py_attr_as_str(&py_dec, a).ok_or_else(|| fail(txt));
    let id = attr("id", "no 'id' attribute")?;
    let name = attr("name", "no 'name' attribute")?;
    let longname = attr("longname", "no 'longname' attribute")?;
    let desc = attr("desc", "no 'desc' attribute")?;
    let license = attr("license", "no 'license' attribute")?;
    let list = |a: &str, txt: &str| py_attr_as_strlist(&py_dec, a).ok_or_else(|| fail(txt));
    let inputs = list("inputs", "missing or malformed 'inputs' attribute")?;
    let outputs = list("outputs", "missing or malformed 'outputs' attribute")?;
    let tags = list("tags", "missing or malformed 'tags' attribute")?;

    let options = get_options(&py_dec, &id, &name).map_err(|_| fail("cannot get options"))?;
    let channels =
        get_channels(&py_dec, &name, "channels", 0).map_err(|_| fail("cannot get channels"))?;
    let opt_channels = get_channels(&py_dec, &name, "optional_channels", channels.len())
        .map_err(|_| fail("cannot get optional channels"))?;
    let (annotations, ann_types) =
        get_annotations(&py_dec, &name).map_err(|_| fail("cannot get annotations"))?;
    let annotation_rows =
        get_annotation_rows(&py_dec, &name).map_err(|_| fail("cannot get annotation rows"))?;
    let binary =
        get_binary_classes(&py_dec, &name).map_err(|_| fail("cannot get binary classes"))?;

    Ok(Some(DecoderInfo {
        id,
        name,
        longname,
        desc,
        license,
        inputs,
        outputs,
        tags,
        channels,
        opt_channels,
        options,
        annotations,
        ann_types,
        annotation_rows,
        binary,
        py_mod: py_mod.into_any().unbind(),
        py_dec: py_dec.unbind(),
    }))
}
