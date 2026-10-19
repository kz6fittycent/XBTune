// SPDX-License-Identifier: GPL-3.0-or-later
//! The Tune view: open an EEPROM image, edit every map, table, value and
//! flag, compare with a second image, undo/redo, save. Works on files only;
//! nothing here talks to the ECM except "Read from ECM" (read-only).

use std::path::{Path, PathBuf};

use egui::{Key, RichText, Ui};
use xbtune_core::image::Image;
use xbtune_core::session::Command;
use xbtune_core::tune::{Clip, Op, Tune, apply_op, element_value, partner};
use xbtune_defs::{EeKind, EeVar, Shape, Value, format_number, unit_symbol};

use crate::app::XbApp;
use crate::grid::{CellMode, GridSpec, Selection, grid, surface};
use crate::theme::{self, Palette};

/// Which file dialog is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Open,
    Compare,
    SaveAs,
}

pub struct TuneUi {
    pub selected: Option<String>,
    filter: String,
    sel: Selection,
    value_text: String,
    link: bool,
    view3d: bool,
    view: (f32, f32),
    mode: CellMode,
    clip: Option<Clip>,
    pub pick: Option<Pick>,
    pub dir: PathBuf,
    file_name: String,
    /// Open/compare a file the user picked while there were unsaved edits.
    confirm_discard: Option<PathBuf>,
    pub read_progress: Option<(usize, usize)>,
    pub status: Option<(String, bool)>,
}

impl Default for TuneUi {
    fn default() -> Self {
        Self {
            selected: Some("Tab_Fuel_Front".into()),
            filter: String::new(),
            sel: Selection::default(),
            value_text: String::new(),
            link: false,
            view3d: false,
            view: (0.6, 0.7),
            mode: CellMode::Values,
            clip: None,
            pick: None,
            dir: tunes_dir(),
            file_name: String::new(),
            confirm_discard: None,
            read_progress: None,
            status: None,
        }
    }
}

/// Where tunes read from the ECM are kept (`<files>/Tunes`).
pub fn tunes_dir() -> PathBuf {
    xbtune_core::env::Dirs::detect().map(|d| d.tunes()).unwrap_or_else(|| PathBuf::from("."))
}

/// Backups taken before burns (`<files>/Backups`).
pub fn backups_dir() -> PathBuf {
    xbtune_core::env::Dirs::detect().map(|d| d.backups()).unwrap_or_else(|| PathBuf::from("."))
}

/// The user's real home folder (in the snap, `$HOME` is per-snap).
fn home_dir() -> PathBuf {
    xbtune_core::env::real_home().unwrap_or_default()
}

/// (rows, cols) of the grid shown for a variable, and how a cell maps to
/// an element index in storage order.
fn layout(v: &EeVar) -> Option<(usize, usize)> {
    match v.shape {
        Shape::Grid { rows, cols, .. } => Some((rows, cols)),
        // Two rows: input, output.
        Shape::Pairs { rows } => Some((2, rows)),
        Shape::List(n) => Some((1, n)),
        _ => None,
    }
}

fn index(v: &EeVar, r: usize, c: usize) -> usize {
    match v.shape {
        Shape::Grid { cols, .. } => r * cols + c,
        Shape::Pairs { .. } => c * 2 + r,
        _ => c,
    }
}

fn as_grid(v: &EeVar, el: &[f64]) -> Vec<Vec<f64>> {
    let Some((rows, cols)) = layout(v) else { return Vec::new() };
    (0..rows)
        .map(|r| (0..cols).map(|c| el.get(index(v, r, c)).copied().unwrap_or(f64::NAN)).collect())
        .collect()
}

fn units(u: &Option<String>) -> String {
    u.as_deref()
        .map(|u| unit_symbol(u).map(str::to_string).unwrap_or_else(|| u.to_string()))
        .unwrap_or_default()
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    toolbar(app, ui);
    if let Some((msg, err)) = &app.tune_ui.status {
        ui.colored_label(if *err { p.bad } else { p.good }, msg);
    }
    ui.add_space(4.0);
    file_dialog(app, ui);
    discard_dialog(app, ui);
    let Some(tune) = &app.tune else {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No tune open").font(theme::strong_font(20.0)));
            ui.label(
                RichText::new(
                    "Open an image (.xbt or EcmDroid .xpr), or read one from the ECM while connected.",
                )
                .color(p.muted),
            );
        });
        return;
    };
    let modified = tune.modified_vars();
    let differs: Vec<String> = match &app.compare {
        Some((_, other)) => tune
            .def
            .eeprom
            .values()
            .filter(|v| v.raw(&tune.image.view()).ok() != v.raw(&other.view()).ok())
            .map(|v| v.name.clone())
            .collect(),
        None => Vec::new(),
    };
    egui::Panel::left("vars").resizable(true).default_size(270.0).show(ui, |ui| {
        var_list(app, ui, &modified, &differs);
    });
    egui::CentralPanel::default()
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 0)))
        .show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| editor(app, ui));
        });
    shortcuts(app, ui);
}

