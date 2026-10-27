//! Port of `pv::data::AnalogSnapshot` (DSView/pv/data/analogsnapshot.cpp):
//! interleaved multi-byte samples in a ring of `total_sample_count`, with a
//! 10-level min/max envelope per channel (16 samples per envelope sample per
//! level) that `AnalogSignal` draws from.
//!
//! Quirks kept from the C++ code:
//! * the envelope takes only the first (low) byte of each sample;
//! * with `unit_pitch > 1` every `pitch`-th sample is kept, but the phase
//!   restarts at each packet's first byte;
//! * a packet longer than the free ring space wraps once only.

/// `AnalogSnapshot::EnvelopeSample` (`uint8_t min, max`).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnvelopeSample {
    pub min: u8,
    pub max: u8,
}

/// `AnalogSnapshot::EnvelopeSection`.
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeSection {
    pub start: u64,
    pub scale: u32,
    pub length: u64,
    pub samples_num: u64,
    /// The level's ring of `samples_num` (≤ count) entries; null if none.
    pub samples: *const EnvelopeSample,
}

const SCALE_STEP_COUNT: usize = 10;
const ENVELOPE_SCALE_POWER: u32 = 4;
const ENVELOPE_SCALE_FACTOR: u64 = 1 << ENVELOPE_SCALE_POWER;
const LEAF_BLOCK_POWER: u64 = 21;
const LEAF_BLOCK_SAMPLES: u64 = 1 << LEAF_BLOCK_POWER;
const LEAF_MASK: u64 = !(!0u64 << LEAF_BLOCK_POWER);
/// C++ `DS_MAX_ANALOG_PROBES_NUM`.
pub const MAX_ANALOG_PROBES: usize = 4;

#[derive(Clone, Default)]
struct Envelope {
    length: u64,
    ring_length: u64,
    count: u64,
    samples: Vec<EnvelopeSample>,
}

/// A channel of the device, as `first_payload` sees `sr_channel`.
#[derive(Clone, Copy, Debug)]
pub struct AnalogChannel {
    pub index: u16,
    pub enabled: bool,
}

pub struct AnalogStore {
    // pv::data::Snapshot
    ch_index: Vec<u16>,
    capacity: u64,
    channel_num: u32,
    sample_count: u64,
    total_sample_count: u64,
    ring_sample_count: u64,
    unit_bytes: u8,
    unit_pitch: u16,
    memory_failed: bool,
    last_ended: bool,

    // pv::data::AnalogSnapshot
    data: Vec<u8>,
    envelope_levels: Vec<[Envelope; SCALE_STEP_COUNT]>,
    enabled_channel_indexs: Vec<u16>,
}

impl Default for AnalogStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalogStore {
    /// An empty store (C++: constructed; total 1, one channel, unit 1 byte).
    pub fn new() -> AnalogStore {
        AnalogStore {
            ch_index: Vec::new(),
            capacity: 0,
            channel_num: 1,
            sample_count: 0,
            total_sample_count: 1,
            ring_sample_count: 0,
            unit_bytes: 1,
            unit_pitch: 0,
            memory_failed: false,
            last_ended: true,
            data: Vec::new(),
            envelope_levels: vec![Default::default(); MAX_ANALOG_PROBES],
            enabled_channel_indexs: Vec::new(),
        }
    }

    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn total_sample_count(&self) -> u64 {
        self.total_sample_count
    }

    pub fn ring_sample_count(&self) -> u64 {
        self.ring_sample_count
    }

    pub fn channel_num(&self) -> u32 {
        self.channel_num
    }

    pub fn unit_bytes(&self) -> u8 {
        self.unit_bytes
    }

    pub fn memory_failed(&self) -> bool {
        self.memory_failed
    }

    pub fn last_ended(&self) -> bool {
        self.last_ended
    }

    pub fn channel_indexes(&self) -> &[u16] {
        &self.ch_index
    }

    pub fn enabled_channel_indexes(&self) -> &[u16] {
        &self.enabled_channel_indexs
    }

    fn free_envelop(&mut self) {
        for e in self.envelope_levels.iter_mut() {
            *e = Default::default();
        }
    }

    fn init_all(&mut self) {
        self.sample_count = 0;
        self.ring_sample_count = 0;
        self.memory_failed = false;
        self.last_ended = true;
        // C++ resets the lengths of the first `channel_num` channels; `count`
        // stays ("fix hang issue")
        for levels in self
            .envelope_levels
            .iter_mut()
            .take(self.channel_num as usize)
        {
            for e in levels.iter_mut() {
                e.length = 0;
                e.ring_length = 0;
            }
        }
    }

    pub fn init(&mut self) {
        self.init_all();
    }

    fn free_data(&mut self) {
        self.capacity = 0;
        self.sample_count = 0;
        self.ch_index.clear();
        self.data = Vec::new();
    }

