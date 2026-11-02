//! Translations: OrcaSlicer's catalogues (packed by `tools/pack_translations.py` down to the
//! English texts CosmicSlicer shows), looked up by the English text as Orca does. Text with no
//! translation stays English. Languages whose script egui's built-in fonts lack (Chinese,
//! Japanese, Korean, Thai) take a font from the system. The language is per thread: the UI's.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const PACK: &[u8] = include_bytes!("../../../assets/i18n/translations.bin");

#[derive(Default, serde::Deserialize)]
struct Pack {
    languages: Vec<(String, String)>,
    strings: HashMap<String, HashMap<String, String>>,
}

fn pack() -> &'static Pack {
    static P: OnceLock<Pack> = OnceLock::new();
    P.get_or_init(|| {
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(PACK, 64 << 20)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    })
}

thread_local! {
    /// The language the UI (one thread) shows; `None` for English.
    static CURRENT: std::cell::RefCell<Option<Arc<HashMap<String, String>>>> = const { std::cell::RefCell::new(None) };
}

/// (code, name in its own language) for every language there is, English first.
pub fn languages() -> Vec<(String, String)> {
    let mut l = pack().languages.clone();
    if l.is_empty() {
        l.push(("en".into(), "English".into()));
    }
    l
}

/// Shows text in language `code` ("en" or "" for English) from now on.
pub fn set_language(code: &str) {
    let map = pack().strings.get(code).map(|m| Arc::new(m.clone()));
    CURRENT.with(|c| *c.borrow_mut() = map);
}

/// `english` in the current language, or as it is.
pub fn tr(english: &str) -> String {
    let lookup = CURRENT.with(|c| c.borrow().as_ref().and_then(|m| m.get(english).cloned()));
    lookup.unwrap_or_else(|| english.to_owned())
}

/// The language the system asks for, if there is a catalogue for it.
pub fn system_language() -> Option<String> {
    let env = ["LC_ALL", "LC_MESSAGES", "LANG"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))?;
    let code = env.split(['.', '@']).next().unwrap_or("");
    let langs = languages();
    langs
        .iter()
        .find(|(c, _)| c == code)
        .or_else(|| langs.iter().find(|(c, _)| code.split('_').next() == Some(c.as_str())))
        .map(|(c, _)| c.clone())
}

/// Font files with the script `code` needs, where the systems keep them.
fn font_candidates(code: &str) -> &'static [&'static str] {
    match code {
        "zh_CN" | "zh_TW" | "ja" | "ko" => &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/arphic/uming.ttc",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\meiryo.ttc",
            "C:\\Windows\\Fonts\\malgun.ttf",
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        ],
        "th" => &[
            "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoLoopedThai-Regular.ttf",
            "/usr/share/fonts/truetype/tlwg/Loma.ttf",
            "C:\\Windows\\Fonts\\LeelawUI.ttf",
            "C:\\Windows\\Fonts\\tahoma.ttf",
            "/System/Library/Fonts/Supplemental/Thonburi.ttc",
            "/System/Library/Fonts/Thonburi.ttc",
        ],
        _ => &[],
    }
}

/// Sets egui's fonts for language `code`: the built-in ones, then a system font for scripts
/// they lack. Returns false when that language needs a font the system does not have.
pub fn install_fonts(ctx: &egui::Context, code: &str) -> bool {
    let mut fonts = egui::FontDefinitions::default();
    let mut found = font_candidates(code).is_empty();
    for path in font_candidates(code) {
        let ok = std::fs::metadata(path).is_ok_and(|m| m.len() < 64 << 20);
        if !ok {
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else { continue };
        let name = format!("system-{code}");
        fonts.font_data.insert(name.clone(), Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.clone());
        }
        found = true;
        break;
    }
    ctx.set_fonts(fonts);
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogues_load_and_translate() {
        let langs = languages();
        assert!(langs.len() > 20, "{}", langs.len());
        assert!(langs.iter().any(|(c, _)| c == "de"));
        set_language("de");
        assert_eq!(tr("Layer height"), "Schichthöhe");
        assert_eq!(tr("no such text"), "no such text");
        set_language("en");
        assert_eq!(tr("Layer height"), "Layer height");
        set_language("xx");
        assert_eq!(tr("Layer height"), "Layer height");
        // Orca has no right-to-left language; nothing here needs mirroring.
        assert!(!langs.iter().any(|(c, _)| ["ar", "he", "fa", "ur"].contains(&c.as_str())));
    }
}