fn toolbar(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    let waiting = app.remote.host.proposals.len();
    if waiting > 0 && crate::remote_sync::host_paired(app) {
        let who = app.remote.peer().unwrap_or_else(|| "Your tuner".into());
        theme::card(ui).stroke(egui::Stroke::new(1.0, p.accent)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(format!("{who} proposed {waiting} edit(s) to this tune."));
                if ui.button("Review in Remote").clicked() {
                    app.view = crate::app::View::Remote;
                }
            });
        });
        ui.add_space(6.0);
    }
    ui.horizontal_wrapped(|ui| {
        ui.heading("Tune");
        if let Some(t) = &app.tune {
            let name = crate::remote_sync::shared_label(app).or_else(|| {
                t.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned())
            });
            let star = if t.is_modified() { " •" } else { "" };
            ui.label(
                RichText::new(format!("{}{star}", name.unwrap_or_else(|| "(not saved yet)".into()))).strong(),
            );
            ui.label(RichText::new(format!("{} · {}", t.ecm.id, t.ecm.family)).color(p.muted));
        }
        ui.separator();
        if ui.button("Open…").clicked() {
            app.tune_ui.pick = Some(Pick::Open);
        }
        let live = app.link.live();
        // On the tuner's side the bike owner shares the tune instead.
        let can_read = live && app.link.remote.is_none();
        if ui
            .add_enabled(can_read && app.tune_ui.read_progress.is_none(), egui::Button::new("Read from ECM"))
            .on_hover_text(
                "Read and verify the whole EEPROM (read-only); saved as a backup in the images folder",
            )
            .clicked()
        {
            app.tune_ui.read_progress = Some((0, 1));
            app.send(Command::ReadEeprom);
        }
        let has = app.tune.is_some();
        let can_burn = has && live && app.writes_permitted() && !app.burn.is_open();
        let burn_btn =
            egui::Button::new(RichText::new("Burn to ECM…").color(if can_burn { p.bad } else { p.muted }));
        let r = ui.add_enabled(can_burn, burn_btn);
        let r = if !app.writes_permitted() {
            r.on_disabled_hover_text(xbtune_core::diag::write_locked_message())
        } else {
            r.on_disabled_hover_text("Connect to the ECM and open a tune first").on_hover_text(
                "Backup, review the differences, confirm, write only what changed, read it back",
            )
        };
        if r.clicked() {
            start_burn(app);
        }
        let may_save = has && !crate::remote_sync::save_blocked(app);
        let blocked = "The bike owner hasn't allowed saving a copy of their tune";
        if ui.add_enabled(may_save, egui::Button::new("Save")).on_disabled_hover_text(blocked).clicked() {
            save(app, false);
        }
        if ui.add_enabled(may_save, egui::Button::new("Save as…")).on_disabled_hover_text(blocked).clicked()
        {
            app.tune_ui.pick = Some(Pick::SaveAs);
        }
        ui.separator();
        let (cu, cr) = app.tune.as_ref().map_or((false, false), |t| (t.can_undo(), t.can_redo()));
        let ul = app.tune.as_ref().and_then(|t| t.undo_label().map(str::to_string)).unwrap_or_default();
        let rl = app.tune.as_ref().and_then(|t| t.redo_label().map(str::to_string)).unwrap_or_default();
        if ui
            .add_enabled(cu, egui::Button::new("Undo"))
            .on_hover_text(format!("Undo {ul} (Ctrl+Z)"))
            .clicked()
        {
            undo(app, false);
        }
        if ui
            .add_enabled(cr, egui::Button::new("Redo"))
            .on_hover_text(format!("Redo {rl} (Ctrl+Y)"))
            .clicked()
        {
            undo(app, true);
        }
        ui.separator();
        if ui.add_enabled(has, egui::Button::new("Compare with…")).clicked() {
            app.tune_ui.pick = Some(Pick::Compare);
        }
        if let Some((path, _)) = &app.compare {
            ui.label(
                RichText::new(format!("vs {}", path.file_name().unwrap_or_default().to_string_lossy()))
                    .color(p.muted),
            );
            if ui.small_button("Stop comparing").clicked() {
                app.compare = None;
                app.tune_ui.mode = CellMode::Values;
            }
        }
        if let Some((done, total)) = app.tune_ui.read_progress {
            ui.separator();
            ui.add(
                egui::ProgressBar::new(done as f32 / total.max(1) as f32)
                    .desired_width(160.0)
                    .show_percentage(),
            );
            if ui.small_button("Cancel").clicked() {
                app.send(Command::CancelRead);
            }
        }
    });
}

fn var_list(app: &mut XbApp, ui: &mut Ui, modified: &[String], differs: &[String]) {
    let p = Palette::for_visuals(ui.visuals());
    let Some(t) = &app.tune else { return };
    ui.add(
        egui::TextEdit::singleline(&mut app.tune_ui.filter)
            .hint_text("Search variables")
            .desired_width(f32::INFINITY),
    );
    let f = app.tune_ui.filter.to_lowercase();
    let mut pick = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for (kind, title) in [
            (EeKind::Map, "Maps"),
            (EeKind::Table, "Tables"),
            (EeKind::Value, "Values"),
            (EeKind::Bits, "Flags"),
            (EeKind::Axis, "Axes"),
            (EeKind::Array, "Arrays"),
        ] {
            let mut vars: Vec<&EeVar> = t
                .def
                .eeprom
                .values()
                .filter(|v| v.kind == kind && !v.secret)
                .filter(|v| {
                    f.is_empty()
                        || v.name.to_lowercase().contains(&f)
                        || v.label().to_lowercase().contains(&f)
                })
                .collect();
            if vars.is_empty() {
                continue;
            }
            vars.sort_by(|a, b| a.label().cmp(b.label()));
            egui::CollapsingHeader::new(format!("{title} ({})", vars.len()))
                .default_open(matches!(kind, EeKind::Map) || !f.is_empty())
                .show(ui, |ui| {
                    for v in vars {
                        let mut text = RichText::new(v.label());
                        if modified.contains(&v.name) {
                            text = text.color(p.accent).strong();
                        }
                        let mut label = text.clone();
                        if differs.contains(&v.name) {
                            label = RichText::new(format!("≠ {}", v.label())).color(p.warn);
                            if modified.contains(&v.name) {
                                label = label.strong();
                            }
                        }
                        let selected = app.tune_ui.selected.as_deref() == Some(&v.name);
                        if ui.selectable_label(selected, label).on_hover_text(&v.name).clicked() {
                            pick = Some(v.name.clone());
                        }
                    }
                });
        }
    });
    if let Some(n) = pick {
        app.tune_ui.selected = Some(n);
        app.tune_ui.sel.clear();
    }
}

