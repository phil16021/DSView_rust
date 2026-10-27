//! Port of `pv::data::LogicSnapshot` (DSView/pv/data/logicsnapshot.cpp).
//!
//! Layout, identical to the C++ class so pointers handed to C decoders mean
//! the same thing:
//!
//! * each enabled channel has a vector of root nodes; a root node covers 64
//!   leaves of 2^24 samples (2^30 samples);
//! * a leaf is one allocation: 2^24 bits of samples (LSB first), then the
//!   toggle mip-map levels 1-3 (one bit per 64, 4096 and 262144 samples);
//! * a finished leaf without toggles is freed; the root node keeps its level
//!   in `first`/`last` and its toggle flag in `tog`;
//! * loop mode keeps a sliding window of `total_sample_count` samples:
//!   `loop_offset` samples at the head are stale, whole root nodes rotate to
//!   the back.
//!
//! The port keeps the C++ semantics including its quirks; where the C++
//! code would read out of bounds or dereference a freed leaf the port returns
//! a neutral value instead (documented on each method). Departures are listed
//! in docs/migration/DEVIATIONS.md.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::BTreeMap;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const SCALE_LEVEL: u64 = 4;
const SCALE_POWER: u64 = 6;
const SCALE: u64 = 1 << SCALE_POWER;
const ROOT_SCALE_POWER: u64 = SCALE_POWER;
const ROOT_SCALE: u64 = 1 << ROOT_SCALE_POWER;
/// Samples per leaf.
pub const LEAF_BLOCK_SAMPLES: u64 = 1 << LEAF_BLOCK_POWER;
const LEAF_BLOCK_POWER: u64 = SCALE_LEVEL * SCALE_POWER;
/// Samples per root node.
pub const ROOT_NODE_SAMPLES: u64 = LEAF_BLOCK_SAMPLES * ROOT_SCALE;
const ROOT_MASK: u64 = !(!0u64 << ROOT_SCALE_POWER) << LEAF_BLOCK_POWER;
const LEAF_MASK: u64 = !(!0u64 << LEAF_BLOCK_POWER);
const LEVEL_MASK: [u64; 4] = [0x3f, 0x3f << 6, 0x3f << 12, 0x3f << 18];
/// Word offsets of the mip-map levels inside a leaf.
const LEVEL_OFFSET: [usize; 4] = [0, 262_144, 262_144 + 4096, 262_144 + 4096 + 64];
/// Words of one leaf: samples plus mip-map levels (C++ `LeafBlockSpace / 8`).
const LEAF_WORDS: usize = 262_144 + 4096 + 64 + 1;
/// The most leaves a capture maps ahead of itself (about 2 GiB).
const PREFAULT_MAX_LEAVES: u64 = 1024;
/// Bytes of sample data in a leaf.
const LEAF_DATA_BYTES: usize = (LEAF_BLOCK_SAMPLES / 8) as usize;
const MSB: u64 = 1 << (SCALE - 1);
const LSB: u64 = 1;
/// C++ `CHANNEL_MAX_COUNT`: at most 63 enabled channels.
pub const CHANNEL_MAX_COUNT: usize = 64;

/// One leaf allocation. Accessed through its raw pointer only, so pointers
/// handed out by [`LogicStore::get_samples`] stay valid while the store
/// keeps writing other words of the same leaf.
pub struct Leaf {
    ptr: NonNull<u64>,
}

// SAFETY: a Leaf owns its allocation; all access goes through &self/&mut self.
unsafe impl Send for Leaf {}
unsafe impl Sync for Leaf {}

impl Leaf {
    fn layout() -> Layout {
        Layout::array::<u64>(LEAF_WORDS).expect("leaf layout")
    }

    /// A zeroed leaf, from the [`leaf_pool`] when it has one, or None when
    /// the allocation fails (C++: malloc failure, the packet is dropped).
    fn new() -> Option<Leaf> {
        if let Some(ptr) = leaf_pool::take() {
            return Some(Leaf { ptr });
        }
        leaf_pool::count_fresh();
        // SAFETY: the layout has a non-zero size.
        let p = unsafe { alloc_zeroed(Self::layout()) } as *mut u64;
        NonNull::new(p).map(|ptr| Leaf { ptr })
    }

    #[inline]
    fn get(&self, i: usize) -> u64 {
        assert!(i < LEAF_WORDS);
        // SAFETY: in bounds; the allocation lives as long as self.
        unsafe { self.ptr.as_ptr().add(i).read() }
    }

    #[inline]
    fn set_byte(&mut self, i: usize, v: u8) {
        assert!(i < LEAF_WORDS * 8);
        // SAFETY: in bounds; &mut self.
        unsafe { (self.ptr.as_ptr() as *mut u8).add(i).write(v) }
    }

    /// Sample words `start..start + len`, for writing.
    #[inline]
    fn words_mut(&mut self, start: usize, len: usize) -> &mut [u64] {
        assert!(start + len <= LEVEL_OFFSET[1]);
        // SAFETY: in bounds; &mut self. Only these words are borrowed, so
        // decoders may keep reading other samples through raw pointers.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr().add(start), len) }
    }

    /// The sample words (read) and the mip-map words (write).
    #[inline]
    fn split(&mut self) -> (&[u64], &mut [u64]) {
        let p = self.ptr.as_ptr();
        // SAFETY: two disjoint ranges of the allocation; &mut self.
        unsafe {
            (
                std::slice::from_raw_parts(p, LEVEL_OFFSET[1]),
                std::slice::from_raw_parts_mut(
                    p.add(LEVEL_OFFSET[1]),
                    LEAF_WORDS - LEVEL_OFFSET[1],
                ),
            )
        }
    }

    fn zero(&mut self) {
        // SAFETY: the whole allocation; &mut self.
        unsafe { std::ptr::write_bytes(self.ptr.as_ptr(), 0, LEAF_WORDS) }
    }

    /// Start of the leaf (sample 0 of the block).
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr() as *const u8
    }
}

impl Drop for Leaf {
    fn drop(&mut self) {
        if !leaf_pool::put(self.ptr, false) {
            // SAFETY: allocated in Leaf::new with the same layout.
            unsafe { dealloc(self.ptr.as_ptr() as *mut u8, Self::layout()) }
        }
    }
}

/// Leaves kept for reuse across captures and stores (process-wide).
///
/// A fresh leaf costs its page faults on first write (2 MiB each), which
/// bound the first capture's ingest at about the speed of a copy into fresh
/// memory. Freed leaves (constant ones during a capture, a store's leaves
/// when it is cleared or dropped) wait here instead, and [`prefault`] fills
/// the pool ahead of a capture on another thread, so the capture writes
/// memory that is already mapped. A leaf is zeroed when it is taken, unless
/// it is known clean.
pub mod leaf_pool {
    use super::{Leaf, NonNull};
    use std::alloc::{alloc_zeroed, dealloc};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    /// Leaves kept while no capture asked for more (about 135 MiB).
    pub const IDLE_LEAVES: usize = 64;

    struct Slot(NonNull<u64>, bool);
    // SAFETY: a pooled leaf is owned by the pool alone.
    unsafe impl Send for Slot {}

    static POOL: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
    /// The most leaves the pool keeps (raised by a pre-fault).
    static CAP: AtomicUsize = AtomicUsize::new(IDLE_LEAVES);
    static FRESH: AtomicU64 = AtomicU64::new(0);
    /// Bumped by [`release`]: a pre-fault still running stops.
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    fn pool() -> std::sync::MutexGuard<'static, Vec<Slot>> {
        POOL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A pooled leaf, zeroed.
    pub(super) fn take() -> Option<NonNull<u64>> {
        let Slot(p, clean) = {
            let mut v = pool();
            // a clean one first: no zeroing on the capture's thread
            match v.iter().rposition(|s| s.1) {
                Some(i) => v.swap_remove(i),
                None => v.pop()?,
            }
        };
        if !clean {
            // SAFETY: the whole leaf allocation, owned by the caller now.
            unsafe { std::ptr::write_bytes(p.as_ptr(), 0, super::LEAF_WORDS) };
        }
        Some(p)
    }

    /// Keeps a freed leaf; false when the pool is full (the caller frees it).
    pub(super) fn put(p: NonNull<u64>, clean: bool) -> bool {
        put_in(p, clean, None)
    }

    /// `put`, for a pre-fault of `generation`: false once [`release`] ran
    /// (checked under the pool's lock, which `release` holds too).
    fn put_in(p: NonNull<u64>, clean: bool, generation: Option<u64>) -> bool {
        let mut v = pool();
        if generation.is_some_and(|g| GENERATION.load(Ordering::SeqCst) != g)
            || v.len() >= CAP.load(Ordering::Relaxed)
        {
            return false;
        }
        v.push(Slot(p, clean));
        true
    }

    pub(super) fn count_fresh() {
        FRESH.fetch_add(1, Ordering::Relaxed);
    }

    /// Leaves allocated fresh so far (not taken from the pool).
    pub fn fresh_allocations() -> u64 {
        FRESH.load(Ordering::Relaxed)
    }

    /// Leaves waiting in the pool.
    pub fn pooled() -> usize {
        pool().len()
    }

    /// Allocates and maps up to `leaves` leaves into the pool on another
    /// thread (the pool's cap rises to fit them). The capture takes them as
    /// it goes; a leaf it needs before the thread made one is allocated as
    /// before.
    pub fn prefault(leaves: usize) -> std::thread::JoinHandle<()> {
        CAP.fetch_max(leaves.max(IDLE_LEAVES), Ordering::Relaxed);
        let generation = GENERATION.load(Ordering::SeqCst);
        let current = move || GENERATION.load(Ordering::SeqCst) == generation;
        std::thread::Builder::new()
            .name("leaf-prefault".into())
            .spawn(move || {
                // zero the pooled leaves first, so the capture takes clean ones
                loop {
                    let dirty = {
                        let mut v = pool();
                        let i = v.iter().position(|s| !s.1);
                        i.map(|i| v.swap_remove(i))
                    };
                    let Some(Slot(p, _)) = dirty else { break };
                    if !current() {
                        // SAFETY: a pooled leaf, allocated with this layout.
                        unsafe { dealloc(p.as_ptr() as *mut u8, Leaf::layout()) };
                        return;
                    }
                    // SAFETY: the whole leaf, out of the pool and owned here.
                    unsafe { std::ptr::write_bytes(p.as_ptr(), 0, super::LEAF_WORDS) };
                    if !put_in(p, true, Some(generation)) {
                        // SAFETY: a pooled leaf, allocated with this layout.
                        unsafe { dealloc(p.as_ptr() as *mut u8, Leaf::layout()) };
                    }
                }
                for _ in 0..leaves {
                    if pooled() >= leaves || !current() {
                        break;
                    }
                    // SAFETY: the layout has a non-zero size.
                    let p = unsafe { alloc_zeroed(Leaf::layout()) } as *mut u64;
                    let Some(p) = NonNull::new(p) else { break };
                    // map every page now: a write per 4 KiB
                    for page in (0..super::LEAF_WORDS).step_by(512) {
                        // SAFETY: inside the allocation; nobody else has it.
                        unsafe { p.as_ptr().add(page).write_volatile(0) };
                    }
                    FRESH.fetch_add(1, Ordering::Relaxed);
                    if !put_in(p, true, Some(generation)) {
                        // SAFETY: allocated above with the same layout.
                        unsafe { dealloc(p.as_ptr() as *mut u8, Leaf::layout()) };
                        break;
                    }
                }
            })
            .expect("pre-fault thread")
    }

