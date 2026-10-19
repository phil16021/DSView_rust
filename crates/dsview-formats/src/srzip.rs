//! srzip session files, as libsigrok4DSL's `output/srzip.c` writes them:
//! a zip of `version` ("2"), `metadata` (a GKeyFile) and the logic data,
//! one entry `logic-1-<n>` per packet. The archive is created by the first
//! logic packet; other packets are ignored, as in C.
//!
//! The entries' contents are the C module's; the compressed bytes differ
//! (DEVIATIONS, "Zip entry bytes"). A chunk number past 32767 counts on
//! instead of wrapping (C casts it to `short`).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::Error;
use crate::cfmt::samplerate_string;
use crate::export::{ChannelKind, ExportChannel, ExportPacket};
use crate::keyfile::escape;

/// libsigrok4DSL's `SR_PACKAGE_VERSION_STRING`.
const SIGROK_VERSION: &str = "0.2.0";

pub struct SrzipWriter {
    path: PathBuf,
    channels: Vec<ExportChannel>,
    /// Entry times: local year, month, day, hour, minute, second.
    time: [u16; 6],
    samplerate: u64,
    zip: Option<zip::ZipWriter<BufWriter<File>>>,
    /// The `[device 1]` lines before `unitsize`, until the first chunk.
    meta: Option<String>,
    chunk: u64,
}

impl SrzipWriter {
    /// A writer for `path` (`init`), with the device's channels in their
    /// list order and the local time the entries carry.
    pub fn new(path: &Path, channels: Vec<ExportChannel>, time: [u16; 6]) -> SrzipWriter {
        SrzipWriter {
            path: path.to_owned(),
            channels,
            time,
            samplerate: 0,
            zip: None,
            meta: None,
            chunk: 0,
        }
    }

    fn add(&mut self, name: &str, data: &[u8]) -> Result<(), Error> {
        let [y, mo, d, h, mi, s] = self.time;
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            // Z_BEST_SPEED
            .compression_level(Some(1))
            .large_file(data.len() as u64 >= u64::from(u32::MAX))
            .last_modified_time(
                zip::DateTime::from_date_and_time(y, mo as u8, d as u8, h as u8, mi as u8, s as u8)
                    .unwrap_or_default(),
            );
        let zip = self.zip.as_mut().ok_or(Error::Missing("open archive"))?;
        zip.start_file(name, opts)?;
        zip.write_all(data)?;
        Ok(())
    }

    /// `create_archive`: the file, `version` and the metadata so far.
    fn create(&mut self) -> Result<(), Error> {
        // C unlinks first and writes in place
        let _ = std::fs::remove_file(&self.path);
        self.zip = Some(zip::ZipWriter::new(BufWriter::new(File::create(
            &self.path,
        )?)));
        self.add("version", b"2")?;
        let logic = || {
            self.channels
                .iter()
                .filter(|c| c.kind == ChannelKind::Logic)
        };
        let mut m = format!("[global]\nsigrok version={SIGROK_VERSION}\n\n[device 1]\n");
        if logic().any(|c| c.enabled) {
            m += "capturefile=logic-1\n";
            m += &format!("total probes={}\n", logic().count());
        }
        m += &format!(
            "samplerate={}\n",
            escape(&samplerate_string(self.samplerate))
        );
        for c in logic().filter(|c| c.enabled) {
            m += &format!("probe{}={}\n", u32::from(c.index) + 1, escape(&c.name));
        }
        self.meta = Some(m);
        Ok(())
    }

    /// `receive`
    pub fn receive(&mut self, p: &ExportPacket<'_>) -> Result<(), Error> {
        match *p {
            ExportPacket::Meta {
                samplerate: Some(rate),
                ..
            } => self.samplerate = rate,
            ExportPacket::Logic { data, unitsize, .. } => {
                if self.zip.is_none() {
                    self.create()?;
                }
                if let Some(mut m) = self.meta.take() {
                    m += &format!("unitsize={unitsize}\n");
                    self.add("metadata", m.as_bytes())?;
                }
                self.chunk += 1;
                let name = format!("logic-1-{}", self.chunk);
                self.add(&name, data)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// `cleanup`: completes the archive. True when one was written (there
    /// was logic data).
    pub fn finish(mut self) -> Result<bool, Error> {
        if self.zip.is_none() {
            return Ok(false);
        }
        if let Some(m) = self.meta.take() {
            self.add("metadata", m.as_bytes())?;
        }
        let zip = self.zip.take().ok_or(Error::Missing("open archive"))?;
        zip.finish()?.flush()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn ch(index: u16, kind: ChannelKind, enabled: bool, name: &str) -> ExportChannel {
        ExportChannel {
            index,
            kind,
            enabled,
            name: name.into(),
            vdiv: 0,
            vfactor: 0,
            hw_offset: 0,
            map_max: 0.0,
            map_min: 0.0,
            map_unit: String::new(),
        }
    }

    fn entries(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut z = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
        (0..z.len())
            .map(|i| {
                let mut e = z.by_index(i).unwrap();
                let mut v = Vec::new();
                e.read_to_end(&mut v).unwrap();
                (e.name().unwrap().into_owned(), v)
            })
            .collect()
    }

    #[test]
    fn writes_srzip_c_layout() {
        let path = std::env::temp_dir().join(format!("dsv-srzip-{}.srzip", std::process::id()));
        let chans = vec![
            ch(0, ChannelKind::Logic, true, "SDA"),
            ch(1, ChannelKind::Logic, false, "SCL"),
            ch(2, ChannelKind::Logic, true, " lead"),
        ];
        let mut w = SrzipWriter::new(&path, chans, [2026, 10, 19, 12, 0, 0]);
        w.receive(&ExportPacket::Meta {
            samplerate: Some(1_000_000),
            limit_samples: Some(16),
            ref_min: None,
            ref_max: None,
        })
        .unwrap();
        for d in [[1u8, 2], [3, 4]] {
            w.receive(&ExportPacket::Logic {
                data: &d,
                unitsize: 1,
                export_original: false,
            })
            .unwrap();
        }
        assert!(w.finish().unwrap());
        let e = entries(&path);
        let names: Vec<&str> = e.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["version", "metadata", "logic-1-1", "logic-1-2"]);
        assert_eq!(e[0].1, b"2");
        assert_eq!(
            String::from_utf8(e[1].1.clone()).unwrap(),
            "[global]\nsigrok version=0.2.0\n\n[device 1]\ncapturefile=logic-1\n\
             total probes=3\nsamplerate=1 MHz\nprobe1=SDA\nprobe3=\\slead\nunitsize=1\n"
        );
        assert_eq!(e[3].1, [3, 4]);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn no_logic_data_writes_nothing() {
        let path = std::env::temp_dir().join(format!("dsv-srzip-none-{}", std::process::id()));
        let w = SrzipWriter::new(&path, Vec::new(), [2026, 1, 1, 0, 0, 0]);
        assert!(!w.finish().unwrap());
        assert!(!path.exists());
    }
}
