//! Reading the Qt DSView's settings file (`QSettings` in INI format,
//! `~/.config/DreamSourceLab/DSView.conf`) for the first-run import, and
//! `AppConfig`'s value rules.
//!
//! Only what DSView writes is handled: `[Group]` headers (`[General]` is the
//! root), `a\b` sub-keys, `%XX` escapes in keys, quoted values with
//! backslash escapes, `@`-prefixed variants (kept as text), `;`/`#`
//! comments.

use std::collections::BTreeMap;

/// The settings as `group/sub/key` → text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct QSettings {
    values: BTreeMap<String, String>,
}

/// `%XX` escapes in a key (QSettings writes non-alphanumeric key bytes so);
/// `\` separates sub-keys.
fn unescape_key(k: &str) -> String {
    let b = k.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&k[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'\\' { b'/' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `QSettingsPrivate::iniUnescapedStringList` for one value: the quotes
/// go (they only protect spaces and commas), escapes are resolved,
/// whitespace at the ends is dropped. A list (unquoted commas) is kept as
/// written.
fn unescape_value(v: &str) -> String {
    let v = v.trim();
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {}
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('a') => out.push('\x07'),
                Some('b') => out.push('\x08'),
                Some('f') => out.push('\x0c'),
                Some('v') => out.push('\x0b'),
                Some('0') => out.push('\0'),
                Some('x') => {
                    let mut hex = String::new();
                    while let Some(&h) = chars.peek() {
                        if h.is_ascii_hexdigit() && hex.len() < 4 {
                            hex.push(h);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(o) => out.push(o),
                None => {}
            },
            _ => out.push(c),
        }
    }
    out
}

impl QSettings {
    pub fn parse(text: &str) -> QSettings {
        let mut values = BTreeMap::new();
        let mut group = String::new();
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() || l.starts_with(';') || l.starts_with('#') {
                continue;
            }
            if let Some(g) = l.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                let g = unescape_key(g);
                group = if g == "General" { String::new() } else { g };
                continue;
            }
            let Some((k, v)) = l.split_once('=') else {
                continue;
            };
            let k = unescape_key(k.trim());
            let key = if group.is_empty() {
                k
            } else {
                format!("{group}/{k}")
            };
            values.insert(key, unescape_value(v));
        }
        QSettings { values }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// `QVariant::toBool` of a string: "true", or a non-zero number.
    pub fn bool(&self, key: &str) -> Option<bool> {
        let v = self.get(key)?.trim();
        Some(match v.to_ascii_lowercase().as_str() {
            "true" => true,
            "false" | "" => false,
            s => s.parse::<f64>().is_ok_and(|n| n != 0.0),
        })
    }

    /// `QVariant::toInt`: None when the text is not a whole number (Qt
    /// gives 0 then; the caller keeps its default).
    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key)?.trim().parse().ok()
    }

    /// `QVariant::toFloat`/`toDouble`.
    pub fn float(&self, key: &str) -> Option<f64> {
        self.get(key)?.trim().parse().ok()
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.values.keys().map(String::as_str)
    }
}

/// `StringToFormatArray`: `"id=fmt;id=fmt"`; pieces without exactly one
/// `=` are skipped.
pub fn protocol_formats(s: &str) -> Vec<(String, String)> {
    s.split(';')
        .filter(|p| !p.is_empty())
        .filter_map(|p| {
            let v: Vec<&str> = p.split('=').collect();
            (v.len() == 2).then(|| (v[0].to_owned(), v[1].to_owned()))
        })
        .collect()
}

/// `AppConfig` language ids.
pub const LAN_CN: i64 = 25;
pub const LAN_EN: i64 = 31;
pub const LAN_DE: i64 = 7;

/// `_loadFrame`'s fallback: the language of the locale (`LANG`, `LC_*`),
/// Chinese and German recognised, else English.
pub fn language_from_locale(locale: &str) -> i64 {
    let l = locale.to_ascii_lowercase();
    if l.starts_with("zh") {
        LAN_CN
    } else if l.starts_with("de") {
        LAN_DE
    } else {
        LAN_EN
    }
}

/// A stored language id, or the locale's when it is unset or unknown.
pub fn language(stored: Option<i64>, locale: &str) -> i64 {
    match stored {
        Some(l) if [LAN_CN, LAN_EN, LAN_DE].contains(&l) => l,
        _ => language_from_locale(locale),
    }
}

/// `NO_POINT_VALUE`: a position that was never saved.
pub const NO_POINT_VALUE: i64 = -10000;

/// The normal (not maximised) window rectangle `MainFrame::ReadSettings`
/// restores: x, y, width, height. `x`/`y` fall back to left/top; a window
/// narrower than 300 px is not restored (Qt then centres a default size).
pub fn window_rect(
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
    x: i64,
    y: i64,
) -> Option<(i64, i64, i64, i64)> {
    let (x, y) = if x == NO_POINT_VALUE {
        (left, top)
    } else {
        (x, y)
    };
    let (w, h) = (right - left, bottom - top);
    (w >= 300 && h >= 70).then_some((x, y.max(0), w, h))
}

