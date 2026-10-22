// SPDX-License-Identifier: GPL-3.0-or-later
//! The splash screen: a tachometer does an ignition-on needle sweep while
//! the name fades in, then the whole thing fades into the app. About 2.4 s;
//! a click or any key skips it. Off with Settings -> Startup.
//!
//! No Buell artwork (the Pegasus logo is a Buell trademark); everything is
//! drawn here.

use std::f32::consts::PI;
use std::time::Instant;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, epaint::PathStroke};

use crate::theme::{self, Palette};

/// When the needle starts up, tops out, and is back at zero (seconds).
const SWEEP_START: f32 = 0.15;
const SWEEP_TOP: f32 = 0.95;
const SWEEP_END: f32 = 1.65;
/// When the overlay starts fading out, and is gone.
const FADE_START: f32 = 2.0;
const DONE: f32 = 2.4;

/// Tach scale, x1000 r/min, and where the red zone starts (an XB's stock
/// hard rev limit is 7500).
const MAX_KRPM: f32 = 9.0;
const RED_KRPM: f32 = 7.5;
/// The dial runs clockwise from lower left to lower right.
const ARC_START: f32 = 0.75 * PI;
const ARC_SWEEP: f32 = 1.5 * PI;

pub struct Splash {
    started: Instant,
    /// `XBTUNE_SHOT_SPLASH=<seconds>`: hold the animation at that time.
    frozen: Option<f32>,
}

impl Splash {
    /// The splash for this start, if it's wanted. Screenshot runs skip it
    /// unless `XBTUNE_SHOT_SPLASH` asks for it.
    pub fn new(enabled: bool, screenshot: bool) -> Option<Self> {
        let frozen = std::env::var("XBTUNE_SHOT_SPLASH").ok().and_then(|t| t.parse().ok());
        match (screenshot, frozen) {
            (true, None) => None,
            (true, Some(_)) => Some(Self { started: Instant::now(), frozen }),
            (false, _) if enabled => Some(Self { started: Instant::now(), frozen: None }),
            _ => None,
        }
    }

    fn t(&self) -> f32 {
        self.frozen.unwrap_or_else(|| self.started.elapsed().as_secs_f32())
    }

    /// Draws the splash over everything. Returns false once it's finished
    /// or skipped.
    pub fn show(&self, ui: &mut Ui) -> bool {
        let t = self.t();
        if t >= DONE {
            return false;
        }
        let ctx = ui.ctx().clone();
        let skipped = self.frozen.is_none()
            && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. })));
        let screen = ctx.content_rect();
        let mut clicked = false;
        egui::Area::new(egui::Id::new("splash")).order(egui::Order::Tooltip).fixed_pos(screen.min).show(
            &ctx,
            |ui| {
                let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
                clicked = resp.clicked();
                draw(ui, rect, t);
            },
        );
        if self.frozen.is_none() {
            ctx.request_repaint();
        }
        !(skipped || clicked)
    }
}

fn ease_in_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    if x < 0.5 { 4.0 * x * x * x } else { 1.0 - (-2.0 * x + 2.0).powi(3) / 2.0 }
}

/// 0..1 between `a` and `b`.
fn between(t: f32, a: f32, b: f32) -> f32 {
    ((t - a) / (b - a)).clamp(0.0, 1.0)
}

/// The needle position, x1000 r/min.
fn needle_krpm(t: f32) -> f32 {
    if t < SWEEP_TOP {
        MAX_KRPM * ease_in_out(between(t, SWEEP_START, SWEEP_TOP))
    } else {
        MAX_KRPM * (1.0 - ease_in_out(between(t, SWEEP_TOP, SWEEP_END)))
    }
}

fn angle(krpm: f32) -> f32 {
    ARC_START + ARC_SWEEP * (krpm / MAX_KRPM)
}

fn on_circle(c: Pos2, r: f32, a: f32) -> Pos2 {
    c + Vec2::new(a.cos(), a.sin()) * r
}

