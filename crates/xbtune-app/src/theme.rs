// SPDX-License-Identifier: GPL-3.0-or-later
//! Look and feel: Inter, Lucide icons, spacing, a dark and a light
//! palette, and the few custom widgets (primary button, toggle switch,
//! sidebar item, dialog buttons in the system's order).

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Stroke, TextStyle,
    Visuals,
};
use serde::{Deserialize, Serialize};

/// Inter 4.1 (SIL OFL 1.1), from the official release; see
/// `assets/fonts/Inter-OFL.txt` and docs/PROVENANCE.md.
const INTER_REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
/// The Lucide icons in `crate::icons` (ISC), a subset made by
/// `tools/subset_lucide.py`; see `assets/fonts/Lucide-LICENSE.txt`.
pub(crate) const LUCIDE: &[u8] = include_bytes!("../assets/fonts/lucide.ttf");

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
    // Icons sit a little low against Inter's x-height; lift them.
    let lucide = FontData::from_static(LUCIDE).tweak(egui::FontTweak {
        scale: 1.05,
        y_offset_factor: -0.04,
        ..Default::default()
    });
    fonts.font_data.insert("lucide".into(), Arc::new(lucide));
    // Lucide first: Inter has glyphs of its own at some of the same
    // private-use codepoints. The subset maps nothing else (a test checks),
    // so it never replaces a letter.
    fonts.families.insert(FontFamily::Proportional, vec!["lucide".into(), "inter".into()]);
    // No monospace font is bundled; Inter stands in.
    fonts.families.insert(FontFamily::Monospace, vec!["lucide".into(), "inter".into()]);
    fonts.families.insert(strong(), vec!["lucide".into(), "inter-semibold".into(), "inter".into()]);
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
        // A checkbox box takes the widget corner radius: big enough that it
        // stays a rounded square, not a dot that reads as a radio button.
        style.spacing.icon_width = 18.0;
        style.spacing.icon_width_inner = 10.0;
        style.spacing.window_margin = egui::Margin::same(16);
        // Dialogs (egui's Modal) use the popup margin: give them room.
        style.spacing.menu_margin = egui::Margin::same(14);
    });
}

/// Apply the palette for a preference, title bar included (Windows draws
/// a light one over a dark app unless told).
pub fn apply(ctx: &egui::Context, pref: ThemePref) {
    ctx.set_theme(match pref {
        ThemePref::System => egui::ThemePreference::System,
        ThemePref::Dark => egui::ThemePreference::Dark,
        ThemePref::Light => egui::ThemePreference::Light,
    });
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(match pref {
        ThemePref::System => egui::SystemTheme::SystemDefault,
        ThemePref::Dark => egui::SystemTheme::Dark,
        ThemePref::Light => egui::SystemTheme::Light,
    }));
    ctx.style_mut_of(egui::Theme::Dark, |s| tune(&mut s.visuals));
    ctx.style_mut_of(egui::Theme::Light, |s| tune(&mut s.visuals));
}

fn tune(v: &mut Visuals) {
    let p = Palette::for_visuals(v);
    let r = CornerRadius::same(6);
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
    // Selected items and text: a tint of the accent, not a solid block.
    v.selection.bg_fill = p.accent.gamma_multiply(if v.dark_mode { 0.30 } else { 0.22 });
    v.selection.stroke = Stroke::new(1.0, v.strong_text_color());
    v.hyperlink_color = p.accent;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = r;
    for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.corner_radius = r;
    // A hairline border on buttons and boxes, as Windows 11 and GNOME draw.
    let border = if v.dark_mode { Color32::from_white_alpha(40) } else { Color32::from_black_alpha(55) };
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    // Checkboxes and radio buttons fill with `bg_fill` (buttons use
    // `weak_bg_fill`): an empty one should look empty, with its outline.
    v.widgets.inactive.bg_fill = v.extreme_bg_color;
}

/// A progress bar in the accent colour (egui's default is the selection
/// colour, a pale tint here).
pub fn progress(fraction: f32) -> egui::ProgressBar {
    egui::ProgressBar::new(fraction).fill(ACCENT_FILL)
}

/// The one main action of a view or dialog: filled with the accent colour.
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(Color32::WHITE).strong()).fill(ACCENT_FILL)
}

/// The fill behind [`primary`] buttons (the light theme's darker orange:
/// white text on it passes WCAG AA in both themes).
const ACCENT_FILL: Color32 = Color32::from_rgb(0xd9, 0x56, 0x00);

/// A square icon-only button; `tip` says what it does.
pub fn icon_button(ui: &mut egui::Ui, enabled: bool, icon: &str, tip: &str) -> egui::Response {
    let b = egui::Button::new(RichText::new(icon).size(16.0)).min_size(egui::vec2(32.0, 30.0));
    ui.add_enabled(enabled, b).on_hover_text(tip).on_disabled_hover_text(tip)
}

