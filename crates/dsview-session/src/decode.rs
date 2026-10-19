//! Protocol decoding of a capture: `pv/data/decoderstack.cpp` with its rows
//! (`decode/row.cpp`), annotation storage (`decode/rowdata.cpp`,
//! `decode/annotation.cpp`) and the option typing of
//! `StoreSession::load_decoders`, on the Rust decoder engine.
//!
//! A [`DecoderStack`] holds one bottom decoder (bound to logic channels) and
//! the decoders stacked on it. [`DecoderStack::decode`] runs them over a
//! finished capture in the chunks DSView uses and files each annotation
//! under the row of the decoder and class that produced it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use dsview_decode::{DecoderInfo, OptValue, OutputData};
use dsview_formats::header::{strtod, strtoll};
use dsview_store::logic::LogicStore;
use serde_json::Value;

/// `DecoderStack::MaxChunkSize`
pub const MAX_CHUNK_SIZE: u64 = 16384;
/// `DECODER_MAX_DATA_BLOCK_LEN`: longer numeric fields are shown as text.
const MAX_DATA_BLOCK_LEN: usize = 256;

/// `IDS_MSG_DECODERSTACK_DECODE_WORK_ERROR`
pub const ERR_REQUIRED_CHANNELS: &str = "One or more required channels have not been specified";
/// `IDS_MSG_DECODERSTACK_DECODE_DATA_ERROR`
pub const ERR_CHANNEL_NOT_ENABLED: &str = "At least one of selected channels are not enabled.";
/// `IDS_MSG_DECODERSTACK_DECODE_STACK_ERROR`
pub const ERR_INSTANCE: &str = "Failed to create decoder instance";

/// One decoder of a stack (`pv::data::decode::Decoder`).
#[derive(Clone)]
pub struct StackDecoder {
    pub info: Arc<DecoderInfo>,
    /// The options set on it (the engine fills in the defaults).
    pub options: HashMap<String, OptValue>,
    /// Decoder channel id -> logic channel index (the bottom decoder only).
    pub probes: Vec<(String, i32)>,
    pub shown: bool,
}

impl StackDecoder {
    pub fn new(info: Arc<DecoderInfo>) -> StackDecoder {
        StackDecoder {
            info,
            options: HashMap::new(),
            probes: Vec::new(),
            shown: true,
        }
    }

    /// `have_probes`: the decoder takes logic channels.
    pub fn has_channels(&self) -> bool {
        !self.info.channels.is_empty() || !self.info.opt_channels.is_empty()
    }

    /// `Decoder::have_required_probes`
    fn have_required_probes(&self) -> bool {
        self.info
            .channels
            .iter()
            .all(|c| self.probes.iter().any(|(id, _)| *id == c.id))
    }

    /// `Decoder::first_probe_index`. C takes the first entry of a map keyed
    /// by channel pointer; this takes the first bound channel in declaration
    /// order (required, then optional).
    fn first_probe_index(&self) -> Option<i32> {
        self.info
            .channels
            .iter()
            .chain(&self.info.opt_channels)
            .find_map(|c| self.probes.iter().find(|(id, _)| *id == c.id))
            .map(|(_, i)| *i)
    }

    /// The channel binding as saved (`gen_decoders_json`).
    pub fn bound_probes(&self) -> Vec<(String, i32)> {
        self.info
            .channels
            .iter()
            .chain(&self.info.opt_channels)
            .filter_map(|c| self.probes.iter().find(|(id, _)| *id == c.id).cloned())
            .collect()
    }
}

/// An annotation row (`pv::data::decode::Row`): one per entry of a
/// decoder's `annotation_rows`, or one for the whole decoder when it has
/// none.
#[derive(Clone)]
pub struct Row {
    pub decoder: Arc<DecoderInfo>,
    /// Index of the row in `annotation_rows`, -1 for a decoder without rows.
    pub order: i32,
    /// Position of the decoder in the engine's load order. C orders rows by
    /// `srd_decoder` address, which follows the load order.
    load_index: usize,
    pub shown: bool,
    /// Shown in the protocol list (`_rows_lshow`): rows about bits or
    /// warnings start hidden there.
    pub list_shown: bool,
}