    /// Pre-faults started by captures, until [`settle`] waits for them.
    static RUNNING: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

    /// [`prefault`] for a capture: the thread runs on its own; [`settle`]
    /// waits for it.
    pub(super) fn prefault_in_background(leaves: usize) {
        let h = prefault(leaves);
        let mut r = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        r.retain(|h| !h.is_finished());
        r.push(h);
    }

    /// Waits until the pre-faults that captures started are done (tests,
    /// and anything that counts [`fresh_allocations`]).
    pub fn settle() {
        let all = std::mem::take(&mut *RUNNING.lock().unwrap_or_else(|e| e.into_inner()));
        for h in all {
            let _ = h.join();
        }
    }

    /// Back to the idle size: the cap drops and the leaves above it are
    /// freed (after a capture).
    pub fn trim_idle() {
        CAP.store(IDLE_LEAVES, Ordering::Relaxed);
        let extra: Vec<Slot> = {
            let mut v = pool();
            let keep = v.len().min(IDLE_LEAVES);
            v.split_off(keep)
        };
        for Slot(p, _) in extra {
            // SAFETY: a pooled leaf, allocated with this layout.
            unsafe { dealloc(p.as_ptr() as *mut u8, Leaf::layout()) };
        }
    }

    /// Frees every pooled leaf (the device or mode changed, memory is
    /// needed elsewhere).
    pub fn release() {
        let all: Vec<Slot> = {
            let mut v = pool();
            GENERATION.fetch_add(1, Ordering::SeqCst);
            std::mem::take(&mut *v)
        };
        for Slot(p, _) in all {
            // SAFETY: a pooled leaf, allocated with this layout.
            unsafe { dealloc(p.as_ptr() as *mut u8, Leaf::layout()) };
        }
    }
}

struct RootNode {
    tog: u64,
    first: u64,
    last: u64,
    lbp: [Option<Leaf>; ROOT_SCALE as usize],
}

impl RootNode {
    fn new() -> RootNode {
        RootNode {
            tog: 0,
            first: 0,
            last: 0,
            lbp: std::array::from_fn(|_| None),
        }
    }
}

/// C++ `_dest_ptr`: where the bytes of an unfinished 64-sample word go.
/// Kept as indexes instead of a pointer; adjusted when root nodes rotate.
#[derive(Clone, Copy)]
struct Dest {
    order: usize,
    root: usize,
    pos: usize,
    byte: usize,
}

/// Result of [`LogicStore::get_samples`].
#[derive(Clone, Copy, Debug)]
pub struct Samples {
    /// Bit-packed samples from `start` (rounded down to a byte), or null when
    /// the block is constant (freed) or the channel has no data.
    pub data: *const u8,
    /// End of the contiguous run that `data` covers (exclusive).
    pub end_sample: u64,
    /// The leaf the run lies in (decoders pin it, see
    /// [`LogicStore::free_decode_lpb`]); null when `data` is null.
    pub lbp: *const u8,
}

/// One pattern element for [`LogicStore::pattern_search`]: `'0'`, `'1'`,
/// `'X'` (don't care), `'R'`, `'F'` (rising/falling edge) or `'C'` (any edge).
pub type Pattern = BTreeMap<u16, u8>;

/// Snapshot counters (C++ `Snapshot` getters).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Counts {
    pub sample_count: u64,
    pub ring_sample_count: u64,
    pub total_sample_count: u64,
    pub loop_offset: u64,
    pub ring_start: u64,
    pub ring_end: u64,
    pub block_num: i32,
    pub last_ended: bool,
    pub channel_num: u32,
}

pub struct LogicStore {
    // pv::data::Snapshot
    ch_index: Vec<u16>,
    channel_num: u32,
    sample_count: u64,
    total_sample_count: u64,
    ring_sample_count: u64,
    memory_failed: bool,
    last_ended: bool,

    // pv::data::LogicSnapshot
    ch_data: Vec<Vec<RootNode>>,
    byte_fraction: u8,
    ch_fraction: u16,
    dest: Option<Dest>,
    last_sample: [u64; CHANNEL_MAX_COUNT],
    last_calc_count: [u64; CHANNEL_MAX_COUNT],
    is_loop: bool,
    loop_offset: u64,
    able_free: bool,
    free_block_list: Vec<Leaf>,
    cur_ref_block_indexs: [(u64, u64); CHANNEL_MAX_COUNT],
    lst_free_block_index: i32,
    is_search_stop: Arc<AtomicBool>,
}

impl Default for LogicStore {
    fn default() -> Self {
        Self::new()
    }
}

impl LogicStore {
    /// An empty store, as DSView uses it: constructed, then `clear()`ed.
    pub fn new() -> LogicStore {
        let mut s = LogicStore {
            ch_index: Vec::new(),
            channel_num: 0,
            sample_count: 0,
            total_sample_count: 0,
            ring_sample_count: 0,
            memory_failed: false,
            last_ended: true,
            ch_data: Vec::new(),
            byte_fraction: 0,
            ch_fraction: 0,
            dest: None,
            last_sample: [0; CHANNEL_MAX_COUNT],
            last_calc_count: [0; CHANNEL_MAX_COUNT],
            is_loop: false,
            loop_offset: 0,
            able_free: true,
            free_block_list: Vec::new(),
            cur_ref_block_indexs: [(0, 0); CHANNEL_MAX_COUNT],
            lst_free_block_index: 0,
            is_search_stop: Arc::new(AtomicBool::new(false)),
        };
        s.clear();
        s
    }

    // ------------------------------------------------------------ Snapshot

    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn total_sample_count(&self) -> u64 {
        self.total_sample_count
    }

    pub fn ring_sample_count(&self) -> u64 {
        self.ring_sample_count
    }

    pub fn ring_start(&self) -> u64 {
        if self.sample_count < self.total_sample_count {
            0
        } else {
            self.ring_sample_count
        }
    }

    pub fn ring_end(&self) -> u64 {
        if self.sample_count == 0 {
            0
        } else if self.ring_sample_count == 0 {
            self.total_sample_count.wrapping_sub(1)
        } else {
            self.ring_sample_count - 1
        }
    }

    pub fn empty(&self) -> bool {
        self.sample_count == 0
    }

    pub fn memory_failed(&self) -> bool {
        self.memory_failed
    }

    pub fn last_ended(&self) -> bool {
        self.last_ended
    }

    pub fn channel_num(&self) -> u32 {
        self.channel_num
    }

    /// Indexes of the channels with data, in storage order.
    pub fn channel_indexes(&self) -> &[u16] {
        &self.ch_index
    }

    pub fn set_loop(&mut self, is_loop: bool) {
        self.is_loop = is_loop;
    }

    pub fn is_loop(&self) -> bool {
        self.is_loop
    }

    pub fn loop_offset(&self) -> u64 {
        self.loop_offset
    }

    pub fn is_able_free(&self) -> bool {
        self.able_free
    }

    pub fn counts(&self) -> Counts {
        Counts {
            sample_count: self.sample_count,
            ring_sample_count: self.ring_sample_count,
            total_sample_count: self.total_sample_count,
            loop_offset: self.loop_offset,
            ring_start: self.ring_start(),
            ring_end: self.ring_end(),
            block_num: self.get_block_num(),
            last_ended: self.last_ended,
            channel_num: self.channel_num,
        }
    }

    /// Stops a running [`pattern_search`](Self::pattern_search) from another
    /// thread.
    pub fn cancel_search(&self) {
        self.is_search_stop.store(true, Ordering::Relaxed);
    }