fn editor(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    let Some(t) = &app.tune else { return };
    let Some(name) = app.tune_ui.selected.clone().filter(|n| t.def.eeprom.contains_key(n)) else {
        ui.label(RichText::new("Pick a variable on the left.").color(p.muted));
        return;
    };
    let v = t.def.eeprom[&name].clone();
    ui.label(RichText::new(v.label()).font(theme::strong_font(18.0)));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&v.name).color(p.muted));
        if let Some(d) = v.names.description.as_ref().filter(|d| Some(*d) != v.names.label.as_ref()) {
            ui.label(RichText::new(d).color(p.muted));
        }
    });
    if let Some(r) = &v.names.remark {
        ui.label(RichText::new(r).small().color(p.muted));
    }
    if let Err(e) = t.editable(&name) {
        ui.colored_label(p.warn, e.to_string());
        return;
    }
    ui.add_space(6.0);
    match &v.shape {
        Shape::Scalar => scalar_editor(app, ui, &v),
        Shape::Bits => bits_editor(app, ui, &v),
        Shape::Grid { .. } | Shape::Pairs { .. } | Shape::List(_) => grid_editor(app, ui, &v),
        Shape::Text | Shape::Invalid(_) => {}
    }
}

fn axis_labels(t: &Tune, axis: &Option<String>) -> Option<(Vec<f64>, Vec<String>)> {
    let a = t.def.eeprom.get(axis.as_ref()?)?;
    let Value::List(v) = a.decode(&t.image.view()).ok()? else { return None };
    let labels = v.iter().map(|x| format_number(*x, a.names.decimals)).collect();
    Some((v, labels))
}

/// Fractional position of `x` in breakpoints `axis` (either direction).
fn frac_index(axis: &[f64], x: f64) -> Option<f32> {
    for i in 0..axis.len().saturating_sub(1) {
        let (a, b) = (axis[i], axis[i + 1]);
        if (a <= x && x <= b) || (b <= x && x <= a) {
            let f = if b != a { (x - a) / (b - a) } else { 0.0 };
            return Some(i as f32 + f as f32);
        }
    }
    let (first, last) = (*axis.first()?, *axis.last()?);
    let nearer_first = (x - first).abs() < (x - last).abs();
    Some(if nearer_first { 0.0 } else { (axis.len() - 1) as f32 })
}

/// Live channel for a map axis, by the axis name (XBTune's reading).
fn live_for_axis(app: &XbApp, axis: &str) -> Option<f64> {
    let s = app.history.latest()?;
    let ch = xbtune_core::autotune::channel_for_axis(axis)?;
    s.value(app.link.channel(ch)?)
}

