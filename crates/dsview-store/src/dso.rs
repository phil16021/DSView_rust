//! Port of `pv::data::DsoSnapshot` (DSView/pv/data/dsosnapshot.cpp): one
//! frame of oscilloscope samples (one byte per sample), de-interleaved into
//! a buffer per channel.
//!
//! The envelope (a min/max mip-map, 256 samples per entry and level, ten
//! levels) is built once `DsoSignal` asks for it: it draws a frame from the
//! envelope at 256 samples per pixel and more.
//!
//! Not ported: `cal_vrms` and `cal_vmean`, which nothing in DSView calls.

/// Bytes per block for saving (C++ `LeafBlockSamples`).
const LEAF_BLOCK_POWER: u64 = 21;
const LEAF_BLOCK_SAMPLES: u64 = 1 << LEAF_BLOCK_POWER;
const LEAF_MASK: u64 = !(!0u64 << LEAF_BLOCK_POWER);

/// `EnvelopeScalePower`, `EnvelopeScaleFactor`, `ScaleStepCount`
const ENVELOPE_SCALE_POWER: u32 = 8;
const ENVELOPE_SCALE_FACTOR: u64 = 1 << ENVELOPE_SCALE_POWER;
const SCALE_STEP_COUNT: usize = 10;

/// One envelope entry: the minimum and maximum code of its samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct EnvelopeSample {
    pub min: u8,
    pub max: u8,
}

/// `DsoSnapshot::EnvelopeSection`: `samples` holds `length` entries of
/// `scale` samples each, the first covering sample `start`.
#[derive(Clone, Copy, Debug)]
pub struct EnvelopeSection<'a> {
    pub start: u64,
    pub scale: u32,
    pub length: u64,
    pub samples: &'a [EnvelopeSample],
}

/// `struct Envelope`: the logical length of one level. The storage only
/// grows, so entries past a shrunken length keep their old values, as in C.
#[derive(Clone, Default)]
struct Envelope {
    length: u64,
    samples: Vec<EnvelopeSample>,
}

impl Envelope {
    /// `reallocate_envelope` plus the room C allocated up front.
    fn reserve(&mut self) {
        if self.samples.len() < self.length as usize {
            self.samples
                .resize(self.length as usize, EnvelopeSample::default());
        }
    }
}

/// A channel of the device, as `first_payload` sees `sr_channel`.
#[derive(Clone, Copy, Debug)]
pub struct DsoChannel {
    pub index: u16,
    pub enabled: bool,
}

#[derive(Default)]
pub struct DsoStore {
    // pv::data::Snapshot
    ch_index: Vec<u16>,
    channel_num: u32,
    sample_count: u64,
    total_sample_count: u64,
    memory_failed: bool,
    last_ended: bool,

    // pv::data::DsoSnapshot
    ch_data: Vec<Vec<u8>>,
    instant: bool,
    is_file: bool,
    ref_min: u32,
    ref_max: u32,
    data_out_off_range: bool,
    /// Per channel (storage order) and level.
    envelope_levels: Vec<[Envelope; SCALE_STEP_COUNT]>,
    envelope_en: bool,
    envelope_done: bool,
}

impl DsoStore {
    /// An empty store (C++: constructed; `total_sample_count` starts at 1).
    pub fn new() -> DsoStore {
        DsoStore {
            total_sample_count: 1,
            last_ended: true,
            ..Default::default()
        }
    }

    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn total_sample_count(&self) -> u64 {
        self.total_sample_count
    }

    pub fn channel_num(&self) -> u32 {
        self.channel_num
    }

    pub fn memory_failed(&self) -> bool {
        self.memory_failed
    }

    pub fn last_ended(&self) -> bool {
        self.last_ended
    }

    pub fn is_file(&self) -> bool {
        self.is_file
    }

    pub fn is_instant(&self) -> bool {
        self.instant
    }

    pub fn data_out_off_range(&self) -> bool {
        self.data_out_off_range
    }

    pub fn set_ref_range(&mut self, ref_max: u32, ref_min: u32) {
        self.ref_max = ref_max;
        self.ref_min = ref_min;
    }

    pub fn channel_indexes(&self) -> &[u16] {
        &self.ch_index
    }

    fn init_all(&mut self) {
        self.sample_count = 0;
        self.memory_failed = false;
        self.last_ended = true;
        self.envelope_done = false;
        self.is_file = false;
        let n = self.channel_num as usize;
        for levels in self.envelope_levels.iter_mut().take(n) {
            for e in levels.iter_mut() {
                e.length = 0;
            }
        }
    }

    pub fn init(&mut self) {
        self.init_all();
    }

    pub fn clear(&mut self) {
        self.ch_index.clear();
        self.sample_count = 0;
        self.ch_data.clear();
        self.envelope_levels.clear();
        self.init_all();
        self.envelope_en = false;
    }

    pub fn capture_ended(&mut self) {
        self.last_ended = true;
    }

    fn get_ch_order(&self, sig_index: i32) -> Option<usize> {
        self.ch_index.iter().position(|&i| i as i32 == sig_index)
    }

