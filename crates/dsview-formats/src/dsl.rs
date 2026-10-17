//! Reader and writer for `.dsl` / `.demo` capture files (zip archives).
//!
//! The reader yields the same data the C virtual-session driver
//! (libsigrok4DSL/session_driver.c) delivers, so that DSView and the decoders
//! see identical samples whichever implementation loaded the file:
//!
//! * logic, version >= 2: the i-th channel's data is the i-th *existing*
//!   `L-<k>` directory (k scanned from 0), blocks `L-<k>/<b>` for b < total blocks;
//!   the stream interleaves one u64 (64 samples) per channel (LA_CROSS_DATA)
//! * logic, version 1: a single `data` entry of 16-bit sample units
//! * DSO, version >= 3: `O-<k>/0` per channel (same directory rule), bytes interleaved
//! * DSO, version <= 2 and analog: `O-0/<b>` / `A-0/<b>`, already interleaved
//!
//! Every entry is size-checked against `Limits` before it is read.

use std::fs::File;
use std::io::{Read, Seek, Write};
use std::path::Path;

use serde_json::Value;

use crate::Error;
use crate::header::{Header, Mode};

/// Samples per logic block / LogicSnapshot leaf.
pub const LOGIC_BLOCK_SAMPLES: u64 = 1 << 24;
const MAX_DIR_SCAN: u32 = 128;

#[derive(Debug, Clone)]
pub struct Limits {
    /// `header`, `session`, `decoders`.
    pub max_meta_bytes: u64,
    /// Any single data entry.
    pub max_entry_bytes: u64,
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_meta_bytes: 64 << 20,
            max_entry_bytes: 512 << 20,
            max_entries: 1 << 20,
        }
    }
}