    /// The flag [`cancel_search`](Self::cancel_search) sets, for cancelling
    /// from another thread while the store is locked by the search (the C++
    /// class sets it without taking its lock).
    pub fn search_stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.is_search_stop)
    }

    // ------------------------------------------------------------ lifecycle

    fn free_data(&mut self) {
        self.sample_count = 0;
        self.ch_index.clear();
        self.ch_data.clear();
        self.free_block_list.clear();
        self.dest = None;
    }

    fn init_all(&mut self) {
        self.sample_count = 0;
        self.ring_sample_count = 0;
        self.byte_fraction = 0;
        self.ch_fraction = 0;
        self.dest = None;
        self.memory_failed = false;
        self.last_ended = true;
        self.loop_offset = 0;
        self.able_free = true;
    }

    /// Resets counters but keeps the allocated data (C++ `init()`).
    pub fn init(&mut self) {
        self.init_all();
    }

    /// Frees all data and resets (C++ `clear()`).
    pub fn clear(&mut self) {
        self.free_data();
        self.init_all();
    }

    /// First packet of a capture. `channels` are the enabled logic channels'
    /// indexes in device order; `able_free` is false while a decoder reads
    /// the capture as it arrives (constant leaves are then kept until
    /// [`free_decode_lpb`](Self::free_decode_lpb)/[`decode_end`](Self::decode_end)).
    ///
    /// C++ asserts 1..=63 channels; the port ignores the packet otherwise.
    pub fn first_payload(
        &mut self,
        data: &[u8],
        total_sample_count: u64,
        channels: &[u16],
        able_free: bool,
    ) {
        let mut channel_changed = false;
        let mut channel_num: u16 = 0;
        self.able_free = able_free;
        self.lst_free_block_index = 0;
        self.free_block_list.clear();

        for &ch in channels {
            channel_num = channel_num.wrapping_add(1);
            if !channel_changed {
                channel_changed = !self.has_data(ch as i32);
            }
        }

        if total_sample_count != self.total_sample_count
            || channel_num as u32 != self.channel_num
            || channel_changed
            || self.is_loop
        {
            self.free_data();
            self.ch_index.clear();
            self.total_sample_count = total_sample_count;
            self.channel_num = channel_num as u32;
            let mut rootnode_size =
                self.total_sample_count.wrapping_add(ROOT_NODE_SAMPLES - 1) / ROOT_NODE_SAMPLES;
            if self.is_loop {
                rootnode_size += 2;
            }
            for &ch in channels {
                self.ch_data
                    .push((0..rootnode_size).map(|_| RootNode::new()).collect());
                self.ch_index.push(ch);
            }
            // map the capture's leaves ahead of it on another thread
            let leaves = (self.total_sample_count.div_ceil(LEAF_BLOCK_SAMPLES) * channel_num as u64)
                .min(PREFAULT_MAX_LEAVES) as usize;
            leaf_pool::prefault_in_background(leaves.saturating_sub(leaf_pool::pooled()));
        } else {
            for chan in &mut self.ch_data {
                for rn in chan.iter_mut() {
                    rn.tog = 0;
                    rn.first = 0;
                    rn.last = 0;
                    for leaf in rn.lbp.iter_mut().flatten() {
                        leaf.zero();
                    }
                }
            }
        }

        self.sample_count = 0;
        self.ring_sample_count = 0;
        let n = (self.channel_num as usize).min(CHANNEL_MAX_COUNT);
        for i in 0..n {
            self.last_sample[i] = 0;
            self.last_calc_count[i] = 0;
            self.cur_ref_block_indexs[i] = (0, 0);
        }

        if self.ch_index.is_empty() || self.channel_num as usize >= CHANNEL_MAX_COUNT {
            return;
        }
        self.append_payload(data);
        self.last_ended = false;
    }

    /// Appends one LA_CROSS packet: per 64-sample group, one little-endian
    /// u64 word per channel. Bytes past the end of `data` read as zero
    /// (C++ reads up to 7 bytes beyond the packet, see DEVIATIONS.md F9).
    pub fn append_payload(&mut self, data: &[u8]) {
        if self.channel_num == 0
            || self.channel_num as usize >= CHANNEL_MAX_COUNT
            || self.ch_data.is_empty()
        {
            return;
        }
        self.append_cross_payload(data);
    }

    fn leaf_mut(&mut self, order: usize, root: usize, pos: usize) -> Option<&mut Leaf> {
        self.ch_data.get_mut(order)?.get_mut(root)?.lbp[pos].as_mut()
    }

    /// The leaf at (order, root, pos), allocated (zeroed) if missing.
    /// None: out of range or allocation failure (C++ logs and drops the rest
    /// of the packet).
    fn ensure_leaf(&mut self, order: usize, root: usize, pos: usize) -> bool {
        let Some(rn) = self.ch_data.get_mut(order).and_then(|c| c.get_mut(root)) else {
            return false;
        };
        if rn.lbp[pos].is_none() {
            match Leaf::new() {
                Some(l) => rn.lbp[pos] = Some(l),
                None => return false,
            }
        }
        true
    }

    fn write_dest_byte(&mut self, v: u8) {
        if let Some(d) = self.dest {
            if let Some(leaf) = self.leaf_mut(d.order, d.root, d.pos) {
                leaf.set_byte(d.byte, v);
            }
            self.dest = Some(Dest {
                byte: d.byte + 1,
                ..d
            });
        }
    }

    fn append_cross_payload(&mut self, data: &[u8]) {
        let n = self.channel_num as u64;
        let mut src = 0usize;
        let mut len = data.len() as u64;
        let mut index0: u64;
        let mut index1: u64;
        let mut offset: u64;

        // "samples not accurate, lead to a larger _sample_count; fixed in the
        // last packet"
        let samples = (data.len() as f64 * 8.0 / n as f64).ceil() as u64;
        if self.sample_count.wrapping_add(samples) < self.total_sample_count {
            self.sample_count += samples;
        } else {
            if self.sample_count == self.total_sample_count && !self.is_loop {
                return;
            }
            self.sample_count = self.total_sample_count;
        }

        if self.is_loop {
            if self.loop_offset >= LEAF_BLOCK_SAMPLES * SCALE {
                self.move_first_node_to_last();
                self.loop_offset -= LEAF_BLOCK_SAMPLES * SCALE;
                self.lst_free_block_index = 0;
            } else {
                let free_count = (self.loop_offset / LEAF_BLOCK_SAMPLES) as i32;
                if free_count > self.lst_free_block_index {
                    self.free_head_blocks(free_count);
                }
            }
        }

        self.ring_sample_count = self.ring_sample_count.wrapping_add(self.loop_offset);

        // bit align: finish the 64-sample group the last packet ended in
        while (self.ch_fraction != 0 || self.byte_fraction != 0) && len > 0 {
            loop {
                self.write_dest_byte(data[src]);
                src += 1;
                self.byte_fraction = (self.byte_fraction + 1) % 8;
                len -= 1;
                if !(self.byte_fraction != 0 && len > 0) {
                    break;
                }
            }

            if self.byte_fraction == 0 {
                index0 = self.ring_sample_count / LEAF_BLOCK_SAMPLES / ROOT_SCALE;
                index1 = (self.ring_sample_count / LEAF_BLOCK_SAMPLES) % ROOT_SCALE;
                offset = (self.ring_sample_count % LEAF_BLOCK_SAMPLES) / 8;

                // switch to the next channel
                self.ch_fraction = ((self.ch_fraction as u64 + 1) % n) as u16;
                let order = self.ch_fraction as usize;
                if !self.ensure_leaf(order, index0 as usize, index1 as usize) {
                    return;
                }
                self.dest = Some(Dest {
                    order,
                    root: index0 as usize,
                    pos: index1 as usize,
                    byte: offset as usize,
                });

                // the last channel's word is complete: the group is done
                if self.ch_fraction == 0 {
                    self.ring_sample_count += SCALE;
                    if self.ring_sample_count.is_multiple_of(LEAF_BLOCK_SAMPLES) {
                        self.calc_mipmap(n as usize - 1, index0, index1, LEAF_BLOCK_SAMPLES, true);
                    }
                    break;
                }
            }
        }

        // whole words
        let mut align_sample_count = self.ring_sample_count;
        let base = src;
        let end_read = base + len as usize;
        let mut read = base;

        let mut filled_sample = align_sample_count % LEAF_BLOCK_SAMPLES;
        let mut old_filled_sample = filled_sample;
        let mut chans_read_addr = [0usize; CHANNEL_MAX_COUNT];
        for (i, a) in chans_read_addr.iter_mut().take(n as usize).enumerate() {
            *a = base + 8 * i;
        }

        let mut fill_chan_index: u64 = 0;
        let mut last_chan_index: u64 = 0;
        index0 = align_sample_count / LEAF_BLOCK_SAMPLES / ROOT_SCALE;
        index1 = (align_sample_count / LEAF_BLOCK_SAMPLES) % ROOT_SCALE;
        offset = align_sample_count % LEAF_BLOCK_SAMPLES;

        if index0 as usize >= self.ch_data[0].len() {
            return;
        }
        if !self.ensure_leaf(0, index0 as usize, index1 as usize) {
            return;
        }
        let mut write = (offset / SCALE) as usize;

        let stride = 8 * n as usize;
        while len >= 8 {
            // C++ copies word by word and checks after each one; copy the
            // run up to the first word where a check fires (end of this
            // channel's leaf, end of the packet) or the last whole word
            let k_end = if read >= end_read {
                1
            } else {
                (end_read - read).div_ceil(stride) as u64
            };
            let k_full = (LEAF_BLOCK_SAMPLES - filled_sample) / SCALE;
            let run = k_end.min(k_full).min(len / 8) as usize;
            if let Some(leaf) =
                self.leaf_mut(fill_chan_index as usize, index0 as usize, index1 as usize)
            {
                copy_strided(leaf.words_mut(write, run), data, read, stride);
            }
            write += run;
            read += stride * run;
            len -= 8 * run as u64;
            filled_sample += SCALE * run as u64;
            last_chan_index = (last_chan_index + run as u64) % n;

            let leaf_full = filled_sample == LEAF_BLOCK_SAMPLES;
            if leaf_full || read >= end_read {
                if leaf_full {
                    self.calc_mipmap(
                        fill_chan_index as usize,
                        index0,
                        index1,
                        LEAF_BLOCK_SAMPLES,
                        true,
                    );
                    // save the channel's read position
                    chans_read_addr[fill_chan_index as usize] = read;
                } else {
                    self.calc_mipmap(
                        fill_chan_index as usize,
                        index0,
                        index1,
                        filled_sample,
                        false,
                    );
                }
                fill_chan_index = (fill_chan_index + 1) % n;

                // the last channel's run ended: advance the group position
                if fill_chan_index == 0 {
                    align_sample_count += filled_sample - old_filled_sample;
                }

                index0 = align_sample_count / LEAF_BLOCK_SAMPLES / ROOT_SCALE;
                index1 = (align_sample_count / LEAF_BLOCK_SAMPLES) % ROOT_SCALE;
                offset = align_sample_count % LEAF_BLOCK_SAMPLES;
                filled_sample = align_sample_count % LEAF_BLOCK_SAMPLES;
                old_filled_sample = filled_sample;

                if !self.ensure_leaf(fill_chan_index as usize, index0 as usize, index1 as usize) {
                    return;
                }
                write = (offset / SCALE) as usize;
                read = chans_read_addr[fill_chan_index as usize];
            }
        }

        self.ring_sample_count = align_sample_count.wrapping_sub(self.loop_offset);

        if align_sample_count > self.total_sample_count {
            // the last packet's whole groups overrun the limit. In loop
            // mode the overrun is the window's stale head; in single mode
            // C++ also kept it as `_loop_offset`, shifting every read and
            // cutting the saved data (DEVIATIONS.md F11): there it is
            // dropped by `capture_ended`
            if self.is_loop {
                self.loop_offset = align_sample_count - self.total_sample_count;
            }
            self.ring_sample_count = self.total_sample_count;
        }

        self.ch_fraction = last_chan_index as u16;
        let order = self.ch_fraction as usize;
        if !self.ensure_leaf(order, index0 as usize, index1 as usize) {
            return;
        }
        self.dest = Some(Dest {
            order,
            root: index0 as usize,
            pos: index1 as usize,
            byte: (offset / 8) as usize,
        });

        if len > 0 {
            let mut s = end_read - len as usize;
            self.byte_fraction = self.byte_fraction.wrapping_add(len as u8);
            while len > 0 {
                let b = data.get(s).copied().unwrap_or(0);
                self.write_dest_byte(b);
                s += 1;
                len -= 1;
            }
        }
    }

    /// Marks the capture complete: zero-fills the open leaf and finishes its
    /// mip-map.
    pub fn capture_ended(&mut self) {
        self.last_ended = true;
        leaf_pool::trim_idle();
        self.sample_count = self.ring_sample_count;
        if !self.is_loop {
            self.trim_single_capture();
            return;
        }
        let ring = self.ring_sample_count.wrapping_add(self.loop_offset);
        let index0 = ring / LEAF_BLOCK_SAMPLES / ROOT_SCALE;
        let index1 = (ring / LEAF_BLOCK_SAMPLES) % ROOT_SCALE;
        let offset = ((ring % LEAF_BLOCK_SAMPLES) / 8) as usize;

        if offset > 0 {
            for chan in 0..self.channel_num as usize {
                let Some(leaf) = self.leaf_mut(chan, index0 as usize, index1 as usize) else {
                    // C++ logs "buffer is null" and then dereferences it
                    continue;
                };
                // u64 stores from byte `offset` while below the end of the
                // sample data: the last one may spill into mip-map level 1
                let mut p = offset;
                while p < LEAF_DATA_BYTES {
                    for b in 0..8 {
                        leaf.set_byte(p + b, 0);
                    }
                    p += 8;
                }
                self.calc_mipmap(chan, index0, index1, offset as u64 * 8, true);
            }
        }
    }

    /// [`capture_ended`](Self::capture_ended) of a single capture (F11): the
    /// samples past the limit (the rest of the last 64-sample group, which
    /// the device fills with whatever it sampled) repeat the last sample,
    /// the rest of the leaf is zeroed, and the leaf's mip-map is rebuilt, as
    /// it may have been finished over the overrun.
    fn trim_single_capture(&mut self) {
        let end = self.ring_sample_count;
        let bit = end % LEAF_BLOCK_SAMPLES;
        if bit == 0 {
            // the limit ends on a leaf: nothing past it was stored
            return;
        }
        let index0 = end / LEAF_BLOCK_SAMPLES / ROOT_SCALE;
        let index1 = (end / LEAF_BLOCK_SAMPLES) % ROOT_SCALE;
        let word = (bit / SCALE) as usize;
        let rem = bit % SCALE;
        let data_words = LEVEL_OFFSET[1];
        for chan in 0..self.channel_num as usize {
            let Some(leaf) = self.leaf_mut(chan, index0 as usize, index1 as usize) else {
                // freed: constant, overrun included
                continue;
            };
            let mut from = word;
            if rem > 0 {
                let w = leaf.get(word);
                let keep = (1u64 << rem) - 1;
                let fill = if w >> (rem - 1) & 1 != 0 { !keep } else { 0 };
                leaf.words_mut(word, 1)[0] = (w & keep) | fill;
                from += 1;
            }
            leaf.words_mut(from, data_words - from).fill(0);
            let (_, mip) = leaf.split();
            mip.fill(0);
            let rn = &mut self.ch_data[chan][index0 as usize];
            let mask = !(1u64 << index1);
            rn.first &= mask;
            rn.last &= mask;
            rn.tog &= mask;
            self.last_calc_count[chan] = 0;
            self.calc_mipmap(chan, index0, index1, bit.div_ceil(SCALE) * SCALE, true);
        }
    }

    /// Updates the toggle mip-map of one leaf up to `samples`; `is_end`
    /// finishes it (a leaf without toggles is then freed).
    fn calc_mipmap(&mut self, order: usize, index0: u64, index1: u64, samples: u64, is_end: bool) {
        // C++ takes the root/leaf index as uint8_t
        let i0 = index0 as u8 as usize;
        let i1 = index1 as u8 as usize;
        let last_count = self.last_calc_count[order];
        let mut last_sample = self.last_sample[order];
        let Some(leaf) = self
            .ch_data
            .get_mut(order)
            .and_then(|c| c.get_mut(i0))
            .and_then(|rn| rn.lbp[i1].as_mut())
        else {
            return;
        };
        let (words, mip) = leaf.split();
        // mip-map words relative to level 1
        const L2: usize = LEVEL_OFFSET[2] - LEVEL_OFFSET[1];
        const L3: usize = LEVEL_OFFSET[3] - LEVEL_OFFSET[1];

        // level 1: bit i set when word i differs from the previous sample.
        // C++ sets one bit at a time; the bits are collected per word here.
        let start = (last_count / SCALE) as usize;
        if start == 0 {
            last_sample = if words[0] & LSB != 0 { !0 } else { 0 };
        }
        let end = ((samples / SCALE) as usize).min(words.len());
        // a word's previous sample is the last bit of the word before it, so
        // whole blocks of 64 words need no carried state (and vectorise)
        let ext = |w: u64| ((w as i64) >> 63) as u64;
        let mut i = start;
        let mut prev = last_sample;
        let one = |i: usize, prev: u64| u64::from(prev ^ words[i] != 0) << (i % 64);
        while i < end && !i.is_multiple_of(64) {
            mip[i / 64] |= one(i, prev);
            prev = ext(words[i]);
            i += 1;
        }
        while i + 64 <= end {
            let b = &words[i..i + 64];
            let mut m = u64::from(prev ^ b[0] != 0);
            for j in 1..64 {
                m |= u64::from(b[j] ^ ext(b[j - 1]) != 0) << j;
            }
            mip[i / 64] |= m;
            prev = ext(b[63]);
            i += 64;
        }
        while i < end {
            mip[i / 64] |= one(i, prev);
            prev = ext(words[i]);
            i += 1;
        }
        last_sample = prev;

        // level 2: the whole level is rescanned, from where it was left
        let start = (last_count / SCALE / SCALE) as usize;
        let mut acc = 0u64;
        for i in start..L2 {
            acc |= u64::from(mip[i] != 0) << (i % 64);
            if i % 64 == 63 {
                mip[L2 + i / 64] |= acc;
                acc = 0;
            }
        }

        // level 3
        let mut acc = 0u64;
        for i in 0..SCALE as usize {
            acc |= u64::from(mip[L2 + i] != 0) << i;
        }
        mip[L3] |= acc;

        let first = leaf.get(0) & LSB != 0;
        let last = leaf.get((LEAF_BLOCK_SAMPLES / SCALE) as usize - 1) & MSB != 0;
        let tog = leaf.get(LEVEL_OFFSET[3]) != 0;
        self.last_sample[order] = last_sample;

        let rn = &mut self.ch_data[order][i0];
        if first {
            rn.first |= 1u64 << i1;
        }
        if last {
            rn.last |= 1u64 << i1;
        }
        if tog {
            rn.tog |= 1u64 << i1;
        } else if is_end {
            let (ref_root, ref_lbp) = self.cur_ref_block_indexs[order];
            let leaf = rn.lbp[i1].take();
            let free = self.able_free
                || i0 as u64 > ref_root
                || (i0 as u64 == ref_root && i1 as u64 > ref_lbp);
            if !free && let Some(l) = leaf {
                self.free_block_list.push(l);
            }
            // C++ keeps a dangling _dest_ptr into the freed leaf
            if let Some(d) = self.dest
                && (d.order, d.root, d.pos) == (order, i0, i1)
            {
                self.dest = None;
            }
        }

        self.last_calc_count[order] = if is_end { 0 } else { samples };
    }

    // ------------------------------------------------------------ reading

    fn get_ch_order(&self, sig_index: i32) -> Option<usize> {
        self.ch_index.iter().position(|&i| i as i32 == sig_index)
    }

    pub fn has_data(&self, sig_index: i32) -> bool {
        self.get_ch_order(sig_index).is_some()
    }

    /// The run of samples from `start` within one block, for decoders. Also
    /// records the block as the one the decoder currently reads.
    ///
    /// C++ asserts `start < ring_sample_count`.
    pub fn get_samples(&mut self, start_sample: u64, sig_index: i32) -> Samples {
        let sample_count = self.ring_sample_count;
        let logic_sample_index = start_sample.wrapping_add(self.loop_offset);
        let order = self.get_ch_order(sig_index);

        let index0 = logic_sample_index / LEAF_BLOCK_SAMPLES / ROOT_SCALE;
        let index1 = (logic_sample_index / LEAF_BLOCK_SAMPLES) % ROOT_SCALE;
        let mut offset = (start_sample % LEAF_BLOCK_SAMPLES) / 8;

        let leaf_ptr = |s: &Self| -> *const u8 {
            order
                .and_then(|o| s.ch_data.get(o))
                .and_then(|c| c.get(index0 as usize))
                .and_then(|rn| rn.lbp[index1 as usize].as_ref())
                .map_or(std::ptr::null(), Leaf::as_ptr)
        };
        let mut block_buffer = leaf_ptr(self);

        let block_num = self.get_block_num();
        let end_sample;
        if self.is_loop && self.loop_offset > 0 {
            let block0_sample = self.get_block_size(0) * 8;
            if start_sample < block0_sample {
                block_buffer = self.get_block_buf(0, sig_index).0;
                end_sample = block0_sample;
                offset = start_sample / 8;
            } else {
                let last_block_sample = self.get_block_size(block_num - 1) * 8;
                offset = ((start_sample - block0_sample) % LEAF_BLOCK_SAMPLES) / 8;
                if start_sample >= sample_count.wrapping_sub(last_block_sample) {
                    end_sample = sample_count;
                } else {
                    let mid_block_num =
                        ((start_sample - block0_sample) / LEAF_BLOCK_SAMPLES + 1) as i32;
                    end_sample = (mid_block_num as i64 as u64)
                        .wrapping_mul(LEAF_BLOCK_SAMPLES)
                        .wrapping_add(block0_sample);
                }
            }
        } else {
            let e = (index0 << (LEAF_BLOCK_POWER + ROOT_SCALE_POWER))
                + (index1 << LEAF_BLOCK_POWER)
                + LEAF_MASK;
            end_sample = (e + 1).min(sample_count);
        }

        match order {
            Some(o) if !block_buffer.is_null() => {
                let lbp = leaf_ptr(self);
                self.cur_ref_block_indexs[o] = (index0, index1);
                Samples {
                    // SAFETY: offset < LEAF_DATA_BYTES, inside the leaf.
                    data: unsafe { block_buffer.add(offset as usize) },
                    end_sample,
                    lbp,
                }
            }
            _ => Samples {
                data: std::ptr::null(),
                end_sample,
                lbp: std::ptr::null(),
            },
        }
    }

    /// Level of one sample; false outside the stored range or for a channel
    /// without data (C++ asserts the channel exists).
    pub fn get_sample(&self, index: u64, sig_index: i32) -> bool {
        let ring = self.ring_sample_count.wrapping_add(self.loop_offset);
        self.get_sample_self(index.wrapping_add(self.loop_offset), sig_index, ring)
    }

    fn get_sample_self(&self, index: u64, sig_index: i32, ring: u64) -> bool {
        let Some(order) = self.get_ch_order(sig_index) else {
            return false;
        };
        if index < ring {
            let index_mask = 1u64 << (index & LEVEL_MASK[0]);
            let index0 = (index >> (LEAF_BLOCK_POWER + ROOT_SCALE_POWER)) as usize;
            let index1 = ((index & ROOT_MASK) >> LEAF_BLOCK_POWER) as usize;
            let root_pos_mask = 1u64 << index1;
            let Some(rn) = self.ch_data[order].get(index0) else {
                return false;
            };
            if rn.tog & root_pos_mask == 0 {
                return rn.first & root_pos_mask != 0;
            }
            return match &rn.lbp[index1] {
                Some(l) => l.get(((index & LEAF_MASK) >> SCALE_POWER) as usize) & index_mask != 0,
                None => false,
            };
        }
        false
    }

    /// The edges to draw for `width` pixels showing samples `start..=end`,
    /// `min_length` samples per pixel. Fills `edges` (is-edge, level per
    /// pixel; `width + 1` entries when an edge falls on the last pixel, as
    /// in C++) and `togs` (pixel, level for up to `max(max_togs, 1)`
    /// toggles including the first and last); returns the level of the
    /// first sample.
    ///
    /// C++ asserts `start <= end < ring_sample_count` and `min_length > 0`.
    #[allow(clippy::too_many_arguments)]
    pub fn get_display_edges(
        &self,
        edges: &mut Vec<(bool, bool)>,
        togs: &mut Vec<(u16, bool)>,
        start: u64,
        end: u64,
        width: u16,
        max_togs: u16,
        pixels_offset: f64,
        min_length: f64,
        sig_index: u16,
    ) -> bool {
        edges.clear();
        togs.clear();
        if self.ring_sample_count == 0 {
            return false;
        }
        let sig = sig_index as i32;
        let width = width as usize;
        let mut index = start;

        let start_sample = self.get_sample(index, sig);
        let mut last_sample = start_sample;
        index = index.wrapping_add(1);
        togs.push((0, last_sample));

        while edges.len() < width {
            let has_edge = self.get_nxt_edge(&mut index, last_sample, end, 0.0, sig);

            let gap = ((index as f64 / min_length) - pixels_offset) as i64;
            let next = (((index as f64 / min_length).floor() + 1.0) * min_length).ceil() as u64;
            index = next.max(index.wrapping_add(1));

            while gap > edges.len() as i64 && edges.len() < width {
                edges.push((false, last_sample));
            }

            last_sample = if index > end {
                self.get_sample(end, sig)
            } else {
                self.get_sample(index - 1, sig)
            };

            if has_edge {
                edges.push((true, last_sample));
                if togs.len() < max_togs as usize {
                    togs.push(((edges.len() as u16).wrapping_sub(1), last_sample));
                }
            }

            while index > end && edges.len() < width {
                edges.push((false, last_sample));
            }
        }

        if togs.len() < max_togs as usize {
            last_sample = self.get_sample(end, sig);
            togs.push(((edges.len() as u16).wrapping_sub(1), last_sample));
        }
        start_sample
    }

    /// Next edge at or after `index` up to `end`, where the level differs
    /// from `last_sample`. On a hit `index` is the first sample of the new
    /// level.
    pub fn get_nxt_edge(
        &self,
        index: &mut u64,
        last_sample: bool,
        end: u64,
        min_length: f64,
        sig_index: i32,
    ) -> bool {
        let lo = self.loop_offset;
        let ring = self.ring_sample_count.wrapping_add(lo);
        let mut i = index.wrapping_add(lo);
        let flag = self.get_nxt_edge_self(
            &mut i,
            last_sample,
            end.wrapping_add(lo),
            min_length,
            sig_index,
            ring,
        );
        *index = i.wrapping_sub(lo);
        flag
    }

    fn get_nxt_edge_self(
        &self,
        index: &mut u64,
        last_sample: bool,
        end: u64,
        min_length: f64,
        sig_index: i32,
        ring: u64,
    ) -> bool {
        if *index > end {
            return false;
        }
        let Some(order) = self.get_ch_order(sig_index) else {
            return false;
        };
        let chd = &self.ch_data[order];
        let min_level = min_level(min_length);
        let root_index = *index >> (LEAF_BLOCK_POWER + ROOT_SCALE_POWER);
        let mut root_pos = ((*index & ROOT_MASK) >> LEAF_BLOCK_POWER) as u8;
        let mut root_last = if root_index != 0 {
            chd.get(root_index as usize - 1)
                .is_some_and(|rn| rn.last & MSB != 0)
        } else {
            chd.first().is_some_and(|rn| rn.first & LSB != 0)
        };
        let mut edge_hit = false;

        // linear search for the next transition on the root level
        let mut i = root_index;
        while !edge_hit && *index <= end && (i as usize) < chd.len() {
            let rn = &chd[i as usize];
            let mut cur_mask = !0u64 << root_pos;
            loop {
                let inner_tog = rn.tog & cur_mask;
                let lbp_tog = ((rn.last << 1).wrapping_add(root_last as u64) & cur_mask)
                    ^ (rn.first & cur_mask);
                let inner_tog_pos = bsf_folded(inner_tog);
                let lbp_tog_pos = bsf_folded(lbp_tog);

                if inner_tog != 0 {
                    if lbp_tog != 0 {
                        // lbp tog before inner tog
                        edge_hit = self.lbp_nxt_edge(
                            index,
                            i,
                            lbp_tog,
                            lbp_tog_pos,
                            true,
                            inner_tog_pos,
                            last_sample,
                            sig_index,
                            ring,
                        );
                    }
                    if !edge_hit {
                        let blk_start = (i << (LEAF_BLOCK_POWER + ROOT_SCALE_POWER))
                            + ((inner_tog_pos as u64) << LEAF_BLOCK_POWER);
                        *index = blk_start.max(*index);
                        if min_level < SCALE_LEVEL as u32 {
                            let block_end = (*index | LEAF_MASK).min(end);
                            edge_hit = match &rn.lbp[inner_tog_pos as usize] {
                                Some(l) => {
                                    block_nxt_edge(l, index, block_end, last_sample, min_level)
                                }
                                None => false,
                            };
                        } else {
                            edge_hit = true;
                        }
                        if inner_tog_pos as u64 == ROOT_SCALE - 1 {
                            break;
                        }
                        cur_mask = !0u64 << (inner_tog_pos + 1);
                    }
                } else if lbp_tog != 0 {
                    // lbp tog
                    edge_hit = self.lbp_nxt_edge(
                        index,
                        i,
                        lbp_tog,
                        lbp_tog_pos,
                        false,
                        (SCALE - 1) as u8,
                        last_sample,
                        sig_index,
                        ring,
                    );
                } else {
                    *index = ((i + 1) << (LEAF_BLOCK_POWER + ROOT_SCALE_POWER)) - 1;
                    break;
                }
                if !(!edge_hit && *index < ((i + 1) << (LEAF_BLOCK_POWER + ROOT_SCALE_POWER)) - 1) {
                    break;
                }
            }
            root_pos = 0;
            root_last = rn.last & MSB != 0;
            i += 1;
        }

        if *index > end {
            // skip edges over right
            edge_hit = false;
        }
        edge_hit
    }

    #[allow(clippy::too_many_arguments)]
    fn lbp_nxt_edge(
        &self,
        index: &mut u64,
        root_index: u64,
        mut lbp_tog: u64,
        mut lbp_tog_pos: u8,
        aft_tog: bool,
        aft_pos: u8,
        last_sample: bool,
        sig_index: i32,
        ring: u64,
    ) -> bool {
        // check last_sample with current index
        let sample = self.get_sample_self(*index, sig_index, ring);
        if sample ^ last_sample {
            return true;
        }

        // find edge between lbp
        let mut edge_hit = false;
        let root_start = root_index << (LEAF_BLOCK_POWER + ROOT_SCALE_POWER);
        let aft_lbp_start = root_start + ((aft_pos as u64) << LEAF_BLOCK_POWER);

        while lbp_tog_pos <= aft_pos {
            let lbp_tog_index = root_start + ((lbp_tog_pos as u64) << LEAF_BLOCK_POWER);
            if lbp_tog_index > aft_lbp_start {
                edge_hit = false;
                break;
            } else if lbp_tog_index > *index {
                *index = lbp_tog_index;
                edge_hit = true;
                break;
            }
            lbp_tog_pos += 1;
            // C++ shifts by 64 when lbp_tog_pos reaches 64 (x86: by 0)
            lbp_tog &= (!0u64).wrapping_shl(lbp_tog_pos as u32);
            if (lbp_tog_pos as u64) < SCALE && lbp_tog != 0 {
                lbp_tog_pos = bsf_folded(lbp_tog);
            } else {
                break;
            }
        }

        let lbp_edge_index = if aft_tog {
            aft_lbp_start
        } else {
            aft_lbp_start + (1u64 << LEAF_BLOCK_POWER) - 1
        };
        if !edge_hit && lbp_edge_index > *index {
            *index = lbp_edge_index;
        }
        edge_hit
    }

    /// Previous edge before `index`: on a hit `index` is the first sample of
    /// the level `last_sample` (the sample after the edge).
    ///
    /// C++ asserts `index < ring_sample_count`.
    pub fn get_pre_edge(
        &self,
        index: &mut u64,
        last_sample: bool,
        min_length: f64,
        sig_index: i32,
    ) -> bool {
        let lo = self.loop_offset;
        let ring = self.ring_sample_count.wrapping_add(lo);
        let mut i = index.wrapping_add(lo);
        let flag = self.get_pre_edge_self(&mut i, last_sample, min_length, sig_index, ring);
        *index = i.saturating_sub(lo);
        flag
    }

    fn get_pre_edge_self(
        &self,
        index: &mut u64,
        last_sample: bool,
        min_length: f64,
        sig_index: i32,
        ring: u64,
    ) -> bool {
        let Some(order) = self.get_ch_order(sig_index) else {
            return false;
        };
        let chd = &self.ch_data[order];
        let min_level = min_level(min_length);
        let root_index = (*index >> (LEAF_BLOCK_POWER + ROOT_SCALE_POWER)) as i32;
        let mut root_pos = ((*index & ROOT_MASK) >> LEAF_BLOCK_POWER) as u8;
        let Some(rn0) = chd.get(root_index as usize) else {
            return false;
        };
        let mut root_first = rn0.last & MSB != 0;
        let mut edge_hit = false;

        // linear search for the previous transition on the root level
        let mut i = root_index as i64;
        while !edge_hit && i >= 0 {
            let rn = &chd[i as usize];
            let mut cur_mask = !0u64 >> (ROOT_SCALE - root_pos as u64 - 1);
            loop {
                let inner_tog = rn.tog & cur_mask;
                let lbp_tog = (rn.last & cur_mask)
                    ^ ((((root_first as u64) << (ROOT_SCALE - 1)).wrapping_add(rn.first >> 1))
                        & cur_mask);
                let inner_tog_pos = bsr64(inner_tog);
                let mut lbp_tog_pos = bsr64(lbp_tog);

                if inner_tog != 0 {
                    if lbp_tog != 0 {
                        // lbp tog before inner tog
                        edge_hit = self.lbp_pre_edge(
                            index,
                            i as u64,
                            lbp_tog,
                            &mut lbp_tog_pos,
                            true,
                            inner_tog_pos,
                            last_sample,
                            sig_index,
                            ring,
                        );
                    }
                    if !edge_hit {
                        let blk_end = (((i as u64) << (LEAF_BLOCK_POWER + ROOT_SCALE_POWER))
                            + ((inner_tog_pos as u64) << LEAF_BLOCK_POWER))
                            | LEAF_MASK;
                        *index = blk_end.min(*index);
                        if min_level < SCALE_LEVEL as u32 {
                            edge_hit = match &rn.lbp[inner_tog_pos as usize] {
                                Some(l) => self.block_pre_edge(
                                    l,
                                    index,
                                    last_sample,
                                    min_level,
                                    sig_index,
                                    ring,
                                ),
                                None => false,
                            };
                        } else {
                            edge_hit = true;
                        }
                        if inner_tog_pos == 0 {
                            break;
                        }
                        cur_mask = !0u64 >> (ROOT_SCALE - inner_tog_pos as u64);
                    }
                } else if lbp_tog != 0 {
                    // lbp tog
                    edge_hit = self.lbp_pre_edge(
                        index,
                        i as u64,
                        lbp_tog,
                        &mut lbp_tog_pos,
                        false,
                        0,
                        last_sample,
                        sig_index,
                        ring,
                    );
                    if lbp_tog_pos == 0 {
                        break;
                    }
                } else {
                    break;
                }
                if edge_hit {
                    break;
                }
            }
            root_pos = (ROOT_SCALE - 1) as u8;
            root_first = rn.first & LSB != 0;
            i -= 1;
        }
        edge_hit
    }

    #[allow(clippy::too_many_arguments)]
    fn lbp_pre_edge(
        &self,
        index: &mut u64,
        root_index: u64,
        mut lbp_tog: u64,
        lbp_tog_pos: &mut u8,
        pre_tog: bool,
        pre_pos: u8,
        last_sample: bool,
        sig_index: i32,
        ring: u64,
    ) -> bool {
        // check last_sample with current index
        let sample = self.get_sample_self(*index, sig_index, ring);
        if sample ^ last_sample {
            *index += 1;
            return true;
        }

        // find edge between lbp
        let mut edge_hit = false;
        let root_start = root_index << (LEAF_BLOCK_POWER + ROOT_SCALE_POWER);
        let pre_lbp_end =
            root_start + ((pre_pos as u64) << LEAF_BLOCK_POWER) + (1u64 << LEAF_BLOCK_POWER) - 1;

        loop {
            let lbp_tog_index = root_start
                + ((*lbp_tog_pos as u64) << LEAF_BLOCK_POWER)
                + (1u64 << LEAF_BLOCK_POWER)
                - 1;
            if lbp_tog_index < pre_lbp_end {
                edge_hit = false;
                break;
            } else if lbp_tog_index < *index {
                *index = lbp_tog_index + 1;
                edge_hit = true;
                break;
            }

            if *lbp_tog_pos > 0 {
                *lbp_tog_pos -= 1;
                lbp_tog &= !0u64 >> (SCALE - *lbp_tog_pos as u64 - 1);
                *lbp_tog_pos = if lbp_tog != 0 { bsr64(lbp_tog) } else { 0 };
            } else {
                lbp_tog = 0;
            }
            if !(lbp_tog != 0 && *lbp_tog_pos >= pre_pos) {
                break;
            }
        }

        let lbp_edge_index = if pre_tog {
            pre_lbp_end
        } else {
            pre_lbp_end + 1 - (1u64 << LEAF_BLOCK_POWER)
        };
        if !edge_hit && lbp_edge_index < *index {
            *index = lbp_edge_index;
        }
        edge_hit
    }

    fn block_pre_edge(
        &self,
        lbp: &Leaf,
        index: &mut u64,
        last_sample: bool,
        min_level: u32,
        sig_index: i32,
        ring: u64,
    ) -> bool {
        let mut level = min_level as u64;
        let last: u64 = if last_sample { !0 } else { 0 };
        let block_start = *index & !LEAF_MASK;

        // search individual samples down to the start of the level-1 block
        if level == 0 {
            let offset = ((*index & LEAF_MASK) >> SCALE_POWER) as usize;
            let sh = SCALE - (*index & LEVEL_MASK[0]) - 1;
            let mask = if last_sample {
                !(!0u64 >> sh)
            } else {
                !0u64 >> sh
            };
            let sample = if last_sample {
                lbp.get(offset) | mask
            } else {
                lbp.get(offset) & mask
            };
            if sample ^ last != 0 {
                *index = (*index & !LEVEL_MASK[0])
                    + bsr64(if last_sample { !sample } else { sample }) as u64
                    + 1;
                return true;
            }
            *index &= !LEVEL_MASK[0];
            if *index == 0 {
                return false;
            }
            *index -= 1;
            // get_sample_self() also covers the previous block
            let sample = self.get_sample_self(*index, sig_index, ring);
            if sample ^ last_sample {
                *index += 1;
                return true;
            } else if *index < block_start {
                return false;
            }
        }

        // zoom out at the ends of mip-map blocks until there is a change
        while *index > block_start {
            if level == 0 {
                level += 1;
            }
            let lsp = (level + 1) * SCALE_POWER;
            let offset = ((*index & LEAF_MASK) >> lsp) as usize;
            let mask = !0u64
                >> (SCALE - ((*index & LEVEL_MASK[level as usize]) >> (level * SCALE_POWER)) - 1);
            let sample = lbp.get(LEVEL_OFFSET[level as usize] + offset) & mask;
            if sample != 0 {
                *index = (*index & (!0u64 << lsp))
                    + ((bsr64(sample) as u64) << (level * SCALE_POWER))
                    + !(!0u64 << (level * SCALE_POWER));
                break;
            }
            *index = (*index >> lsp) << lsp;
            if *index == 0 {
                return false;
            }
            *index -= 1;
        }

        // zoom in until min_level
        while *index >= block_start && level > min_level as u64 {
            level -= 1;
            let lsp = (level + 1) * SCALE_POWER;
            let offset = ((*index & LEAF_MASK) >> lsp) as usize;
            let sh = SCALE - ((*index & LEVEL_MASK[level as usize]) >> (level * SCALE_POWER)) - 1;
            let low = level == 0 && last_sample;
            let mask = if low { !(!0u64 >> sh) } else { !0u64 >> sh };
            let word = lbp.get(LEVEL_OFFSET[level as usize] + offset);
            let sample = if low { word | mask } else { word & mask };
            let probe = if level == 0 { sample ^ last } else { sample };
            if probe != 0 {
                *index = (*index & (!0u64 << lsp))
                    + ((bsr64(probe) as u64) << (level * SCALE_POWER))
                    + !(!0u64 << (level * SCALE_POWER));
                if level == min_level as u64 {
                    *index += 1;
                    break;
                }
            } else {
                *index &= !0u64 << lsp;
            }
        }

        *index >= block_start && *index != 0
    }

    /// Searches `start..=end` from `index` (forwards or backwards) for the
    /// first position where every channel matches its pattern element.
    /// Returns true for an empty pattern. Stops early when
    /// [`cancel_search`](Self::cancel_search) is called.
    pub fn pattern_search(
        &self,
        start: i64,
        end: i64,
        index: &mut i64,
        pattern: &Pattern,
        is_next: bool,
    ) -> bool {
        self.is_search_stop.store(false, Ordering::Relaxed);
        let lo = self.loop_offset as i64;
        let ring = self.ring_sample_count.wrapping_add(self.loop_offset);
        let mut idx = index.wrapping_add(lo);
        let flag = self.pattern_search_self(
            start.wrapping_add(lo),
            end.wrapping_add(lo),
            &mut idx,
            pattern,
            is_next,
            ring,
        );
        *index = idx.wrapping_sub(lo);
        flag
    }

    fn pattern_search_self(
        &self,
        start: i64,
        end: i64,
        index: &mut i64,
        pattern: &Pattern,
        is_next: bool,
        ring: u64,
    ) -> bool {
        if pattern.is_empty() {
            return true;
        }
        let mut flags: Vec<u8> = Vec::new();
        let mut chans: Vec<i32> = Vec::new();
        let mut edge_flag = false;
        for (&channel, &flag) in pattern {
            if flag != b'X' && self.has_data(channel as i32) {
                if flags.len() >= CHANNEL_MAX_COUNT {
                    break;
                }
                flags.push(flag);
                chans.push(channel as i32);
                if matches!(flag, b'R' | b'F' | b'C') {
                    edge_flag = true;
                }
            }
        }
        if flags.is_empty() {
            return true;
        }

        let to = if is_next {
            end.wrapping_add(1)
        } else {
            start.wrapping_sub(1)
        };
        let step: i64 = if is_next { 1 } else { -1 };
        let stop = || self.is_search_stop.load(Ordering::Relaxed);
        let mut lst = vec![false; flags.len()];

        // values before the first position, for edge elements
        if edge_flag {
            for (l, &ch) in lst.iter_mut().zip(&chans) {
                *l = self.get_sample_self(*index as u64, ch, ring);
            }
            *index = index.wrapping_add(step);
        }
        if *index < start {
            *index = start;
        }
        if *index > end {
            *index = end;
        }

        while *index != to && !stop() {
            let mut matched = 0usize;
            for i in 0..flags.len() {
                if stop() {
                    break;
                }
                let val = self.get_sample_self(*index as u64, chans[i], ring);
                let prev = lst[i];
                matched += match flags[i] {
                    b'0' => !val,
                    b'1' => val,
                    b'R' if is_next => !prev && val,
                    b'R' => prev && !val,
                    b'F' if is_next => prev && !val,
                    b'F' => !prev && val,
                    b'C' => prev != val,
                    _ => false,
                } as usize;
                lst[i] = val;
            }
            if matched == flags.len() {
                if !is_next {
                    // move to the previous position
                    *index += 1;
                }
                return true;
            }
            *index = index.wrapping_add(step);
        }
        false
    }

    // ------------------------------------------------------------ blocks

    /// Number of blocks (leaves) the stored range spans.
    pub fn get_block_num(&self) -> i32 {
        let align_sample_count = self.ring_sample_count;
        let mut block = (align_sample_count / LEAF_BLOCK_SAMPLES) as i32;
        if !align_sample_count.is_multiple_of(LEAF_BLOCK_SAMPLES) {
            block += 1;
        }
        if self.loop_offset > 0 {
            let diff1 = align_sample_count % LEAF_BLOCK_SAMPLES;
            let diff2 = self.loop_offset % LEAF_BLOCK_SAMPLES;
            if (diff1 == 0 && diff2 != 0) || (diff1 != 0 && diff1 + diff2 > LEAF_BLOCK_SAMPLES) {
                block += 1;
            }
        }
        block
    }

    /// Bytes of sample data in block `block_index` (C++ asserts it exists).
    pub fn get_block_size(&self, block_index: i32) -> u64 {
        let block_num = self.get_block_num();
        let align_sample_count = self.ring_sample_count;
        if self.loop_offset > 0 {
            if block_index > 0 && block_index < block_num - 1 {
                LEAF_BLOCK_SAMPLES / 8
            } else if block_index == 0 {
                if align_sample_count + self.loop_offset <= LEAF_BLOCK_SAMPLES {
                    align_sample_count / 8
                } else {
                    (LEAF_BLOCK_SAMPLES - self.loop_offset % LEAF_BLOCK_SAMPLES) / 8
                }
            } else {
                let sum = align_sample_count + self.loop_offset;
                if sum.is_multiple_of(LEAF_BLOCK_SAMPLES) {
                    LEAF_BLOCK_SAMPLES / 8
                } else {
                    (sum % LEAF_BLOCK_SAMPLES) / 8
                }
            }
        } else if block_index < block_num - 1
            || align_sample_count.is_multiple_of(LEAF_BLOCK_SAMPLES)
        {
            LEAF_BLOCK_SAMPLES / 8
        } else {
            // whole 64-sample words: a .dsl block is a sequence of them,
            // and the header's length cuts the last one (F11)
            (align_sample_count % LEAF_BLOCK_SAMPLES).div_ceil(SCALE) * 8
        }
    }

    /// Block data for saving/exporting: the bytes (null for a constant
    /// block) and, for a constant block, its level.
    pub fn get_block_buf(&self, block_index: i32, sig_index: i32) -> (*const u8, bool) {
        let Some(order) = self.get_ch_order(sig_index) else {
            return (std::ptr::null(), false);
        };
        let block_index0 = block_index;
        let block_index = block_index.wrapping_add((self.loop_offset / LEAF_BLOCK_SAMPLES) as i32);
        if block_index < 0 {
            return (std::ptr::null(), false);
        }
        let block_index = block_index as u64;
        let index = (block_index / ROOT_SCALE) as usize;
        let pos = (block_index % ROOT_SCALE) as usize;
        let Some(rn) = self.ch_data[order].get(index) else {
            return (std::ptr::null(), false);
        };
        match &rn.lbp[pos] {
            None => (std::ptr::null(), rn.first & (1u64 << pos) != 0),
            Some(l) => {
                let mut p = l.as_ptr();
                if self.loop_offset > 0 && block_index0 == 0 {
                    // SAFETY: < LEAF_DATA_BYTES into the leaf
                    p = unsafe { p.add(((self.loop_offset % LEAF_BLOCK_SAMPLES) / 8) as usize) };
                }
                (p, false)
            }
        }
    }

    /// Block index and sample offset within it for `sample_index`.
    pub fn get_block_index_with_sample(&self, sample_index: u64) -> (i32, u64) {
        if self.is_loop && self.loop_offset > 0 {
            let block_sample0 = self.get_block_size(0) * 8;
            if sample_index < block_sample0 {
                (0, sample_index)
            } else {
                (
                    ((sample_index - block_sample0) / LEAF_BLOCK_SAMPLES + 1) as i32,
                    (sample_index - block_sample0) % LEAF_BLOCK_SAMPLES,
                )
            }
        } else {
            (
                (sample_index / LEAF_BLOCK_SAMPLES) as i32,
                sample_index % LEAF_BLOCK_SAMPLES,
            )
        }
    }

    // ------------------------------------------------------------ loop mode, decoder pins

    fn move_first_node_to_last(&mut self) {
        for chan in self.ch_data.iter_mut().take(self.channel_num as usize) {
            if chan.is_empty() {
                continue;
            }
            let mut rn = chan.remove(0);
            rn.lbp = std::array::from_fn(|_| None);
            rn.tog = 0;
            rn.first = 0;
            rn.last = 0;
            chan.push(rn);
        }
        self.dest = match self.dest {
            Some(d) if d.root > 0 => Some(Dest {
                root: d.root - 1,
                ..d
            }),
            _ => None,
        };
    }

    fn free_head_blocks(&mut self, count: i32) {
        let from = self.lst_free_block_index.max(0) as usize;
        let count_u = count.clamp(0, 63) as usize;
        for chan in self.ch_data.iter_mut().take(self.channel_num as usize) {
            let Some(rn) = chan.first_mut() else { continue };
            for j in from..count_u {
                rn.lbp[j] = None;
                rn.tog = (rn.tog >> count_u) << count_u;
                rn.first = (rn.first >> count_u) << count_u;
                rn.last = (rn.last >> count_u) << count_u;
            }
        }
        if let Some(d) = self.dest
            && d.root == 0
            && d.pos >= from
            && d.pos < count_u
        {
            self.dest = None;
        }
        self.lst_free_block_index = count;
    }

    /// Frees all leaves kept for a running decoder.
    pub fn decode_end(&mut self) {
        self.free_block_list.clear();
    }

    /// Frees one kept leaf once the decoder moved past it (`lbp` as returned
    /// by [`get_samples`](Self::get_samples)).
    pub fn free_decode_lpb(&mut self, lbp: *const u8) {
        if let Some(i) = self.free_block_list.iter().position(|l| l.as_ptr() == lbp) {
            self.free_block_list.remove(i);
        }
    }

    /// Leaves currently kept for a decoder.
    pub fn kept_leaves(&self) -> usize {
        self.free_block_list.len()
    }
}