    pub fn has_data(&self, sig_index: i32) -> bool {
        self.get_ch_order(sig_index).is_some()
    }

    /// First frame. `channels`: the device's DSO channels in order; a file
    /// session uses all of them, a device the enabled ones. `data` holds
    /// `num_samples` interleaved samples (one byte per channel).
    // the datafeed packet's fields, as the C++ signature takes them
    #[allow(clippy::too_many_arguments)]
    pub fn first_payload(
        &mut self,
        data: &[u8],
        num_samples: u64,
        total_sample_count: u64,
        channels: &[DsoChannel],
        instant: bool,
        is_file: bool,
        samplerate_tog: bool,
    ) {
        self.is_file = is_file;
        let mut channel_changed = false;
        let mut channel_num: u16 = 0;
        for ch in channels.iter().filter(|c| c.enabled || is_file) {
            channel_num = channel_num.wrapping_add(1);
            if !channel_changed {
                channel_changed = !self.has_data(ch.index as i32);
            }
        }
        self.instant = instant;
        let mut ok = true;

        if total_sample_count != self.total_sample_count
            || channel_num as u32 != self.channel_num
            || channel_changed
            || is_file
        {
            self.ch_index.clear();
            self.sample_count = 0;
            self.ch_data.clear();
            self.total_sample_count = total_sample_count;
            self.channel_num = channel_num as u32;
            // free_envelop: every level's length back to 0
            self.envelope_levels.clear();
            for ch in channels.iter().filter(|c| c.enabled || is_file) {
                let buf = total_sample_count
                    .checked_add(1)
                    .and_then(|n| usize::try_from(n).ok())
                    .and_then(crate::try_zeroed);
                let Some(buf) = buf else {
                    ok = false;
                    break;
                };
                self.ch_data.push(buf);
                self.ch_index.push(ch.index);
                self.envelope_levels.push(Default::default());
            }
        }

        if ok {
            self.memory_failed = false;
            self.append_payload(data, num_samples, samplerate_tog);
            self.last_ended = false;
        } else {
            self.ch_index.clear();
            self.sample_count = 0;
            self.ch_data.clear();
            self.envelope_levels.clear();
            self.memory_failed = true;
        }
    }

    /// Further frames (instant mode appends; otherwise each frame replaces
    /// the previous one). Samples missing from a short `data` read as 0.
    /// `samplerate_tog` (the packet's flag) rebuilds the envelope from the
    /// start instead of extending it.
    pub fn append_payload(&mut self, data: &[u8], num_samples: u64, samplerate_tog: bool) {
        if self.channel_num > 0 && num_samples > 0 {
            self.append_data(data, num_samples);
            if self.envelope_en {
                self.append_payload_to_envelope_levels(samplerate_tog);
            }
        }
    }

    /// `enable_envelope`: builds the envelope of the frame so far, and keeps
    /// it up to date with the frames that follow.
    pub fn enable_envelope(&mut self, enable: bool) {
        if !self.envelope_done && enable {
            self.append_payload_to_envelope_levels(true);
        }
        self.envelope_en = enable;
    }

