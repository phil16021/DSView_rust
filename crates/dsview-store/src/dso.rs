//! Port of `pv::data::DsoSnapshot` (DSView/pv/data/dsosnapshot.cpp): one
//! frame of oscilloscope samples (one byte per sample), de-interleaved into
//! a buffer per channel.
//!
//! Not ported: the envelope mip-map, `cal_vrms` and `cal_vmean`. Nothing in
//! DSView calls them (`enable_envelope` is never called, so the envelope is
//! never built); `get_envelope_section` answers "no envelope" as the C++
//! class does before `enable_envelope`.

/// Bytes per block for saving (C++ `LeafBlockSamples`).
const LEAF_BLOCK_POWER: u64 = 21;
const LEAF_BLOCK_SAMPLES: u64 = 1 << LEAF_BLOCK_POWER;
const LEAF_MASK: u64 = !(!0u64 << LEAF_BLOCK_POWER);

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
        self.is_file = false;
    }

    pub fn init(&mut self) {
        self.init_all();
    }

    pub fn clear(&mut self) {
        self.ch_index.clear();
        self.sample_count = 0;
        self.ch_data.clear();
        self.init_all();
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
    pub fn first_payload(
        &mut self,
        data: &[u8],
        num_samples: u64,
        total_sample_count: u64,
        channels: &[DsoChannel],
        instant: bool,
        is_file: bool,
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
            }
        }

        if ok {
            self.memory_failed = false;
            self.append_payload(data, num_samples);
            self.last_ended = false;
        } else {
            self.ch_index.clear();
            self.sample_count = 0;
            self.ch_data.clear();
            self.memory_failed = true;
        }
    }

    /// Further frames (instant mode appends; otherwise each frame replaces
    /// the previous one). Samples missing from a short `data` read as 0.
    pub fn append_payload(&mut self, data: &[u8], num_samples: u64) {
        if self.channel_num > 0 && num_samples > 0 {
            self.append_data(data, num_samples);
        }
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
        s.first_payload(&[1, 100, 2, 101, 3, 102], 3, 3, &chans, false, false);
        assert_eq!(s.get_samples(0, 0).unwrap()[..3], [1, 2, 3]);
        assert_eq!(s.get_samples(1, 1).unwrap()[..2], [101, 102]);
        assert!(s.data_out_off_range());
        assert_eq!(s.get_max_min_value(1), Some((102, 100)));
        assert_eq!((s.get_block_num(), s.get_block_size(0)), (1, 6));
    }
}