impl Row {
    fn row(&self) -> Option<&dsview_decode::decoder::AnnotationRow> {
        usize::try_from(self.order)
            .ok()
            .and_then(|o| self.decoder.annotation_rows.get(o))
    }

    /// `annotation_rows[i].id`, none for a decoder without rows.
    pub fn id(&self) -> Option<&str> {
        self.row().map(|r| r.id.as_str())
    }

    /// `Row::title()`: "<decoder name>: <row desc>"
    pub fn title(&self) -> String {
        match self.row() {
            Some(r) => format!("{}: {}", self.decoder.name, r.desc),
            None => self.decoder.name.clone(),
        }
    }

    /// `Row::title_id()`: "<decoder id>: <row desc>"
    pub fn title_id(&self) -> String {
        match self.row() {
            Some(r) => format!("{}: {}", self.decoder.id, r.desc),
            None => self.decoder.id.clone(),
        }
    }

    fn key(&self) -> (usize, i32) {
        (self.load_index, self.order)
    }
}

/// One stored annotation; `text` is the first text line as DSView shows it
/// (numbers in hex).
#[derive(Clone, Debug)]
pub struct Ann {
    pub start: u64,
    pub end: u64,
    /// `ann_type` (colour variant, marks on channels).
    pub ty: i32,
    /// The text lines as shown, longest first by decoder convention.
    pub lines: Arc<Vec<String>>,
    /// A numeric annotation's source: shown again in another format.
    pub num: Option<Arc<NumText>>,
}

/// Interned annotation texts: the hex lines and the numeric source.
type Texts = HashMap<String, (Arc<Vec<String>>, Option<Arc<NumText>>)>;

/// A numeric annotation (`AnnotationSourceItem` with `str_number_hex`):
/// its source lines (with `{$}` or `%s` for the number), the number in hex,
/// and the lines already made per format.
#[derive(Debug)]
pub struct NumText {
    src: Vec<String>,
    hex: String,
    made: Mutex<[Option<Arc<Vec<String>>>; 5]>,
}

impl Ann {
    /// `annotations().front()`: what the protocol list and the control API
    /// show.
    pub fn text(&self) -> &str {
        self.lines.first().map_or("", String::as_str)
    }

    /// `annotations()` with the decoder's number format.
    pub fn lines_in(&self, fmt: crate::numfmt::Format) -> Arc<Vec<String>> {
        use crate::numfmt::{Format, format_numberic};
        let Some(n) = self.num.as_ref().filter(|_| fmt != Format::Hex) else {
            return self.lines.clone();
        };
        let mut made = lock(&n.made);
        if let Some(l) = &made[fmt as usize] {
            return l.clone();
        }
        let num = format_numberic(&n.hex, fmt);
        let lines: Vec<String> = if n.src.is_empty() {
            vec![num]
        } else {
            n.src
                .iter()
                .map(|t| c_format_s(&t.replace("{$}", "%s"), &num))
                .collect()
        };
        let lines = Arc::new(lines);
        made[fmt as usize] = Some(lines.clone());
        lines
    }

    /// The first line in `fmt`.
    pub fn text_in(&self, fmt: crate::numfmt::Format) -> String {
        self.lines_in(fmt).first().cloned().unwrap_or_default()
    }
}

/// One row's annotations (`RowData`) with an index for range queries: the
/// positions sorted by start, and the longest annotation. An annotation
/// that overlaps `start..` begins after `start - longest`, so a query only
/// looks at the starts in that window.
#[derive(Clone, Default)]
struct RowData {
    anns: Vec<Ann>,
    /// Positions in `anns`, stably sorted by start sample.
    by_start: Vec<u32>,
    longest: u64,
    /// `_min_annotation`: the shortest non-zero length.
    shortest: u64,
}