fn grid_editor(app: &mut XbApp, ui: &mut Ui, v: &EeVar) {
    let p = Palette::for_visuals(ui.visuals());
    let t = app.tune.as_ref().expect("tune open");
    let Ok(el) = t.elements(&v.name) else { return };
    let values = as_grid(v, &el);
    let saved = t.saved_elements(&v.name).ok().map(|s| as_grid(v, &s));
    let other = app.compare.as_ref().and_then(|(_, img)| {
        let raw = v.raw(&img.view()).ok()?;
        let el: Vec<f64> = raw.iter().enumerate().map(|(i, r)| element_value(v, i, *r)).collect();
        Some(as_grid(v, &el))
    });
    let (rows, cols) = (values.len(), values.first().map_or(0, |r| r.len()));
    let xs = axis_labels(t, &v.x_axis);
    let ys = axis_labels(t, &v.y_axis);
    let col_labels = match (&v.shape, &xs) {
        (_, Some((_, l))) => l.clone(),
        _ => (0..cols).map(|c| format!("{}", c + 1)).collect(),
    };
    let row_labels = match (&v.shape, &ys) {
        (Shape::Pairs { .. }, _) => {
            vec![format!("in {}", units(&v.names.axis_units)), format!("out {}", units(&v.names.units))]
        }
        (_, Some((_, l))) => l.clone(),
        _ => (0..rows).map(|r| format!("{}", r + 1)).collect(),
    };
    // Live operating point (or the log playhead), if both axes follow a
    // live channel.
    let live = match (&xs, &ys, &v.x_axis, &v.y_axis) {
        (Some((xv, _)), Some((yv, _)), Some(xn), Some(yn)) if app.link.live() || app.logs.play.is_some() => {
            let x = live_for_axis(app, xn).and_then(|x| frac_index(xv, x));
            let y = live_for_axis(app, yn).and_then(|y| frac_index(yv, y));
            x.zip(y).map(|(c, r)| (r, c))
        }
        _ => None,
    };
    let decimals = v.names.decimals;
    let partner_name = partner(&t.def, &v.name);

    // Axis names and mode.
    ui.horizontal_wrapped(|ui| {
        let ax =
            |n: &Option<String>| n.as_ref().and_then(|n| t.def.eeprom.get(n)).map(|a| a.label().to_string());
        if let (Some(y), Some(x)) = (ax(&v.y_axis), ax(&v.x_axis)) {
            ui.label(
                RichText::new(format!("rows: {y} · columns: {x} · values in {}", units(&v.names.units)))
                    .color(p.muted),
            );
        } else if matches!(v.shape, Shape::Pairs { .. }) {
            ui.label(
                RichText::new(
                    "input -> output pairs (input scaling is XBTune's reading of the data; unconfirmed)",
                )
                .color(p.warn),
            );
        }
        if live.is_some() {
            ui.label(RichText::new("○ live point (approx.)").color(p.muted)).on_hover_text(
                "Columns follow the RPM channel and rows the Load channel; this pairing is XBTune's reading",
            );
        }
    });
    // The AI advisor, on fuel and spark maps.
    let advisable = v.name.starts_with("Tab_Fuel") || v.name.starts_with("Tab_Spark");
    let ask_ai = advisable
        && matches!(v.shape, Shape::Grid { .. })
        && ui
            .button("Ask the AI advisor…")
            .on_hover_text("Ask a language model about this map (answers only; it changes nothing)")
            .clicked();
    if ask_ai {
        // Opened by ai_view::show (the tune is borrowed here).
        app.ai.request = Some(v.name.clone());
    }
    // Log overlays and auto-tune, on the fuel maps once logs are open.
    let is_fuel = xbtune_core::autotune::fuel_maps(t).contains(&v.name);
    if is_fuel
        && ui
            .button("Fold AFV into the maps…")
            .on_hover_text(
                "Multiply the fuel maps by the learned AFV and set AFV to 100% (edits the tune only)",
            )
            .clicked()
    {
        app.afv_fold.open = true;
    }
    let log_stats = if is_fuel && !app.logs.logs.is_empty() {
        ui.horizontal_wrapped(|ui| {
            use crate::autotune_view::Overlay;
            ui.label(format!("Logs ({}):", app.logs.logs.len()));
            ui.selectable_value(&mut app.autotune.overlay, Overlay::Off, "Off");
            ui.selectable_value(&mut app.autotune.overlay, Overlay::Hits, "Samples per cell");
            ui.selectable_value(&mut app.autotune.overlay, Overlay::Ego, "Average EGO");
            if crate::autotune_view::logs_have_wideband(app) {
                ui.selectable_value(&mut app.autotune.overlay, Overlay::Afr, "Average AFR (wideband)");
            }
            if ui.button("Auto-tune…").clicked() {
                crate::autotune_view::open(app);
            }
        });
        if app.autotune.overlay == crate::autotune_view::Overlay::Off {
            None
        } else {
            crate::autotune_view::analysis(app, &v.name)
        }
    } else {
        None
    };
    if other.is_some() {
        ui.horizontal_wrapped(|ui| {
            ui.label("Show:");
            ui.selectable_value(&mut app.tune_ui.mode, CellMode::Values, "This tune");
            ui.selectable_value(&mut app.tune_ui.mode, CellMode::Other, "Other image");
            ui.selectable_value(&mut app.tune_ui.mode, CellMode::Difference, "Difference (this − other)");
            if ui.button("Copy this table from the other image").clicked()
                && let Some((_, img)) = &app.compare
            {
                let img = img.clone();
                let r = app.tune.as_mut().expect("open").copy_from(&img, &v.name);
                report(app, r.map(|a| format!("Copied {} ({} changed)", v.label(), a.changed)));
            }
        });
    }

    // How-to line and shortcuts help.
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(
                "1. Select cells: click; drag or Shift-click for a block; Ctrl-click to add; click a row or column heading for the whole line.   2. Type a number.   3. Press an action.",
            )
            .small()
            .color(p.muted),
        );
        ui.menu_button(RichText::new("Shortcuts").small(), |ui| {
            egui::Grid::new("keys").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                for (k, what) in [
                    ("Enter (in the value box)", "Set the selected cells to the value"),
                    ("Arrow keys", "Move the selection"),
                    ("Ctrl+C / Ctrl+V", "Copy / paste cells (works with spreadsheets)"),
                    ("Ctrl+Z", "Undo"),
                    ("Ctrl+Y or Ctrl+Shift+Z", "Redo"),
                    ("Ctrl+S", "Save (asks for a new name for an ECM backup)"),
                    ("Drag in 3D", "Rotate the surface"),
                ] {
                    ui.label(RichText::new(k).strong());
                    ui.label(what);
                    ui.end_row();
                }
            });
        });
    });

    // Operations toolbar.
    let mut op: Option<Op> = None;
    let mut copy = false;
    let mut paste = false;
    ui.horizontal_wrapped(|ui| {
        let n = app.tune_ui.sel.cells.len();
        ui.label(
            RichText::new(if n == 0 { "Select cells".into() } else { format!("{n} selected") })
                .color(p.muted),
        );
        let resp = ui.add(
            egui::TextEdit::singleline(&mut app.tune_ui.value_text).hint_text("number").desired_width(70.0),
        );
        let num: Option<f64> = app.tune_ui.value_text.trim().parse().ok();
        let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let can = n > 0;
        if (ui.add_enabled(can && num.is_some(), egui::Button::new("Set")).clicked() || (enter && can))
            && let Some(x) = num
        {
            op = Some(Op::Set(x));
        }
        if ui
            .add_enabled(can && num.is_some(), egui::Button::new("+ Add"))
            .on_hover_text("Add the value")
            .clicked()
        {
            op = num.map(Op::Add);
        }
        if ui
            .add_enabled(can && num.is_some(), egui::Button::new("− Subtract"))
            .on_hover_text("Subtract the value")
            .clicked()
        {
            op = num.map(|x| Op::Add(-x));
        }
        if ui
            .add_enabled(can && num.is_some(), egui::Button::new("× Multiply"))
            .on_hover_text("Multiply by the value")
            .clicked()
        {
            op = num.map(Op::Multiply);
        }
        if ui
            .add_enabled(can && num.is_some(), egui::Button::new("% Change"))
            .on_hover_text("Change by the value in percent (5 = +5%)")
            .clicked()
        {
            op = num.map(Op::Percent);
        }
        if ui
            .add_enabled(can, egui::Button::new("Smooth"))
            .on_hover_text("Average each cell with its neighbours")
            .clicked()
        {
            op = Some(Op::Smooth);
        }
        if ui
            .add_enabled(can, egui::Button::new("Fill across ↔"))
            .on_hover_text("Straight line across each selected row")
            .clicked()
        {
            op = Some(Op::InterpolateRows);
        }
        if ui
            .add_enabled(can, egui::Button::new("Fill down ↕"))
            .on_hover_text("Straight line down each selected column")
            .clicked()
        {
            op = Some(Op::InterpolateCols);
        }
        ui.separator();
        copy = ui.add_enabled(can, egui::Button::new("Copy")).on_hover_text("Ctrl+C").clicked();
        paste = ui
            .add_enabled(
                app.tune_ui.sel.anchor.is_some() && app.tune_ui.clip.is_some(),
                egui::Button::new("Paste"),
            )
            .on_hover_text("Ctrl+V; pastes at the top-left selected cell")
            .clicked();
        if let Some(pn) = &partner_name {
            ui.checkbox(&mut app.tune_ui.link, "Link front/rear")
                .on_hover_text(format!("Apply the same operation to {pn}"));
        }
        if matches!(v.shape, Shape::Grid { .. }) {
            ui.checkbox(&mut app.tune_ui.view3d, "3D");
        }
    });

    if app.tune_ui.view3d && matches!(v.shape, Shape::Grid { .. }) {
        ui.add_space(4.0);
        surface(ui, &values, &app.tune_ui.sel, &mut app.tune_ui.view, 300.0);
    }
    ui.add_space(4.0);
    let spec = GridSpec {
        values: &values,
        saved: saved.as_deref(),
        other: other.as_deref(),
        mode: match (&log_stats, app.autotune.overlay) {
            (Some(_), crate::autotune_view::Overlay::Hits) => CellMode::LogHits,
            (Some(_), crate::autotune_view::Overlay::Ego) => CellMode::LogEgo,
            (Some(_), crate::autotune_view::Overlay::Afr) => CellMode::LogAfr,
            _ if other.is_some() => app.tune_ui.mode,
            _ => CellMode::Values,
        },
        row_labels,
        col_labels,
        decimals,
        live,
        log: log_stats.as_ref().map(|a| a.cells.as_slice()),
        min_hits: app.autotune.settings.min_hits,
    };
    egui::ScrollArea::horizontal().id_salt("grid").show(ui, |ui| {
        grid(ui, &spec, &mut app.tune_ui.sel);
    });
    if let Some((_, l)) = &ys
        && matches!(v.shape, Shape::Grid { .. })
        && l.len() != rows
    {
        ui.colored_label(p.warn, "Axis length doesn't match the map.");
    }

    // Keyboard shortcuts for the grid (when no text box has focus).
    let typing = ui.ctx().egui_wants_keyboard_input();
    if !typing {
        let (c, v_paste, pasted, arrows) = ui.input(|i| {
            let ctrl = i.modifiers.command;
            let pasted = i.events.iter().find_map(|e| match e {
                egui::Event::Paste(t) => Some(t.clone()),
                _ => None,
            });
            let mut arrows = (0i64, 0i64);
            if i.key_pressed(Key::ArrowUp) {
                arrows.0 -= 1;
            }
            if i.key_pressed(Key::ArrowDown) {
                arrows.0 += 1;
            }
            if i.key_pressed(Key::ArrowLeft) {
                arrows.1 -= 1;
            }
            if i.key_pressed(Key::ArrowRight) {
                arrows.1 += 1;
            }
            (
                ctrl && i.key_pressed(Key::C) || i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                ctrl && i.key_pressed(Key::V),
                pasted,
                arrows,
            )
        });
        copy |= c;
        if let Some(text) = pasted {
            if let Some(clip) = Clip::from_text(&text) {
                app.tune_ui.clip = Some(clip);
            }
            paste = true;
        } else if v_paste {
            paste = true;
        }
        if arrows != (0, 0) {
            app.tune_ui.sel.step(arrows.0, arrows.1, rows, cols);
        }
    }

    let cells = app.tune_ui.sel.list();
    if copy && let Some(clip) = Clip::copy(&values, &cells) {
        ui.ctx().copy_text(clip.to_tsv(decimals));
        app.tune_ui.clip = Some(clip);
        app.tune_ui.status = Some((format!("Copied {} cells", cells.len()), false));
    }
    if paste && let (Some(at), Some(clip)) = (cells.first().copied(), app.tune_ui.clip.clone()) {
        let changes = clip.paste_at(at, rows, cols);
        apply_changes(app, v, &changes, "paste");
    }
    if let Some(op) = op {
        let changes = apply_op(&values, &cells, op);
        let label = format!("{op:?} on {}", v.label());
        apply_changes(app, v, &changes, &label);
        if app.tune_ui.link
            && let Some(pn) = &partner_name
        {
            let t = app.tune.as_ref().expect("open");
            let pv = t.def.eeprom[pn].clone();
            if let Ok(pel) = t.elements(pn) {
                let pgrid = as_grid(&pv, &pel);
                let pchanges = apply_op(&pgrid, &cells, op);
                apply_changes(app, &pv, &pchanges, &format!("{op:?} on {} (linked)", pv.label()));
            }
        }
    }
}

