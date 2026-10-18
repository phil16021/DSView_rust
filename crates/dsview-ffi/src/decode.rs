//! `dsv_decode_*`: the Rust decoder engine behind libsigrokdecode4DSL's
//! public API (`DSVIEW_IMPL_DECODE=rust`). The C library keeps its structs
//! (the Qt GUI reads them directly) and mirrors what the engine reports:
//! decoder metadata as `DsvDecoderMeta` snapshots, instances by handle,
//! outputs through one event callback per session.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use dsview_decode::decoder::{ChannelInfo, OptionInfo};
use dsview_decode::{DecoderInfo, Instance, MetaValue, OptValue, OutputData, SendError, Session};

use crate::error::{DSV_ERR, DSV_ERR_ARG, DSV_OK, guard, set_error};

// ------------------------------------------------------------------ values

/// A decoder option value. `kind`: 0 none, 1 string (`s`), 2 int64 (`i`),
/// 3 double (`d`).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DsvValue {
    pub kind: i32,
    pub s: *const c_char,
    pub i: i64,
    pub d: f64,
}

/// A NULL-terminated list of strings with its length.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DsvStrList {
    pub items: *const *const c_char,
    pub n: u32,
}

#[repr(C)]
pub struct DsvChannelMeta {
    pub id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub order: i32,
    pub ty: i32,
    /// NULL when absent.
    pub idn: *const c_char,
}

#[repr(C)]
pub struct DsvOptionMeta {
    pub id: *const c_char,
    /// NULL when absent.
    pub idn: *const c_char,
    /// NULL when absent.
    pub desc: *const c_char,
    pub def: DsvValue,
    pub values: *const DsvValue,
    pub n_values: u32,
}

#[repr(C)]
pub struct DsvRowMeta {
    pub id: *const c_char,
    pub desc: *const c_char,
    pub classes: *const usize,
    pub n_classes: u32,
}

/// One loaded decoder (`struct srd_decoder` fields, decoder order).
#[repr(C)]
pub struct DsvDecoderMeta {
    pub id: *const c_char,
    pub name: *const c_char,
    pub longname: *const c_char,
    pub desc: *const c_char,
    pub license: *const c_char,
    pub inputs: DsvStrList,
    pub outputs: DsvStrList,
    pub tags: DsvStrList,
    pub channels: *const DsvChannelMeta,
    pub n_channels: u32,
    pub opt_channels: *const DsvChannelMeta,
    pub n_opt_channels: u32,
    pub options: *const DsvOptionMeta,
    pub n_options: u32,
    /// Annotation classes as declared (C builds its GSList reversed).
    pub annotations: *const DsvStrList,
    /// One per annotation class.
    pub ann_types: *const i32,
    pub n_annotations: u32,
    pub rows: *const DsvRowMeta,
    pub n_rows: u32,
    pub binary: *const DsvStrList,
    pub n_binary: u32,
}

/// Owns everything the `DsvDecoderMeta` pointers point into.
#[derive(Default)]
struct MetaStore {
    strings: Vec<CString>,
    ptr_lists: Vec<Vec<*const c_char>>,
    str_lists: Vec<Vec<DsvStrList>>,
    values: Vec<Vec<DsvValue>>,
    channels: Vec<Vec<DsvChannelMeta>>,
    options: Vec<Vec<DsvOptionMeta>>,
    rows: Vec<Vec<DsvRowMeta>>,
    classes: Vec<Vec<usize>>,
    types: Vec<Vec<i32>>,
    metas: Vec<DsvDecoderMeta>,
}

// SAFETY: raw pointers into the store's own heap data, only read under the lock.
unsafe impl Send for MetaStore {}

impl MetaStore {
    fn s(&mut self, v: &str) -> *const c_char {
        let c = CString::new(v.replace('\0', " ")).unwrap_or_default();
        let p = c.as_ptr();
        self.strings.push(c);
        p
    }

    fn opt_s(&mut self, v: Option<&String>) -> *const c_char {
        v.map_or(std::ptr::null(), |x| self.s(x))
    }

