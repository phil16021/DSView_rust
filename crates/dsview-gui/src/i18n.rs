//! Languages (`pv/ui/langresource.cpp`): the texts of `lang/<cn|en|de>/*.json`
//! by page and id, `L_S(page, id, default)`.
//!
//! The Rust GUI writes its texts in English, as Qt's defaults are. A text is
//! found by Qt's English text for the same id (`lang/en`), compared without
//! the `&` accelerators, a trailing ellipsis or colon, and case; the
//! translation keeps the ellipsis or colon of the Rust text. Device option
//! texts (`STR_PAGE_DSL`, the text is the id) and decoder texts
//! (`STR_PAGE_DECODER`, by `idn`) exist for Chinese only, as in Qt
//! (`is_lang_en()` is true for German too).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use dsview_viewmodel::qsettings::{LAN_CN, LAN_DE, LAN_EN};

/// `lang_key_item`: the directory of a language id.
pub fn dir_name(lang: i64) -> &'static str {
    match lang {
        LAN_CN => "cn",
        LAN_DE => "de",
        _ => "en",
    }
}

/// The language files: `<data>/lang`.
pub fn lang_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("lang")
}

/// The pages with ids (toolbar, msg, dlg).
const PAGES: [&str; 3] = ["toolbar", "msg", "dlg"];

#[derive(Default)]
struct Lang {
    lang: i64,
    /// normalised English text → the ids that carry it (page, id).
    by_english: HashMap<String, Vec<(usize, String)>>,
    /// (page, id) → text in the language.
    texts: HashMap<(usize, String), String>,
    /// Device option texts (`dsl_*.json`), Chinese only.
    dsl: HashMap<String, String>,
    /// Decoder texts by `idn` (`dec/*.json`), Chinese only.
    dec: HashMap<String, String>,
}

fn store() -> &'static RwLock<Arc<Lang>> {
    static S: std::sync::OnceLock<RwLock<Arc<Lang>>> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        RwLock::new(Arc::new(Lang {
            lang: LAN_EN,
            ..Lang::default()
        }))
    })
}