fn apply_changes(app: &mut XbApp, v: &EeVar, changes: &[((usize, usize), f64)], label: &str) {
    let list: Vec<(String, usize, f64)> =
        changes.iter().map(|((r, c), x)| (v.name.clone(), index(v, *r, *c), *x)).collect();
    let r = app.tune.as_mut().expect("open").set(label, &list);
    report(
        app,
        r.map(|a| {
            let mut m = format!("{label}: {} cell(s) changed", a.changed);
            if a.clamped > 0 {
                m.push_str(&format!(", {} clamped to the allowed range", a.clamped));
            }
            m
        }),
    );
}

fn report(app: &mut XbApp, r: Result<String, xbtune_core::tune::TuneError>) {
    app.tune_ui.status = Some(match r {
        Ok(m) => (m, false),
        Err(e) => (e.to_string(), true),
    });
}

fn scalar_editor(app: &mut XbApp, ui: &mut Ui, v: &EeVar) {
    let p = Palette::for_visuals(ui.visuals());
    let t = app.tune.as_ref().expect("open");
    let Ok(el) = t.elements(&v.name) else { return };
    let cur = el[0];
    let saved = t.saved_elements(&v.name).ok().map(|s| s[0]);
    let other = app.compare.as_ref().and_then(|(_, img)| v.raw(&img.view()).ok()).map(|r| v.scaled(r[0]));
    let u = units(&v.names.units);
    let raw = v.raw(&t.image.view()).ok().map(|r| r[0]);
    theme::card(ui).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format_number(cur, v.names.decimals)).font(theme::strong_font(26.0)));
            ui.label(RichText::new(&u).color(p.muted));
        });
        if let Some(s) = saved.filter(|s| *s != cur) {
            ui.label(
                RichText::new(format!("was {} {u} when saved", format_number(s, v.names.decimals)))
                    .color(p.accent),
            );
        }
        if let Some(o) = other {
            let c = if o == cur { p.muted } else { p.warn };
            ui.label(
                RichText::new(format!("other image: {} {u}", format_number(o, v.names.decimals))).color(c),
            );
        }
        let max = if v.width >= 4 { u32::MAX as f64 } else { ((1u64 << (8 * v.width)) - 1) as f64 };
        ui.label(
            RichText::new(format!(
                "raw {} · step {} · range {} to {} {u}",
                raw.map_or("-".into(), |r| r.to_string()),
                format_number(v.scale, None),
                format_number(v.translate, v.names.decimals),
                format_number(max * v.scale + v.translate, v.names.decimals)
            ))
            .small()
            .color(p.muted),
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut app.tune_ui.value_text)
                    .hint_text("new value")
                    .desired_width(110.0),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let num: Option<f64> = app.tune_ui.value_text.trim().parse().ok();
            let mut set = None;
            if ui.add_enabled(num.is_some(), egui::Button::new("Set")).clicked() || enter {
                set = num;
            }
            if ui.button("− step").clicked() {
                set = Some(cur - v.scale);
            }
            if ui.button("+ step").clicked() {
                set = Some(cur + v.scale);
            }
            if let Some(x) = set {
                apply_changes(app, v, &[((0, 0), x)], &format!("set {}", v.label()));
            }
        });
    });
}