/// A chunk of the sample stream, as the C driver would packetize it.
#[derive(Debug)]
pub enum Packet<'a> {
    /// LA_CROSS_DATA: for each 64 samples, one u64 per channel in list order.
    Logic(&'a [u8]),
    /// u8 samples interleaved over the channels.
    Dso { data: &'a [u8], num_samples: u64 },
    /// Interleaved samples of `Header::unit_bytes` each.
    Analog { data: &'a [u8], num_samples: u64 },
}

pub struct DslReader<R: Read + Seek = File> {
    zip: zip::ZipArchive<R>,
    header: Header,
    limits: Limits,
}

impl DslReader<File> {
    pub fn open(path: &Path) -> Result<Self, Error> {
        Self::open_with(path, Limits::default())
    }

    pub fn open_with(path: &Path, limits: Limits) -> Result<Self, Error> {
        Self::new(File::open(path)?, limits)
    }
}

impl<R: Read + Seek> DslReader<R> {
    pub fn new(reader: R, limits: Limits) -> Result<Self, Error> {
        let mut zip = zip::ZipArchive::new(reader)?;
        if zip.len() > limits.max_entries {
            return Err(Error::Limit(format!("{} zip entries", zip.len())));
        }
        let header = read_entry(&mut zip, "header", limits.max_meta_bytes)?
            .ok_or(Error::Missing("header"))?;
        let header = Header::parse(&String::from_utf8_lossy(&header))?;
        Ok(DslReader {
            zip,
            header,
            limits,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// The `session` entry (`.dsc` profile JSON), if present and valid.
    pub fn session_json(&mut self) -> Result<Option<Value>, Error> {
        self.json_entry("session")
    }

    /// The `decoders` entry, if present and valid.
    pub fn decoders_json(&mut self) -> Result<Option<Value>, Error> {
        self.json_entry("decoders")
    }

    fn json_entry(&mut self, name: &str) -> Result<Option<Value>, Error> {
        let Some(raw) = read_entry(&mut self.zip, name, self.limits.max_meta_bytes)? else {
            return Ok(None);
        };
        // DSView reads up to the first NUL (MainWindow, QString from a C string).
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        Ok(serde_json::from_slice(&raw[..end]).ok())
    }

    /// Decoder stacks DSView restores: `session.decoder`, else the `decoders` entry.
    pub fn decoder_stacks(&mut self) -> Result<Vec<Value>, Error> {
        if let Some(list) = self
            .session_json()?
            .and_then(|s| s.get("decoder").and_then(Value::as_array).cloned())
        {
            return Ok(list);
        }
        Ok(match self.decoders_json()? {
            Some(Value::Array(list)) => list,
            _ => Vec::new(),
        })
    }

    /// Raw bytes of a metadata entry (`session`, `decoders`, `header`), size-limited.
    pub fn meta_entry(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        read_entry(&mut self.zip, name, self.limits.max_meta_bytes)
    }

    fn entry(&mut self, name: &str) -> Result<Vec<u8>, Error> {
        read_entry(&mut self.zip, name, self.limits.max_entry_bytes)?
            .ok_or_else(|| Error::MissingEntry(name.to_string()))
    }

    /// Directory index for each of the first `total probes` channels (`L-`/`O-` prefix).
    fn dir_map(&self, prefix: char) -> Vec<u32> {
        let names: std::collections::HashSet<String> = self
            .zip
            .file_names()
            .filter_map(|n| n.ok().map(|c| c.into_owned()))
            .collect();
        let mut map = Vec::new();
        let mut dex = 0u32;
        for _ in 0..self.header.total_probes {
            while dex <= MAX_DIR_SCAN {
                let name = format!("{prefix}-{dex}/0");
                dex += 1;
                if names.contains(name.as_str()) {
                    map.push(dex - 1);
                    break;
                }
            }
        }
        map
    }

    /// Bit-packed (LSB-first) samples of the channel at `pos` in the channel list,
    /// all blocks concatenated, as stored (whole 64-bit words). Logic version >= 2.
    pub fn logic_channel(&mut self, pos: usize) -> Result<Vec<u8>, Error> {
        if self.header.mode != Mode::Logic || self.header.version < 2 {
            return Err(Error::Unsupported(
                "per-channel logic data needs a logic file of version 2 or later",
            ));
        }
        let dir = *self
            .dir_map('L')
            .get(pos)
            .ok_or(Error::Missing("logic channel directory"))?;
        let mut out = Vec::new();
        for b in 0..self.header.total_blocks {
            out.extend(self.entry(&format!("L-{dir}/{b}"))?);
        }
        Ok(out)
    }

    /// Pull-based packet stream with the C driver's packet boundaries.
    pub fn packets(&mut self) -> PacketStream<'_, R> {
        let state = self.start_stream();
        PacketStream {
            reader: self,
            state,
        }
    }

    /// Stream position kept separately from the reader, for handles that own both.
    pub fn start_stream(&self) -> StreamState {
        let h = &self.header;
        let state = match (h.mode, h.version) {
            (Mode::Logic, v) if v >= 2 => State::Interleaved(Interleave::new(
                'L',
                self.dir_map('L'),
                h.total_blocks,
                8,
                LOGIC_POST_WORDS,
            )),
            (Mode::Dso, v) if v >= 3 => State::Interleaved(Interleave::new(
                'O',
                self.dir_map('O'),
                1,
                1,
                DSO_POST_SAMPLES,
            )),
            (Mode::Logic, _) => State::Chunked {
                names: vec!["data".to_string()],
                next: 0,
                data: Vec::new(),
                pos: 0,
            },
            (mode, _) => {
                let prefix = if mode == Mode::Dso { 'O' } else { 'A' };
                State::Chunked {
                    names: (0..h.total_blocks)
                        .map(|b| format!("{prefix}-0/{b}"))
                        .collect(),
                    next: 0,
                    data: Vec::new(),
                    pos: 0,
                }
            }
        };
        StreamState {
            state,
            buf: Vec::new(),
        }
    }

    /// Calls `f` for every packet; see [`DslReader::packets`].
    pub fn stream(&mut self, mut f: impl FnMut(Packet<'_>)) -> Result<(), Error> {
        let mut packets = self.packets();
        while let Some(p) = packets.next_packet()? {
            f(p);
        }
        Ok(())
    }

    /// The next packet of stream `st`, or `None` at the end (where the C driver sends SR_DF_END).
    pub fn next_packet<'s>(
        &mut self,
        st: &'s mut StreamState,
    ) -> Result<Option<Packet<'s>>, Error> {
        let mode = self.header.mode;
        let nprobes = self.header.total_probes.max(1);
        let unit_bytes = self.header.unit_bytes();
        let StreamState { state, buf } = st;
        match state {
            State::Interleaved(st) => {
                if st.done {
                    return Ok(None);
                }
                let nch = st.dirs.len();
                let cap = nch * st.unit * st.post_units;
                buf.clear();
                while buf.len() < cap {
                    let block_len = st.blocks.first().map_or(0, Vec::len);
                    if st.read >= block_len {
                        if st.block >= st.num_blocks {
                            st.done = true;
                            break;
                        }
                        let mut blocks = Vec::with_capacity(nch);
                        for d in &st.dirs {
                            let name = if st.prefix == 'L' {
                                format!("L-{d}/{}", st.block)
                            } else {
                                format!("O-{d}/0")
                            };
                            blocks.push(self.entry(&name)?);
                        }
                        if blocks.iter().any(|b| b.len() != blocks[0].len()) {
                            return Err(Error::Corrupt(format!(
                                "block {}: channel sizes differ",
                                st.block
                            )));
                        }
                        st.blocks = blocks;
                        st.block += 1;
                        st.read = 0;
                        continue;
                    }
                    // Copy unit by unit, rotating through the channels.
                    while buf.len() + st.unit <= cap && block_len - st.read >= st.unit {
                        buf.extend_from_slice(&st.blocks[st.chan][st.read..st.read + st.unit]);
                        st.chan += 1;
                        if st.chan == nch {
                            st.chan = 0;
                            st.read += st.unit;
                        }
                    }
                    if (block_len - st.read) % st.unit != 0 {
                        // C ends the stream here ("The block data is not align with 8 byte.").
                        return Err(Error::Corrupt("logic block is not 8-byte aligned".into()));
                    }
                }
                if buf.len() < nch * st.unit {
                    return Ok(None);
                }
                Ok(Some(if st.prefix == 'L' {
                    Packet::Logic(buf)
                } else {
                    Packet::Dso {
                        data: buf,
                        num_samples: (buf.len() / nch) as u64,
                    }
                }))
            }
            State::Chunked {
                names,
                next,
                data,
                pos,
            } => {
                while *pos >= data.len() {
                    if *next >= names.len() {
                        return Ok(None);
                    }
                    let name = names[*next].clone();
                    *next += 1;
                    *data = self.entry(&name)?;
                    *pos = 0;
                }
                let end = (*pos + CHUNK_SIZE).min(data.len());
                let chunk = &data[*pos..end];
                *pos = end;
                Ok(Some(match mode {
                    Mode::Logic => {
                        // Version 1: 16-bit units, channel bit = probe index, enabled channels only.
                        buf.clear();
                        let enabled: Vec<u16> = self
                            .header
                            .channels
                            .iter()
                            .filter(|c| c.enabled)
                            .map(|c| c.index)
                            .collect();
                        for block in chunk.as_chunks::<128>().0 {
                            for &idx in &enabled {
                                let mut word = 0u64;
                                for j in 0..64 {
                                    let unit = u16::from_le_bytes([block[j * 2], block[j * 2 + 1]]);
                                    if idx < 16 && unit & (1 << idx) != 0 {
                                        word |= 1 << j;
                                    }
                                }
                                buf.extend_from_slice(&word.to_le_bytes());
                            }
                        }
                        Packet::Logic(buf)
                    }
                    Mode::Dso => Packet::Dso {
                        data: chunk,
                        num_samples: chunk.len() as u64 / nprobes,
                    },
                    Mode::Analog => Packet::Analog {
                        data: chunk,
                        num_samples: chunk.len() as u64 / nprobes / unit_bytes,
                    },
                }))
            }
        }
    }
}

/// session_driver.c: logic post buffer = channels * 8 * 200000 bytes.
const LOGIC_POST_WORDS: usize = 200_000;
/// session_driver.c: DSO post buffer = channels * 10000 bytes.
const DSO_POST_SAMPLES: usize = 10_000;
/// session_driver.c CHUNKSIZE: receive_data() reads entries in 512 KiB pieces.
const CHUNK_SIZE: usize = 512 * 1024;

/// receive_data_logic_dso_v2(): per-channel block files, interleaved in units
/// of `unit` bytes into packets of `post_units` units per channel.
struct Interleave {
    prefix: char,
    dirs: Vec<u32>,
    num_blocks: u64,
    unit: usize,
    post_units: usize,
    block: u64,
    blocks: Vec<Vec<u8>>,
    read: usize,
    chan: usize,
    done: bool,
}

impl Interleave {
    fn new(prefix: char, dirs: Vec<u32>, num_blocks: u64, unit: usize, post_units: usize) -> Self {
        let done = dirs.is_empty();
        Interleave {
            prefix,
            dirs,
            num_blocks,
            unit,
            post_units,
            block: 0,
            blocks: Vec::new(),
            read: 0,
            chan: 0,
            done,
        }
    }
}

enum State {
    Interleaved(Interleave),
    /// receive_data(): consecutive entries delivered in CHUNK_SIZE pieces.
    Chunked {
        names: Vec<String>,
        next: usize,
        data: Vec<u8>,
        pos: usize,
    },
}

/// Position in the sample stream; see [`DslReader::next_packet`].
pub struct StreamState {
    state: State,
    buf: Vec<u8>,
}

/// A reader borrowed together with its stream position.
pub struct PacketStream<'r, R: Read + Seek> {
    reader: &'r mut DslReader<R>,
    state: StreamState,
}

impl<R: Read + Seek> PacketStream<'_, R> {
    /// The next packet, or `None` at the end.
    pub fn next_packet(&mut self) -> Result<Option<Packet<'_>>, Error> {
        self.reader.next_packet(&mut self.state)
    }
}

