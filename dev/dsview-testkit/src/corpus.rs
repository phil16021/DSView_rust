//! `testdata/corpus.json`: every capture used by differential tests, pinned
//! by SHA-256 so a changed input can never silently change a golden.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Path relative to the repository root.
    pub path: String,
    pub sha256: String,
    /// "logic", "dso" or "analog".
    pub mode: String,
    /// Run the decoder stacks saved in the file through the decode oracle.
    #[serde(default)]
    pub decode: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Corpus {
    pub files: Vec<Entry>,
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&data)))
}

impl Corpus {
    pub fn manifest_path(root: &Path) -> PathBuf {
        root.join("testdata/corpus.json")
    }

    pub fn load(root: &Path) -> Result<Self> {
        let path = Self::manifest_path(root);
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(Self::manifest_path(root), text)?;
        Ok(())
    }

    /// Checks every file exists and still has its recorded hash.
    pub fn verify(&self, root: &Path) -> Result<()> {
        let mut bad = Vec::new();
        for e in &self.files {
            match sha256_file(&root.join(&e.path)) {
                Ok(h) if h == e.sha256 => {}
                Ok(h) => bad.push(format!(
                    "{}: sha256 {h}, manifest says {}",
                    e.path, e.sha256
                )),
                Err(err) => bad.push(format!("{}: {err}", e.path)),
            }
        }
        if !bad.is_empty() {
            bail!("corpus mismatch:\n  {}", bad.join("\n  "));
        }
        Ok(())
    }

    /// File stem used to name goldens, e.g. "logic-protocol" for DSView/demo/logic/protocol.demo.
    pub fn golden_name(entry: &Entry) -> String {
        let p = Path::new(&entry.path);
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("capture");
        format!("{}-{stem}", entry.mode)
    }
}