    fn list(&mut self, items: &[String]) -> DsvStrList {
        let mut ptrs: Vec<*const c_char> = items.iter().map(|x| self.s(x)).collect();
        ptrs.push(std::ptr::null());
        let p = ptrs.as_ptr();
        self.ptr_lists.push(ptrs);
        DsvStrList {
            items: p,
            n: items.len() as u32,
        }
    }

    fn value(&mut self, v: Option<&OptValue>) -> DsvValue {
        let mut out = DsvValue {
            kind: 0,
            s: std::ptr::null(),
            i: 0,
            d: 0.0,
        };
        match v {
            None => {}
            Some(OptValue::Str(s)) => {
                out.kind = 1;
                out.s = self.s(s);
            }
            Some(OptValue::Int(i)) => {
                out.kind = 2;
                out.i = *i;
            }
            Some(OptValue::Float(d)) => {
                out.kind = 3;
                out.d = *d;
            }
        }
        out
    }

    fn channels(&mut self, chans: &[ChannelInfo]) -> (*const DsvChannelMeta, u32) {
        let v: Vec<DsvChannelMeta> = chans
            .iter()
            .map(|c| DsvChannelMeta {
                id: self.s(&c.id),
                name: self.s(&c.name),
                desc: self.s(&c.desc),
                order: c.order,
                ty: c.ty,
                idn: self.opt_s(c.idn.as_ref()),
            })
            .collect();
        let p = v.as_ptr();
        self.channels.push(v);
        (p, chans.len() as u32)
    }

    fn options(&mut self, opts: &[OptionInfo]) -> (*const DsvOptionMeta, u32) {
        let v: Vec<DsvOptionMeta> = opts
            .iter()
            .map(|o| {
                let vals: Vec<DsvValue> = o.values.iter().map(|x| self.value(Some(x))).collect();
                let vp = vals.as_ptr();
                self.values.push(vals);
                DsvOptionMeta {
                    id: self.s(&o.id),
                    idn: self.opt_s(o.idn.as_ref()),
                    desc: self.opt_s(o.desc.as_ref()),
                    def: self.value(o.def.as_ref()),
                    values: vp,
                    n_values: o.values.len() as u32,
                }
            })
            .collect();
        let p = v.as_ptr();
        self.options.push(v);
        (p, opts.len() as u32)
    }

    fn add(&mut self, d: &DecoderInfo) {
        let (channels, n_channels) = self.channels(&d.channels);
        let (opt_channels, n_opt_channels) = self.channels(&d.opt_channels);
        let (options, n_options) = self.options(&d.options);
        let anns: Vec<DsvStrList> = d.annotations.iter().map(|a| self.list(a)).collect();
        let ap = anns.as_ptr();
        self.str_lists.push(anns);
        let types = d.ann_types.clone();
        let tp = types.as_ptr();
        self.types.push(types);
        let rows: Vec<DsvRowMeta> = d
            .annotation_rows
            .iter()
            .map(|r| {
                let cl = r.classes.clone();
                let cp = cl.as_ptr();
                self.classes.push(cl);
                DsvRowMeta {
                    id: self.s(&r.id),
                    desc: self.s(&r.desc),
                    classes: cp,
                    n_classes: r.classes.len() as u32,
                }
            })
            .collect();
        let rp = rows.as_ptr();
        self.rows.push(rows);
        let bins: Vec<DsvStrList> = d.binary.iter().map(|b| self.list(b)).collect();
        let bp = bins.as_ptr();
        self.str_lists.push(bins);
        let meta = DsvDecoderMeta {
            id: self.s(&d.id),
            name: self.s(&d.name),
            longname: self.s(&d.longname),
            desc: self.s(&d.desc),
            license: self.s(&d.license),
            inputs: self.list(&d.inputs),
            outputs: self.list(&d.outputs),
            tags: self.list(&d.tags),
            channels,
            n_channels,
            opt_channels,
            n_opt_channels,
            options,
            n_options,
            annotations: ap,
            ann_types: tp,
            n_annotations: d.annotations.len() as u32,
            rows: rp,
            n_rows: d.annotation_rows.len() as u32,
            binary: bp,
            n_binary: d.binary.len() as u32,
        };
        self.metas.push(meta);
    }
}