    pub fn clear(&mut self) {
        self.free_data();
        self.free_envelop();
        self.init_all();
    }

    pub fn capture_ended(&mut self) {
        self.last_ended = true;
    }

    /// First packet. `channels`: the device's analog channels in order
    /// (disabled ones included, as in C++). `data`: `num_samples` samples of
    /// `channel_num * unit_bytes` bytes, `unit_pitch` as in the datafeed.
    pub fn first_payload(
        &mut self,
        data: &[u8],
        num_samples: u64,
        unit_bits: u8,
        unit_pitch: u16,
        total_sample_count: u64,
        channels: &[AnalogChannel],
    ) {
        self.total_sample_count = total_sample_count;
        self.unit_bytes = unit_bits.div_ceil(8);
        // C++ asserts 1..=8 bytes per sample
        if self.unit_bytes == 0 || self.unit_bytes > 8 {
            self.free_data();
            self.memory_failed = true;
            return;
        }
        self.channel_num = channels.len() as u32;

        let unit_stride = self.channel_num as u64 * self.unit_bytes as u64;
        if unit_stride != 0 && self.total_sample_count > (u64::MAX - 8) / unit_stride {
            self.free_data();
            self.memory_failed = true;
            return;
        }
        let size = self.total_sample_count * unit_stride + 8;

        let mut ok = true;
        if size != self.capacity {
            self.free_data();
            match usize::try_from(size).ok().and_then(crate::try_zeroed) {
                Some(buf) => {
                    self.data = buf;
                    self.free_envelop();
                    'chans: for levels in self
                        .envelope_levels
                        .iter_mut()
                        .take(self.channel_num as usize)
                    {
                        let mut count = self.total_sample_count / ENVELOPE_SCALE_FACTOR;
                        for e in levels.iter_mut() {
                            e.count = count;
                            if count == 0 {
                                break;
                            }
                            let Some(v) = crate::try_zeroed(count as usize) else {
                                ok = false;
                                break 'chans;
                            };
                            e.samples = v;
                            count /= ENVELOPE_SCALE_FACTOR;
                        }
                    }
                }
                None => ok = false,
            }
        }

        if ok {
            self.ch_index = channels.iter().map(|c| c.index).collect();
            self.enabled_channel_indexs = channels
                .iter()
                .filter(|c| c.enabled)
                .map(|c| c.index)
                .collect();
            self.capacity = size;
            self.memory_failed = false;
            self.append_payload(data, num_samples, unit_pitch);
            self.last_ended = false;
        } else {
            self.free_data();
            self.free_envelop();
            self.memory_failed = true;
        }
    }

    pub fn append_payload(&mut self, data: &[u8], num_samples: u64, unit_pitch: u16) {
        if self.data.is_empty() {
            return;
        }
        self.append_data(data, num_samples, unit_pitch);
        if num_samples != 0 {
            self.append_payload_to_envelope_levels();
        }
    }

    /// Copies `n` bytes from `data` at `from` (zeros past its end) to the
    /// ring at byte `to`.
    fn copy_in(&mut self, to: u64, data: &[u8], from: usize, n: u64) {
        let (to, n) = (to as usize, n as usize);
        let Some(dest) = self.data.get_mut(to..to + n) else {
            return;
        };
        let src = data.get(from..).unwrap_or(&[]);
        let k = n.min(src.len());
        dest[..k].copy_from_slice(&src[..k]);
        dest[k..].fill(0);
    }

    fn append_data(&mut self, data: &[u8], mut samples: u64, pitch: u16) {
        let bps = self.unit_bytes as u64 * self.channel_num as u64;
        let total = self.total_sample_count;
        if total == 0 {
            return;
        }
        if pitch <= 1 {
            if self.sample_count + samples < total {
                self.sample_count += samples;
            } else {
                self.sample_count = total;
            }
            if self.ring_sample_count + samples >= total {
                let first = total - self.ring_sample_count;
                self.copy_in(self.ring_sample_count * bps, data, 0, first * bps);
                let from = (first * bps) as usize;
                self.ring_sample_count = (samples + self.ring_sample_count - total) % total;
                self.copy_in(0, data, from, self.ring_sample_count * bps);
            } else {
                self.copy_in(self.ring_sample_count * bps, data, 0, samples * bps);
                self.ring_sample_count += samples;
            }
        } else {
            let mut from = 0usize;
            while samples > 0 {
                samples -= 1;
                if self.unit_pitch == 0 {
                    if self.sample_count < total {
                        self.sample_count += 1;
                    }
                    self.copy_in(self.ring_sample_count * bps, data, from, bps);
                    from += (bps * pitch as u64) as usize;
                    self.ring_sample_count = (self.ring_sample_count + 1) % total;
                    self.unit_pitch = pitch;
                }
                self.unit_pitch -= 1;
            }
        }
    }