fn current() -> Arc<Lang> {
    store().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The language shown.
pub fn language() -> i64 {
    current().lang
}

/// One JSON page: `[{"id": .., "text": ..}]`.
fn read_page(path: &Path) -> Vec<(String, String)> {
    let Ok(t) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&t) else {
        dsview_drivers::sys::log(2, &format!("Language file {}: not a list", path.display()));
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|e| {
            Some((
                e.get("id")?.as_str()?.to_owned(),
                e.get("text")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Qt's accelerators out: `&` before a letter (`&&` is a literal `&`), and
/// a trailing `(&C)`.
fn strip_accel(t: &str) -> String {
    let t = t.trim_end();
    let t = match t.rfind("(&") {
        Some(i) if t.ends_with(')') && t.len() - i <= 5 => t[..i].trim_end(),
        _ => t,
    };
    let mut out = String::with_capacity(t.len());
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                out.push('&');
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// A text split into its core and the ending kept from the Rust text: an
/// ellipsis (`…` or `...`), a colon, trailing spaces.
fn split_ending(t: &str) -> (&str, &str) {
    let core = t.trim_end();
    let core = core
        .strip_suffix('…')
        .or_else(|| core.strip_suffix("..."))
        .or_else(|| core.strip_suffix(':'))
        .or_else(|| core.strip_suffix('：'))
        .unwrap_or(core)
        .trim_end();
    (core, &t[core.len()..])
}

fn normalise(t: &str) -> String {
    split_ending(&strip_accel(t)).0.to_lowercase()
}

/// `LangResource::Load`: reads the language from `lang_dir`
/// (`<data>/lang`) and shows it.
pub fn load(lang_dir: &Path, lang: i64) {
    *store().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(Lang::read(lang_dir, lang));
}

/// `L_S` for an English GUI text: the language's text for the id with that
/// English text, else the text itself.
pub fn tr(text: &str) -> String {
    current().tr(text)
}

/// `L_S(STR_PAGE_DSL, text, text)`: a device option's text (Chinese only).
pub fn tr_dsl(text: &str) -> String {
    current().tr_dsl(text)
}

/// A decoder text by its `idn` (Chinese only; `transDecoderDlg`).
pub fn tr_dec(idn: &str, default: &str) -> String {
    current().tr_dec(idn, default)
}

impl Lang {
    fn read(lang_dir: &Path, lang: i64) -> Lang {
        let lang = if [LAN_CN, LAN_EN, LAN_DE].contains(&lang) {
            lang
        } else {
            LAN_EN
        };
        let mut l = Lang {
            lang,
            ..Lang::default()
        };
        for (p, page) in PAGES.iter().enumerate() {
            for (id, text) in read_page(&lang_dir.join("en").join(format!("{page}.json"))) {
                l.by_english
                    .entry(normalise(&text))
                    .or_default()
                    .push((p, id));
            }
            if lang != LAN_EN {
                let file = lang_dir.join(dir_name(lang)).join(format!("{page}.json"));
                for (id, text) in read_page(&file) {
                    l.texts.insert((p, id), strip_accel(&text));
                }
            }
        }
        if lang == LAN_CN {
            for f in ["dsl_list", "dsl_label", "dsl_channel"] {
                l.dsl
                    .extend(read_page(&lang_dir.join("cn").join(format!("{f}.json"))));
            }
            let dec_dir: PathBuf = lang_dir.join("cn").join("dec");
            if let Ok(rd) = std::fs::read_dir(&dec_dir) {
                let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
                files.sort();
                for f in files {
                    l.dec.extend(read_page(&f));
                }
            }
        }
        l
    }

    fn tr(&self, text: &str) -> String {
        let l = self;
        if l.lang == LAN_EN || l.texts.is_empty() {
            return text.to_owned();
        }
        let (core, ending) = split_ending(text);
        let found = l
            .by_english
            .get(&normalise(core))
            .and_then(|ids| ids.iter().find_map(|k| l.texts.get(k)));
        match found {
            Some(t) => {
                let (t, _) = split_ending(t);
                format!("{t}{ending}")
            }
            None => text.to_owned(),
        }
    }

    fn tr_dsl(&self, text: &str) -> String {
        self.dsl
            .get(text)
            .cloned()
            .unwrap_or_else(|| text.to_owned())
    }

    fn tr_dec(&self, idn: &str, default: &str) -> String {
        if idn.is_empty() {
            return default.to_owned();
        }
        self.dec
            .get(idn)
            .cloned()
            .unwrap_or_else(|| default.to_owned())
    }
}

/// System fonts with CJK glyphs, and the face to use (the collections'
/// Simplified Chinese one).
const CJK_FONTS: [(&str, u32); 4] = [
    (
        "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
        0,
    ),
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 2),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 2),
    (
        "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        0,
    ),
];

/// The first CJK font this system has.
pub fn cjk_font() -> Option<(PathBuf, u32)> {
    CJK_FONTS
        .iter()
        .find(|(p, _)| Path::new(p).is_file())
        .map(|(p, i)| (PathBuf::from(p), *i))
}

/// Noto Sans CJK SC cut down to the Chinese translation's characters, the
/// 3755 common hanzi (GB 2312 level 1) and CJK punctuation; used where the
/// system has none of [`CJK_FONTS`] (`assets/fonts/OFL.txt`).
pub const BUNDLED_CJK_FONT: &[u8] =
    include_bytes!("../../../assets/fonts/NotoSansCJKsc-DSViewR-subset.otf");

/// A font with CJK glyphs, added as the last fallback of every family
/// (egui's own fonts have none): a system one, else the bundled subset.
/// Returns where it came from.
pub fn add_cjk_font(ctx: &eframe::egui::Context) -> String {
    use eframe::egui::{FontData, FontFamily};
    use eframe::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    let system = cjk_font().and_then(|(path, index)| {
        let bytes = std::fs::read(&path).ok()?;
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        Some((data, path.display().to_string()))
    });
    let (data, from) = system.unwrap_or_else(|| {
        (
            FontData::from_static(BUNDLED_CJK_FONT),
            "bundled Noto Sans CJK SC subset".into(),
        )
    });
    ctx.add_font(FontInsert::new(
        "cjk-fallback",
        data,
        vec![
            InsertFontFamily {
                family: FontFamily::Proportional,
                priority: FontPriority::Lowest,
            },
            InsertFontFamily {
                family: FontFamily::Monospace,
                priority: FontPriority::Lowest,
            },
        ],
    ));
    from
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_lang() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/lang")
    }

    /// Every CJK character of the Chinese translation (and the language
    /// menu's 中文) has a glyph in the bundled font.
    #[test]
    fn bundled_cjk_font_covers_the_translation() {
        use eframe::egui::{self, FontData, FontDefinitions, FontFamily, FontId};
        let ctx = egui::Context::default();
        let mut defs = FontDefinitions::empty();
        defs.font_data
            .insert("cjk".into(), FontData::from_static(BUNDLED_CJK_FONT).into());
        defs.families
            .insert(FontFamily::Proportional, vec!["cjk".into()]);
        ctx.set_fonts(defs);
        let mut out = ctx.run_ui(Default::default(), |_| {});
        out.textures_delta.clear();
        let mut text = String::from("中文");
        for e in std::fs::read_dir(repo_lang().join("cn")).unwrap() {
            text += &std::fs::read_to_string(e.unwrap().path()).unwrap_or_default();
        }
        let font = FontId::proportional(14.0);
        let mut missing: Vec<char> = ctx.fonts_mut(|f| {
            text.chars()
                .filter(|&c| c >= '\u{2e80}')
                .filter(|&c| !f.has_glyphs(&font, &c.to_string()))
                .collect()
        });
        missing.dedup();
        assert!(missing.is_empty(), "no glyph for {missing:?}");
    }

    #[test]
    fn accelerators_and_endings() {
        assert_eq!(strip_accel("&Open..."), "Open...");
        assert_eq!(strip_accel("中文(&C)"), "中文");
        assert_eq!(strip_accel("Fra&ppé"), "Frappé");
        assert_eq!(strip_accel("A && B"), "A & B");
        assert_eq!(split_ending("Save…"), ("Save", "…"));
        assert_eq!(split_ending("Map Zoom: "), ("Map Zoom", ": "));
        assert_eq!(normalise("&Load Session"), "load session");
    }

    /// Local `Lang`s: the shown language is global and other tests draw.
    #[test]
    fn languages() {
        let cn = Lang::read(&repo_lang(), LAN_CN);
        // toolbar "&File", with the Rust ellipsis kept
        assert_ne!(cn.tr("File"), "File");
        assert!(cn.tr("Open…").ends_with('…'));
        assert!(!cn.tr("Open…").contains('&'));
        assert_eq!(
            cn.tr("Something only Rust says"),
            "Something only Rust says"
        );
        assert_eq!(cn.tr_dsl("Sample rate"), "采样率");
        assert_eq!(cn.tr_dec("dec_0i2c_chan_scl", "SCL"), "串行时钟线");
        assert_eq!(cn.tr_dec("", "SCL"), "SCL");
        let de = Lang::read(&repo_lang(), LAN_DE);
        assert_eq!(de.tr("Log Options"), "Protokolloptionen");
        // German has no device or decoder texts
        assert_eq!(de.tr_dsl("Sample rate"), "Sample rate");
        let en = Lang::read(&repo_lang(), LAN_EN);
        assert_eq!(en.tr("Log Options"), "Log Options");
        // unknown ids fall back to English
        assert_eq!(Lang::read(&repo_lang(), 99).lang, LAN_EN);
    }
}