    /// `get_envelope_section`: the level for `min_length` samples per pixel
    /// over `start..end` of channel `probe_index` (storage position); length
    /// 0 while there is no envelope. C++ asserts `start <= end <=
    /// sample_count` and `min_length > 0`.
    pub fn get_envelope_section(
        &self,
        start: u64,
        end: u64,
        min_length: f32,
        probe_index: usize,
    ) -> EnvelopeSection<'_> {
        let none = EnvelopeSection {
            start: 0,
            scale: 0,
            length: 0,
            samples: &[],
        };
        if !self.envelope_done {
            return none;
        }
        let log_factor = (ENVELOPE_SCALE_FACTOR as f32).ln();
        let min_level = ((min_length.ln() / log_factor).floor() as i32 - 1).max(0) as u32;
        let scale_power = (min_level + 1) * ENVELOPE_SCALE_POWER;
        let (start, end) = (start >> scale_power, end >> scale_power);
        let Some(level) = self
            .envelope_levels
            .get(probe_index)
            .and_then(|l| l.get(min_level as usize))
        else {
            // C reads an all-zero level of its fixed array
            return EnvelopeSection {
                start: start << scale_power,
                scale: 1 << scale_power,
                ..none
            };
        };
        let length = if level.length == 0 { 0 } else { end - start };
        EnvelopeSection {
            start: start << scale_power,
            scale: 1 << scale_power,
            length,
            samples: level.samples.get(start as usize..).unwrap_or(&[]),
        }
    }

    /// `append_payload_to_envelope_levels`: the first level from the
    /// samples, every further level from the one below. Only what was added
    /// since the last call is computed, unless `header` or a length that did
    /// not change asks for all of it. A channel whose frame is shorter than
    /// one entry ends the whole update (and leaves the envelope not done),
    /// as in C.
    fn append_payload_to_envelope_levels(&mut self, header: bool) {
        let factor = ENVELOPE_SCALE_FACTOR as usize;
        for i in 0..self.channel_num as usize {
            let Some(levels) = self.envelope_levels.get_mut(i) else {
                return;
            };
            let e0 = &mut levels[0];
            let mut prev_length = if header { 0 } else { e0.length };
            e0.length = self.sample_count / ENVELOPE_SCALE_FACTOR;
            if e0.length == 0 {
                return;
            }
            if e0.length == prev_length {
                prev_length = 0;
            }
            e0.reserve();
            let src = &self.ch_data[i];
            for k in prev_length as usize..e0.length as usize {
                let block = &src[k * factor..(k + 1) * factor];
                let mut s = EnvelopeSample {
                    min: block[0],
                    max: block[0],
                };
                for &v in block {
                    s.min = s.min.min(v);
                    s.max = s.max.max(v);
                }
                e0.samples[k] = s;
            }
            for level in 1..SCALE_STEP_COUNT {
                let (lower, upper) = levels.split_at_mut(level);
                let el = &lower[level - 1];
                let e = &mut upper[0];
                let mut prev_length = if header { 0 } else { e.length };
                e.length = el.length / ENVELOPE_SCALE_FACTOR;
                if e.length == 0 {
                    break;
                }
                if e.length == prev_length {
                    prev_length = 0;
                }
                e.reserve();
                for k in prev_length as usize..e.length as usize {
                    let block = &el.samples[k * factor..(k + 1) * factor];
                    let mut s = block[0];
                    for v in &block[1..] {
                        s.min = s.min.min(v.min);
                        s.max = s.max.max(v.max);
                    }
                    e.samples[k] = s;
                }
            }
        }
        self.envelope_done = true;
    }

    fn append_data(&mut self, data: &[u8], mut samples: u64) {
        let old_sample_count = self.sample_count;
        self.data_out_off_range = false;
        if self.instant {
            if self.sample_count + samples > self.total_sample_count {
                samples = self.total_sample_count.wrapping_sub(self.sample_count);
            }
            self.sample_count = self.sample_count.wrapping_add(samples);
        } else {
            samples = samples.min(self.total_sample_count);
            self.sample_count = samples;
        }

        let n = self.channel_num as usize;
        for (ch, buf) in self.ch_data.iter_mut().enumerate().take(n) {
            let dest0 = if self.instant {
                old_sample_count as usize
            } else {
                0
            };
            for i in 0..samples as usize {
                let v = data.get(ch + i * n).copied().unwrap_or(0);
                if let Some(d) = buf.get_mut(dest0 + i) {
                    *d = v;
                }
                if v as u32 > self.ref_max || (v as u32) < self.ref_min {
                    self.data_out_off_range = true;
                }
            }
        }
    }

    /// Channel `sig_index`'s samples from `start` (C++ asserts the range and
    /// the channel); None for an unknown channel.
    pub fn get_samples(&self, start_sample: u64, sig_index: i32) -> Option<&[u8]> {
        let order = self.get_ch_order(sig_index)?;
        self.ch_data[order].get(start_sample as usize..)
    }

    /// Bytes of block `block_index` when saving.
    pub fn get_block_num(&self) -> i32 {
        let size = self.sample_count * self.channel_num as u64;
        ((size >> LEAF_BLOCK_POWER) + u64::from(size & LEAF_MASK != 0)) as i32
    }

    pub fn get_block_size(&self, block_index: i32) -> u64 {
        if block_index < self.get_block_num() - 1 {
            LEAF_BLOCK_SAMPLES
        } else {
            let size = self.sample_count * self.channel_num as u64;
            if size.is_multiple_of(LEAF_BLOCK_SAMPLES) {
                LEAF_BLOCK_SAMPLES
            } else {
                size % LEAF_BLOCK_SAMPLES
            }
        }
    }

    /// (max, min) of channel `chan_order` (storage order, not index).
    pub fn get_max_min_value(&self, chan_order: i32) -> Option<(u8, u8)> {
        if self.sample_count == 0 || chan_order < 0 {
            return None;
        }
        let buf = self.ch_data.get(chan_order as usize)?;
        let s = &buf[..(self.sample_count as usize).min(buf.len())];
        let first = *s.first()?;
        Some(
            s.iter()
                .fold((first, first), |(mx, mn), &v| (mx.max(v), mn.min(v))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_deinterleaved() {
        let mut s = DsoStore::new();
        let chans = [
            DsoChannel {
                index: 0,
                enabled: true,
            },
            DsoChannel {
                index: 1,
                enabled: true,
            },
        ];
        s.set_ref_range(200, 10);
        s.first_payload(&[1, 100, 2, 101, 3, 102], 3, 3, &chans, false, false, false);
        assert_eq!(s.get_samples(0, 0).unwrap()[..3], [1, 2, 3]);
        assert_eq!(s.get_samples(1, 1).unwrap()[..2], [101, 102]);
        assert!(s.data_out_off_range());
        assert_eq!(s.get_max_min_value(1), Some((102, 100)));
        assert_eq!((s.get_block_num(), s.get_block_size(0)), (1, 6));
    }
}
