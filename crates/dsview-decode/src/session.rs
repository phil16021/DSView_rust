//! Decoder sessions: port of libsigrokdecode4DSL `session.c` plus the
//! instance lifecycle of `instance.c` (creation, channels, stacking, the
//! per-stack worker thread, start/decode/end/terminate).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, RwLock, Weak};

use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::decoder::OptValue;
use crate::exception::exception_catch;
use crate::instance::{Instance, PdOutput, SRD_ERR, State, new_state, option_values, reset_state};
use crate::log::{dbg, err, warn};
use crate::module::{MetaValue, SRD_CONF_SAMPLERATE};

/// An annotation as handed to the frontend (`srd_proto_data_annotation`).
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub class: i32,
    pub ann_type: i32,
    /// Numeric field as upper-case hex ("" when the decoder gave none).
    pub hex: String,
    pub num: i64,
    /// Text lines (empty when the decoder gave only a number).
    pub texts: Vec<String>,
}

pub enum OutputData<'a> {
    Annotation(&'a Annotation),
    Binary {
        class: i32,
        data: &'a [u8],
    },
    Meta(MetaValue),
    /// OUTPUT_PYTHON (tests only; the callback runs with the GIL held).
    Python(&'a Bound<'a, PyAny>),
}

/// One decoder output (`srd_proto_data`).
pub struct Output<'a> {
    pub start: u64,
    pub end: u64,
    pub inst: &'a Instance,
    pub pdo: &'a PdOutput,
    pub data: OutputData<'a>,
}

type Callback = Arc<dyn Fn(&Output<'_>) + Send + Sync>;

pub(crate) struct SessionInner {
    di_list: Mutex<Vec<Arc<Instance>>>,
    callbacks: RwLock<Vec<(i32, Callback)>>,
}

impl SessionInner {
    pub(crate) fn has_callback(&self, output_type: i32) -> bool {
        self.find_callback(output_type).is_some()
    }

    fn find_callback(&self, output_type: i32) -> Option<Callback> {
        self.callbacks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(t, _)| *t == output_type)
            .map(|(_, c)| Arc::clone(c))
    }

    /// Calls the frontend callback for `pdo`'s output type, without the GIL
    /// (as C does) except for OUTPUT_PYTHON.
    pub(crate) fn emit(
        &self,
        py: Python<'_>,
        di: &Instance,
        pdo: &PdOutput,
        start: u64,
        end: u64,
        data: OutputData<'_>,
    ) {
        let Some(cb) = self.find_callback(pdo.output_type) else {
            return;
        };
        match data {
            OutputData::Python(obj) => cb(&Output {
                start,
                end,
                inst: di,
                pdo,
                data: OutputData::Python(obj),
            }),
            OutputData::Annotation(a) => py.detach(|| {
                cb(&Output {
                    start,
                    end,
                    inst: di,
                    pdo,
                    data: OutputData::Annotation(a),
                })
            }),
            OutputData::Binary { class, data } => py.detach(|| {
                cb(&Output {
                    start,
                    end,
                    inst: di,
                    pdo,
                    data: OutputData::Binary { class, data },
                })
            }),
            OutputData::Meta(v) => py.detach(|| {
                cb(&Output {
                    start,
                    end,
                    inst: di,
                    pdo,
                    data: OutputData::Meta(v),
                })
            }),
        }
    }
}

/// Instances by their Python object (C walks all sessions' stacks).
static REGISTRY: Mutex<Option<HashMap<usize, Weak<Instance>>>> = Mutex::new(None);

pub(crate) fn find_instance(py_obj: usize) -> Option<Arc<Instance>> {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()?
        .get(&py_obj)?
        .upgrade()
}

fn register(py_obj: usize, inst: &Arc<Instance>) {
    REGISTRY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(py_obj, Arc::downgrade(inst));
}

fn unregister(py_obj: usize) {
    if let Some(m) = REGISTRY.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(&py_obj);
    }
}

/// Why `send` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// Bad arguments (C `SRD_ERR_ARG`, no message).
    Arg(&'static str),
    /// The decoder failed (`SRD_ERR_TERM_REQ` with the error text).
    Decoder(String),
}