impl RowData {
    fn push(&mut self, a: Ann) {
        let pos = self.anns.len() as u32;
        let len = a.end.saturating_sub(a.start);
        self.longest = self.longest.max(len);
        if len != 0 {
            self.shortest = if self.shortest == 0 {
                len
            } else {
                self.shortest.min(len)
            };
        }
        // decoders mostly emit in order: appending is the common case
        let at = if self
            .by_start
            .last()
            .is_none_or(|&l| self.anns[l as usize].start <= a.start)
        {
            self.by_start.len()
        } else {
            self.by_start
                .partition_point(|&i| self.anns[i as usize].start <= a.start)
        };
        self.anns.push(a);
        self.by_start.insert(at, pos);
    }

    /// `get_annotation_subset`: `end > start_q && start <= end_q`, in
    /// arrival order.
    fn subset(&self, start_q: u64, end_q: u64) -> Vec<Ann> {
        let from = start_q.saturating_sub(self.longest);
        let lo = self
            .by_start
            .partition_point(|&i| self.anns[i as usize].start < from);
        let hi = self
            .by_start
            .partition_point(|&i| self.anns[i as usize].start <= end_q);
        let mut hits: Vec<u32> = self.by_start[lo..hi.max(lo)]
            .iter()
            .copied()
            .filter(|&i| self.anns[i as usize].end > start_q)
            .collect();
        hits.sort_unstable();
        hits.into_iter()
            .map(|i| self.anns[i as usize].clone())
            .collect()
    }
}

#[derive(Default)]
struct Results {
    /// Per row of [`DecoderStack::rows`].
    rows: Vec<RowData>,
    count: u64,
    /// Counts the clears: with `count`, which results a view has seen.
    generation: u64,
    progress: i32,
    error: String,
    samplerate: f64,
    /// `AnnotationResTable`: the shown text per text key.
    texts: Texts,
}

/// `pv::data::DecoderStack`
pub struct DecoderStack {
    decoders: Vec<StackDecoder>,
    rows: Vec<Row>,
    /// (decoder load index, annotation class) -> row
    class_rows: HashMap<(usize, i32), usize>,
    results: Arc<Mutex<Results>>,
    stop: Arc<AtomicBool>,
    /// `Decoder::set_decode_region` for the whole stack: first and last
    /// sample (`u64::MAX`: the capture's end).
    region: Mutex<(u64, u64)>,
    /// The protocol list's number format (`DecoderStatus::m_format`).
    format: std::sync::atomic::AtomicU8,
    /// `set_rows_lshow`: rows the protocol list shows, changed by the user
    /// (else `Row::list_shown`).
    list_shown: Mutex<HashMap<usize, bool>>,
    /// `Decoder::show` and `set_rows_gshow` while the stack runs (the
    /// decoder options' show buttons): decoder and row overrides.
    dec_shown: Mutex<HashMap<usize, bool>>,
    row_shown: Mutex<HashMap<usize, bool>>,
    /// `_mark_index`: the middle sample of the annotation chosen in the
    /// protocol list, -1 for none.
    mark: std::sync::atomic::AtomicI64,
}

fn load_index(info: &DecoderInfo) -> usize {
    dsview_decode::decoders()
        .iter()
        .position(|d| d.id == info.id)
        .unwrap_or(usize::MAX)
}

impl DecoderStack {
    /// A stack of `decoders` (bottom first).
    pub fn new(decoders: Vec<StackDecoder>) -> DecoderStack {
        let mut rows: Vec<Row> = Vec::new();
        let mut class_rows = HashMap::new();
        // build_row: rows keyed by (decoder, order); a decoder stacked twice
        // shares its rows
        for d in &decoders {
            let li = load_index(&d.info);
            let mut add = |order: i32| {
                if !rows.iter().any(|r| r.key() == (li, order)) {
                    let mut r = Row {
                        decoder: d.info.clone(),
                        order,
                        load_index: li,
                        shown: true,
                        list_shown: true,
                    };
                    let t = r.title().to_lowercase();
                    r.list_shown = !(t.contains("bit") || t.contains("warning"));
                    rows.push(r);
                }
            };
            if d.info.annotation_rows.is_empty() {
                add(-1);
            }
            for (order, _) in d.info.annotation_rows.iter().enumerate() {
                add(order as i32);
            }
        }
        rows.sort_by_key(Row::key);
        for d in &decoders {
            let li = load_index(&d.info);
            for (order, r) in d.info.annotation_rows.iter().enumerate() {
                let ri = rows
                    .iter()
                    .position(|x| x.key() == (li, order as i32))
                    .expect("row added above");
                for &c in &r.classes {
                    class_rows.insert((li, c as i32), ri);
                }
            }
        }
        let n = rows.len();
        DecoderStack {
            decoders,
            rows,
            class_rows,
            results: Arc::new(Mutex::new(Results {
                rows: vec![RowData::default(); n],
                ..Results::default()
            })),
            stop: Arc::new(AtomicBool::new(false)),
            region: Mutex::new((0, u64::MAX)),
            format: std::sync::atomic::AtomicU8::new(0),
            list_shown: Mutex::new(HashMap::new()),
            dec_shown: Mutex::new(HashMap::new()),
            row_shown: Mutex::new(HashMap::new()),
            mark: std::sync::atomic::AtomicI64::new(-1),
        }
    }