/// tog/first/last bits and the 64 leaves' words of one root node.
#[doc(hidden)]
pub type RootView<'a> = ((u64, u64, u64), Vec<Option<&'a [u64]>>);

/// Internal state for differential tests against the C++ class.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Internals {
    pub byte_fraction: u8,
    pub ch_fraction: u16,
    pub dest: Option<(usize, usize, usize, usize)>,
    pub last_sample: Vec<u64>,
    pub last_calc_count: Vec<u64>,
    pub cur_ref: Vec<(u64, u64)>,
    pub is_loop: bool,
    pub able_free: bool,
    pub kept_leaves: usize,
    pub lst_free_block_index: i32,
    pub orders: usize,
}

#[doc(hidden)]
impl LogicStore {
    pub fn internals(&self) -> Internals {
        let n = (self.channel_num as usize).min(CHANNEL_MAX_COUNT);
        Internals {
            byte_fraction: self.byte_fraction,
            ch_fraction: self.ch_fraction,
            dest: self.dest.map(|d| (d.order, d.root, d.pos, d.byte)),
            last_sample: self.last_sample[..n].to_vec(),
            last_calc_count: self.last_calc_count[..n].to_vec(),
            cur_ref: self.cur_ref_block_indexs[..n].to_vec(),
            is_loop: self.is_loop,
            able_free: self.able_free,
            kept_leaves: self.free_block_list.len(),
            lst_free_block_index: self.lst_free_block_index,
            orders: self.ch_data.len(),
        }
    }

