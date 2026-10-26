// SPDX-License-Identifier: GPL-3.0-or-later
//! The map grid (heatmap, selection, change markers) and the 3D surface,
//! drawn with the egui painter.

use std::collections::BTreeSet;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use xbtune_defs::format_number;

use crate::theme::Palette;

/// Selected cells, plus where a range selection started.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub cells: BTreeSet<(usize, usize)>,
    pub anchor: Option<(usize, usize)>,
    drag_from: Option<(usize, usize)>,
    /// A value being typed into a cell.
    pub editing: Option<CellEdit>,
}

/// Typing in a cell, as in a spreadsheet: Enter sets every cell that was
/// selected when it started, Escape cancels, clicking elsewhere sets.
#[derive(Debug, Clone, PartialEq)]
pub struct CellEdit {
    /// Where the editor is drawn (storage order).
    pub at: (usize, usize),
    /// The cells the value goes to.
    pub cells: Vec<(usize, usize)>,
    pub text: String,
    /// Until the editor has the focus: ask for it, then put the cursor at
    /// the end.
    fresh: bool,
}

impl Selection {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn rect(&mut self, a: (usize, usize), b: (usize, usize)) {
        self.cells.clear();
        for r in a.0.min(b.0)..=a.0.max(b.0) {
            for c in a.1.min(b.1)..=a.1.max(b.1) {
                self.cells.insert((r, c));
            }
        }
    }

    pub fn list(&self) -> Vec<(usize, usize)> {
        self.cells.iter().copied().collect()
    }

    /// Start typing in the anchor cell, with `text` in the editor: the
    /// typed characters, or the cell's value to change (F2/double-click).
    pub fn start_edit(&mut self, text: String) {
        let Some(at) = self.anchor.filter(|a| self.cells.contains(a)).or(self.cells.first().copied()) else {
            return;
        };
        self.editing = Some(CellEdit { at, cells: self.list(), text, fresh: true });
    }

    /// Move a single-cell selection with the arrow keys.
    pub fn step(&mut self, dr: i64, dc: i64, rows: usize, cols: usize) {
        let (r, c) = self.anchor.unwrap_or((0, 0));
        let r = (r as i64 + dr).clamp(0, rows as i64 - 1) as usize;
        let c = (c as i64 + dc).clamp(0, cols as i64 - 1) as usize;
        self.cells.clear();
        self.cells.insert((r, c));
        self.anchor = Some((r, c));
    }
}

/// What the cells show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellMode {
    Values,
    /// The comparison image's values.
    Other,
    /// This minus the comparison image.
    Difference,
    /// Logged sample weight per cell.
    LogHits,
    /// Average EGO correction per cell (%).
    LogEgo,
    /// Average wideband AFR per cell, coloured against the target.
    LogAfr,
}

pub struct GridSpec<'a> {
    pub values: &'a [Vec<f64>],
    /// As last saved, to mark edited cells.
    pub saved: Option<&'a [Vec<f64>]>,
    /// Comparison image.
    pub other: Option<&'a [Vec<f64>]>,
    pub mode: CellMode,
    pub row_labels: Vec<String>,
    pub col_labels: Vec<String>,
    pub decimals: Option<u32>,
    /// Live operating point (fractional row, col), if known.
    pub live: Option<(f32, f32)>,
    /// Per-cell log statistics, for the LogHits/LogEgo modes.
    pub log: Option<&'a [Vec<xbtune_core::autotune::CellStat>]>,
    /// Cells with fewer hits are drawn faded in the log modes.
    pub min_hits: f64,
    /// How AFR overlay cells are shown (AFR or lambda).
    pub wb_unit: crate::settings::WbUnit,
    /// Draw columns right to left: the map stores RPM high to low, and
    /// riders (and other tuning tools) read RPM rising to the right.
    /// Everything else keeps storage order; only drawing and the mouse map
    /// through this.
    pub flip_cols: bool,
}

/// On-screen column of storage column `c` (its own inverse).
pub fn view_col(c: usize, cols: usize, flip: bool) -> usize {
    if flip { cols - 1 - c } else { c }
}

