//! Golden files (gzip-compressed JSON lines) and a diff that reports the
//! first divergence with context.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::{Compression, read::GzDecoder};

pub fn write_gz(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Fixed header (no mtime/name) keeps the bytes reproducible.
    let file = std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut enc = flate2::GzBuilder::new()
        .mtime(0)
        .write(file, Compression::best());
    enc.write_all(text.as_bytes())?;
    enc.finish()?;
    Ok(())
}

pub fn read_gz(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut text = String::new();
    GzDecoder::new(file).read_to_string(&mut text)?;
    Ok(text)
}

#[derive(Debug, PartialEq)]
pub struct Divergence {
    /// 1-based line number of the first differing line.
    pub line: usize,
    pub expected: Option<String>,
    pub actual: Option<String>,
    /// Up to 3 preceding (identical) lines.
    pub context: Vec<String>,
}

impl std::fmt::Display for Divergence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "first divergence at line {}", self.line)?;
        for c in &self.context {
            writeln!(f, "    {c}")?;
        }
        writeln!(
            f,
            "  - {}",
            self.expected.as_deref().unwrap_or("<end of expected>")
        )?;
        write!(
            f,
            "  + {}",
            self.actual.as_deref().unwrap_or("<end of actual>")
        )
    }
}

/// Compares line by line; `None` when identical.
pub fn first_divergence(expected: &str, actual: &str) -> Option<Divergence> {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    let n = e.len().max(a.len());
    (0..n).find(|&i| e.get(i) != a.get(i)).map(|i| Divergence {
        line: i + 1,
        expected: e.get(i).map(|s| s.to_string()),
        actual: a.get(i).map(|s| s.to_string()),
        context: e[i.saturating_sub(3)..i.min(e.len())]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
}

/// Count of differing lines (for summaries).
pub fn count_differences(expected: &str, actual: &str) -> usize {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    (0..e.len().max(a.len()))
        .filter(|&i| e.get(i) != a.get(i))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_has_no_divergence() {
        assert_eq!(first_divergence("a\nb\n", "a\nb\n"), None);
    }

    #[test]
    fn reports_first_difference_with_context() {
        let d = first_divergence("a\nb\nc\nd\n", "a\nb\nX\nd\n").unwrap();
        assert_eq!(d.line, 3);
        assert_eq!(d.expected.as_deref(), Some("c"));
        assert_eq!(d.actual.as_deref(), Some("X"));
        assert_eq!(d.context, vec!["a", "b"]);
    }

    #[test]
    fn length_mismatch() {
        let d = first_divergence("a\nb\n", "a\n").unwrap();
        assert_eq!(d.line, 2);
        assert_eq!(d.actual, None);
        assert_eq!(count_differences("a\nb\n", "a\n"), 1);
    }

    #[test]
    fn gzip_round_trip_is_reproducible() {
        let dir = std::env::temp_dir().join(format!("dsview-testkit-{}", std::process::id()));
        let (p1, p2) = (dir.join("a.jsonl.gz"), dir.join("b.jsonl.gz"));
        write_gz(&p1, "{\"x\":1}\n").unwrap();
        write_gz(&p2, "{\"x\":1}\n").unwrap();
        assert_eq!(read_gz(&p1).unwrap(), "{\"x\":1}\n");
        assert_eq!(std::fs::read(&p1).unwrap(), std::fs::read(&p2).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