static META: Mutex<Option<MetaStore>> = Mutex::new(None);

/// # Safety
/// `s` NUL-terminated or NULL.
unsafe fn cstr<'a>(s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        None
    } else {
        // SAFETY: per the contract.
        unsafe { CStr::from_ptr(s) }.to_str().ok()
    }
}

// ------------------------------------------------------------------ library

/// Routes the engine's log messages to `cb` (level as in libsigrokdecode).
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_set_log(cb: Option<extern "C" fn(level: i32, msg: *const c_char)>) {
    if let Some(cb) = cb {
        dsview_decode::log::set_sink(move |level, msg| {
            if let Ok(c) = CString::new(msg.replace('\0', " ")) {
                cb(level as i32, c.as_ptr());
            }
        });
    }
}

/// Most verbose engine log level forwarded (1 error .. 5 detail; default 3).
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_set_log_level(level: i32) {
    use dsview_decode::log::Level;
    dsview_decode::log::set_level(match level {
        i32::MIN..=1 => Level::Err,
        2 => Level::Warn,
        3 => Level::Info,
        4 => Level::Dbg,
        _ => Level::Detail,
    });
}

/// `srd_init`: starts Python with the decoder directory `path` on sys.path.
///
/// # Safety
/// `path` NUL-terminated or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_init(path: *const c_char) -> i32 {
    // SAFETY: per the contract.
    let path = unsafe { cstr(path) }.map(std::path::PathBuf::from);
    guard(|| {
        let paths: Vec<&std::path::Path> = path.iter().map(|p| p.as_path()).collect();
        match dsview_decode::init(&paths) {
            Ok(()) => DSV_OK,
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

/// `srd_decoder_load_all`.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_load_all() -> i32 {
    guard(|| {
        dsview_decode::load_all();
        let mut store = MetaStore::default();
        for d in dsview_decode::decoders() {
            store.add(&d);
        }
        *META.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
        DSV_OK
    })
}

/// The loaded decoders' metadata, valid until the next load/unload.
///
/// # Safety
/// `out` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_list(out: *mut *const DsvDecoderMeta) -> u32 {
    let g = META.lock().unwrap_or_else(|e| e.into_inner());
    match g.as_ref() {
        Some(s) => {
            // SAFETY: per the contract.
            unsafe { *out = s.metas.as_ptr() };
            s.metas.len() as u32
        }
        None => 0,
    }
}

/// `srd_decoder_unload_all`.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_unload_all() {
    dsview_decode::unload_all();
    *META.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

// ------------------------------------------------------------------ sessions

/// One output, as libsigrokdecode's `srd_proto_data` needs it. Strings are
/// valid during the callback only.
#[repr(C)]
pub struct DsvDecodeEvent {
    pub start: u64,
    pub end: u64,
    /// `dsv_decode_inst_set_user` value of the instance that put.
    pub inst_user: *mut c_void,
    pub output_type: i32,
    pub pdo_id: i32,
    pub proto_id: *const c_char,
    /// OUTPUT_META registration: -1 none, 0 int, 1 float.
    pub meta_float: i32,
    pub meta_name: *const c_char,
    pub meta_descr: *const c_char,
    pub ann_class: i32,
    pub ann_type: i32,
    pub ann_hex: *const c_char,
    pub ann_num: i64,
    /// NULL when the annotation has no text.
    pub ann_texts: *const *const c_char,
    pub n_texts: u32,
    pub bin_class: i32,
    pub bin_data: *const u8,
    pub bin_len: u64,
    pub meta_int: i64,
    pub meta_double: f64,
}

type EventFn = extern "C" fn(user: *mut c_void, ev: *const DsvDecodeEvent);

/// Opaque handle owned by C between `dsv_decode_session_new` and `_free`.
pub struct DsvDecodeSession {
    sess: Session,
    insts: Mutex<HashMap<usize, Arc<Instance>>>,
}

/// An instance handle (lives as long as its session).
pub type DsvDecodeInst = c_void;

#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_session_new() -> *mut DsvDecodeSession {
    Box::into_raw(Box::new(DsvDecodeSession {
        sess: Session::new(),
        insts: Mutex::new(HashMap::new()),
    }))
}