/// A decoder session (`struct srd_session`).
pub struct Session {
    pub(crate) inner: Arc<SessionInner>,
    /// Every instance created in this session (stacked ones included).
    all: Mutex<Vec<Arc<Instance>>>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub fn new() -> Session {
        Session {
            inner: Arc::new(SessionInner {
                di_list: Mutex::new(Vec::new()),
                callbacks: RwLock::new(Vec::new()),
            }),
            all: Mutex::new(Vec::new()),
        }
    }

    fn di_list(&self) -> Vec<Arc<Instance>> {
        self.inner
            .di_list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `srd_inst_find_by_id`: bottom instances and their direct stack.
    pub fn find_by_id(&self, inst_id: &str) -> Option<Arc<Instance>> {
        for di in self.di_list() {
            if di.inst_id == inst_id {
                return Some(di);
            }
            if let Some(t) = di.stacked().into_iter().find(|t| t.inst_id == inst_id) {
                return Some(t);
            }
        }
        None
    }

    /// `srd_inst_new`. `options` may name an instance id with key "id".
    pub fn inst_new(
        &self,
        decoder_id: &str,
        options: Option<HashMap<String, OptValue>>,
        inst_id: Option<&str>,
    ) -> Result<Arc<Instance>, String> {
        let Some(dec) = crate::decoder_by_id(decoder_id) else {
            let m = format!("Protocol decoder {decoder_id} not found.");
            err(&m);
            return Err(m);
        };
        let inst_id = match inst_id {
            Some(i) => i.to_owned(),
            None => {
                let mut i = 1;
                loop {
                    let id = format!("{decoder_id}-{i}");
                    i += 1;
                    if self.find_by_id(&id).is_none() {
                        break id;
                    }
                }
            }
        };
        let n = dec.num_channels();
        let inst = Python::attach(|py| -> Result<Instance, String> {
            let py_inst = match dec.py_dec.bind(py).call0() {
                Ok(o) => o,
                Err(e) => {
                    e.restore(py);
                    return Err(exception_catch(
                        py,
                        &format!("Failed to create {decoder_id} instance"),
                    ));
                }
            };
            if let Some(opts) = &options {
                set_options(py, &dec, &py_inst, opts, &inst_id)?;
            }
            Ok(Instance {
                decoder: Arc::clone(&dec),
                inst_id: inst_id.clone(),
                py_inst: py_inst.unbind(),
                py_pinvalues: (n > 0).then(|| {
                    // PyTuple_New(n): items set on every wait()
                    // SAFETY: a new tuple of n NULL items, filled before use.
                    unsafe {
                        Bound::from_owned_ptr(
                            py,
                            pyo3::ffi::PyTuple_New(n as pyo3::ffi::Py_ssize_t),
                        )
                    }
                    .unbind()
                }),
                channelmap: Mutex::new((0..n as i32).collect()),
                pd_output: Mutex::new(Vec::new()),
                next_di: Mutex::new(Vec::new()),
                session: Arc::downgrade(&self.inner),
                state: Mutex::new(new_state(n)),
                got_new_samples_cond: Condvar::new(),
                handled_all_samples_cond: Condvar::new(),
                thread: Mutex::new(None),
                user: std::sync::atomic::AtomicUsize::new(0),
            })
        })?;
        let inst = Arc::new(inst);
        register(
            Python::attach(|py| inst.py_inst.bind(py).as_ptr() as usize),
            &inst,
        );
        self.inner
            .di_list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&inst));
        self.all
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(&inst));
        dbg(&format!("Creating new {decoder_id} instance {inst_id}."));
        Ok(inst)
    }

    /// `srd_inst_channel_set_all`: decoder channel id -> input signal index.
    pub fn channel_set_all(&self, di: &Instance, channels: &[(String, i32)]) -> Result<(), String> {
        if channels.is_empty() {
            return Ok(());
        }
        let n = di.num_channels();
        if n == 0 {
            let m = format!(
                "Protocol decoder {} has no channels to define.",
                di.decoder.name
            );
            err(&m);
            return Err(m);
        }
        let mut map = vec![-1i32; n];
        for (id, idx) in channels {
            let Some(ch) = di
                .decoder
                .channels
                .iter()
                .chain(&di.decoder.opt_channels)
                .find(|c| &c.id == id)
            else {
                let m = format!(
                    "Protocol decoder {} has no channel '{id}'.",
                    di.decoder.name
                );
                err(&m);
                return Err(m);
            };
            map[ch.order as usize] = *idx;
        }
        for (i, ch) in di.decoder.channels.iter().enumerate() {
            if map[i] == -1 {
                let m = format!(
                    "Required channel '{}' (index {i}) was not specified.",
                    ch.id
                );
                err(&m);
                return Err(m);
            }
        }
        *di.channelmap.lock().unwrap_or_else(|e| e.into_inner()) = map;
        Ok(())
    }

    /// `srd_inst_stack`.
    pub fn stack(&self, bottom: &Arc<Instance>, top: &Arc<Instance>) {
        self.inner
            .di_list
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|d| !Arc::ptr_eq(d, top));
        let matched = bottom
            .decoder
            .outputs
            .iter()
            .any(|o| top.decoder.inputs.iter().any(|i| i == o));
        if !matched {
            warn(&format!(
                "No matching in-/output when stacking {} onto {}.",
                top.inst_id, bottom.inst_id
            ));
        }
        bottom
            .next_di
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::clone(top));
        dbg(&format!(
            "Stacking {} onto {}.",
            top.inst_id, bottom.inst_id
        ));
    }

    /// `srd_pd_output_callback_add`.
    pub fn callback_add(&self, output_type: i32, cb: impl Fn(&Output<'_>) + Send + Sync + 'static) {
        self.inner
            .callbacks
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push((output_type, Arc::new(cb)));
    }

    /// `srd_session_metadata_set(SRD_CONF_SAMPLERATE, ...)`.
    pub fn metadata_set_samplerate(&self, samplerate: u64) {
        fn send(py: Python<'_>, di: &Instance, rate: u64) {
            let inst = di.py_inst.bind(py);
            if inst.hasattr("metadata").unwrap_or(false) {
                // C ignores the result (an exception stays pending there)
                let _ = inst.call_method1("metadata", (SRD_CONF_SAMPLERATE as i64, rate));
            }
            for next in di.stacked() {
                send(py, &next, rate);
            }
        }
        Python::attach(|py| {
            for di in self.di_list() {
                send(py, &di, samplerate);
            }
        });
    }

    /// `srd_session_start`.
    pub fn start(&self) -> Result<(), String> {
        fn start(py: Python<'_>, di: &Instance) -> Result<(), String> {
            dbg(&format!("Calling start() of instance {}.", di.inst_id));
            let inst = di.py_inst.bind(py);
            if let Err(e) = inst.call_method0("start") {
                e.restore(py);
                return Err(exception_catch(
                    py,
                    &format!("Protocol decoder instance {}", di.inst_id),
                ));
            }
            {
                let mut st = di.lock();
                st.first_pos = true;
                st.abs_cur_matched = false;
                st.skip_zero = false;
            }
            let _ = inst.setattr("samplenum", 0);
            let _ = inst.setattr("matched", 0);
            for next in di.stacked() {
                start(py, &next)?;
            }
            Ok(())
        }
        Python::attach(|py| {
            for di in self.di_list() {
                start(py, &di)?;
            }
            Ok(())
        })
    }

    /// `srd_session_send`: one chunk for every decoder stack. `bufs[i]` is
    /// decoder channel i's bits from `start & !7` (None: constant chunk at
    /// level `consts[i]`). Returns when the stacks have used up the chunk.
    pub fn send(
        &self,
        start: u64,
        end: u64,
        bufs: &[Option<&[u8]>],
        consts: &[u8],
    ) -> Result<(), SendError> {
        for di in self.di_list() {
            inst_decode(&di, start, end, bufs, consts)?;
        }
        Ok(())
    }

    /// `srd_session_end`: calls `end()` (with `last_samplenum` set) on the
    /// bottom decoders, then on their stacks.
    pub fn end(&self) -> Result<(), String> {
        fn sub_end(py: Python<'_>, di: &Instance) -> Result<(), String> {
            for sub in di.stacked() {
                let inst = sub.py_inst.bind(py);
                if inst.hasattr("end").unwrap_or(false)
                    && let Err(e) = inst.call_method0("end")
                {
                    e.restore(py);
                    return Err(exception_catch(
                        py,
                        &format!("Protocol decoder instance {}", sub.inst_id),
                    ));
                }
                if !sub.stacked().is_empty() {
                    sub_end(py, &sub)?;
                }
            }
            Ok(())
        }
        let list = self.di_list();
        if list.is_empty() {
            return Err(String::new());
        }
        Python::attach(|py| {
            for di in list {
                let inst = di.py_inst.bind(py);
                if inst.hasattr("end").unwrap_or(false) {
                    let _ = inst.setattr("last_samplenum", di.cur_samplenum());
                    if let Err(e) = inst.call_method0("end") {
                        e.restore(py);
                        return Err(exception_catch(
                            py,
                            &format!("Protocol decoder instance {}", di.inst_id),
                        ));
                    }
                }
                if !di.stacked().is_empty() {
                    sub_end(py, &di)?;
                }
            }
            Ok(())
        })
    }

    /// `srd_session_terminate_reset`: stops the stacks, resets their state
    /// and calls the decoders' `reset()`. Returns C's `decoder_state` (-1
    /// when a decoder's `decode()` returned).
    pub fn terminate_reset(&self) -> i32 {
        fn reset(di: &Instance) -> i32 {
            join_decode_thread(di);
            reset_state(&mut di.lock());
            Python::attach(|py| {
                let inst = di.py_inst.bind(py);
                if inst.hasattr("reset").unwrap_or(false) {
                    dbg(&format!("Calling reset() of instance {}", di.inst_id));
                    let _ = inst.call_method0("reset");
                }
            });
            for next in di.stacked() {
                let r = reset(&next);
                if r != 0 {
                    return r;
                }
            }
            di.lock().decoder_state
        }
        for di in self.di_list() {
            let r = reset(&di);
            if r != 0 {
                return r;
            }
        }
        0
    }

    /// The bottom (frontend-fed) instances.
    pub fn instances(&self) -> Vec<Arc<Instance>> {
        self.di_list()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // srd_inst_free for the bottom instances: join, reset
        for di in self.di_list() {
            join_decode_thread(&di);
            reset_state(&mut di.lock());
        }
        for di in self.all.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            unregister(Python::attach(|py| di.py_inst.bind(py).as_ptr() as usize));
        }
    }
}