fn bits_editor(app: &mut XbApp, ui: &mut Ui, v: &EeVar) {
    let p = Palette::for_visuals(ui.visuals());
    let t = app.tune.as_ref().expect("open");
    let Ok(Value::Bits(byte)) = t.value(&v.name) else { return };
    let saved = t.saved_elements(&v.name).ok().map(|s| s[0] as u8);
    let other = app.compare.as_ref().and_then(|(_, img)| v.raw(&img.view()).ok()).map(|r| r[0] as u8);
    let mut new = byte;
    theme::card(ui).show(ui, |ui| {
        ui.label(RichText::new(format!("{byte:08b}")).font(theme::strong_font(18.0)));
        let named: Vec<_> = v.bits.as_ref().map(|b| b.bits.clone()).unwrap_or_default();
        for nr in 0..8u8 {
            let b = named.iter().find(|b| b.nr == nr);
            let name = b.map(|b| b.name.clone()).unwrap_or_else(|| format!("bit {nr}"));
            let mut on = new & (1 << nr) != 0;
            ui.horizontal(|ui| {
                if ui.checkbox(&mut on, &name).changed() {
                    new ^= 1 << nr;
                }
                if let Some(d) = b.and_then(|b| b.description.as_ref()).filter(|d| **d != name) {
                    ui.label(RichText::new(d).small().color(p.muted));
                }
                if saved.is_some_and(|s| s & (1 << nr) != byte & (1 << nr)) {
                    ui.label(RichText::new("edited").small().color(p.accent));
                }
                if other.is_some_and(|o| o & (1 << nr) != byte & (1 << nr)) {
                    ui.label(RichText::new("≠ other").small().color(p.warn));
                }
            });
        }
    });
    if new != byte {
        let r = app.tune.as_mut().expect("open").set_bits(&v.name, new);
        report(app, r.map(|_| format!("{} changed", v.label())));
    }
}