    pub fn roots(&self, order: usize) -> usize {
        self.ch_data[order].len()
    }

    /// tog/first/last bits and the 64 leaves' words of one root node.
    pub fn root(&self, order: usize, root: usize) -> RootView<'_> {
        let rn = &self.ch_data[order][root];
        (
            (rn.tog, rn.first, rn.last),
            rn.lbp
                .iter()
                .map(|l| {
                    // SAFETY: the whole leaf allocation, borrowed from &self.
                    l.as_ref().map(|l| unsafe {
                        std::slice::from_raw_parts(l.ptr.as_ptr() as *const u64, LEAF_WORDS)
                    })
                })
                .collect(),
        )
    }
}

/// Fills `dst` with the words at `start`, `start + stride`, ... of `data`
/// (bytes past its end read as 0).
#[inline]
fn copy_strided(dst: &mut [u64], data: &[u8], start: usize, stride: usize) {
    let Some(last) = dst.len().checked_sub(1) else {
        return;
    };
    let end = start + last * stride + 8;
    if end <= data.len() {
        for (d, c) in dst.iter_mut().zip(data[start..end].chunks(stride)) {
            *d = u64::from_le_bytes(c[..8].try_into().expect("8 bytes"));
        }
    } else {
        for (i, d) in dst.iter_mut().enumerate() {
            *d = read_word(data, start + i * stride);
        }
    }
}