fn arc(c: Pos2, r: f32, from: f32, to: f32, stroke: impl Into<PathStroke>) -> Shape {
    let n = (((to - from).abs() * r / 4.0) as usize).max(2);
    let pts = (0..=n).map(|i| on_circle(c, r, from + (to - from) * i as f32 / n as f32)).collect();
    Shape::line(pts, stroke)
}

/// A ring from `inner` (colour `col`) fading smoothly to nothing at `outer`.
fn glow(c: Pos2, inner: f32, outer: f32, col: Color32) -> Shape {
    const SEGMENTS: u32 = 96;
    // Radii and how much of `col` each ring keeps: an ease-out falloff.
    const RINGS: [(f32, f32); 5] = [(0.0, 1.0), (0.2, 0.55), (0.45, 0.25), (0.7, 0.08), (1.0, 0.0)];
    let mut mesh = egui::Mesh::default();
    for (k, keep) in RINGS {
        let rad = inner + (outer - inner) * k;
        for i in 0..SEGMENTS {
            let a = 2.0 * PI * i as f32 / SEGMENTS as f32;
            mesh.colored_vertex(on_circle(c, rad, a), col.gamma_multiply(keep));
        }
    }
    for ring in 0..RINGS.len() as u32 - 1 {
        for i in 0..SEGMENTS {
            let j = (i + 1) % SEGMENTS;
            let (a, b) = (ring * SEGMENTS + i, ring * SEGMENTS + j);
            let (c2, d) = (a + SEGMENTS, b + SEGMENTS);
            mesh.add_triangle(a, b, d);
            mesh.add_triangle(a, d, c2);
        }
    }
    Shape::mesh(mesh)
}

fn fade(c: Color32, a: f32) -> Color32 {
    c.gamma_multiply(a.clamp(0.0, 1.0))
}