/// `srd_inst_option_set`.
fn set_options(
    py: Python<'_>,
    dec: &crate::decoder::DecoderInfo,
    py_inst: &Bound<'_, PyAny>,
    opts: &HashMap<String, OptValue>,
    inst_id: &str,
) -> Result<(), String> {
    if !dec.py_dec.bind(py).hasattr("options").unwrap_or(false) {
        if opts.is_empty() {
            return Ok(());
        }
        let m = "Protocol decoder has no options.".to_owned();
        err(&m);
        return Err(m);
    }
    let values = option_values(dec, opts, inst_id).inspect_err(|m| err(m))?;
    // a fresh dict, not the class attribute
    let d = PyDict::new(py);
    for (k, v) in values {
        let r = match v {
            OptValue::Str(s) => d.set_item(k, s),
            OptValue::Int(i) => d.set_item(k, i),
            OptValue::Float(f) => d.set_item(k, f),
        };
        if let Err(e) = r {
            e.restore(py);
            return Err(exception_catch(
                py,
                "Stray exception in srd_inst_option_set()",
            ));
        }
    }
    if let Err(e) = py_inst.setattr("options", d) {
        e.restore(py);
        return Err(exception_catch(
            py,
            "Stray exception in srd_inst_option_set()",
        ));
    }
    Ok(())
}