/// Heatmap colour for `t` in 0..=1 (blue, cyan, green, yellow, red).
pub fn heat(t: f32) -> Color32 {
    const STOPS: [(f32, [u8; 3]); 5] = [
        (0.0, [0x2c, 0x7b, 0xb6]),
        (0.25, [0x00, 0xa6, 0xca]),
        (0.5, [0x7c, 0xcf, 0x8a]),
        (0.75, [0xf9, 0xd0, 0x57]),
        (1.0, [0xd7, 0x19, 0x1c]),
    ];
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.5 };
    let i = STOPS.windows(2).position(|w| t <= w[1].0).unwrap_or(STOPS.len() - 2);
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    let f = (t - a.0) / (b.0 - a.0);
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * f).round() as u8;
    Color32::from_rgb(mix(a.1[0], b.1[0]), mix(a.1[1], b.1[1]), mix(a.1[2], b.1[2]))
}

/// Colour for a difference: blue below zero, red above, grey at zero.
pub fn diff_colour(d: f64, max_abs: f64) -> Color32 {
    if d == 0.0 || max_abs == 0.0 {
        return Color32::from_gray(90);
    }
    let t = (d.abs() / max_abs).clamp(0.15, 1.0) as f32;
    let base = if d > 0.0 { [0xd7, 0x30, 0x27] } else { [0x45, 0x75, 0xb4] };
    let g = 90.0;
    let mix = |x: u8| (g + (x as f32 - g) * t) as u8;
    Color32::from_rgb(mix(base[0]), mix(base[1]), mix(base[2]))
}

fn text_on(c: Color32) -> Color32 {
    let l = 0.299 * c.r() as f32 + 0.587 * c.g() as f32 + 0.114 * c.b() as f32;
    if l > 150.0 { Color32::from_gray(20) } else { Color32::WHITE }
}

fn range(grid: &[Vec<f64>]) -> (f64, f64) {
    let all = grid.iter().flatten().filter(|v| v.is_finite());
    let lo = all.clone().fold(f64::INFINITY, |a, b| a.min(*b));
    let hi = all.fold(f64::NEG_INFINITY, |a, b| a.max(*b));
    (lo, hi)
}

/// What happened in the grid this frame.
#[derive(Debug, Default)]
pub struct GridEvents {
    /// A typed value to set: the cells and the text as typed.
    pub set: Option<(Vec<(usize, usize)>, String)>,
}