/// Stops the session's decoder threads and frees it.
///
/// # Safety
/// `p` from `dsv_decode_session_new` (or NULL), not used after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_session_free(p: *mut DsvDecodeSession) {
    if !p.is_null() {
        // SAFETY: per the contract.
        let b = unsafe { Box::from_raw(p) };
        guard(move || {
            drop(b);
            DSV_OK
        });
    }
}

fn session<'a>(p: *mut DsvDecodeSession) -> Option<&'a DsvDecodeSession> {
    // SAFETY: handles come from dsv_decode_session_new.
    unsafe { p.as_ref() }
}

fn inst(s: &DsvDecodeSession, h: *const DsvDecodeInst) -> Option<Arc<Instance>> {
    s.insts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(h as usize))
        .cloned()
}

/// Installs the output callback for annotation, binary and meta outputs.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_set_callback(
    p: *mut DsvDecodeSession,
    cb: Option<extern "C" fn(user: *mut c_void, ev: *const DsvDecodeEvent)>,
    user: *mut c_void,
) {
    let (Some(s), Some(cb)) = (session(p), cb) else {
        return;
    };
    let user = user as usize;
    for t in [
        dsview_decode::module::OUTPUT_ANN,
        dsview_decode::module::OUTPUT_BINARY,
        dsview_decode::module::OUTPUT_META,
    ] {
        s.sess
            .callback_add(t, move |o| emit_event(cb, user as *mut c_void, o));
    }
}

fn emit_event(cb: EventFn, user: *mut c_void, o: &dsview_decode::Output<'_>) {
    let c = |s: &str| CString::new(s.replace('\0', " ")).unwrap_or_default();
    let proto = c(&o.pdo.proto_id);
    let (meta_float, mname, mdescr) = match &o.pdo.meta {
        Some((f, n, d)) => (i32::from(*f), c(n), c(d)),
        None => (-1, CString::default(), CString::default()),
    };
    let mut ev = DsvDecodeEvent {
        start: o.start,
        end: o.end,
        inst_user: o.inst.user.load(Ordering::Relaxed) as *mut c_void,
        output_type: o.pdo.output_type,
        pdo_id: o.pdo.pdo_id,
        proto_id: proto.as_ptr(),
        meta_float,
        meta_name: mname.as_ptr(),
        meta_descr: mdescr.as_ptr(),
        ann_class: 0,
        ann_type: 0,
        ann_hex: std::ptr::null(),
        ann_num: 0,
        ann_texts: std::ptr::null(),
        n_texts: 0,
        bin_class: 0,
        bin_data: std::ptr::null(),
        bin_len: 0,
        meta_int: 0,
        meta_double: 0.0,
    };
    match &o.data {
        OutputData::Annotation(a) => {
            let hex = c(&a.hex);
            let texts: Vec<CString> = a.texts.iter().map(|t| c(t)).collect();
            let mut ptrs: Vec<*const c_char> = texts.iter().map(|t| t.as_ptr()).collect();
            ptrs.push(std::ptr::null());
            ev.ann_class = a.class;
            ev.ann_type = a.ann_type;
            ev.ann_hex = hex.as_ptr();
            ev.ann_num = a.num;
            if !texts.is_empty() {
                ev.ann_texts = ptrs.as_ptr();
                ev.n_texts = texts.len() as u32;
            }
            cb(user, &ev);
        }
        OutputData::Binary { class, data } => {
            ev.bin_class = *class;
            ev.bin_data = data.as_ptr();
            ev.bin_len = data.len() as u64;
            cb(user, &ev);
        }
        OutputData::Meta(v) => {
            match v {
                MetaValue::Int(i) => ev.meta_int = *i,
                MetaValue::Float(d) => ev.meta_double = *d,
            }
            cb(user, &ev);
        }
        OutputData::Python(_) => {}
    }
}