/// `srd_inst_decode`.
fn inst_decode(
    di: &Arc<Instance>,
    start: u64,
    end: u64,
    bufs: &[Option<&[u8]>],
    consts: &[u8],
) -> Result<(), SendError> {
    if bufs.is_empty() && di.num_channels() > 0 {
        return Err(SendError::Arg("NULL buffer pointer"));
    }
    if end <= start {
        return Err(SendError::Arg("empty buffer"));
    }
    {
        let mut st = di.lock();
        if st.first_pos {
            st.abs_cur = start;
        }
        if start != st.abs_cur || end < start {
            dbg(&format!(
                "Incorrect sample numbers: start={start}, cur={}, end={end}.",
                st.abs_cur
            ));
            return Err(SendError::Arg("incorrect sample numbers"));
        }
    }
    if crate::log::enabled(crate::log::Level::Dbg) {
        dbg(&format!(
            "Decoding: abs start sample {start}, abs end sample {end} ({} samples), instance {}.",
            end - start,
            di.inst_id
        ));
    }

    // first call: start the stack's worker thread
    {
        let mut t = di.thread.lock().unwrap_or_else(|e| e.into_inner());
        if t.is_none() {
            let worker = Arc::clone(di);
            *t = Some(
                std::thread::Builder::new()
                    .name(di.inst_id.clone())
                    .spawn(move || di_thread(&worker))
                    .expect("decoder thread"),
            );
        }
    }

    // hand over the chunk and wait until it is used up, without the GIL
    let wait = || {
        let mut st = di.lock();
        st.set_chunk(start, end, bufs, consts, end - start);
        st.got_new_samples = true;
        st.handled_all_samples = false;
        di.got_new_samples_cond.notify_one();
        while !st.handled_all_samples && !st.want_wait_terminate {
            st = di
                .handled_all_samples_cond
                .wait(st)
                .unwrap_or_else(|e| e.into_inner());
        }
        st.python_proc_error.take()
    };
    let error = detached(wait);
    match error {
        Some(e) => Err(SendError::Decoder(e)),
        None => Ok(()),
    }
}

