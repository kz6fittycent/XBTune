// SPDX-License-Identifier: GPL-3.0-or-later
//! Look and feel: Inter, spacing, and a dark and a light palette.

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals,
};
use serde::{Deserialize, Serialize};

/// Inter 4.1 (SIL OFL 1.1), from the official release; see
/// `assets/fonts/Inter-OFL.txt` and docs/PROVENANCE.md.
const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");

/// Font family for headings and big numbers.
pub fn strong() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn strong_font(size: f32) -> FontId {
    FontId::new(size, strong())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    System,
    Dark,
    Light,
}

/// Colours that egui's `Visuals` don't cover.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub accent: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
    pub muted: Color32,
    pub card: Color32,
    pub track: Color32,
    pub series: [Color32; 6],
}

impl Palette {
    pub fn for_visuals(v: &Visuals) -> Self {
        if v.dark_mode {
            Self {
                accent: Color32::from_rgb(0xff, 0x7a, 0x1a),
                good: Color32::from_rgb(0x3d, 0xd6, 0x8c),
                warn: Color32::from_rgb(0xf5, 0xc2, 0x42),
                bad: Color32::from_rgb(0xff, 0x55, 0x55),
                muted: Color32::from_rgb(0x8a, 0x93, 0xa3),
                card: Color32::from_rgb(0x1e, 0x22, 0x29),
                track: Color32::from_rgb(0x2e, 0x34, 0x3e),
                series: [
                    Color32::from_rgb(0xff, 0x7a, 0x1a),
                    Color32::from_rgb(0x4d, 0xa3, 0xff),
                    Color32::from_rgb(0x3d, 0xd6, 0x8c),
                    Color32::from_rgb(0xd0, 0x7a, 0xff),
                    Color32::from_rgb(0xf5, 0xc2, 0x42),
                    Color32::from_rgb(0xff, 0x6b, 0x9a),
                ],
            }
        } else {
            Self {
                accent: Color32::from_rgb(0xe8, 0x5d, 0x00),
                good: Color32::from_rgb(0x12, 0x8a, 0x4f),
                warn: Color32::from_rgb(0xb7, 0x80, 0x00),
                bad: Color32::from_rgb(0xd0, 0x2b, 0x2b),
                muted: Color32::from_rgb(0x5f, 0x67, 0x75),
                card: Color32::from_rgb(0xff, 0xff, 0xff),
                track: Color32::from_rgb(0xe3, 0xe6, 0xeb),
                series: [
                    Color32::from_rgb(0xe8, 0x5d, 0x00),
                    Color32::from_rgb(0x1f, 0x6f, 0xd1),
                    Color32::from_rgb(0x12, 0x8a, 0x4f),
                    Color32::from_rgb(0x8a, 0x3f, 0xd1),
                    Color32::from_rgb(0xb7, 0x80, 0x00),
                    Color32::from_rgb(0xc8, 0x2f, 0x6b),
                ],
            }
        }
    }
}

/// Install fonts and text styles. Call once.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::empty();
    fonts.font_data.insert("inter".into(), Arc::new(FontData::from_static(INTER_REGULAR)));
    fonts.font_data.insert("inter-semibold".into(), Arc::new(FontData::from_static(INTER_SEMIBOLD)));
    fonts.families.insert(FontFamily::Proportional, vec!["inter".into()]);
    // No monospace font is bundled; Inter stands in.
    fonts.families.insert(FontFamily::Monospace, vec!["inter".into()]);
    fonts.families.insert(strong(), vec!["inter-semibold".into(), "inter".into()]);
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
            (TextStyle::Heading, strong_font(20.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.window_margin = egui::Margin::same(16);
    });
}

/// Apply the palette for a preference.
pub fn apply(ctx: &egui::Context, pref: ThemePref) {
    ctx.set_theme(match pref {
        ThemePref::System => egui::ThemePreference::System,
        ThemePref::Dark => egui::ThemePreference::Dark,
        ThemePref::Light => egui::ThemePreference::Light,
    });
    ctx.style_mut_of(egui::Theme::Dark, |s| tune(&mut s.visuals));
    ctx.style_mut_of(egui::Theme::Light, |s| tune(&mut s.visuals));
}