/// Draw the grid, update `sel` from the mouse, and run the cell editor.
/// Cells can be typed into only while they show the values themselves.
pub fn grid(ui: &mut Ui, spec: &GridSpec, sel: &mut Selection) -> GridEvents {
    let mut out = GridEvents::default();
    let p = Palette::for_visuals(ui.visuals());
    let rows = spec.values.len();
    let cols = spec.values.first().map_or(0, |r| r.len());
    if rows == 0 || cols == 0 {
        return out;
    }
    let editable = spec.mode == CellMode::Values;
    if !editable {
        sel.editing = None;
    }
    let header_w = 64.0;
    let header_h = 26.0;
    let avail = ui.available_width() - header_w - 4.0;
    let cell_w = (avail / cols as f32).clamp(46.0, 84.0);
    let cell_h = 26.0;
    let size = vec2(header_w + cell_w * cols as f32, header_h + cell_h * rows as f32);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let origin = rect.min + vec2(header_w, header_h);
    // Storage cell (r, c) -> its place on screen.
    let cell_rect = |r: usize, c: usize| {
        let c = view_col(c, cols, spec.flip_cols);
        Rect::from_min_size(origin + vec2(c as f32 * cell_w, r as f32 * cell_h), vec2(cell_w, cell_h))
    };

    let log_mode =
        matches!(spec.mode, CellMode::LogHits | CellMode::LogEgo | CellMode::LogAfr) && spec.log.is_some();
    if log_mode && let Some(log) = spec.log {
        draw_log(&painter, spec, log, rows, cols, &cell_rect);
    }
    let shown: Vec<Vec<f64>> = match (spec.mode, spec.other) {
        (CellMode::Other, Some(o)) => o.to_vec(),
        (CellMode::Difference, Some(o)) => {
            spec.values.iter().zip(o).map(|(a, b)| a.iter().zip(b).map(|(x, y)| x - y).collect()).collect()
        }
        _ => spec.values.to_vec(),
    };
    let (lo, hi) = range(&shown);
    let max_abs = lo.abs().max(hi.abs());
    let font = FontId::proportional(12.5);

    for r in 0..rows {
        for c in 0..cols {
            if log_mode {
                break;
            }
            let v = shown[r][c];
            let fill = if spec.mode == CellMode::Difference {
                diff_colour(v, max_abs)
            } else {
                heat(if hi > lo { ((v - lo) / (hi - lo)) as f32 } else { 0.5 })
            };
            let cr = cell_rect(r, c);
            painter.rect_filled(cr.shrink(0.5), 2.0, fill);
            let text = if spec.mode == CellMode::Difference && v == 0.0 {
                String::new()
            } else if spec.mode == CellMode::Difference && v > 0.0 {
                format!("+{}", format_number(v, spec.decimals))
            } else {
                format_number(v, spec.decimals)
            };
            painter.text(cr.center(), Align2::CENTER_CENTER, text, font.clone(), text_on(fill));
            let edited = spec.saved.is_some_and(|s| s[r][c] != spec.values[r][c]);
            let differs =
                spec.mode == CellMode::Values && spec.other.is_some_and(|o| o[r][c] != spec.values[r][c]);
            if edited {
                painter.rect_stroke(
                    cr.shrink(2.0),
                    2.0,
                    Stroke::new(2.0, Color32::WHITE),
                    StrokeKind::Inside,
                );
            } else if differs {
                painter.circle_filled(cr.right_top() + vec2(-5.0, 5.0), 2.5, Color32::WHITE);
            }
        }
    }
    // Selection outline.
    for &(r, c) in &sel.cells {
        if r < rows && c < cols {
            painter.rect_stroke(cell_rect(r, c), 0.0, Stroke::new(2.0, p.accent), StrokeKind::Inside);
        }
    }
    // Live operating point.
    if let Some((lr, lc)) = spec.live {
        let lc = if spec.flip_cols { (cols - 1) as f32 - lc } else { lc };
        let at = origin + vec2((lc + 0.5) * cell_w, (lr + 0.5) * cell_h);
        painter.circle(at, 7.0, Color32::from_black_alpha(80), Stroke::new(2.5, Color32::WHITE));
    }
    // Headers.
    let hfont = FontId::proportional(11.5);
    for (c, l) in spec.col_labels.iter().enumerate().take(cols) {
        let vc = view_col(c, cols, spec.flip_cols);
        let cr = Rect::from_min_size(pos2(origin.x + vc as f32 * cell_w, rect.min.y), vec2(cell_w, header_h));
        painter.text(cr.center(), Align2::CENTER_CENTER, l, hfont.clone(), p.muted);
    }
    for (r, l) in spec.row_labels.iter().enumerate().take(rows) {
        let cr =
            Rect::from_min_size(pos2(rect.min.x, origin.y + r as f32 * cell_h), vec2(header_w - 6.0, cell_h));
        painter.text(cr.right_center(), Align2::RIGHT_CENTER, l, hfont.clone(), p.muted);
    }

    // The cell editor, over its cell.
    if let Some(ed) = &mut sel.editing
        && ed.at.0 < rows
        && ed.at.1 < cols
    {
        let id = ui.id().with("cell_edit");
        let resp = ui.put(
            cell_rect(ed.at.0, ed.at.1),
            egui::TextEdit::singleline(&mut ed.text)
                .id(id)
                .font(font.clone())
                .horizontal_align(egui::Align::Center)
                .vertical_align(egui::Align::Center)
                .margin(vec2(2.0, 0.0)),
        );
        if ed.fresh && !resp.has_focus() {
            resp.request_focus();
            ui.ctx().request_repaint();
        } else if std::mem::take(&mut ed.fresh) {
            let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
            let end = egui::text::CCursor::new(ed.text.chars().count());
            state.cursor.set_char_range(Some(egui::text::CCursorRange::one(end)));
            state.store(ui.ctx(), id);
        } else if resp.lost_focus() {
            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                out.set = Some((std::mem::take(&mut ed.cells), std::mem::take(&mut ed.text)));
            }
            sel.editing = None;
        }
        if let Some(ed) = &sel.editing {
            let n = ed.cells.len();
            let tip = if n > 1 { format!("Enter: set all {n} selected cells") } else { "Enter: set".into() };
            resp.on_hover_text(format!("{tip} · Esc: cancel"));
        }
    }

    // Mouse.
    let hit = |pos: Pos2| -> Option<(Option<usize>, Option<usize>)> {
        let d = pos - origin;
        let r = (d.y >= 0.0).then(|| (d.y / cell_h) as usize).filter(|r| *r < rows);
        let c = (d.x >= 0.0)
            .then(|| (d.x / cell_w) as usize)
            .filter(|c| *c < cols)
            .map(|c| view_col(c, cols, spec.flip_cols));
        let in_rows = d.y < rows as f32 * cell_h;
        let in_cols = d.x < cols as f32 * cell_w;
        (in_rows && in_cols).then_some((if d.y < 0.0 { None } else { r }, if d.x < 0.0 { None } else { c }))
    };
    let mods = ui.input(|i| i.modifiers);
    if response.drag_started() || response.clicked() {
        if let Some(pos) = response.interact_pointer_pos()
            && let Some(h) = hit(pos)
        {
            match h {
                (Some(r), Some(c)) if mods.shift => {
                    let a = sel.anchor.unwrap_or((r, c));
                    sel.rect(a, (r, c));
                }
                (Some(r), Some(c)) if mods.command => {
                    if !sel.cells.remove(&(r, c)) {
                        sel.cells.insert((r, c));
                    }
                    sel.anchor = Some((r, c));
                }
                (Some(r), Some(c)) => {
                    sel.cells.clear();
                    sel.cells.insert((r, c));
                    sel.anchor = Some((r, c));
                    sel.drag_from = Some((r, c));
                }
                (None, Some(c)) => {
                    sel.rect((0, c), (rows - 1, c));
                    sel.anchor = Some((0, c));
                }
                (Some(r), None) => {
                    sel.rect((r, 0), (r, cols - 1));
                    sel.anchor = Some((r, 0));
                }
                (None, None) => {
                    sel.rect((0, 0), (rows - 1, cols - 1));
                    sel.anchor = Some((0, 0));
                }
            }
        }
    } else if response.dragged()
        && let (Some(from), Some(pos)) = (sel.drag_from, response.interact_pointer_pos())
        && let Some((Some(r), Some(c))) = hit(pos)
    {
        sel.rect(from, (r, c));
    }
    if response.drag_stopped() {
        sel.drag_from = None;
    }
    // Double-click a cell to change its value.
    if editable
        && response.double_clicked()
        && let Some((Some(r), Some(c))) = response.interact_pointer_pos().and_then(hit)
    {
        sel.cells.clear();
        sel.cells.insert((r, c));
        sel.anchor = Some((r, c));
        sel.start_edit(format_number(spec.values[r][c], spec.decimals));
    }
    out
}