    fn append_payload_to_envelope_levels(&mut self) {
        let ub = self.unit_bytes as u64;
        let n = self.channel_num as u64;
        let total = self.total_sample_count;
        let src_size = total * ub * n;
        for i in 0..(self.channel_num as usize).min(MAX_ANALOG_PROBES) {
            let levels = &mut self.envelope_levels[i];
            let e0 = &mut levels[0];
            e0.length = self.sample_count / ENVELOPE_SCALE_FACTOR;
            let prev_length = e0.ring_length;
            e0.ring_length = self.ring_sample_count / ENVELOPE_SCALE_FACTOR;
            if e0.length == 0 {
                continue;
            }

            let mut dest = prev_length;
            let e0_sample_num = if e0.ring_length > prev_length {
                e0.ring_length - prev_length
            } else {
                e0.ring_length + total / ENVELOPE_SCALE_FACTOR - prev_length
            };
            let mut src = (prev_length * ENVELOPE_SCALE_FACTOR * n + i as u64) * ub;
            for _ in 0..e0_sample_num {
                let mut end_src = src + ENVELOPE_SCALE_FACTOR * ub * n;
                if end_src >= src_size {
                    end_src -= src_size;
                }
                let v = self.data[src as usize];
                let mut sub = EnvelopeSample { min: v, max: v };
                src += n * ub;
                while src != end_src {
                    // C++ advances once without wrapping and reads past the
                    // ring: into its 8 spare bytes, or beyond them for rows
                    // wider than 8 bytes (F12); those bytes read as 0 here
                    let v = self.data.get(src as usize).copied().unwrap_or(0);
                    sub.min = sub.min.min(v);
                    sub.max = sub.max.max(v);
                    src += n * ub;
                    if src >= src_size {
                        src -= src_size;
                    }
                }
                // C++ writes one entry past the ring when it starts at `count`
                if dest < e0.count {
                    e0.samples[dest as usize] = sub;
                }
                dest += 1;
                if dest >= e0.count {
                    dest = 0;
                }
            }

            for level in 1..SCALE_STEP_COUNT {
                let (lower, upper) = levels.split_at_mut(level);
                let el = &lower[level - 1];
                let e = &mut upper[0];
                e.length = el.length / ENVELOPE_SCALE_FACTOR;
                let prev_length = e.ring_length;
                e.ring_length = el.ring_length / ENVELOPE_SCALE_FACTOR;
                if e.ring_length == prev_length {
                    break;
                }

                let mut src = prev_length * ENVELOPE_SCALE_FACTOR;
                let end_dest = if e.ring_length == e.count {
                    0
                } else {
                    e.ring_length
                };
                let mut dest = if prev_length == e.count {
                    0
                } else {
                    prev_length
                };
                while dest != end_dest {
                    let mut end_src = src + ENVELOPE_SCALE_FACTOR;
                    if end_src >= el.count {
                        end_src -= el.count;
                    }
                    // C++ reads one entry past the lower ring before it wraps
                    // (a heap over-read, DEVIATIONS.md F12); that entry is
                    // left out here
                    let mut sub = el
                        .samples
                        .get(src as usize)
                        .copied()
                        .unwrap_or(EnvelopeSample { min: 0xff, max: 0 });
                    src += 1;
                    while src != end_src {
                        if let Some(s) = el.samples.get(src as usize) {
                            sub.min = sub.min.min(s.min);
                            sub.max = sub.max.max(s.max);
                        }
                        src += 1;
                        if src >= el.count {
                            src = 0;
                        }
                    }
                    e.samples[dest as usize] = sub;
                    dest += 1;
                    if dest >= e.count {
                        dest = 0;
                    }
                }
            }
        }
    }

    /// Samples from `start_sample` (all channels interleaved). C++ asserts
    /// `start_sample < sample_count`.
    pub fn get_samples(&self, start_sample: u64) -> Option<&[u8]> {
        let at = start_sample.checked_mul(self.unit_bytes as u64 * self.channel_num as u64)?;
        self.data.get(at as usize..)
    }

    /// The whole ring (C++ `get_data`).
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The envelope level for `min_length` samples per pixel of channel
    /// `probe_index` (storage order).
    pub fn get_envelope_section(
        &self,
        start: u64,
        count: i64,
        min_length: f32,
        probe_index: i32,
    ) -> EnvelopeSection {
        let log_factor = (ENVELOPE_SCALE_FACTOR as f32).ln();
        let min_level = (((min_length.ln() / log_factor).floor() as i32) - 1).max(0) as u32;
        let scale_power = (min_level + 1) * ENVELOPE_SCALE_POWER;
        let e = (probe_index >= 0)
            .then(|| self.envelope_levels.get(probe_index as usize))
            .flatten()
            .and_then(|l| l.get(min_level as usize));
        EnvelopeSection {
            start: start.checked_shr(scale_power).unwrap_or(0),
            scale: 1u32.checked_shl(scale_power).unwrap_or(0),
            length: ((count as u64).checked_shr(scale_power)).unwrap_or(0),
            samples_num: e.map_or(0, |e| e.length),
            samples: e
                .filter(|e| !e.samples.is_empty())
                .map_or(std::ptr::null(), |e| e.samples.as_ptr()),
        }
    }