fn read_entry<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Result<Option<Vec<u8>>, Error> {
    let file = match zip.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if file.size() > limit {
        return Err(Error::Limit(format!(
            "entry {name} is {} bytes (limit {limit})",
            file.size()
        )));
    }
    let declared = file.size();
    let mut buf = Vec::with_capacity(declared as usize);
    // Never trust the declared size: cap what is actually decompressed too.
    file.take(limit + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > limit || buf.len() as u64 != declared {
        return Err(Error::Corrupt(format!("entry {name}: size mismatch")));
    }
    Ok(Some(buf))
}

// ------------------------------------------------------------------ writer

/// One channel of a capture to be written.
#[derive(Debug, Clone)]
pub struct WriteChannel {
    pub index: u16,
    pub name: String,
}

/// Writes a version-3 logic capture the way `StoreSession` does: entries
/// `header`, `decoders`, `session`, then `L-<index>/<block>` per channel.
pub struct LogicWriter {
    pub driver: String,
    pub samplerate: u64,
    pub total_samples: u64,
    pub trigger_time_ms: i64,
    pub trigger_pos: u64,
    pub channels: Vec<WriteChannel>,
    pub decoders_json: String,
    pub session_json: String,
}

impl LogicWriter {
    /// Header text in `StoreSession::meta_gen` layout (logic mode).
    pub fn header_text(&self) -> String {
        let blocks = self.total_samples.div_ceil(LOGIC_BLOCK_SAMPLES);
        let mut s = String::new();
        s += "[version]\nversion = 3\n[header]\n";
        s += &format!("driver = {}\ndevice mode = 0\n", self.driver);
        s += "capturefile = data\n";
        s += &format!("total samples = {}\n", self.total_samples);
        s += &format!(
            "total probes = {}\ntotal blocks = {blocks}\n",
            self.channels.len()
        );
        s += &format!(
            "samplerate = {}\n",
            crate::samplerate::format(self.samplerate)
        );
        s += &format!("trigger time = {}\n", self.trigger_time_ms);
        s += &format!("trigger pos = {}\n", self.trigger_pos);
        for ch in &self.channels {
            s += &format!("probe{} = {}\n", ch.index, escape(&ch.name));
        }
        s
    }

    /// `channel_bits[i]`: bit-packed samples of `channels[i]`, at least
    /// `ceil(total_samples / 64) * 8` bytes.
    pub fn write<W: Write + Seek>(&self, out: W, channel_bits: &[&[u8]]) -> Result<W, Error> {
        if channel_bits.len() != self.channels.len() {
            return Err(Error::Unsupported("one bit buffer per channel is required"));
        }
        let words = self.total_samples.div_ceil(64) as usize;
        if channel_bits.iter().any(|b| b.len() < words * 8) {
            return Err(Error::Unsupported(
                "channel buffer shorter than total samples",
            ));
        }
        let mut zip = zip::ZipWriter::new(out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("header", opts)?;
        zip.write_all(self.header_text().as_bytes())?;
        zip.start_file("decoders", opts)?;
        zip.write_all(self.decoders_json.as_bytes())?;
        zip.start_file("session", opts)?;
        zip.write_all(self.session_json.as_bytes())?;

        let block_bytes = (LOGIC_BLOCK_SAMPLES / 8) as usize;
        let total_bytes = words * 8;
        for (ch, bits) in self.channels.iter().zip(channel_bits) {
            for (b, start) in (0..total_bytes).step_by(block_bytes).enumerate() {
                let end = (start + block_bytes).min(total_bytes);
                zip.start_file(format!("L-{}/{b}", ch.index), opts)?;
                zip.write_all(&bits[start..end])?;
            }
        }
        Ok(zip.finish()?)
    }
}

/// GKeyFile escaping for values written into the header.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            ' ' if i == 0 => out.push_str("\\s"),
            c => out.push(c),
        }
    }
    out
}
