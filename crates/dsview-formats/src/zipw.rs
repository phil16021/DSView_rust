//! Writing `.dsl` archives entry by entry (what `ZipMaker` does for
//! `StoreSession`), with the safety the C version lacks:
//!
//! * only DSView's entry names are accepted (`header`, `decoders`,
//!   `session`, `L|O|A-<n>/<n>`), each at most once
//! * the `header` must parse, so a saved file can always be opened again
//! * every write error is reported (ZipMaker ignores them)
//! * the archive is written to a temporary file next to the target, checked
//!   by reopening it, and only then renamed into place: a failed or
//!   cancelled save never leaves a truncated `.dsl`

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::Error;
use crate::dsl::DslReader;
use crate::header::Header;

pub struct DslZipWriter {
    zip: Option<zip::ZipWriter<BufWriter<File>>>,
    tmp: PathBuf,
    dest: PathBuf,
    names: HashSet<String>,
    done: bool,
}

fn valid_name(name: &str) -> bool {
    if matches!(name, "header" | "decoders" | "session") {
        return true;
    }
    let Some(rest) = name
        .strip_prefix("L-")
        .or_else(|| name.strip_prefix("O-"))
        .or_else(|| name.strip_prefix("A-"))
    else {
        return false;
    };
    let Some((a, b)) = rest.split_once('/') else {
        return false;
    };
    let num = |s: &str| !s.is_empty() && s.len() <= 9 && s.bytes().all(|c| c.is_ascii_digit());
    num(a) && num(b)
}

impl DslZipWriter {
    pub fn create(dest: &Path) -> Result<Self, Error> {
        let file_name = dest
            .file_name()
            .ok_or(Error::Missing("file name"))?
            .to_string_lossy()
            .into_owned();
        let tmp = dest.with_file_name(format!(".{file_name}.part-{}", std::process::id()));
        let file = File::create(&tmp)?;
        Ok(DslZipWriter {
            zip: Some(zip::ZipWriter::new(BufWriter::new(file))),
            tmp,
            dest: dest.to_path_buf(),
            names: HashSet::new(),
            done: false,
        })
    }

    pub fn add(&mut self, name: &str, data: &[u8]) -> Result<(), Error> {
        if !valid_name(name) {
            return Err(Error::Corrupt(format!(
                "refusing to write zip entry {name:?}"
            )));
        }
        if !self.names.insert(name.to_string()) {
            return Err(Error::Corrupt(format!("zip entry {name:?} written twice")));
        }
        if name == "header" {
            Header::parse(&String::from_utf8_lossy(data))?;
        }
        let zip = self.zip.as_mut().ok_or(Error::Missing("open archive"))?;
        // ZipMaker uses Z_BEST_SPEED.
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(1))
            .large_file(data.len() as u64 >= u64::from(u32::MAX));
        zip.start_file(name, opts)?;
        zip.write_all(data)?;
        Ok(())
    }

    /// Completes the archive, verifies it opens, and moves it into place.
    pub fn finish(mut self) -> Result<(), Error> {
        let zip = self.zip.take().ok_or(Error::Missing("open archive"))?;
        let mut out = zip.finish()?;
        out.flush()?;
        out.get_ref().sync_all()?;
        drop(out);
        if !self.names.contains("header") {
            return Err(Error::Missing("header"));
        }
        DslReader::open(&self.tmp)?;
        std::fs::rename(&self.tmp, &self.dest)?;
        self.done = true;
        Ok(())
    }
}

impl Drop for DslZipWriter {
    fn drop(&mut self) {
        if !self.done {
            drop(self.zip.take());
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "[version]\nversion = 3\n[header]\ndriver = x\ndevice mode = 0\ncapturefile = data\ntotal samples = 64\ntotal probes = 1\ntotal blocks = 1\nsamplerate = 1 MHz\nprobe0 = A\n";

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dsview-zipw-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn writes_a_readable_capture_atomically() {
        let d = dir("ok");
        let dest = d.join("cap.dsl");
        let mut w = DslZipWriter::create(&dest).unwrap();
        w.add("header", HEADER.as_bytes()).unwrap();
        w.add("session", b"{}").unwrap();
        w.add("L-0/0", &[0xAA; 8]).unwrap();
        assert!(!dest.exists(), "nothing is visible before finish");
        w.finish().unwrap();
        let mut r = DslReader::open(&dest).unwrap();
        assert_eq!(r.logic_channel(0).unwrap(), vec![0xAA; 8]);
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            1,
            "no temporary file left"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn rejects_bad_names_duplicates_and_headers() {
        let d = dir("bad");
        let mut w = DslZipWriter::create(&d.join("x.dsl")).unwrap();
        assert!(w.add("../evil", b"x").is_err());
        assert!(w.add("/abs", b"x").is_err());
        assert!(w.add("L-1", b"x").is_err());
        assert!(w.add("header", b"[header]\ntotal probes = 0\n").is_err());
        w.add("L-0/0", b"12345678").unwrap();
        assert!(w.add("L-0/0", b"12345678").is_err());
        assert!(w.finish().is_err(), "no header");
        assert_eq!(
            std::fs::read_dir(&d).unwrap().count(),
            0,
            "temporary file removed"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn abandoned_writer_leaves_nothing() {
        let d = dir("drop");
        {
            let mut w = DslZipWriter::create(&d.join("x.dsl")).unwrap();
            w.add("header", HEADER.as_bytes()).unwrap();
        }
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
