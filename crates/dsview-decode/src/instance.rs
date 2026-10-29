//! Decoder instances: port of libsigrokdecode4DSL `instance.c` and the
//! condition handling of `type_decoder.c`.
//!
//! As in C, each stack's bottom instance runs its Python `decode()` in a
//! worker thread. `wait()` releases the GIL and scans the chunk the session
//! thread handed over (`send`) until a condition matches; `send` returns
//! once the chunk is used up. The matching code is transliterated with its
//! quirks (constant-chunk shortcut, `skip: 0` handling, sample-0 seeding),
//! except the chunk-edge step (F32, F38).

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use pyo3::prelude::*;

use crate::decoder::{DecoderInfo, OptValue};
use crate::log::dbg;

/// `srd_initial_pin`.
pub const INITIAL_PIN_LOW: u8 = 0;
pub const INITIAL_PIN_HIGH: u8 = 1;
pub const INITIAL_PIN_SAME_AS_SAMPLE0: u8 = 2;

/// `SRD_OK` / `SRD_ERR` for `decoder_state`.
pub const SRD_OK: i32 = 0;
pub const SRD_ERR: i32 = -1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TermType {
    High,
    Low,
    Rising,
    Falling,
    Either,
    NoEdge,
    Skip,
    /// `get_term_type` returned -1 (unknown letter).
    Invalid,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Term {
    pub ty: TermType,
    pub channel: i64,
    pub num_samples_to_skip: u64,
    pub num_samples_already_skipped: u64,
}

/// One chunk of input: per decoder channel a bit-packed buffer (LSB first,
/// starting at `abs_start & !7`) or none for a constant chunk.
#[derive(Clone, Copy)]
pub(crate) struct ChunkBuf {
    ptr: *const u8,
    len: usize,
}

// SAFETY: the buffers outlive the `send` call that installs them, and `send`
// blocks until the worker thread is done with them.
unsafe impl Send for ChunkBuf {}

/// Mutable state guarded by `data_mutex` in C.
pub(crate) struct State {
    /// `condition_list`: None = NULL list; an inner None = a NULL (empty) condition.
    pub conditions: Option<Vec<Option<Vec<Term>>>>,
    pub match_array: u64,
    pub abs_start: u64,
    pub abs_end: u64,
    pub inbuf: Vec<Option<ChunkBuf>>,
    pub inbuf_const: Vec<u8>,
    pub inbuflen: u64,
    pub abs_cur: u64,
    pub abs_cur_matched: bool,
    /// The step past a match used up the chunk: the next chunk starts as if
    /// that match were still the last sample seen (F38).
    pub matched_before_chunk: bool,
    /// The chunk's first sample (`abs_start` is rounded down to a byte).
    pub chunk_start: u64,
    pub old_pins: Option<Vec<u8>>,
    pub got_new_samples: bool,
    pub handled_all_samples: bool,
    pub want_wait_terminate: bool,
    pub first_pos: bool,
    pub skip_zero: bool,
    pub decoder_state: i32,
    pub python_proc_error: Option<String>,
    pub is_task_stop_signal: bool,
}

/// A registered output (`struct srd_pd_output`).
#[derive(Clone, Debug)]
pub struct PdOutput {
    pub pdo_id: i32,
    pub output_type: i32,
    pub proto_id: String,
    /// OUTPUT_META: (is_float, name, description).
    pub meta: Option<(bool, String, String)>,
}

/// `struct srd_decoder_inst`.
pub struct Instance {
    pub decoder: Arc<DecoderInfo>,
    pub inst_id: String,
    pub(crate) py_inst: Py<PyAny>,
    /// Set when the decoder has channels; wait() returns a fresh pin
    /// tuple each time (F13; C refilled this one in place).
    pub(crate) py_pinvalues: Option<Py<PyAny>>,
    pub(crate) channelmap: Mutex<Vec<i32>>,
    pub(crate) pd_output: Mutex<Vec<PdOutput>>,
    pub(crate) next_di: Mutex<Vec<Arc<Instance>>>,
    pub(crate) session: std::sync::Weak<crate::session::SessionInner>,
    pub(crate) state: Mutex<State>,
    pub(crate) got_new_samples_cond: Condvar,
    pub(crate) handled_all_samples_cond: Condvar,
    pub(crate) thread: Mutex<Option<JoinHandle<()>>>,
    /// Opaque frontend pointer (the C seam keeps its `srd_decoder_inst` here).
    pub user: std::sync::atomic::AtomicUsize,
}

impl Instance {
    pub fn num_channels(&self) -> usize {
        self.decoder.num_channels()
    }

    /// Decoder channel -> input signal index (-1: unused optional channel).
    pub fn channelmap(&self) -> Vec<i32> {
        self.channelmap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn outputs(&self) -> Vec<PdOutput> {
        self.pd_output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn stacked(&self) -> Vec<Arc<Instance>> {
        self.next_di
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current decode position (`abs_cur_samplenum`).
    pub fn cur_samplenum(&self) -> u64 {
        self.lock().abs_cur
    }

    /// `srd_inst_initial_pins_set_all`.
    pub fn set_initial_pins(&self, pins: &[u8]) -> Result<(), String> {
        let n = self.num_channels();
        if pins.len() != n {
            return Err(format!(
                "Incorrect number of channels (need {n}, got {}).",
                pins.len()
            ));
        }
        if let Some((i, p)) = pins.iter().enumerate().find(|(_, p)| **p > 2) {
            return Err(format!("Invalid initial channel {i} pin state: {p}."));
        }
        let mut st = self.lock();
        oldpins_array_seed(&mut st, n);
        st.old_pins = Some(pins.to_vec());
        Ok(())
    }
}

pub(crate) fn new_state(num_channels: usize) -> State {
    let mut st = State {
        conditions: None,
        match_array: 0,
        abs_start: 0,
        abs_end: 0,
        inbuf: Vec::new(),
        inbuf_const: Vec::new(),
        inbuflen: 0,
        abs_cur: 0,
        abs_cur_matched: false,
        matched_before_chunk: false,
        chunk_start: 0,
        old_pins: None,
        got_new_samples: false,
        handled_all_samples: false,
        want_wait_terminate: false,
        first_pos: false,
        skip_zero: false,
        decoder_state: SRD_OK,
        python_proc_error: None,
        is_task_stop_signal: false,
    };
    // default to the initial pins being the same as in sample 0
    oldpins_array_seed(&mut st, num_channels);
    st
}

fn oldpins_array_seed(st: &mut State, n: usize) {
    if st.old_pins.is_none() {
        st.old_pins = Some(vec![INITIAL_PIN_SAME_AS_SAMPLE0; n]);
    }
}

/// `srd_inst_reset_state`.
pub(crate) fn reset_state(st: &mut State) {
    st.conditions = None;
    st.abs_start = 0;
    st.abs_end = 0;
    st.inbuf.clear();
    st.inbuflen = 0;
    st.abs_cur = 0;
    st.matched_before_chunk = false;
    st.chunk_start = 0;
    st.old_pins = None;
    st.got_new_samples = false;
    st.handled_all_samples = false;
    st.want_wait_terminate = false;
    st.decoder_state = SRD_OK;
}

// ------------------------------------------------------------------ input bits

impl State {
    /// Sample of decoder channel `ch` at `abs_cur` (C reads the buffer
    /// pointer, or the chunk's constant level when it is NULL). Bits past a
    /// short buffer read as 0.
    fn sample(&self, ch: usize) -> (u8, bool) {
        // `skip: 0` right after a match on the previous chunk's last sample
        // steps back to it; its levels are still the old pins (F38)
        if self.abs_cur.wrapping_add(1) == self.chunk_start
            && let Some(p) = &self.old_pins
        {
            return (p.get(ch).copied().unwrap_or(0) & 1, false);
        }
        match self.inbuf.get(ch).copied().flatten() {
            None => (
                u8::from(self.inbuf_const.get(ch).copied().unwrap_or(0) != 0),
                true,
            ),
            Some(b) => {
                let rel = self.abs_cur.wrapping_sub(self.abs_start);
                let byte = (rel / 8) as usize;
                let v = if byte < b.len {
                    // SAFETY: inside the buffer `send` installed.
                    unsafe { *b.ptr.add(byte) }
                } else {
                    0
                };
                ((v >> (rel % 8)) & 1, false)
            }
        }
    }

    /// Installs a chunk (`srd_inst_decode`).
    pub(crate) fn set_chunk(
        &mut self,
        start: u64,
        end: u64,
        bufs: &[Option<&[u8]>],
        consts: &[u8],
        len: u64,
    ) {
        self.abs_start = start & !7;
        self.chunk_start = start;
        self.abs_end = end;
        self.inbuf = bufs
            .iter()
            .map(|b| {
                b.map(|s| ChunkBuf {
                    ptr: s.as_ptr(),
                    len: s.len(),
                })
            })
            .collect();
        self.inbuf_const = consts.to_vec();
        self.inbuflen = len;
    }
}

/// `get_current_pinvalues`: 0/1 per channel, 0xff for an unused one.
pub(crate) fn current_pinvalues(st: &State, channelmap: &[i32]) -> Vec<u8> {
    channelmap
        .iter()
        .enumerate()
        .map(|(i, &m)| if m == -1 { 0xff } else { st.sample(i).0 })
        .collect()
}

// ------------------------------------------------------------------ matching

fn sample_matches(old: u8, sample: u8, term: &mut Term) -> bool {
    match term.ty {
        TermType::High => sample == 1,
        TermType::Low => sample == 0,
        TermType::Rising => old == 0 && sample == 1,
        TermType::Falling => old == 1 && sample == 0,
        TermType::Either => (old == 1 && sample == 0) || (old == 0 && sample == 1),
        TermType::NoEdge => (old == 0 && sample == 0) || (old == 1 && sample == 1),
        TermType::Skip => {
            if term.num_samples_already_skipped == term.num_samples_to_skip {
                return true;
            }
            term.num_samples_already_skipped += 1;
            false
        }
        TermType::Invalid => {
            crate::log::err("Unknown term type -1.");
            false
        }
    }
}

fn update_old_pins(st: &mut State, n: usize) {
    oldpins_array_seed(st, n);
    // read before replacing: the sample may be the old pins themselves (F38)
    let pins: Vec<u8> = (0..n).map(|i| st.sample(i).0).collect();
    st.old_pins = Some(pins);
}

fn update_old_pins_initial(st: &mut State, n: usize) {
    oldpins_array_seed(st, n);
    for i in 0..n {
        if st.old_pins.as_ref().map(|p| p[i]) != Some(INITIAL_PIN_SAME_AS_SAMPLE0) {
            continue;
        }
        let v = st.sample(i).0;
        if let Some(p) = st.old_pins.as_mut() {
            p[i] = v;
        }
    }
}

/// `term_matches`; the bool is C's `skip_allow`.
fn term_matches(st: &mut State, term: &mut Term) -> (bool, bool) {
    if term.ty == TermType::Skip {
        if st.abs_cur_matched && term.num_samples_to_skip == 0 {
            st.skip_zero = true;
        }
        return (sample_matches(0, 0, term), false);
    }
    let ch = term.channel as usize;
    let (sample, skip_allow) = st.sample(ch);
    let old = st
        .old_pins
        .as_ref()
        .and_then(|p| p.get(ch).copied())
        .unwrap_or(0);
    (sample_matches(old, sample, term), skip_allow)
}

/// `all_terms_match`: the bool is the failing (or last) term's `skip_allow`.
fn all_terms_match(st: &mut State, cond: &mut [Term]) -> (bool, bool) {
    let mut skip_allow = false;
    for term in cond.iter_mut() {
        let (m, sa) = term_matches(st, term);
        skip_allow = sa;
        if !m {
            return (false, skip_allow);
        }
    }
    if st.skip_zero {
        st.abs_cur = st.abs_cur.wrapping_sub(1);
        st.skip_zero = false;
    }
    (true, skip_allow)
}

fn find_match(st: &mut State, n: usize) -> bool {
    let Some(mut conds) = st.conditions.take() else {
        dbg("NULL/empty condition list, automatic match.");
        return true;
    };
    let result = find_match_in(st, n, &mut conds);
    st.conditions = Some(conds);
    result
}

fn find_match_in(st: &mut State, n: usize, conds: &mut [Option<Vec<Term>>]) -> bool {
    if !conds.iter().any(Option::is_some) {
        dbg("Only NULL conditions in list, automatic match.");
        return true;
    }
    st.match_array = 0;
    // sample 0: seed the old pins of SRD_INITIAL_PIN_SAME_AS_SAMPLE0 channels
    if st.first_pos {
        st.first_pos = false;
        update_old_pins_initial(st, n);
    }
    // a step past a match taken at the end of the previous chunk is not
    // taken again (F32), but the match still counts for `skip: 0` (F38)
    if std::mem::take(&mut st.matched_before_chunk) {
        st.abs_cur_matched = true;
    } else if st.abs_cur_matched {
        st.abs_cur += 1;
    }
    let mut all_skip_allow = true;
    while st.abs_cur < st.abs_end {
        // check all conditions, even after a match (logical OR)
        for (j, cond) in conds.iter_mut().enumerate() {
            let Some(cond) = cond else { continue };
            let (m, skip_allow) = all_terms_match(st, cond);
            if m {
                all_skip_allow = false;
                // C: `1 << j` on int
                st.match_array |= (1i32.wrapping_shl(j as u32)) as i64 as u64;
            } else {
                all_skip_allow &= skip_allow;
            }
        }
        update_old_pins(st, n);
        st.abs_cur_matched = st.match_array != 0;
        if st.abs_cur_matched {
            return true;
        }
        if all_skip_allow {
            st.abs_cur = st.abs_end;
        } else {
            st.abs_cur += 1;
        }
    }
    // the step past the last match is taken: when it used up the chunk, the
    // next chunk starts at its first sample. C kept the flag and stepped
    // again there, skipping that sample (an annotation moved by one; chunks
    // of one sample decoded nothing, F32). When no sample was looked at
    // since the match (the flag is still set), the next chunk still starts
    // right after it (F38).
    st.matched_before_chunk = st.abs_cur_matched;
    st.abs_cur_matched = false;
    false
}

/// `process_samples_until_condition_match`.
pub(crate) fn process_until_match(st: &mut State, n: usize) -> bool {
    if st.want_wait_terminate {
        return false;
    }
    loop {
        let found = find_match(st, n);
        if st.abs_cur >= st.abs_end {
            if crate::log::enabled(crate::log::Level::Dbg) {
                dbg(&format!(
                    "Done, handled all samples (abs cur {} / abs end {}).",
                    st.abs_cur, st.abs_end
                ));
            }
            return found;
        }
        if found {
            return true;
        }
    }
}

/// `set_skip_condition`.
pub(crate) fn set_skip_condition(st: &mut State, count: u64) {
    st.conditions = Some(vec![Some(vec![Term {
        ty: TermType::Skip,
        channel: 0,
        num_samples_to_skip: count,
        num_samples_already_skipped: if st.abs_cur_matched {
            u64::from(count != 0)
        } else {
            0
        },
    }])]);
}

/// `get_term_type`.
pub(crate) fn term_type(v: &str) -> TermType {
    match v.as_bytes().first() {
        Some(b'h') => TermType::High,
        Some(b'l') => TermType::Low,
        Some(b'r') => TermType::Rising,
        Some(b'f') => TermType::Falling,
        Some(b'e') => TermType::Either,
        Some(b'n') => TermType::NoEdge,
        _ => TermType::Invalid,
    }
}

/// Option dict for the Python instance (`srd_inst_option_set`).
pub(crate) fn option_values(
    decoder: &DecoderInfo,
    supplied: &std::collections::HashMap<String, OptValue>,
    inst_id: &str,
) -> Result<Vec<(String, OptValue)>, String> {
    let mut out = Vec::new();
    let mut used = 0;
    for o in &decoder.options {
        let value = match (supplied.get(&o.id), &o.def) {
            (Some(v), Some(def)) => {
                if !v.same_type(def) {
                    return Err(format!(
                        "Option '{}' should have the same type as the default value.",
                        o.id
                    ));
                }
                used += 1;
                v.clone()
            }
            (Some(_), None) => return Err(format!("Option '{}' has no default value.", o.id)),
            (None, Some(def)) => def.clone(),
            (None, None) => continue,
        };
        out.push((o.id.clone(), value));
    }
    if supplied.len() != used {
        crate::log::warn(&format!("Unknown options specified for '{inst_id}'"));
    }
    Ok(out)
}