/// An on/off switch: for settings that take effect at once (Windows 11
/// and GNOME use switches there; checkboxes are for choices in a form).
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, label: &str) -> egui::Response {
    let p = Palette::for_visuals(ui.visuals());
    let size = egui::vec2(36.0, 20.0);
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::TextStyle::Body.resolve(ui.style()),
        ui.visuals().text_color(),
    );
    let total = egui::vec2(size.x + 10.0 + galley.size().x, size.y.max(galley.size().y) + 6.0);
    let (rect, mut resp) = ui.allocate_exact_size(total, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *on, label));
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(resp.id, *on);
        let track = egui::Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - size.y / 2.0), size);
        let off_fill = if ui.visuals().dark_mode { p.track } else { Color32::from_gray(0xc4) };
        let fill = lerp_color(off_fill, ACCENT_FILL, t);
        let painter = ui.painter();
        painter.rect_filled(track, CornerRadius::same(10), fill);
        if resp.has_focus() {
            painter.rect_stroke(
                track.expand(2.0),
                CornerRadius::same(12),
                Stroke::new(1.5, p.accent),
                egui::StrokeKind::Outside,
            );
        }
        let r = size.y / 2.0 - 3.0 + if resp.hovered() { 0.5 } else { 0.0 };
        let x = egui::lerp(track.left() + size.y / 2.0..=track.right() - size.y / 2.0, t);
        painter.circle_filled(egui::pos2(x, track.center().y), r, Color32::WHITE);
        let text_pos = egui::pos2(track.right() + 10.0, rect.center().y - galley.size().y / 2.0);
        painter.galley(text_pos, galley, ui.visuals().text_color());
    }
    resp
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// A sidebar entry: icon and label, left-aligned; the selected one gets a
/// tinted background and an accent bar on its left edge.
pub fn nav_item(ui: &mut egui::Ui, selected: bool, icon: &str, label: &str) -> egui::Response {
    let p = Palette::for_visuals(ui.visuals());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, label));
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let text_color = if selected { ui.visuals().strong_text_color() } else { ui.visuals().text_color() };
        if selected {
            painter.rect_filled(rect, CornerRadius::same(6), p.accent.gamma_multiply(0.16));
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.left(), rect.center().y - 8.0),
                egui::vec2(3.0, 16.0),
            );
            painter.rect_filled(bar, CornerRadius::same(2), p.accent);
        } else if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(6), ui.visuals().widgets.hovered.weak_bg_fill);
        }
        if resp.has_focus() {
            painter.rect_stroke(
                rect,
                CornerRadius::same(6),
                Stroke::new(1.5, p.accent),
                egui::StrokeKind::Inside,
            );
        }
        let icon_color = if selected { p.accent } else { p.muted };
        painter.text(
            egui::pos2(rect.left() + 14.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            icon,
            FontId::proportional(16.0),
            icon_color,
        );
        let font = if selected { strong_font(14.5) } else { FontId::proportional(14.5) };
        painter.text(
            egui::pos2(rect.left() + 42.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            text_color,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A small heading over a group of sidebar entries.
pub fn nav_group(ui: &mut egui::Ui, title: &str) {
    let p = Palette::for_visuals(ui.visuals());
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.label(RichText::new(title.to_uppercase()).size(11.0).color(p.muted).strong());
    });
}

/// Dialog buttons, right-aligned, in the system's order: the main one
/// first on Windows ("Burn  Cancel"), last elsewhere ("Cancel  Burn", as
/// GNOME and macOS do). Returns (main clicked, cancel clicked).
pub fn dialog_buttons(
    ui: &mut egui::Ui,
    main: egui::Button<'_>,
    main_enabled: bool,
    cancel: &str,
) -> (bool, bool) {
    let mut out = (false, false);
    let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        // Right to left: the first added is the rightmost.
        if cfg!(windows) {
            out.1 = ui.button(cancel).clicked();
            out.0 = ui.add_enabled(main_enabled, main).clicked();
        } else {
            out.0 = ui.add_enabled(main_enabled, main).clicked();
            out.1 = ui.button(cancel).clicked();
        }
    });
    out
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
pub(crate) mod tests {
    use super::*;

    /// The characters a TrueType font maps (its cmap, format 4 or 12).
    /// egui's own `has_glyph` can't tell when a family has a single font.
    pub(crate) fn mapped_chars(font: &[u8]) -> std::collections::HashSet<char> {
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

    /// The icon font comes first in every family, so it must map icons
    /// (private use area) only.
    #[test]
    fn icon_font_maps_only_private_use() {
        let other: Vec<_> =
            mapped_chars(LUCIDE).into_iter().filter(|c| !('\u{e000}'..='\u{f8ff}').contains(c)).collect();
        assert!(other.is_empty(), "lucide.ttf maps {other:?}");
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