fn tune(v: &mut Visuals) {
    let p = Palette::for_visuals(v);
    let r = CornerRadius::same(8);
    if v.dark_mode {
        v.panel_fill = Color32::from_rgb(0x15, 0x18, 0x1d);
        v.window_fill = Color32::from_rgb(0x1b, 0x1f, 0x26);
        v.extreme_bg_color = Color32::from_rgb(0x10, 0x12, 0x16);
        v.faint_bg_color = Color32::from_rgb(0x1b, 0x1f, 0x26);
        v.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(0xd8, 0xdc, 0xe2);
        v.widgets.inactive.fg_stroke.color = Color32::from_rgb(0xe4, 0xe7, 0xec);
    } else {
        v.panel_fill = Color32::from_rgb(0xf3, 0xf4, 0xf7);
        v.window_fill = Color32::WHITE;
        v.extreme_bg_color = Color32::WHITE;
        v.faint_bg_color = Color32::from_rgb(0xf7, 0xf8, 0xfa);
        v.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(0x22, 0x26, 0x2d);
        v.widgets.inactive.fg_stroke.color = Color32::from_rgb(0x1a, 0x1d, 0x22);
    }
    v.selection.bg_fill = p.accent.gamma_multiply(0.85);
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.hyperlink_color = p.accent;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = r;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.corner_radius = r;
}

/// A rounded card to group content.
pub fn card(ui: &egui::Ui) -> egui::Frame {
    let p = Palette::for_visuals(ui.visuals());
    egui::Frame::new()
        .fill(p.card)
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::same(14))
        .stroke(Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The characters a TrueType font maps (its cmap, format 4 or 12).
    /// egui's own `has_glyph` can't tell when a family has a single font.
    fn mapped_chars(font: &[u8]) -> std::collections::HashSet<char> {
        let u16_at = |o: usize| u16::from_be_bytes([font[o], font[o + 1]]) as usize;
        let u32_at = |o: usize| u32::from_be_bytes([font[o], font[o + 1], font[o + 2], font[o + 3]]) as usize;
        let cmap = (0..u16_at(4))
            .map(|i| 12 + 16 * i)
            .find(|&r| &font[r..r + 4] == b"cmap")
            .map(|r| u32_at(r + 8))
            .expect("cmap table");
        let mut out = std::collections::HashSet::new();
        for i in 0..u16_at(cmap + 2) {
            let sub = cmap + u32_at(cmap + 4 + 8 * i + 4);
            match u16_at(sub) {
                12 => {
                    for g in 0..u32_at(sub + 12) {
                        let (a, b) = (u32_at(sub + 16 + 12 * g), u32_at(sub + 20 + 12 * g));
                        out.extend((a..=b).filter_map(|c| char::from_u32(c as u32)));
                    }
                }
                4 => {
                    let segs = u16_at(sub + 6) / 2;
                    for g in 0..segs {
                        let end = u16_at(sub + 14 + 2 * g);
                        let start = u16_at(sub + 16 + 2 * segs + 2 * g);
                        out.extend((start..=end.min(0xfffe)).filter_map(|c| char::from_u32(c as u32)));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Inter has no emoji and few symbols; a missing one shows as "?" (the
    /// owner hit ✕ and ✔). Every non-ASCII character in UI text, here and
    /// in xbtune-core and xbtune-remote (their messages are shown too), must be in both Inter
    /// files. Comments are skipped.
    #[test]
    fn every_symbol_in_the_ui_is_in_the_font() {
        let regular = mapped_chars(INTER_REGULAR);
        let semibold = mapped_chars(INTER_SEMIBOLD);
        assert!(regular.contains(&'A') && regular.contains(&'≠') && !regular.contains(&'\u{2714}'));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut missing = Vec::new();
        for dir in [root.join("src"), root.join("../xbtune-core/src"), root.join("../xbtune-remote/src")] {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|x| x != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for (n, line) in text.lines().enumerate() {
                    let code = line.trim_start();
                    if code.starts_with("//") {
                        continue;
                    }
                    let code = code.split(" // ").next().unwrap_or(code);
                    for c in code.chars().filter(|c| !c.is_ascii()) {
                        if !regular.contains(&c) || !semibold.contains(&c) {
                            let file = path.file_name().unwrap().to_string_lossy();
                            missing.push(format!("{file}:{}: {c} U+{:04X}", n + 1, c as u32));
                        }
                    }
                }
            }
        }
        assert!(missing.is_empty(), "not in Inter:\n{}", missing.join("\n"));
    }
}