    /// The entries of the level `get_envelope_section` picks for
    /// `min_length` (its `samples`, `samples_num` long) as a slice.
    pub fn get_envelope_samples(&self, min_length: f32, probe_index: i32) -> &[EnvelopeSample] {
        let log_factor = (ENVELOPE_SCALE_FACTOR as f32).ln();
        let min_level = (((min_length.ln() / log_factor).floor() as i32) - 1).max(0) as usize;
        (probe_index >= 0)
            .then(|| self.envelope_levels.get(probe_index as usize))
            .flatten()
            .and_then(|l| l.get(min_level))
            .map_or(&[], |e| {
                &e.samples[..(e.length as usize).min(e.samples.len())]
            })
    }

    /// `ring_start()`: the oldest sample's position once the ring wrapped.
    pub fn ring_start(&self) -> u64 {
        if self.sample_count < self.total_sample_count {
            0
        } else {
            self.ring_sample_count
        }
    }

    /// `ring_end()`: the newest sample's position.
    pub fn ring_end(&self) -> u64 {
        if self.sample_count == 0 {
            0
        } else if self.ring_sample_count == 0 {
            self.total_sample_count - 1
        } else {
            self.ring_sample_count - 1
        }
    }

    /// One envelope level (for tests): length, ring length, count, entries.
    #[doc(hidden)]
    pub fn envelope(&self, probe: usize, level: usize) -> (u64, u64, u64, &[EnvelopeSample]) {
        let e = &self.envelope_levels[probe][level];
        (e.length, e.ring_length, e.count, &e.samples)
    }

    fn get_ch_order(&self, sig_index: i32) -> Option<usize> {
        self.ch_index.iter().position(|&i| i as i32 == sig_index)
    }

    pub fn ch_order(&self, sig_index: i32) -> i32 {
        self.get_ch_order(sig_index).map_or(-1, |o| o as i32)
    }

    pub fn has_data(&self, sig_index: i32) -> bool {
        self.get_ch_order(sig_index).is_some()
    }

    pub fn has_enabled_channel(&self, sig_index: i32) -> bool {
        self.enabled_channel_indexs
            .iter()
            .any(|&i| i as i32 == sig_index)
    }

    pub fn get_block_num(&self) -> i32 {
        let size = self.sample_count * self.unit_bytes as u64 * self.channel_num as u64;
        ((size >> LEAF_BLOCK_POWER) + u64::from(size & LEAF_MASK != 0)) as i32
    }

    pub fn get_block_size(&self, block_index: i32) -> u64 {
        if block_index < self.get_block_num() - 1 {
            LEAF_BLOCK_SAMPLES
        } else {
            let size = self.sample_count * self.unit_bytes as u64 * self.channel_num as u64;
            if size.is_multiple_of(LEAF_BLOCK_SAMPLES) {
                LEAF_BLOCK_SAMPLES
            } else {
                size % LEAF_BLOCK_SAMPLES
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found by fuzzing: 4 channels of 32-bit samples (16-byte rows) wrapping
    /// the ring read past its spare bytes.
    #[test]
    fn wide_rows_wrapping_the_ring() {
        let mut s = AnalogStore::new();
        let chans: Vec<AnalogChannel> = (0..4)
            .map(|i| AnalogChannel {
                index: i,
                enabled: true,
            })
            .collect();
        let total = 1025;
        let data = vec![7u8; 3000 * 16];
        s.first_payload(&data, 3000, 32, 1, total, &chans);
        s.append_payload(&data, 1000, 1);
        assert_eq!(s.sample_count(), total);
    }

    #[test]
    fn ring_and_envelope() {
        let mut s = AnalogStore::new();
        let chans = [AnalogChannel {
            index: 0,
            enabled: true,
        }];
        let data: Vec<u8> = (0..64u8).collect();
        s.first_payload(&data, 64, 8, 1, 48, &chans);
        // 48-sample ring: samples 48..63 wrap to the front
        assert_eq!(s.sample_count(), 48);
        assert_eq!(s.ring_sample_count(), 16);
        assert_eq!(s.get_samples(0).unwrap()[..2], [48, 49]);
        let (len, ring, count, env) = s.envelope(0, 0);
        assert_eq!((len, ring, count), (3, 1, 3));
        assert_eq!(env[0], EnvelopeSample { min: 48, max: 63 });
    }
}
