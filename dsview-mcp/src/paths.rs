//! Which files the agent may open or write.
//!
//! Every path a tool receives must resolve inside one of the allowed
//! directories (`DSVIEW_MCP_ALLOWED_DIRS`, default `~/DSView`). Paths are
//! canonicalized, so `..` and symlinked directories cannot escape a root,
//! and an existing symlink is never written through.

use std::path::{Path, PathBuf};

use crate::bridge::home_dir;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Access {
    /// The file must already exist.
    Read,
    /// The file may be created or replaced.
    Write,
}

#[derive(Debug)]
pub struct PathPolicy {
    roots: Vec<PathBuf>,
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None if raw == "~" => home_dir(),
        None => PathBuf::from(raw),
    }
}

impl PathPolicy {
    pub fn from_env() -> Result<Self, String> {
        let roots: Vec<PathBuf> = match std::env::var("DSVIEW_MCP_ALLOWED_DIRS") {
            Ok(list) if !list.trim().is_empty() => {
                list.split(':').filter(|s| !s.trim().is_empty()).map(|s| expand_tilde(s.trim())).collect()
            }
            _ => vec![home_dir().join("DSView")],
        };
        for root in &roots {
            std::fs::create_dir_all(root)
                .map_err(|e| format!("cannot create allowed directory {}: {e}", root.display()))?;
        }
        Self::new(&roots)
    }

    pub fn new(roots: &[PathBuf]) -> Result<Self, String> {
        let mut canonical = Vec::new();
        for root in roots {
            let c = root
                .canonicalize()
                .map_err(|e| format!("allowed directory {} is not usable: {e}", root.display()))?;
            if !c.is_dir() {
                return Err(format!("allowed directory {} is not a directory", root.display()));
            }
            canonical.push(c);
        }
        if canonical.is_empty() {
            return Err("no allowed directories configured".into());
        }
        Ok(Self { roots: canonical })
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    fn inside(&self, path: &Path) -> bool {
        // Path::starts_with compares whole components, so /a/bc is not inside /a/b.
        self.roots.iter().any(|root| path.starts_with(root))
    }

    /// Resolves `raw` (absolute, `~/...`, or relative to the first allowed
    /// directory) and checks it against the roots and allowed extensions.
    pub fn check(&self, raw: &str, access: Access, extensions: &[&str]) -> Result<PathBuf, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("path is empty".into());
        }
        let mut path = expand_tilde(raw);
        if path.is_relative() {
            path = self.roots[0].join(path);
        }

        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        if !ext.as_deref().is_some_and(|e| extensions.contains(&e)) {
            return Err(format!("file must end with .{}", extensions.join(" or .")));
        }

        let resolved = match access {
            Access::Read => {
                let c = path.canonicalize().map_err(|e| format!("cannot open {}: {e}", path.display()))?;
                if !c.is_file() {
                    return Err(format!("{} is not a regular file", path.display()));
                }
                c
            }
            Access::Write => {
                let name = path.file_name().ok_or_else(|| "path has no file name".to_string())?;
                let parent = path.parent().ok_or_else(|| "path has no parent directory".to_string())?;
                let parent = parent
                    .canonicalize()
                    .map_err(|e| format!("directory {} does not exist: {e}", parent.display()))?;
                let target = parent.join(name);
                if let Ok(meta) = std::fs::symlink_metadata(&target) {
                    if meta.file_type().is_symlink() {
                        return Err(format!("refusing to write through symlink {}", target.display()));
                    }
                    if meta.is_dir() {
                        return Err(format!("{} is a directory", target.display()));
                    }
                }
                target
            }
        };

        if !self.inside(&resolved) {
            return Err(format!(
                "{} is outside the allowed directories ({}); set DSVIEW_MCP_ALLOWED_DIRS to change them",
                resolved.display(),
                self.roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", ")
            ));
        }
        Ok(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("dsview-mcp-paths-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(tag: &str) -> (TempDir, PathPolicy, PathBuf) {
        let tmp = TempDir::new(tag);
        let root = tmp.0.join("allowed");
        let outside = tmp.0.join("outside");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let policy = PathPolicy::new(std::slice::from_ref(&root)).unwrap();
        (tmp, policy, outside)
    }

    #[test]
    fn write_inside_root_is_allowed() {
        let (_tmp, policy, _) = setup("inside");
        let p = policy.check("capture.dsl", Access::Write, &["dsl"]).unwrap();
        assert!(p.starts_with(&policy.roots()[0]));
        assert!(p.ends_with("capture.dsl"));
    }

    #[test]
    fn outside_and_dotdot_are_rejected() {
        let (_tmp, policy, outside) = setup("outside");
        let abs = outside.join("x.dsl");
        assert!(policy.check(abs.to_str().unwrap(), Access::Write, &["dsl"]).is_err());
        assert!(policy.check("../outside/x.dsl", Access::Write, &["dsl"]).is_err());
    }

    #[test]
    fn similar_prefix_is_not_inside() {
        let tmp = TempDir::new("prefix");
        let root = tmp.0.join("data");
        let sibling = tmp.0.join("data2");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let policy = PathPolicy::new(&[root]).unwrap();
        let p = sibling.join("x.csv");
        assert!(policy.check(p.to_str().unwrap(), Access::Write, &["csv"]).is_err());
    }

    #[test]
    fn symlink_escapes_are_rejected() {
        let (_tmp, policy, outside) = setup("symlink");
        let root = policy.roots()[0].clone();

        // A symlinked directory pointing outside.
        symlink(&outside, root.join("link")).unwrap();
        assert!(policy.check("link/x.dsl", Access::Write, &["dsl"]).is_err());

        // A symlinked file, even one pointing inside, is never written through.
        std::fs::write(outside.join("victim.csv"), "x").unwrap();
        symlink(outside.join("victim.csv"), root.join("out.csv")).unwrap();
        assert!(policy.check("out.csv", Access::Write, &["csv"]).is_err());

        // Reading through a symlink that leads outside is rejected too.
        std::fs::write(outside.join("secret.dsl"), "x").unwrap();
        symlink(outside.join("secret.dsl"), root.join("secret.dsl")).unwrap();
        assert!(policy.check("secret.dsl", Access::Read, &["dsl"]).is_err());
    }

    #[test]
    fn extension_is_enforced() {
        let (_tmp, policy, _) = setup("ext");
        assert!(policy.check("x.txt", Access::Write, &["csv", "vcd"]).is_err());
        assert!(policy.check("x", Access::Write, &["dsl"]).is_err());
        assert!(policy.check("X.CSV", Access::Write, &["csv"]).is_ok());
    }

    #[test]
    fn read_requires_existing_file() {
        let (_tmp, policy, _) = setup("read");
        assert!(policy.check("missing.dsl", Access::Read, &["dsl"]).is_err());
        std::fs::write(policy.roots()[0].join("have.dsl"), "x").unwrap();
        assert!(policy.check("have.dsl", Access::Read, &["dsl"]).is_ok());
    }
}