/// Runs `f` with the GIL released, whether or not the caller holds it.
fn detached<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    Python::attach(|py| py.detach(f))
}

/// The per-stack worker (`di_thread`): runs `decode()`, which only returns
/// on an error or termination.
fn di_thread(di: &Arc<Instance>) {
    Python::attach(|py| {
        dbg(&format!("{}: Calling decode().", di.inst_id));
        let res = di.py_inst.bind(py).call_method0("decode");
        let stop = di.lock().is_task_stop_signal;
        let mut error = None;
        match res {
            // decode() returning at all is an error for DSView
            Ok(v) => {
                let msg = match v.cast::<pyo3::types::PyString>() {
                    Ok(s) => {
                        let s = s.to_string_lossy().into_owned();
                        err(&format!("python method decode() returns an error:\n {s}"));
                        s
                    }
                    Err(_) => "python method decode() returns an unknown type error!".to_owned(),
                };
                di.lock().decoder_state = SRD_ERR;
                error = Some(msg);
            }
            Err(e) => {
                if !stop {
                    e.restore(py);
                    error = Some(exception_catch(
                        py,
                        &format!("Protocol decoder instance {}: ", di.inst_id),
                    ));
                }
            }
        }
        // unblock a send() waiting for this chunk
        py.detach(|| {
            let mut st: std::sync::MutexGuard<'_, State> = di.lock();
            if error.is_some() {
                st.python_proc_error = error;
            }
            st.want_wait_terminate = true;
            st.handled_all_samples = true;
            di.handled_all_samples_cond.notify_one();
        });
        PyErr::take(py);
    });
}

/// `srd_inst_join_decode_thread`.
fn join_decode_thread(di: &Instance) {
    let handle = di.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(handle) = handle else {
        return;
    };
    {
        let mut st = di.lock();
        st.want_wait_terminate = true;
        st.is_task_stop_signal = true;
        di.got_new_samples_cond.notify_one();
    }
    detached(move || {
        let _ = handle.join();
    });
}