fn draw(ui: &Ui, rect: Rect, t: f32) {
    let p = Palette::for_visuals(ui.visuals());
    let v = ui.visuals();
    let text = v.strong_text_color();
    let painter = ui.painter_at(rect);
    // The whole overlay fades out at the end; the parts fade in on their own.
    let all = 1.0 - ease_in_out(between(t, FADE_START, DONE));
    let parts = all * ease_in_out(between(t, 0.0, 0.35));
    let words = all * ease_in_out(between(t, 0.35, 0.95));

    painter.rect_filled(rect, 0.0, fade(v.extreme_bg_color, all));

    let size = rect.width().min(rect.height() * 1.25);
    let r = (size * 0.24).clamp(90.0, 190.0);
    let c = Pos2::new(rect.center().x, rect.center().y - r * 0.32);
    let krpm = needle_krpm(t);
    let glow_on = (krpm / MAX_KRPM).powf(0.7);

    // A soft glow behind the dial that follows the needle.
    let strength = if v.dark_mode { 0.22 } else { 0.16 };
    painter.add(glow(c, r * 1.06, r * 1.65, fade(p.accent, parts * strength * (0.35 + 0.65 * glow_on))));
    // Dial face and track.
    painter.circle_filled(c, r * 1.08, fade(p.card, parts));
    painter.circle_stroke(c, r * 1.08, Stroke::new(1.5, fade(p.track, parts)));
    let track_r = r * 0.92;
    painter.add(arc(
        c,
        track_r,
        ARC_START,
        ARC_START + ARC_SWEEP,
        Stroke::new(r * 0.07, fade(p.track, parts)),
    ));
    // Red zone, outside the track.
    painter.add(arc(
        c,
        r * 1.0,
        angle(RED_KRPM),
        angle(MAX_KRPM),
        Stroke::new(r * 0.035, fade(p.bad, parts)),
    ));
    // The lit part of the track, up to the needle.
    if krpm > 0.02 {
        let lit = Stroke::new(r * 0.07, fade(p.accent, parts));
        painter.add(arc(c, track_r, ARC_START, angle(krpm.min(RED_KRPM)), lit));
        if krpm > RED_KRPM {
            let red = Stroke::new(r * 0.07, fade(p.bad, parts));
            painter.add(arc(c, track_r, angle(RED_KRPM), angle(krpm), red));
        }
    }
    // Ticks: majors every 1000 with numbers, minors every 500.
    for half in 0..=(MAX_KRPM as i32 * 2) {
        let k = half as f32 / 2.0;
        let a = angle(k);
        let major = half % 2 == 0;
        let red = k >= RED_KRPM;
        let col = if red {
            p.bad
        } else if major {
            text
        } else {
            p.muted
        };
        let (r0, r1) = if major { (r * 0.70, r * 0.82) } else { (r * 0.75, r * 0.82) };
        painter.line_segment(
            [on_circle(c, r0, a), on_circle(c, r1, a)],
            Stroke::new(if major { 2.5 } else { 1.5 }, fade(col, parts)),
        );
        if major {
            painter.text(
                on_circle(c, r * 0.57, a),
                Align2::CENTER_CENTER,
                format!("{}", k as i32),
                theme::strong_font(r * 0.13),
                fade(if red { p.bad } else { text }, parts),
            );
        }
    }
    painter.text(
        c + Vec2::new(0.0, r * 0.36),
        Align2::CENTER_CENTER,
        "x1000 r/min",
        FontId::proportional(r * 0.085),
        fade(p.muted, parts),
    );
    // Needle: a soft wide stroke under a sharp one, then the hub.
    let a = angle(krpm);
    let tip = on_circle(c, r * 0.86, a);
    let tail = on_circle(c, r * 0.16, a + PI);
    painter.line_segment([tail, tip], Stroke::new(r * 0.07, fade(p.accent, parts * 0.25)));
    painter.line_segment([tail, tip], Stroke::new(r * 0.028, fade(p.accent, parts)));
    painter.circle_filled(c, r * 0.085, fade(p.accent, parts));
    painter.circle_filled(c, r * 0.04, fade(v.extreme_bg_color, parts));

    // The name, the tagline and the version, under the dial.
    let name_size = (r * 0.42).clamp(40.0, 72.0);
    let y = c.y + r * 1.08 + name_size * 0.95;
    let font = theme::strong_font(name_size);
    let xb = painter.layout_no_wrap("XB".into(), font.clone(), fade(p.accent, words));
    let tune = painter.layout_no_wrap("Tune".into(), font, fade(text, words));
    let w = xb.size().x + tune.size().x;
    let left = Pos2::new(rect.center().x - w / 2.0, y - xb.size().y / 2.0);
    let tune_x = left.x + xb.size().x;
    painter.galley(left, xb, Color32::WHITE);
    painter.galley(Pos2::new(tune_x, left.y), tune, Color32::WHITE);
    let mut y = y + name_size * 0.75;
    for (line, size, col) in [
        ("Diagnostics, logging and tuning for Buell DDFI motorcycles", 16.0, p.muted),
        (&format!("v{} · free software (GPL-3.0)", crate::report::version()), 12.5, p.muted),
    ] {
        painter.text(
            Pos2::new(rect.center().x, y),
            Align2::CENTER_CENTER,
            line,
            FontId::proportional(size),
            fade(col, words),
        );
        y += size * 1.7;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needle_sweeps_up_and_back() {
        assert_eq!(needle_krpm(0.0), 0.0);
        assert!((needle_krpm(SWEEP_TOP) - MAX_KRPM).abs() < 1e-3);
        assert!(needle_krpm(SWEEP_END) < 1e-3);
        assert!(needle_krpm(0.5) > 0.0 && needle_krpm(1.3) > 0.0);
    }

    #[test]
    fn screenshots_skip_it_unless_asked() {
        // XBTUNE_SHOT_SPLASH isn't set in tests.
        assert!(Splash::new(true, true).is_none());
        assert!(Splash::new(false, false).is_none());
        assert!(Splash::new(true, false).is_some());
    }
}