fn shortcuts(app: &mut XbApp, ui: &mut Ui) {
    if ui.ctx().egui_wants_keyboard_input() {
        return;
    }
    let (z, y, s) = ui.input(|i| {
        let c = i.modifiers.command;
        (
            c && !i.modifiers.shift && i.key_pressed(Key::Z),
            c && (i.key_pressed(Key::Y) || i.modifiers.shift && i.key_pressed(Key::Z)),
            c && i.key_pressed(Key::S),
        )
    });
    if z {
        undo(app, false);
    }
    if y {
        undo(app, true);
    }
    if s && app.tune.is_some() {
        save(app, false);
    }
}

fn undo(app: &mut XbApp, redo: bool) {
    let Some(t) = &mut app.tune else { return };
    let label = if redo { t.redo() } else { t.undo() };
    if let Some(l) = label {
        app.tune_ui.status = Some((format!("{} {l}", if redo { "Redid" } else { "Undid" }), false));
    }
}

/// Save to the current file, or ask for a name. A pristine read from the
/// ECM is never overwritten: it's the backup.
fn save(app: &mut XbApp, _force_dialog: bool) {
    let Some(t) = &app.tune else { return };
    let protected = t.image.meta.source.starts_with("read from ECM") && t.image.meta.modified.is_empty();
    match (&t.path, protected) {
        (Some(p), false) => {
            let p = p.clone();
            do_save(app, &p);
        }
        _ => app.tune_ui.pick = Some(Pick::SaveAs),
    }
}

fn do_save(app: &mut XbApp, path: &Path) {
    if crate::remote_sync::save_blocked(app) {
        return app.say("The bike owner hasn't allowed saving a copy of their tune.", true);
    }
    let catalog = app.catalog.clone();
    let r = app.tune.as_mut().expect("open").save(path, &catalog);
    report(app, r.map(|_| format!("Saved {}", path.display())));
}

/// Open `path` as the tune (or as the comparison image).
pub fn open(app: &mut XbApp, path: &Path, compare: bool) {
    if compare {
        match Image::load(path, None, &app.catalog) {
            Ok(img) if app.tune.as_ref().is_some_and(|t| t.image.ecm_id != img.ecm_id) => {
                app.tune_ui.status = Some((
                    format!(
                        "{} is for a {}, not a {}",
                        path.display(),
                        img.ecm_id,
                        app.tune.as_ref().unwrap().ecm.id
                    ),
                    true,
                ));
            }
            Ok(img) => {
                app.compare = Some((path.to_path_buf(), img));
                app.tune_ui.status = Some((format!("Comparing with {}", path.display()), false));
            }
            Err(e) => app.tune_ui.status = Some((format!("{}: {e}", path.display()), true)),
        }
        return;
    }
    match Tune::open(path, &app.catalog) {
        Ok(t) => {
            app.tune = Some(t);
            app.tune_ui.sel.clear();
            app.tune_ui.status = Some((format!("Opened {}", path.display()), false));
            app.settings.last_tune = Some(path.to_path_buf());
            app.save_settings();
        }
        Err(e) => app.tune_ui.status = Some((format!("{}: {e}", path.display()), true)),
    }
}