/// `ReadSettings`' screen check: a window (x, y, w, h) showing less than
/// 70 × 70 px of a `screen_w` × `screen_h` screen is moved to the screen's
/// middle (its size kept, at most the screen's). None when it is fine.
pub fn onto_screen(win: (f32, f32, f32, f32), screen_w: f32, screen_h: f32) -> Option<(f32, f32)> {
    let (x, y, w, h) = win;
    let iw = (x + w).min(screen_w) - x.max(0.0);
    let ih = (y + h).min(screen_h) - y.max(0.0);
    if iw >= 70.0 && ih >= 70.0 {
        return None;
    }
    Some((
        ((screen_w - w.min(screen_w)) / 2.0).max(0.0),
        ((screen_h - h.min(screen_h)) / 2.0).max(0.0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = r#"[Application]
ableSaveLog=false
fontSize=11
logLevel=3
protocalFormats="0:uart=hex;1:i2c=dec"
quickScroll=true
traceHeightFactor=1.5
rulerTimeUnits=Samples

[MainFrame]
LOGIC_DOCK\decodeDoc=true
LOGIC_DOCK\triggerDoc=false
bottom=525
isMax=true
language=31
left=-1701
ox=-10000
right=-666
style=frappe
top=0
windowState=@ByteArray(\0\0\0\xff\0\0\0\0\xfd)
x=-1701
y=0

[UserHistory]
protocolExportPath=/home/user/Documents
saveDir="/home/user/a, b"
showDocuments=false
openDir=/home/user/caf\x00e9
"#;

    #[test]
    fn parses_dsview_conf() {
        let s = QSettings::parse(CONF);
        assert_eq!(s.bool("Application/ableSaveLog"), Some(false));
        assert_eq!(s.float("Application/fontSize"), Some(11.0));
        assert_eq!(s.int("Application/logLevel"), Some(3));
        assert_eq!(s.bool("Application/quickScroll"), Some(true));
        assert_eq!(s.get("Application/rulerTimeUnits"), Some("Samples"));
        assert_eq!(
            protocol_formats(s.get("Application/protocalFormats").unwrap()),
            vec![
                ("0:uart".to_owned(), "hex".to_owned()),
                ("1:i2c".to_owned(), "dec".to_owned())
            ]
        );
        assert_eq!(s.bool("MainFrame/LOGIC_DOCK/decodeDoc"), Some(true));
        assert_eq!(s.bool("MainFrame/LOGIC_DOCK/triggerDoc"), Some(false));
        assert_eq!(s.int("MainFrame/left"), Some(-1701));
        assert_eq!(s.get("MainFrame/style"), Some("frappe"));
        assert_eq!(s.get("UserHistory/saveDir"), Some("/home/user/a, b"));
        assert_eq!(s.get("UserHistory/openDir"), Some("/home/user/café"));
        assert_eq!(s.bool("UserHistory/showDocuments"), Some(false));
        assert_eq!(s.bool("Application/missing"), None);
        // numbers that are not whole stay unset
        assert_eq!(s.int("Application/traceHeightFactor"), None);
    }

    #[test]
    fn root_group_comments_and_key_escapes() {
        let s = QSettings::parse("; c\n# c\na=1\n[General]\nb=2\n[G]\nk%20x=3\nbad line\n");
        assert_eq!(s.get("a"), Some("1"));
        assert_eq!(s.get("b"), Some("2"));
        assert_eq!(s.get("G/k x"), Some("3"));
        assert_eq!(s.keys().count(), 3);
    }

    #[test]
    fn language_and_window() {
        assert_eq!(language(Some(25), "en_US.UTF-8"), LAN_CN);
        assert_eq!(language(Some(-1), "de_AT.UTF-8"), LAN_DE);
        assert_eq!(language(None, "zh_CN"), LAN_CN);
        assert_eq!(language(Some(99), "C"), LAN_EN);
        assert_eq!(
            window_rect(100, 50, 1300, 850, NO_POINT_VALUE, NO_POINT_VALUE),
            Some((100, 50, 1200, 800))
        );
        assert_eq!(window_rect(0, 0, 200, 800, 0, 0), None);
        assert_eq!(
            window_rect(0, -20, 1000, 700, 0, -20),
            Some((0, 0, 1000, 720))
        );
    }

    #[test]
    fn window_back_onto_the_screen() {
        assert_eq!(
            onto_screen((100.0, 50.0, 1200.0, 800.0), 1920.0, 1080.0),
            None
        );
        // a monitor that is gone (left of this one)
        assert_eq!(
            onto_screen((-1701.0, 0.0, 1035.0, 525.0), 1920.0, 1080.0),
            Some((442.5, 277.5))
        );
        // 60 px visible at the right edge
        assert!(onto_screen((1860.0, 0.0, 800.0, 600.0), 1920.0, 1080.0).is_some());
    }
}