/// A little-endian u64 from `data` at `at`; bytes past the end read as 0.
#[inline]
fn read_word(data: &[u8], at: usize) -> u64 {
    match data.get(at..at + 8) {
        Some(b) => u64::from_le_bytes(b.try_into().expect("8 bytes")),
        None => {
            let mut b = [0u8; 8];
            if let Some(rest) = data.get(at..) {
                b[..rest.len()].copy_from_slice(rest);
            }
            u64::from_le_bytes(b)
        }
    }
}

/// C++ `max((int)(log2f(min_length) - 1) / (int)ScalePower, 0)`.
fn min_level(min_length: f64) -> u32 {
    let l = ((min_length as f32).log2() - 1.0) as i32;
    (l / SCALE_POWER as i32).max(0) as u32
}

/// Index of the lowest set bit; 63 for 0 (C++ `bsf_folded`, a folded
/// De Bruijn scan).
#[inline]
pub fn bsf_folded(bb: u64) -> u8 {
    const LSB_64_TABLE: [u8; 64] = [
        63, 30, 3, 32, 59, 14, 11, 33, 60, 24, 50, 9, 55, 19, 21, 34, 61, 29, 2, 53, 51, 23, 41,
        18, 56, 28, 1, 43, 46, 27, 0, 35, 62, 31, 58, 4, 5, 49, 54, 6, 15, 52, 12, 40, 7, 42, 45,
        16, 25, 57, 48, 13, 10, 39, 8, 44, 20, 47, 38, 22, 17, 37, 36, 26,
    ];
    let bb = bb ^ bb.wrapping_sub(1);
    let folded = (bb as u32) ^ ((bb >> 32) as u32);
    LSB_64_TABLE[(folded.wrapping_mul(0x7829_1ACF) >> 26) as usize]
}