    /// `get_rows_lshow()`: whether row `i` is a column of the protocol list.
    pub fn row_list_shown(&self, i: usize) -> bool {
        let over = lock(&self.list_shown).get(&i).copied();
        over.unwrap_or_else(|| self.rows.get(i).is_some_and(|r| r.list_shown))
    }

    /// `set_rows_lshow` (the protocol list options' row checkboxes).
    pub fn set_row_list_shown(&self, i: usize, shown: bool) {
        lock(&self.list_shown).insert(i, shown);
    }

    /// `Decoder::shown()` of decoder `k`.
    pub fn decoder_shown(&self, k: usize) -> bool {
        let over = lock(&self.dec_shown).get(&k).copied();
        over.unwrap_or_else(|| self.decoders.get(k).is_some_and(|d| d.shown))
    }

    /// `Decoder::show`: the trace draws decoder `k`'s rows or "Unshown".
    pub fn set_decoder_shown(&self, k: usize, shown: bool) {
        lock(&self.dec_shown).insert(k, shown);
    }

    /// `get_rows_gshow()` of row `i`: drawn in the trace.
    pub fn row_shown(&self, i: usize) -> bool {
        let over = lock(&self.row_shown).get(&i).copied();
        over.unwrap_or_else(|| self.rows.get(i).is_some_and(|r| r.shown))
    }

    /// `set_rows_gshow` while the stack runs.
    pub fn show_row(&self, i: usize, shown: bool) {
        lock(&self.row_shown).insert(i, shown);
    }

    /// The number format annotations show in.
    pub fn format(&self) -> crate::numfmt::Format {
        crate::numfmt::Format::from_index(self.format.load(Ordering::Relaxed))
    }

    pub fn set_format(&self, f: crate::numfmt::Format) {
        self.format.store(f as u8, Ordering::Relaxed);
    }

    /// `set_decode_region` on every decoder: decode `start..=end` (the
    /// decoder options' start and end cursors); `u64::MAX` is the end.
    pub fn set_region(&self, start: u64, end: u64) {
        *self.region.lock().unwrap_or_else(|e| e.into_inner()) = (start, end);
    }

    pub fn region(&self) -> (u64, u64) {
        *self.region.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn decoders(&self) -> &[StackDecoder] {
        &self.decoders
    }

    /// `get_rows_gshow()`: every row, in map order.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn set_row_shown(&mut self, i: usize, shown: bool) {
        self.rows[i].shown = shown;
    }

    /// All annotations of row `row` sorted by start (the protocol list and
    /// its export: `get_annotation_subset(row, 0, count - 1)` then sorted).
    pub fn row_sorted(&self, row: usize, sample_count: u64) -> Vec<Ann> {
        let mut v = self.annotations(row, 0, sample_count.saturating_sub(1));
        v.sort_by_key(|a| a.start);
        v
    }

