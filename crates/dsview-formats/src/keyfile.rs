//! Parser for the INI dialect of GLib's `GKeyFile`, which DSView's C loader
//! (`g_key_file_load_from_data` + `g_key_file_get_string`) uses for the
//! `header` entry of `.dsl` files. Behaviour copied from GKeyFile:
//!
//! * leading whitespace of every line is ignored; `#` lines and blank lines are comments
//! * `[group]` starts a group; a repeated group name continues the same group
//! * `key = value`: whitespace around the key and before the value is dropped,
//!   trailing whitespace of the value is kept
//! * a repeated key keeps its first position and takes the last value
//! * a key outside any group, or a line that is neither, makes the whole file invalid
//! * values are unescaped on read: `\s \n \t \r \\`; any other escape makes
//!   `get_string` fail (`None`), as in GLib

use crate::Error;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    /// Raw (still escaped) values, in first-seen order.
    entries: Vec<(String, String)>,
}

impl Group {
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    /// `g_key_file_get_string`: the unescaped value, `None` for a missing key
    /// or an invalid escape sequence.
    pub fn get(&self, key: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| unescape(v))
    }

    /// Keys with their unescaped values, in file order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Option<String>)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), unescape(v)))
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct KeyFile {
    groups: Vec<Group>,
}

impl KeyFile {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let mut kf = KeyFile::default();
        let mut current: Option<usize> = None;

        for (lineno, raw) in text.split('\n').enumerate() {
            let line = raw.trim_start_matches(|c: char| c.is_ascii_whitespace());
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                let end = rest.find(']').ok_or_else(|| {
                    Error::Header(format!("line {}: unterminated group", lineno + 1))
                })?;
                let name = &rest[..end];
                if name.is_empty() || !rest[end + 1..].trim().is_empty() {
                    return Err(Error::Header(format!("line {}: invalid group", lineno + 1)));
                }
                current = Some(match kf.groups.iter().position(|g| g.name == name) {
                    Some(i) => i,
                    None => {
                        kf.groups.push(Group {
                            name: name.to_string(),
                            entries: Vec::new(),
                        });
                        kf.groups.len() - 1
                    }
                });
                continue;
            }
            let Some(eq) = line.find('=') else {
                return Err(Error::Header(format!(
                    "line {}: not a group, key or comment",
                    lineno + 1
                )));
            };
            let Some(gi) = current else {
                return Err(Error::Header(format!(
                    "line {}: key outside any group",
                    lineno + 1
                )));
            };
            let key = line[..eq].trim_end_matches(|c: char| c.is_ascii_whitespace());
            if key.is_empty() {
                return Err(Error::Header(format!("line {}: empty key", lineno + 1)));
            }
            let value = line[eq + 1..].trim_start_matches(|c: char| c.is_ascii_whitespace());
            let entries = &mut kf.groups[gi].entries;
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(e) => e.1 = value.to_string(),
                None => entries.push((key.to_string(), value.to_string())),
            }
        }
        Ok(kf)
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }
}

/// GKeyFile value escaping (`g_key_file_parse_string_as_value`, as
/// `g_key_file_set_string` uses it): backslash, newline and return always;
/// spaces and tabs only before the first other character.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut leading = true;
    for c in s.chars() {
        match c {
            ' ' if leading => out.push_str("\\s"),
            '\t' if leading => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            c => {
                leading = false;
                out.push(c);
            }
        }
    }
    out
}

/// GKeyFile value unescaping; `None` on an invalid escape or a trailing backslash.
pub fn unescape(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            's' => out.push(' '),
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '\\' => out.push('\\'),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsview_header() {
        let kf = KeyFile::parse("[version]\nversion = 3\n[header]\ndriver = virtual-demo\n probe0 = SDA\n trigger0 = R\n").unwrap();
        assert_eq!(
            kf.group("version").unwrap().get("version").as_deref(),
            Some("3")
        );
        let h = kf.group("header").unwrap();
        assert_eq!(
            h.keys().collect::<Vec<_>>(),
            ["driver", "probe0", "trigger0"]
        );
        assert_eq!(h.get("probe0").as_deref(), Some("SDA"));
    }

    #[test]
    fn repeated_group_and_key_merge() {
        let kf = KeyFile::parse("[a]\nx=1\ny=2\n[b]\n[a]\nx=3\n").unwrap();
        assert_eq!(kf.groups().len(), 2);
        let a = kf.group("a").unwrap();
        assert_eq!(a.keys().collect::<Vec<_>>(), ["x", "y"]);
        assert_eq!(a.get("x").as_deref(), Some("3"));
    }

    #[test]
    fn whitespace_rules() {
        let kf = KeyFile::parse("[g]\n  key  =   value  \n").unwrap();
        assert_eq!(
            kf.group("g").unwrap().get("key").as_deref(),
            Some("value  ")
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape(r"a\sb\nc\\d").as_deref(), Some("a b\nc\\d"));
        assert_eq!(unescape(r"bad\q"), None);
        assert_eq!(unescape("trailing\\"), None);
    }

    #[test]
    fn invalid_files() {
        assert!(KeyFile::parse("x=1\n").is_err());
        assert!(KeyFile::parse("[g]\njust text\n").is_err());
        assert!(KeyFile::parse("[g\n").is_err());
        assert!(KeyFile::parse("[g]junk\n").is_err());
        assert!(KeyFile::parse("# comment\n\n[g]\n").is_ok());
    }
}