/// Index of the highest set bit; 0 for 0 (C++ `bsr64`).
#[inline]
pub fn bsr64(bb: u64) -> u8 {
    if bb == 0 {
        0
    } else {
        63 - bb.leading_zeros() as u8
    }
}

fn block_nxt_edge(
    lbp: &Leaf,
    index: &mut u64,
    block_end: u64,
    last_sample: bool,
    min_level: u32,
) -> bool {
    let mut level = min_level as u64;
    let mut fast_forward = true;
    let last: u64 = if last_sample { !0 } else { 0 };

    // search individual samples up to the next level-1 block
    if level == 0 {
        let offset = ((*index & LEAF_MASK) >> SCALE_POWER) as usize;
        let sh = *index & LEVEL_MASK[0];
        let mask = if last_sample {
            !(!0u64 << sh)
        } else {
            !0u64 << sh
        };
        let sample = if last_sample {
            lbp.get(offset) | mask
        } else {
            lbp.get(offset) & mask
        };
        if sample ^ last != 0 {
            *index = (*index & !LEVEL_MASK[0])
                + bsf_folded(if last_sample { !sample } else { sample }) as u64;
            fast_forward = false;
        } else {
            *index = ((*index >> SCALE_POWER) + 1) << SCALE_POWER;
        }
    } else {
        *index = ((*index >> (level * SCALE_POWER)) + 1) << (level * SCALE_POWER);
    }

    if fast_forward {
        // zoom out at the starts of mip-map blocks until there is a change
        while *index <= block_end {
            if level == 0 {
                level += 1;
            }
            let lsp = (level + 1) * SCALE_POWER;
            let offset = ((*index & LEAF_MASK) >> lsp) as usize;
            let mask = !0u64 << ((*index & LEVEL_MASK[level as usize]) >> (level * SCALE_POWER));
            let sample = lbp.get(LEVEL_OFFSET[level as usize] + offset) & mask;
            if sample != 0 {
                *index = (*index & (!0u64 << lsp))
                    + ((bsf_folded(sample) as u64) << (level * SCALE_POWER));
                break;
            }
            *index = ((*index >> lsp) + 1) << lsp;
            level += 1;
        }

        // zoom in until min_level
        while *index <= block_end && level > min_level as u64 {
            level -= 1;
            let lsp = (level + 1) * SCALE_POWER;
            let offset = ((*index & LEAF_MASK) >> lsp) as usize;
            let sh = (*index & LEVEL_MASK[level as usize]) >> (level * SCALE_POWER);
            let low = level == 0 && last_sample;
            let mask = if low { !(!0u64 << sh) } else { !0u64 << sh };
            let word = lbp.get(LEVEL_OFFSET[level as usize] + offset);
            let sample = if low { word | mask } else { word & mask };
            let probe = if level == 0 { sample ^ last } else { sample };
            if probe != 0 {
                *index = (*index & (!0u64 << lsp))
                    + ((bsf_folded(probe) as u64) << (level * SCALE_POWER));
                if level == min_level as u64 {
                    break;
                }
            }
        }
    }

    *index <= block_end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_scans_match_reference() {
        assert_eq!(bsf_folded(0), 63);
        assert_eq!(bsr64(0), 0);
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..100_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            for v in [x, x & x.wrapping_neg(), x >> (x % 64)] {
                if v != 0 {
                    assert_eq!(bsf_folded(v) as u32, v.trailing_zeros());
                    assert_eq!(bsr64(v) as u32, 63 - v.leading_zeros());
                }
            }
        }
    }

    #[test]
    fn leaf_words_match_cpp_leaf_block_space() {
        let space =
            (SCALE + SCALE * SCALE + SCALE * SCALE * SCALE + SCALE * SCALE * SCALE * SCALE) / 8;
        assert_eq!(space % 8, 0);
        assert_eq!(LEAF_WORDS as u64, space / 8);
    }

    fn cross(words: &[u64]) -> Vec<u8> {
        words.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    #[test]
    fn single_channel_roundtrip_and_edges() {
        let mut s = LogicStore::new();
        // 256 samples: low for 100, high for 100, low for 56
        let mut words = [0u64; 4];
        for i in 100..200 {
            words[i / 64] |= 1 << (i % 64);
        }
        s.first_payload(&cross(&words), 256, &[0], true);
        s.capture_ended();
        assert_eq!(s.sample_count(), 256);
        assert!(!s.get_sample(99, 0));
        assert!(s.get_sample(100, 0));
        assert!(!s.get_sample(200, 0));
        let mut idx = 0;
        assert!(s.get_nxt_edge(&mut idx, false, 255, 1.0, 0));
        assert_eq!(idx, 100);
        let mut idx = 101;
        assert!(s.get_nxt_edge(&mut idx, true, 255, 1.0, 0));
        assert_eq!(idx, 200);
        let mut idx = 150;
        assert!(s.get_pre_edge(&mut idx, true, 1.0, 0));
        assert_eq!(idx, 100);
    }

    /// Bits (one `Vec` per channel, LSB first) as LA_CROSS packets of whole
    /// 64-sample groups, the tail of the last group past the data set to
    /// `pad`, as a device's last packet carries it.
    fn cross_channels(chans: &[Vec<bool>], pad: bool) -> Vec<u8> {
        let n = chans[0].len();
        let mut out = Vec::new();
        for g in 0..n.div_ceil(64) {
            for ch in chans {
                let mut w = 0u64;
                for b in 0..64 {
                    if ch.get(g * 64 + b).copied().unwrap_or(pad) {
                        w |= 1 << b;
                    }
                }
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
        out
    }

    /// F11: a length that is not a multiple of 64 is neither shifted nor cut.
    #[test]
    fn unaligned_capture_is_not_shifted() {
        // DEVIATIONS.md F11: 1000 samples, rising edge at 500
        let bits: Vec<bool> = (0..1000).map(|i| i >= 500).collect();
        let mut s = LogicStore::new();
        s.first_payload(&cross_channels(&[bits], true), 1000, &[0], true);
        s.capture_ended();
        assert_eq!(s.sample_count(), 1000);
        assert_eq!(s.ring_sample_count(), 1000);
        assert_eq!(s.loop_offset(), 0);
        assert!(!s.get_sample(499, 0));
        assert!(s.get_sample(500, 0));
        assert!(s.get_sample(999, 0));
        let mut idx = 0;
        assert!(s.get_nxt_edge(&mut idx, false, 999, 1.0, 0));
        assert_eq!(idx, 500);
        // no phantom falling edge in the data
        let mut idx = 501;
        assert!(!s.get_nxt_edge(&mut idx, true, 999, 1.0, 0));
        // one block of 16 whole 64-sample words
        assert_eq!(s.get_block_num(), 1);
        assert_eq!(s.get_block_size(0), 128);
    }

    /// The last block holds the partial word of a length that is not a
    /// multiple of 64.
    #[test]
    fn unaligned_block_size_rounds_up() {
        let bits: Vec<bool> = (0..1025).map(|i| i == 1000).collect();
        let mut s = LogicStore::new();
        s.first_payload(&cross_channels(&[bits], false), 1025, &[0], true);
        s.capture_ended();
        assert_eq!(s.get_block_size(0), 136);
        assert!(s.get_sample(1000, 0));
        assert!(!s.get_sample(999, 0));
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]
        /// Every sample reads back at its index, whatever the length, the
        /// channel count and the packet size.
        #[test]
        fn any_length_reads_back(
            len in 1usize..5000,
            nch in proptest::sample::select(vec![1usize, 2, 3, 16]),
            seed in proptest::prelude::any::<u64>(),
            pad in proptest::prelude::any::<bool>(),
            packet_groups in 1usize..20,
        ) {
            let mut x = seed | 1;
            let mut next = || {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x
            };
            // runs of random length so the edges are sparse and dense
            let chans: Vec<Vec<bool>> = (0..nch)
                .map(|_| {
                    let mut v = Vec::with_capacity(len);
                    let mut level = next() & 1 == 1;
                    while v.len() < len {
                        let run = (next() % 70 + 1) as usize;
                        v.extend(std::iter::repeat_n(level, run.min(len - v.len())));
                        level = !level;
                    }
                    v
                })
                .collect();
            let data = cross_channels(&chans, pad);
            let idx: Vec<u16> = (0..nch as u16).collect();
            let mut s = LogicStore::new();
            let step = packet_groups * 8 * nch;
            let mut parts = data.chunks(step);
            s.first_payload(parts.next().unwrap(), len as u64, &idx, true);
            for p in parts {
                s.append_payload(p);
            }
            s.capture_ended();
            proptest::prop_assert_eq!(s.sample_count(), len as u64);
            proptest::prop_assert_eq!(s.loop_offset(), 0);
            for (c, bits) in chans.iter().enumerate() {
                for (i, &b) in bits.iter().enumerate() {
                    proptest::prop_assert_eq!(s.get_sample(i as u64, c as i32), b, "ch {} sample {}", c, i);
                }
                // the edges found are the data's edges
                let mut want = Vec::new();
                for i in 1..len {
                    if bits[i] != bits[i - 1] {
                        want.push(i as u64);
                    }
                }
                let mut got = Vec::new();
                let mut i = 0u64;
                let mut level = bits[0];
                while s.get_nxt_edge(&mut i, level, len as u64 - 1, 1.0, c as i32) {
                    got.push(i);
                    level = !level;
                }
                proptest::prop_assert_eq!(got, want, "ch {}", c);
            }
        }
    }
}