/// `srd_inst_new`. `opts`: `n_opts` (key, value) pairs; `inst_id` may be
/// NULL. Sets `*out` to the instance handle.
///
/// # Safety
/// Strings NUL-terminated; `keys`/`vals`: `n_opts` entries; `out` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_inst_new(
    p: *mut DsvDecodeSession,
    decoder_id: *const c_char,
    keys: *const *const c_char,
    vals: *const DsvValue,
    n_opts: u32,
    with_options: bool,
    inst_id: *const c_char,
    out: *mut *const DsvDecodeInst,
) -> i32 {
    let Some(s) = session(p) else {
        return DSV_ERR_ARG;
    };
    // SAFETY: per the contract.
    let Some(id) = (unsafe { cstr(decoder_id) }) else {
        return DSV_ERR_ARG;
    };
    let mut opts = HashMap::new();
    for i in 0..n_opts as usize {
        // SAFETY: per the contract.
        let (k, v) = unsafe { (cstr(*keys.add(i)), *vals.add(i)) };
        let Some(k) = k else { continue };
        let v = match v.kind {
            // SAFETY: per the contract.
            1 => OptValue::Str(unsafe { cstr(v.s) }.unwrap_or("").to_owned()),
            2 => OptValue::Int(v.i),
            3 => OptValue::Float(v.d),
            _ => continue,
        };
        opts.insert(k.to_owned(), v);
    }
    // SAFETY: per the contract.
    let inst_id = unsafe { cstr(inst_id) }.map(str::to_owned);
    guard(|| {
        match s
            .sess
            .inst_new(id, with_options.then_some(opts), inst_id.as_deref())
        {
            Ok(i) => {
                let h = Arc::as_ptr(&i) as *const DsvDecodeInst;
                s.insts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(h as usize, i);
                // SAFETY: per the contract.
                unsafe { *out = h };
                DSV_OK
            }
            Err(e) => {
                set_error(e);
                DSV_ERR
            }
        }
    })
}

/// Copies the instance id (NUL-terminated, truncated to `cap`); returns
/// its full length.
///
/// # Safety
/// `buf`: room for `cap` bytes (may be NULL when `cap` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_inst_id(
    h: *const DsvDecodeInst,
    buf: *mut c_char,
    cap: usize,
) -> usize {
    if h.is_null() {
        return 0;
    }
    // SAFETY: handles are Arc<Instance> pointers kept alive by their session.
    let id = unsafe { &*(h as *const Instance) }.inst_id.as_bytes();
    if cap > 0 {
        let n = id.len().min(cap - 1);
        // SAFETY: n + 1 <= cap.
        unsafe {
            std::ptr::copy_nonoverlapping(id.as_ptr(), buf as *mut u8, n);
            *buf.add(n) = 0;
        }
    }
    id.len()
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_inst_set_user(h: *const DsvDecodeInst, user: *mut c_void) {
    if !h.is_null() {
        // SAFETY: handles are Arc<Instance> pointers kept alive by their session.
        unsafe { &*(h as *const Instance) }
            .user
            .store(user as usize, Ordering::Relaxed);
    }
}

/// Copies the decoder channel -> signal map (up to `cap`); returns its length.
///
/// # Safety
/// `out`: room for `cap` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_inst_channelmap(
    h: *const DsvDecodeInst,
    out: *mut i32,
    cap: u32,
) -> u32 {
    if h.is_null() {
        return 0;
    }
    // SAFETY: handles are Arc<Instance> pointers kept alive by their session.
    let map = unsafe { &*(h as *const Instance) }.channelmap();
    for (i, v) in map.iter().take(cap as usize).enumerate() {
        // SAFETY: i < cap.
        unsafe { *out.add(i) = *v };
    }
    map.len() as u32
}