/// The log overlay: hits as a heatmap, average EGO coloured by how far
/// it is from 100% (red: the ECM added fuel, blue: it took fuel away), or
/// average wideband AFR against the target (red: leaner, blue: richer).
/// Cells below `min_hits` are faded; cells without data are blank.
fn draw_log(
    painter: &egui::Painter,
    spec: &GridSpec,
    log: &[Vec<xbtune_core::autotune::CellStat>],
    rows: usize,
    cols: usize,
    cell_rect: &dyn Fn(usize, usize) -> Rect,
) {
    let font = FontId::proportional(12.5);
    let max_hits = log.iter().flatten().map(|s| s.hits).fold(0.0, f64::max);
    let max_dev =
        log.iter().flatten().filter(|s| s.hits > 0.0).map(|s| (s.ego - 100.0).abs()).fold(0.0, f64::max);
    for (r, row) in log.iter().enumerate().take(rows) {
        for (c, &s) in row.iter().enumerate().take(cols) {
            let cr = cell_rect(r, c);
            if s.hits <= 0.0 {
                painter.rect_filled(cr.shrink(0.5), 2.0, Color32::from_gray(60));
                continue;
            }
            let (fill, text) = match spec.mode {
                CellMode::LogHits => (
                    heat(if max_hits > 0.0 { (s.hits.ln_1p() / max_hits.ln_1p()) as f32 } else { 0.0 }),
                    format!("{:.0}", s.hits),
                ),
                CellMode::LogAfr => (diff_colour(s.afr - s.target, 1.0), spec.wb_unit.fmt(s.afr)),
                _ => (diff_colour(s.ego - 100.0, max_dev.max(2.0)), format!("{:.1}", s.ego)),
            };
            let enough = s.hits >= spec.min_hits;
            let fill = if enough { fill } else { fill.gamma_multiply(0.45) };
            painter.rect_filled(cr.shrink(0.5), 2.0, fill);
            let tc = if enough { text_on(fill) } else { text_on(fill).gamma_multiply(0.6) };
            painter.text(cr.center(), Align2::CENTER_CENTER, text, font.clone(), tc);
        }
    }
}