fn file_dialog(app: &mut XbApp, ui: &mut Ui) {
    let Some(pick) = app.tune_ui.pick else { return };
    let p = Palette::for_visuals(ui.visuals());
    let title = match pick {
        Pick::Open => "Open a tune",
        Pick::Compare => "Compare with",
        Pick::SaveAs => "Save as",
    };
    if pick == Pick::SaveAs && app.tune_ui.file_name.is_empty() {
        let stem = app
            .tune
            .as_ref()
            .and_then(|t| t.path.as_ref())
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| app.tune.as_ref().map_or("tune".into(), |t| t.ecm.id.clone()));
        app.tune_ui.file_name = format!("{stem}_edited.xbt");
    }
    let mut chosen: Option<PathBuf> = None;
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("file-dialog")).show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new(title).font(theme::strong_font(18.0)));
        ui.horizontal_wrapped(|ui| {
            for (label, dir) in [("Tunes", tunes_dir()), ("Backups", backups_dir()), ("Home", home_dir())] {
                if ui.small_button(label).clicked() {
                    app.tune_ui.dir = dir;
                }
            }
            if ui.small_button("Up").clicked()
                && let Some(parent) = app.tune_ui.dir.parent()
            {
                app.tune_ui.dir = parent.to_path_buf();
            }
            ui.label(RichText::new(app.tune_ui.dir.display().to_string()).small().color(p.muted));
        });
        let mut entries: Vec<(bool, PathBuf)> = std::fs::read_dir(&app.tune_ui.dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                    .filter(|p| {
                        p.is_dir()
                            || p.extension().is_some_and(|e| {
                                e.eq_ignore_ascii_case("xbt") || e.eq_ignore_ascii_case("xpr")
                            })
                    })
                    .map(|p| (p.is_dir(), p))
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            if entries.is_empty() {
                ui.label(RichText::new("No folders or .xbt/.xpr files here.").color(p.muted));
            }
            for (is_dir, path) in &entries {
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let label = if *is_dir { format!("{name}/") } else { name.clone() };
                if ui.selectable_label(false, label).clicked() {
                    if *is_dir {
                        app.tune_ui.dir = path.clone();
                    } else if pick == Pick::SaveAs {
                        app.tune_ui.file_name = name;
                    } else {
                        chosen = Some(path.clone());
                    }
                }
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(if pick == Pick::SaveAs { "File name" } else { "Or a path" });
            ui.add(egui::TextEdit::singleline(&mut app.tune_ui.file_name).desired_width(300.0));
            let go = if pick == Pick::SaveAs { "Save" } else { "Open" };
            if ui.add_enabled(!app.tune_ui.file_name.trim().is_empty(), egui::Button::new(go)).clicked() {
                let n = PathBuf::from(app.tune_ui.file_name.trim());
                chosen = Some(if n.is_absolute() { n } else { app.tune_ui.dir.join(n) });
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
        if pick == Pick::SaveAs {
            ui.label(
                RichText::new(".xbt keeps notes and history; .xpr is EcmDroid's raw format.")
                    .small()
                    .color(p.muted),
            );
        }
    });
    if modal.should_close() {
        close = true;
    }
    if let Some(path) = chosen {
        app.tune_ui.pick = None;
        app.tune_ui.file_name.clear();
        match pick {
            Pick::SaveAs => {
                let path = if path.extension().is_none() { path.with_extension("xbt") } else { path };
                do_save(app, &path);
            }
            Pick::Compare => open(app, &path, true),
            Pick::Open if app.tune.as_ref().is_some_and(|t| t.is_modified()) => {
                app.tune_ui.confirm_discard = Some(path)
            }
            Pick::Open => open(app, &path, false),
        }
    } else if close {
        app.tune_ui.pick = None;
        app.tune_ui.file_name.clear();
    }
}

fn discard_dialog(app: &mut XbApp, ui: &mut Ui) {
    let Some(path) = app.tune_ui.confirm_discard.clone() else { return };
    let mut decided = None;
    egui::Modal::new(egui::Id::new("discard")).show(ui.ctx(), |ui| {
        ui.label(RichText::new("Unsaved changes").font(theme::strong_font(17.0)));
        ui.label("The open tune has changes that aren't saved. Discard them and open the other file?");
        ui.horizontal(|ui| {
            if ui.button("Discard and open").clicked() {
                decided = Some(true);
            }
            if ui.button("Keep editing").clicked() {
                decided = Some(false);
            }
        });
    });
    if let Some(d) = decided {
        app.tune_ui.confirm_discard = None;
        if d {
            open(app, &path, false);
        }
    }
}

/// Screenshot hook: `XBTUNE_SHOT_TUNE="select=0,0:2,3;op=add:5;3d;compare=PATH;var=NAME;mode=diff"`.
pub fn scripted(app: &mut XbApp, script: &str) {
    for step in script.split(';').map(str::trim) {
        let (k, v) = step.split_once('=').unwrap_or((step, ""));
        match k {
            "var" => app.tune_ui.selected = Some(v.to_string()),
            "select" => {
                let n: Vec<usize> = v.split([',', ':']).filter_map(|x| x.parse().ok()).collect();
                if n.len() == 4 {
                    app.tune_ui.sel.rect((n[0], n[1]), (n[2], n[3]));
                    app.tune_ui.sel.anchor = Some((n[0], n[1]));
                }
            }
            "op" => {
                let (name, x) = v.split_once(':').unwrap_or((v, "0"));
                let x: f64 = x.parse().unwrap_or(0.0);
                let op = match name {
                    "set" => Op::Set(x),
                    "add" => Op::Add(x),
                    "mul" => Op::Multiply(x),
                    "pct" => Op::Percent(x),
                    "smooth" => Op::Smooth,
                    "rows" => Op::InterpolateRows,
                    _ => Op::InterpolateCols,
                };
                let (Some(t), Some(name)) = (&app.tune, app.tune_ui.selected.clone()) else { continue };
                let var = t.def.eeprom[&name].clone();
                let Ok(el) = t.elements(&name) else { continue };
                let changes = apply_op(&as_grid(&var, &el), &app.tune_ui.sel.list(), op);
                apply_changes(app, &var, &changes, &format!("{op:?}"));
            }
            "3d" => app.tune_ui.view3d = true,
            "compare" => open(app, Path::new(v), true),
            "mode" if v == "diff" => app.tune_ui.mode = CellMode::Difference,
            _ => {}
        }
    }
}

/// Open the Burn dialog for the open tune (Tune and Setup views).
pub fn start_burn(app: &mut XbApp) {
    if let Some(t) = &app.tune {
        let mode = crate::burn_view::Mode::Tune {
            tune: Box::new(t.image.clone()),
            opened: t.opened_bytes().to_vec(),
        };
        crate::burn_view::start(app, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frac_index_both_directions() {
        let up = [800.0, 1000.0, 1350.0];
        assert_eq!(frac_index(&up, 900.0), Some(0.5));
        let down = [8000.0, 7000.0, 6000.0];
        assert_eq!(frac_index(&down, 6500.0), Some(1.5));
        assert_eq!(frac_index(&down, 9000.0), Some(0.0));
        assert_eq!(frac_index(&down, 100.0), Some(2.0));
    }
}