    fn results(&self) -> MutexGuard<'_, Results> {
        lock(&self.results)
    }

    pub fn progress(&self) -> i32 {
        self.results().progress
    }

    /// Changes with every clear (a new run, region or capture).
    pub fn generation(&self) -> u64 {
        self.results().generation
    }

    /// `get_result_count()`: annotations received in the last run.
    pub fn result_count(&self) -> u64 {
        self.results().count
    }

    pub fn error_message(&self) -> String {
        self.results().error.clone()
    }

    /// `sample_rate()`: the rate of the last run, truncated.
    pub fn sample_rate(&self) -> u64 {
        self.results().samplerate as u64
    }

    /// `get_annotation_subset` of row `row`: annotations with
    /// `end > start && start <= end_q`, in arrival order.
    pub fn annotations(&self, row: usize, start: u64, end: u64) -> Vec<Ann> {
        self.results()
            .rows
            .get(row)
            .map(|r| r.subset(start, end))
            .unwrap_or_default()
    }

    /// (count, shortest non-zero length, longest length) of row `row`
    /// (`has_annotations`, `get_min_annotation`, `get_max_annotation`).
    pub fn row_stats(&self, row: usize) -> (usize, u64, u64) {
        self.results()
            .rows
            .get(row)
            .map_or((0, 0, 0), |r| (r.anns.len(), r.shortest, r.longest))
    }

    /// Annotations of row `row` (all of them, in arrival order).
    pub fn row_len(&self, row: usize) -> usize {
        self.results().rows.get(row).map_or(0, |r| r.anns.len())
    }

    /// The `index`-th annotation of row `row` (`list_annotation`).
    pub fn annotation_at(&self, row: usize, index: usize) -> Option<Ann> {
        self.results().rows.get(row)?.anns.get(index).cloned()
    }

    /// Asks a running [`decode`](Self::decode) to stop.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// `set_mark_index`
    pub fn set_mark(&self, index: i64) {
        self.mark.store(index, Ordering::Relaxed);
    }

    /// `get_mark_index`
    pub fn mark(&self) -> i64 {
        self.mark.load(Ordering::Relaxed)
    }

    /// `DecoderStack::clear()`/`init()`: drops the results (progress stays).
    pub fn clear(&self) {
        self.set_mark(-1);
        let mut r = self.results();
        let n = r.rows.len();
        r.rows = vec![RowData::default(); n];
        r.count = 0;
        r.generation += 1;
        r.error.clear();
        r.texts.clear();
    }

    fn set_error(&self, e: &str) {
        sr_err!("ERROR:{e}");
        self.results().error = e.to_owned();
    }

    /// `begin_decode_work` for a finished capture: decodes samples
    /// `0..=min(last_sample, captured - 1)` of `store`. `has_signal(i)`:
    /// logic channel `i` is shown (a decoder reads the capture of its first
    /// bound channel's signal).
    ///
    /// While `capture_ended` is false the capture is still running (a
    /// stream shown as it arrives): the decoder waits for its data and stops
    /// at the capture's end (`set_capture_end_flag`).
    pub fn decode(
        &self,
        store: &Mutex<LogicStore>,
        has_signal: &dyn Fn(i32) -> bool,
        samplerate: f64,
        last_sample: u64,
        capture_ended: &AtomicBool,
    ) {
        self.stop.store(false, Ordering::SeqCst);
        self.clear();

        if !self.decoders.iter().all(StackDecoder::have_required_probes) {
            self.set_error(ERR_REQUIRED_CHANNELS);
            return;
        }
        let have_snapshot = self
            .decoders
            .iter()
            .filter(|d| d.has_channels())
            .find_map(StackDecoder::first_probe_index)
            .is_some_and(has_signal);
        if !have_snapshot {
            self.set_error(ERR_REQUIRED_CHANNELS);
            return;
        }
        let live = !capture_ended.load(Ordering::Acquire);
        let ring = {
            let s = store.lock().unwrap_or_else(|e| e.into_inner());
            if !live && s.empty() {
                sr_err!("ERROR:Decode data is empty.");
                return;
            }
            s.ring_sample_count()
        };
        if samplerate == 0.0 {
            sr_err!("ERROR:Decode data got an invalid sample rate.");
            return;
        }
        self.results().samplerate = samplerate;
        // a live decode runs to the capture's limit; a finished one to its data
        let last = if live {
            last_sample
        } else {
            last_sample.min(ring.wrapping_sub(1))
        };
        // execute_decode_stack(): the decoders' region inside the data
        let (start, end) = self.region();
        self.execute(store, start, end.min(last), samplerate, capture_ended);
    }

    /// `execute_decode_stack` + `decode_data`
    fn execute(
        &self,
        store: &Mutex<LogicStore>,
        decode_start: u64,
        decode_end: u64,
        rate: f64,
        capture_ended: &AtomicBool,
    ) {
        let session = dsview_decode::Session::new();
        let mut prev: Option<Arc<dsview_decode::Instance>> = None;
        let mut bottom = None;
        for d in &self.decoders {
            let Ok(di) = session.inst_new(&d.info.id, Some(d.options.clone()), None) else {
                self.set_error(ERR_INSTANCE);
                return;
            };
            let _ = session.channel_set_all(&di, &d.probes);
            match &prev {
                Some(p) => session.stack(p, &di),
                None => bottom = Some(di.clone()),
            }
            prev = Some(di);
        }
        session.metadata_set_samplerate(rate as u64);

        let class_rows = self.class_rows.clone();
        let rowless = self.rows_by_decoder();
        let ids: HashMap<String, usize> = self
            .decoders
            .iter()
            .map(|d| (d.info.id.clone(), load_index(&d.info)))
            .collect();
        let (results, stop) = (self.results.clone(), self.stop.clone());
        session.callback_add(dsview_decode::module::OUTPUT_ANN, move |o| {
            if let OutputData::Annotation(a) = &o.data
                && !stop.load(Ordering::Relaxed)
            {
                let li = ids.get(&o.inst.decoder.id).copied().unwrap_or(usize::MAX);
                annotation(&results, &class_rows, &rowless, li, o.start, o.end, a);
            }
        });
        match session.start() {
            Ok(()) => self.decode_data(
                store,
                &session,
                bottom.as_ref(),
                decode_start,
                decode_end,
                capture_ended,
            ),
            Err(e) if !e.is_empty() => self.results().error = e,
            Err(_) => {}
        }
        drop(session);
    }

    /// (decoder load index -> its rowless row), for classes without a row.
    fn rows_by_decoder(&self) -> HashMap<usize, usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.order < 0)
            .map(|(i, r)| (r.load_index, i))
            .collect()
    }

    fn decode_data(
        &self,
        store: &Mutex<LogicStore>,
        session: &dsview_decode::Session,
        bottom: Option<&Arc<dsview_decode::Instance>>,
        decode_start: u64,
        decode_end: u64,
        capture_ended: &AtomicBool,
    ) {
        let map = bottom.map(|b| b.channelmap()).unwrap_or_default();
        let mut i = decode_start;
        let mut end_index = decode_end;
        let mut decoded = 0u64;
        let mut end_time = false;
        let mut failed = false;
        let mut checked_end = false;
        // the leaf each decoder channel reads (kept while a live capture
        // still frees constant leaves)
        let mut pinned: Vec<*const u8> = vec![std::ptr::null(); map.len()];
        self.results().progress = 0;

        while i <= end_index && !self.stop.load(Ordering::Relaxed) {
            if capture_ended.load(Ordering::Acquire) {
                if !checked_end {
                    checked_end = true;
                    let count = store
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .ring_sample_count();
                    if count == 0 {
                        sr_info!("Have no data to decode.");
                        return;
                    }
                    if end_index >= count {
                        end_index = count - 1;
                    }
                    if i >= count {
                        sr_info!("ERROR: the decoding sample index is out of range.");
                        break;
                    }
                }
            } else {
                let count = store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .ring_sample_count();
                if i >= count {
                    // wait for the data
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
            }
            let mut s = store.lock().unwrap_or_else(|e| e.into_inner());
            let mut chunk_end = end_index;
            let mut ptrs: Vec<(*const u8, usize)> = Vec::with_capacity(map.len());
            let mut consts = Vec::with_capacity(map.len());
            for &sig in &map {
                if sig == -1 {
                    ptrs.push((std::ptr::null(), 0));
                    consts.push(0);
                    continue;
                }
                if !s.has_data(sig) {
                    drop(s);
                    self.set_error(ERR_CHANNEL_NOT_ENABLED);
                    return;
                }
                let run = s.get_samples(i, sig);
                chunk_end = run.end_sample;
                let k = ptrs.len();
                if !s.is_able_free() && pinned[k] != run.lbp {
                    if !pinned[k].is_null() {
                        s.free_decode_lpb(pinned[k]);
                    }
                    pinned[k] = run.lbp;
                }
                let len = (run.end_sample.saturating_sub(i & !7)).div_ceil(8) as usize;
                ptrs.push((run.data, len));
                consts.push(u8::from(s.get_sample(i, sig)));
            }
            if chunk_end >= end_index {
                chunk_end = end_index + 1;
            }
            if chunk_end - i > MAX_CHUNK_SIZE {
                chunk_end = i + MAX_CHUNK_SIZE;
            }
            end_time = chunk_end > end_index;
            // SAFETY: the leaves stay put while the store is locked.
            let bufs: Vec<Option<&[u8]>> = ptrs
                .iter()
                .map(|&(p, n)| (!p.is_null()).then(|| unsafe { std::slice::from_raw_parts(p, n) }))
                .collect();
            let sent = session.send(i, chunk_end, &bufs, &consts);
            drop(bufs);
            drop(s);
            if let Err(e) = sent {
                if let dsview_decode::SendError::Decoder(m) = e {
                    self.results().error = m;
                }
                failed = true;
                break;
            }
            decoded += chunk_end - i;
            // C divides by end_index (and faults on a one-sample capture)
            self.results().progress = (decoded * 100 / end_index.max(1)) as i32;
            i = chunk_end;
        }
        self.results().progress = 100;

        if !failed
            && end_time
            && let Err(e) = session.end()
            && !e.is_empty()
        {
            sr_err!("Failed to call srd_session_end:{e}");
            self.results().error = e;
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `annotation_callback`: files an annotation under the row of its decoder
/// and class (a class no row lists is counted but dropped).
fn annotation(
    results: &Mutex<Results>,
    class_rows: &HashMap<(usize, i32), usize>,
    rowless: &HashMap<usize, usize>,
    li: usize,
    start: u64,
    end: u64,
    a: &dsview_decode::Annotation,
) {
    let mut r = lock(results);
    r.count += 1;
    let Some(&row) = class_rows.get(&(li, a.class)).or_else(|| rowless.get(&li)) else {
        sr_err!(
            "Unexpected annotation: decoder = {li}, format = {}",
            a.class
        );
        return;
    };
    let (text, num) = shown_text(&mut r.texts, a);
    r.rows[row].push(Ann {
        start,
        end,
        ty: a.ann_type,
        lines: text,
        num,
    });
}

/// The first text line of an annotation as `Annotation::annotations()`
/// shows it, interned by the same key as `AnnotationResTable::MakeIndex`
/// (all texts plus the number), so equal keys share the first text seen.
fn shown_text(
    table: &mut Texts,
    a: &dsview_decode::Annotation,
) -> (Arc<Vec<String>>, Option<Arc<NumText>>) {
    let lines: Vec<&str> = a
        .texts
        .iter()
        .map(String::as_str)
        .filter(|t| !t.starts_with('\n'))
        .collect();
    let mut key: String = lines.concat();
    key.push_str(&a.hex);
    if let Some(t) = table.get(&key) {
        return t.clone();
    }
    let numeric = !a.hex.is_empty() && a.hex.len() <= MAX_DATA_BLOCK_LEN;
    let shown: Vec<String> = if numeric {
        // the protocol list's default format is hex: the number as given
        if lines.is_empty() {
            vec![a.hex.clone()]
        } else {
            lines
                .iter()
                .map(|t| c_format_s(&t.replace("{$}", "%s"), &a.hex))
                .collect()
        }
    } else {
        lines.iter().map(|t| (*t).to_owned()).collect()
    };
    let shown = Arc::new(shown);
    let num = numeric.then(|| {
        Arc::new(NumText {
            src: lines.iter().map(|t| (*t).to_owned()).collect(),
            hex: a.hex.clone(),
            made: Mutex::new(Default::default()),
        })
    });
    table.insert(key, (shown.clone(), num.clone()));
    (shown, num)
}

/// `sprintf(fmt, s)` for the conversions decoder texts use: the first `%s`
/// takes `s`, `%%` is a percent sign; anything else is kept as written
/// (C would read a missing argument).
fn c_format_s(fmt: &str, s: &str) -> String {
    let mut out = String::with_capacity(fmt.len() + s.len());
    let mut used = false;
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c == '%' {
            match it.peek() {
                Some('s') if !used => {
                    it.next();
                    out.push_str(s);
                    used = true;
                    continue;
                }
                Some('%') => {
                    it.next();
                    out.push('%');
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

// ------------------------------------------------------------ saved stacks

/// One decoder entry of a saved stack, as `StoreSession::load_decoders`
/// reads it.
pub struct SavedDecoder<'a> {
    pub id: &'a str,
    pub options: Option<&'a serde_json::Map<String, Value>>,
}

/// Option values typed by each option's default, as `load_decoders` does:
/// doubles from numbers or numeric strings, integers as 32-bit, strings as
/// given (a number for a string option becomes ""). Unknown options and
/// other types are skipped.
pub fn typed_options(
    info: &DecoderInfo,
    options: Option<&serde_json::Map<String, Value>>,
) -> HashMap<String, OptValue> {
    let mut out = HashMap::new();
    let Some(options) = options else { return out };
    for opt in &info.options {
        let Some(v) = options.get(&opt.id) else {
            continue;
        };
        let (vs, num) = match v {
            Value::String(s) => (s.as_str(), 0.0),
            Value::Number(n) => ("", n.as_f64().unwrap_or(0.0)),
            Value::Bool(b) => ("", f64::from(u8::from(*b))),
            _ => ("", 0.0),
        };
        let value = match &opt.def {
            Some(OptValue::Float(_)) => Some(OptValue::Float(if vs.is_empty() {
                num
            } else {
                strtod(vs)
            })),
            // QJsonValue::toInt() / QString::toInt(): 32-bit
            Some(OptValue::Int(_)) => Some(OptValue::Int(if vs.is_empty() {
                num as i32 as i64
            } else {
                strtoll(vs) as i32 as i64
            })),
            Some(OptValue::Str(_)) => Some(OptValue::Str(vs.to_owned())),
            None => None,
        };
        if let Some(v) = value {
            out.insert(opt.id.clone(), v);
        }
    }
    out
}

/// A decoder option value as JSON (`variant_to_json`, `gen_decoders_json`).
pub fn opt_json(v: &OptValue) -> Value {
    match v {
        OptValue::Str(s) => Value::from(s.as_str()),
        OptValue::Int(i) => Value::from(*i),
        OptValue::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_subset_matches_a_scan() {
        let mut r = RowData::default();
        let mut x = 7u64;
        let mut next = || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x >> 33
        };
        for _ in 0..3000 {
            // mostly in order, sometimes earlier, some long ones
            let start = r.anns.last().map_or(0, |a| a.start) + next() % 50;
            let start = if next() % 10 == 0 {
                start.saturating_sub(next() % 400)
            } else {
                start
            };
            let len = if next() % 50 == 0 {
                next() % 5000
            } else {
                next() % 30
            };
            r.push(Ann {
                start,
                end: start + len,
                ty: 0,
                lines: Arc::new(Vec::new()),
                num: None,
            });
        }
        for _ in 0..500 {
            let a = next() % 80_000;
            let b = a + next() % 3000;
            let scan: Vec<(u64, u64)> = r
                .anns
                .iter()
                .filter(|x| x.end > a && x.start <= b)
                .map(|x| (x.start, x.end))
                .collect();
            let idx: Vec<(u64, u64)> = r.subset(a, b).iter().map(|x| (x.start, x.end)).collect();
            assert_eq!(scan, idx, "{a}..{b}");
        }
    }

    #[test]
    fn printf_subset() {
        assert_eq!(c_format_s("Data write: %s", "04"), "Data write: 04");
        assert_eq!(c_format_s("50%% %s %s", "A"), "50% A %s");
        assert_eq!(c_format_s("no number", "FF"), "no number");
    }
}
