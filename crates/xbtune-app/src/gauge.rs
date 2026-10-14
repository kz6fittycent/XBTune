// SPDX-License-Identifier: GPL-3.0-or-later
//! A round gauge drawn with the egui painter.

use std::f32::consts::PI;

use egui::{Align2, Color32, Pos2, Response, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};
use xbtune_defs::RtVar;

use crate::theme::{Palette, strong_font};

/// Display range of a gauge, and where its warning zone starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub min: f64,
    pub max: f64,
    pub warn: Option<f64>,
}

/// Sensible gauge ranges for the common XB channels. The definitions' own
/// limits are raw-type limits (e.g. 0-2000 V), which make useless gauges.
pub fn range_for(var: &RtVar) -> Range {
    let r = |min: f64, max: f64, warn: Option<f64>| Range { min, max, warn };
    match var.name.as_str() {
        "RPM" => r(0.0, 8000.0, Some(7000.0)),
        "TPD" => r(0.0, 90.0, None),
        "TPP" | "Fan Duty" => r(0.0, 100.0, None),
        "spark1" | "spark2" => r(0.0, 50.0, None),
        "pw1" | "pw2" => r(0.0, 20.0, None),
        "CLT" => r(0.0, 300.0, Some(230.0)),
        "CLT_F" => r(0.0, 570.0, Some(446.0)),
        "MAT" => r(-20.0, 80.0, None),
        "MAT_F" => r(0.0, 180.0, None),
        "Batt. Voltg." => r(8.0, 16.0, Some(15.0)),
        "O2" => r(0.0, 1.2, None),
        "EGO Corr." | "AFV" | "WUE" => r(70.0, 130.0, None),
        "veCurr1" | "veCurr2" => r(0.0, 255.0, None),
        _ => match (var.low, var.high) {
            (Some(lo), Some(hi)) if hi > lo && hi.is_finite() && lo.is_finite() => r(lo, hi, None),
            _ => r(0.0, 100.0, None),
        },
    }
}

/// Angle (radians, screen coordinates) for a fraction 0..=1 of the dial.
fn angle(frac: f32) -> f32 {
    // 240° sweep from lower-left (210°) clockwise to lower-right (-30°).
    let start = 150.0_f32.to_radians();
    start + frac.clamp(0.0, 1.0) * (240.0_f32.to_radians())
}

fn on_circle(c: Pos2, r: f32, a: f32) -> Pos2 {
    pos2(c.x + r * a.cos(), c.y + r * a.sin())
}

fn arc(c: Pos2, r: f32, from: f32, to: f32, stroke: Stroke) -> Shape {
    let n = (((to - from).abs() / PI * 48.0).ceil() as usize).max(2);
    let pts = (0..=n).map(|i| on_circle(c, r, from + (to - from) * i as f32 / n as f32)).collect();
    Shape::line(pts, stroke)
}

/// Draw a gauge. `value` is `None` when there is no data yet.
pub fn gauge(
    ui: &mut Ui,
    size: f32,
    label: &str,
    units: &str,
    text: &str,
    value: Option<f64>,
    range: Range,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let p = Palette::for_visuals(ui.visuals());
    let painter = ui.painter_at(rect.expand(2.0));
    let c = rect.center() + vec2(0.0, size * 0.06);
    let radius = size * 0.42;
    let width = (size * 0.06).max(5.0);
    let frac = |v: f64| ((v - range.min) / (range.max - range.min)) as f32;

    painter.add(arc(c, radius, angle(0.0), angle(1.0), Stroke::new(width, p.track)));
    if let Some(w) = range.warn {
        painter.add(arc(
            c,
            radius + width * 0.9,
            angle(frac(w)),
            angle(1.0),
            Stroke::new(width * 0.35, p.bad),
        ));
    }
    if let Some(v) = value {
        let f = frac(v);
        let color = match range.warn {
            Some(w) if v >= w => p.bad,
            _ => p.accent,
        };
        if f > 0.0 {
            painter.add(arc(c, radius, angle(0.0), angle(f), Stroke::new(width, color)));
        }
        let tip = on_circle(c, radius, angle(f));
        painter.circle_filled(tip, width * 0.75, color);
    }
    // Ticks and labels at the ends and middle.
    let text_color = ui.visuals().strong_text_color();
    for (i, frac_at) in [0.0f32, 0.25, 0.5, 0.75, 1.0].into_iter().enumerate() {
        let a = angle(frac_at);
        painter.line_segment(
            [on_circle(c, radius - width * 0.9, a), on_circle(c, radius - width * 1.6, a)],
            Stroke::new(1.0, p.muted),
        );
        if i % 2 == 0 {
            let v = range.min + (range.max - range.min) * frac_at as f64;
            painter.text(
                on_circle(c, radius - width * 2.8, a),
                Align2::CENTER_CENTER,
                tick_label(v),
                egui::FontId::proportional((size * 0.065).max(9.0)),
                p.muted,
            );
        }
    }
    let big = (size * 0.17).max(14.0);
    painter.text(c - vec2(0.0, size * 0.03), Align2::CENTER_CENTER, text, strong_font(big), text_color);
    painter.text(
        c + vec2(0.0, big * 0.75),
        Align2::CENTER_CENTER,
        units,
        egui::FontId::proportional((size * 0.075).max(10.0)),
        p.muted,
    );
    painter.text(
        pos2(rect.center().x, rect.bottom() - size * 0.05),
        Align2::CENTER_BOTTOM,
        label,
        egui::FontId::proportional((size * 0.08).max(11.0)),
        text_color,
    );
    if response.hovered() {
        painter.rect_stroke(
            rect,
            12.0,
            Stroke::new(1.0, p.muted.gamma_multiply(0.5)),
            egui::StrokeKind::Inside,
        );
    }
    response
}

fn tick_label(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{}k", (v / 1000.0 * 10.0).round() / 10.0)
    } else if v.fract().abs() < 1e-9 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}

/// A small coloured status chip. A real widget, so wrapped layouts can
/// place it.
pub fn chip(ui: &mut Ui, text: &str, on: bool) {
    let p = Palette::for_visuals(ui.visuals());
    let (fill, fg) = if on {
        (p.accent.gamma_multiply(0.25), ui.visuals().strong_text_color())
    } else {
        (Color32::TRANSPARENT, p.muted)
    };
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(fg).size(12.5))
            .fill(fill)
            .stroke(Stroke::new(1.0, if on { p.accent } else { p.track }))
            .corner_radius(egui::CornerRadius::same(255))
            .sense(Sense::hover()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dial_angles_and_labels() {
        assert!((angle(0.0) - 150f32.to_radians()).abs() < 1e-6);
        assert!((angle(1.0) - 390f32.to_radians()).abs() < 1e-6);
        assert_eq!(angle(2.0), angle(1.0));
        assert_eq!(tick_label(8000.0), "8k");
        assert_eq!(tick_label(7500.0), "7.5k");
        assert_eq!(tick_label(1.2), "1.2");
        assert_eq!(tick_label(-20.0), "-20");
    }
}