/// A rotatable 3D surface of the grid. `view` is (yaw, pitch) in radians,
/// changed by dragging.
/// `flip_cols` as in [`GridSpec`], so the surface reads the same way.
pub fn surface(
    ui: &mut Ui,
    values: &[Vec<f64>],
    sel: &Selection,
    view: &mut (f32, f32),
    height: f32,
    flip_cols: bool,
) {
    let rows = values.len();
    let cols = values.first().map_or(0, |r| r.len());
    if rows < 2 || cols < 2 {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::drag());
    if response.dragged() {
        let d = response.drag_delta();
        view.0 += d.x * 0.01;
        view.1 = (view.1 + d.y * 0.01).clamp(0.1, 1.4);
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 8.0, ui.visuals().extreme_bg_color);
    let (lo, hi) = range(values);
    let span = if hi > lo { hi - lo } else { 1.0 };
    let (yaw, pitch) = *view;
    let scale = rect.width().min(rect.height() * 1.6) * 0.36;
    let center = rect.center() + vec2(0.0, rect.height() * 0.12);
    // World: x across columns, z across rows (both -1..1), y up 0..0.8.
    let project = |r: usize, c: usize| -> (Pos2, f32) {
        let x = view_col(c, cols, flip_cols) as f32 / (cols - 1) as f32 * 2.0 - 1.0;
        let z = r as f32 / (rows - 1) as f32 * 2.0 - 1.0;
        let y = ((values[r][c] - lo) / span) as f32 * 0.8;
        let (xs, zs) = (x * yaw.cos() - z * yaw.sin(), x * yaw.sin() + z * yaw.cos());
        let ys = y * pitch.cos() - zs * pitch.sin();
        let depth = y * pitch.sin() + zs * pitch.cos();
        (pos2(center.x + xs * scale, center.y - ys * scale), depth)
    };
    let mut quads: Vec<(f32, [Pos2; 4], Color32, bool)> = Vec::new();
    for r in 0..rows - 1 {
        for c in 0..cols - 1 {
            let pts = [project(r, c), project(r, c + 1), project(r + 1, c + 1), project(r + 1, c)];
            let depth = pts.iter().map(|p| p.1).sum::<f32>() / 4.0;
            let avg = (values[r][c] + values[r][c + 1] + values[r + 1][c] + values[r + 1][c + 1]) / 4.0;
            let selected =
                [(r, c), (r, c + 1), (r + 1, c), (r + 1, c + 1)].iter().any(|x| sel.cells.contains(x));
            quads.push((
                depth,
                [pts[0].0, pts[1].0, pts[2].0, pts[3].0],
                heat(((avg - lo) / span) as f32),
                selected,
            ));
        }
    }
    // Far to near.
    quads.sort_by(|a, b| b.0.total_cmp(&a.0));
    // Each quad is filled as two plain mesh triangles (no edge feathering,
    // which turns near-degenerate triangles into spikes), then outlined.
    for (_, pts, fill, selected) in quads {
        let mut mesh = egui::Mesh::default();
        for pt in pts {
            mesh.colored_vertex(pt, fill);
        }
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(mesh));
        let stroke = if selected {
            Stroke::new(1.5, p.accent)
        } else {
            Stroke::new(0.6, Color32::from_black_alpha(120))
        };
        // Separate segments: a joined outline of a collapsed quad spikes.
        for i in 0..4 {
            painter.line_segment([pts[i], pts[(i + 1) % 4]], stroke);
        }
    }
    painter.text(
        rect.left_bottom() + vec2(10.0, -8.0),
        Align2::LEFT_BOTTOM,
        "Drag to rotate",
        FontId::proportional(11.0),
        p.muted,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run frames of the grid with `events`, as the app would.
    fn frame(
        ctx: &egui::Context,
        spec: &GridSpec,
        sel: &mut Selection,
        events: Vec<egui::Event>,
    ) -> GridEvents {
        let input = egui::RawInput {
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 600.0))),
            ..Default::default()
        };
        let mut out = GridEvents::default();
        ctx.run_ui(input, |ui| out = grid(ui, spec, sel)).textures_delta.clear();
        out
    }

    fn key(k: egui::Key) -> egui::Event {
        egui::Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn typing_in_a_cell_sets_the_cells_selected_then() {
        let values = vec![vec![1.0, 2.0], vec![3.0, 4.0]];
        let spec = GridSpec {
            values: &values,
            saved: None,
            other: None,
            mode: CellMode::Values,
            row_labels: vec!["a".into(), "b".into()],
            col_labels: vec!["x".into(), "y".into()],
            decimals: Some(0),
            live: None,
            log: None,
            min_hits: 0.0,
            wb_unit: Default::default(),
            flip_cols: false,
        };
        let ctx = egui::Context::default();
        // The app's fonts: without any, text has no glyphs and no cursor places.
        crate::theme::install(&ctx);
        let mut sel = Selection::default();
        sel.rect((0, 0), (0, 1));
        sel.anchor = Some((0, 0));
        frame(&ctx, &spec, &mut sel, vec![]);
        // A typed "5" opens the editor with it; then "7" and Enter.
        sel.start_edit("5".into());
        assert!(frame(&ctx, &spec, &mut sel, vec![]).set.is_none());
        assert!(frame(&ctx, &spec, &mut sel, vec![]).set.is_none());
        assert!(frame(&ctx, &spec, &mut sel, vec![egui::Event::Text("7".into())]).set.is_none());
        let out = frame(&ctx, &spec, &mut sel, vec![key(egui::Key::Enter)]);
        assert_eq!(out.set, Some((vec![(0, 0), (0, 1)], "57".to_string())));
        assert!(sel.editing.is_none());

        // F2/double-click: the old value, cursor after it; Escape cancels.
        sel.start_edit("1".into());
        frame(&ctx, &spec, &mut sel, vec![]);
        frame(&ctx, &spec, &mut sel, vec![]);
        frame(&ctx, &spec, &mut sel, vec![egui::Event::Text("9".into())]);
        assert_eq!(sel.editing.as_ref().map(|e| e.text.as_str()), Some("19"));
        assert!(frame(&ctx, &spec, &mut sel, vec![key(egui::Key::Escape)]).set.is_none());
        assert!(sel.editing.is_none());

        // Overlays aren't typed into.
        sel.start_edit("3".into());
        let hits = GridSpec { mode: CellMode::Difference, ..spec };
        frame(&ctx, &hits, &mut sel, vec![]);
        assert!(sel.editing.is_none());
    }

    #[test]
    fn flipped_columns_map_both_ways() {
        // 13 RPM columns stored 8000..0: the 0 rpm column is drawn first.
        assert_eq!(view_col(12, 13, true), 0);
        assert_eq!(view_col(0, 13, true), 12);
        assert!((0..13).all(|c| view_col(view_col(c, 13, true), 13, true) == c));
        assert_eq!(view_col(4, 13, false), 4);
    }

    #[test]
    fn heat_endpoints_and_selection() {
        assert_eq!(heat(0.0), Color32::from_rgb(0x2c, 0x7b, 0xb6));
        assert_eq!(heat(1.0), Color32::from_rgb(0xd7, 0x19, 0x1c));
        assert_eq!(heat(f32::NAN), heat(0.5));
        assert_eq!(diff_colour(0.0, 5.0), Color32::from_gray(90));
        let mut s = Selection::default();
        s.rect((2, 3), (0, 1));
        assert_eq!(s.cells.len(), 9);
        s.anchor = Some((0, 0));
        s.step(-1, 5, 4, 4);
        assert_eq!(s.list(), vec![(0, 3)]);
    }
}