/// `srd_inst_channel_set_all`.
///
/// # Safety
/// `ids`/`idx`: `n` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_channel_set_all(
    p: *mut DsvDecodeSession,
    h: *const DsvDecodeInst,
    ids: *const *const c_char,
    idx: *const i32,
    n: u32,
) -> i32 {
    let Some(s) = session(p) else {
        return DSV_ERR_ARG;
    };
    let Some(i) = inst(s, h) else {
        return DSV_ERR_ARG;
    };
    let chans: Vec<(String, i32)> = (0..n as usize)
        // SAFETY: per the contract.
        .filter_map(|k| unsafe { cstr(*ids.add(k)).map(|id| (id.to_owned(), *idx.add(k))) })
        .collect();
    guard(|| match s.sess.channel_set_all(&i, &chans) {
        Ok(()) => DSV_OK,
        Err(e) => {
            set_error(e);
            DSV_ERR
        }
    })
}

/// `srd_inst_stack`.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_stack(
    p: *mut DsvDecodeSession,
    bottom: *const DsvDecodeInst,
    top: *const DsvDecodeInst,
) -> i32 {
    let Some(s) = session(p) else {
        return DSV_ERR_ARG;
    };
    let (Some(b), Some(t)) = (inst(s, bottom), inst(s, top)) else {
        return DSV_ERR_ARG;
    };
    guard(|| {
        s.sess.stack(&b, &t);
        DSV_OK
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_set_samplerate(p: *mut DsvDecodeSession, samplerate: u64) {
    if let Some(s) = session(p) {
        guard(|| {
            s.sess.metadata_set_samplerate(samplerate);
            DSV_OK
        });
    }
}

/// `srd_session_start`; on failure the error text is `dsv_last_error()`.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_start(p: *mut DsvDecodeSession) -> i32 {
    let Some(s) = session(p) else {
        return DSV_ERR_ARG;
    };
    guard(|| match s.sess.start() {
        Ok(()) => DSV_OK,
        Err(e) => {
            set_error(e);
            DSV_ERR
        }
    })
}

/// `srd_session_send`: `bufs[i]` holds decoder channel i's bits from
/// `start & ~7` (NULL: constant at `consts[i]`), `n` channels. Returns
/// DSV_ERR_ARG for bad sample numbers, DSV_ERR with the decoder's error text.
///
/// # Safety
/// `bufs`/`consts`: `n` entries; each buffer covers `start & ~7 .. end`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dsv_decode_send(
    p: *mut DsvDecodeSession,
    start: u64,
    end: u64,
    bufs: *const *const u8,
    consts: *const u8,
    n: u32,
) -> i32 {
    let Some(s) = session(p) else {
        return DSV_ERR_ARG;
    };
    let len = (end.saturating_sub(start & !7)).div_ceil(8) as usize;
    let slices: Vec<Option<&[u8]>> = (0..n as usize)
        .map(|i| {
            // SAFETY: per the contract.
            let b = unsafe { *bufs.add(i) };
            // SAFETY: per the contract (len bytes from b).
            (!b.is_null()).then(|| unsafe { std::slice::from_raw_parts(b, len) })
        })
        .collect();
    let consts: Vec<u8> = if consts.is_null() {
        vec![0; n as usize]
    } else {
        // SAFETY: per the contract.
        unsafe { std::slice::from_raw_parts(consts, n as usize) }.to_vec()
    };
    guard(|| match s.sess.send(start, end, &slices, &consts) {
        Ok(()) => DSV_OK,
        Err(SendError::Arg(m)) => {
            set_error(m);
            DSV_ERR_ARG
        }
        Err(SendError::Decoder(m)) => {
            set_error(m);
            DSV_ERR
        }
    })
}

/// `srd_session_end`.
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_end(p: *mut DsvDecodeSession) -> i32 {
    let Some(s) = session(p) else {
        return DSV_ERR_ARG;
    };
    guard(|| match s.sess.end() {
        Ok(()) => DSV_OK,
        Err(e) => {
            set_error(e);
            DSV_ERR
        }
    })
}

/// `srd_session_terminate_reset`: C's decoder state (0 or -1).
#[unsafe(no_mangle)]
pub extern "C" fn dsv_decode_terminate_reset(p: *mut DsvDecodeSession) -> i32 {
    let Some(s) = session(p) else {
        return DSV_ERR_ARG;
    };
    let mut r = 0;
    guard(|| {
        r = s.sess.terminate_reset();
        DSV_OK
    });
    r
}
