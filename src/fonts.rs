use std::sync::Arc;

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};

/// Install CJK-capable system fonts as fallbacks for egui's small built-in font set.
///
/// We intentionally keep egui's bundled Latin font first so the existing visual style stays
/// unchanged. The system font is only used when the built-in fonts do not contain a glyph.
pub fn configure(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut loaded = 0usize;

    for font in platform_fonts() {
        let key = format!("baby-cjk-{loaded}");
        fonts.font_data.insert(key.clone(), Arc::new(font));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .push(key.clone());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(key);
        loaded += 1;
    }

    if loaded > 0 {
        ctx.set_fonts(fonts);
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn platform_fonts() -> Vec<FontData> {
    use std::{collections::HashSet, fs};

    use font_kit::{
        family_name::FamilyName, handle::Handle, properties::Properties, source::SystemSource,
    };

    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &[
        // Hiragino Sans GB is bundled on macOS and covers Simplified Chinese plus Japanese kana.
        // Prefer it over hard-coded PingFang paths, which moved/changed format on newer macOS.
        "Hiragino Sans GB",
        "Hiragino Sans",
        "Hiragino Kaku Gothic ProN",
        "PingFang SC",
        "YuGothic",
        "Yu Gothic",
    ];

    #[cfg(target_os = "windows")]
    const CANDIDATES: &[&str] = &[
        "Microsoft YaHei UI",
        "Microsoft YaHei",
        "Yu Gothic UI",
        "Yu Gothic",
        "Meiryo UI",
        "Meiryo",
        "MS Gothic",
    ];

    let source = SystemSource::new();
    let mut result = Vec::new();
    let mut seen = HashSet::new();

    // Two successful fonts are enough: one broad CJK face and one secondary fallback.
    for family in CANDIDATES {
        let Ok(handle) = source.select_best_match(
            &[FamilyName::Title((*family).to_owned())],
            &Properties::new(),
        ) else {
            continue;
        };

        let (bytes, index, identity) = match handle {
            Handle::Path { path, font_index } => {
                let identity = format!("{}#{font_index}", path.display());
                let Ok(bytes) = fs::read(&path) else {
                    continue;
                };
                (bytes, font_index, identity)
            }
            Handle::Memory { bytes, font_index } => {
                let identity = format!("memory:{}:{font_index}", bytes.len());
                (bytes.as_ref().clone(), font_index, identity)
            }
        };

        if !seen.insert(identity) {
            continue;
        }

        let mut data = FontData::from_owned(bytes);
        data.index = index;
        result.push(data);

        if result.len() >= 2 {
            break;
        }
    }

    result
}

#[cfg(target_os = "linux")]
fn platform_fonts() -> Vec<FontData> {
    use std::fs;

    const CANDIDATES: &[&str] = &[
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJKsc-Regular.otf",
        "/usr/share/fonts/opentype/noto/NotoSansCJKjp-Regular.otf",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
    ];

    CANDIDATES
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .take(2)
        .map(FontData::from_owned)
        .collect()
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn platform_fonts() -> Vec<FontData> {
    Vec::new()
}
