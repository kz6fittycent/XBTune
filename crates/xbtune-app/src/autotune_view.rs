// SPDX-License-Identifier: GPL-3.0-or-later
//! The auto-tune dialog (KICKOFF §5 "Tuning") and the log overlays on the
//! fuel maps. The math is `xbtune_core::autotune`; this lets the owner set
//! the filters, review every proposed cell, untick any, and apply the rest
//! to the open tune as one undoable step. Nothing here talks to the ECM:
//! burning the result is the normal Burn dialog.

use std::sync::Arc;

use egui::{RichText, Ui};
use xbtune_core::autotune::{self, Analysis, Filters, Proposal, Settings};
use xbtune_core::logfile::Log;

use crate::app::XbApp;
use crate::settings::{AutotuneState, WbUnit};
use crate::theme::{self, Palette};

/// What the fuel-map overlay shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overlay {
    #[default]
    Off,
    Hits,
    Ego,
    /// Average wideband AFR (wideband logs only).
    Afr,
}

impl Overlay {
    /// The name saved in a bike state.
    pub fn key(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Hits => "hits",
            Self::Ego => "ego",
            Self::Afr => "afr",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "hits" => Self::Hits,
            "ego" => Self::Ego,
            "afr" => Self::Afr,
            _ => Self::Off,
        }
    }
}

#[derive(Default)]
pub struct AutotuneUi {
    pub open: bool,
    pub settings: Settings,
    pub front: bool,
    pub rear: bool,
    /// Proposals and whether each is ticked; `None` until analysed.
    proposals: Option<Vec<(Proposal, bool)>>,
    summary: Vec<String>,
    afv: Option<f64>,
    pub overlay: Overlay,
    /// (map, logs generation, filters) -> analysis, for the overlay.
    cache: Vec<(String, u64, Filters, Arc<Analysis>)>,
    /// Wideband: correct toward a target AFR (needs wideband logs).
    pub wideband: bool,
    /// The targets as typed ("load:AFR,..." or "load:lambda,...") in
    /// `target_unit`, and why they don't parse.
    target_text: String,
    target_unit: WbUnit,
    target_error: Option<String>,
    lag: f64,
    /// Wideband: cells measured lean at high load, as text.
    lean: Vec<String>,
}

impl AutotuneUi {
    /// The choices to keep with the bike.
    pub fn state(&self) -> AutotuneState {
        let d = Settings::default();
        let set =
            self.target_text != autotune::TargetAfr::default().text() || self.target_unit != WbUnit::Afr;
        AutotuneState {
            front: (self.front || self.rear).then_some(self.front),
            rear: (self.front || self.rear).then_some(self.rear),
            targets: if set { self.target_text.clone() } else { String::new() },
            target_unit: self.target_unit,
            lag: if self.lag != autotune::DEFAULT_LAG { self.lag } else { 0.0 },
            min_hits: if self.settings.min_hits != d.min_hits { self.settings.min_hits } else { 0.0 },
            max_change: if self.settings.max_change != d.max_change { self.settings.max_change } else { 0.0 },
            min_clt: if self.settings.filters.min_clt != d.filters.min_clt {
                self.settings.filters.min_clt
            } else {
                0.0
            },
        }
    }

    /// Put saved choices back (zero / empty: XBTune's defaults).
    pub fn restore(&mut self, s: &AutotuneState) {
        let d = Settings::default();
        self.front = s.front.unwrap_or(true);
        self.rear = s.rear.unwrap_or(true);
        if s.targets.is_empty() {
            self.target_text = autotune::TargetAfr::default().text();
            self.target_unit = WbUnit::Afr;
        } else {
            self.target_text = s.targets.clone();
            self.target_unit = s.target_unit;
        }
        self.lag = if s.lag > 0.0 { s.lag } else { autotune::DEFAULT_LAG };
        self.settings.min_hits = if s.min_hits > 0.0 { s.min_hits } else { d.min_hits };
        self.settings.max_change = if s.max_change > 0.0 { s.max_change } else { d.max_change };
        self.settings.filters.min_clt = if s.min_clt > 0.0 { s.min_clt } else { d.filters.min_clt };
    }
}

/// Whether every open log has wideband readings.
pub fn logs_have_wideband(app: &XbApp) -> bool {
    wideband_missing(app).is_none()
}

/// Why the wideband options are off, or `None` when every open log has
/// wideband readings. A tune file holds no AFR: it comes from logs.
pub fn wideband_missing(app: &XbApp) -> Option<String> {
    let logs = &app.logs.logs;
    let without: Vec<String> = logs
        .iter()
        .filter(|l| l.log.wideband.is_none())
        .map(|l| l.log.path.file_name().unwrap_or_default().to_string_lossy().into_owned())
        .collect();
    if logs.is_empty() {
        Some("Needs a log recorded with a wideband connected: open one in Logs".into())
    } else if without.len() == logs.len() {
        Some(format!(
            "The open log{} {} no wideband readings. Record with Connect > Wideband O2 set, then open that log",
            if logs.len() == 1 { "" } else { "s" },
            if logs.len() == 1 { "has" } else { "have" },
        ))
    } else if !without.is_empty() {
        Some(format!(
            "{} of {} open logs have no wideband readings ({}): close them in Logs",
            without.len(),
            logs.len(),
            without.join(", ")
        ))
    } else {
        None
    }
}

/// The filters to use: the dialog's, in wideband mode when chosen (or
/// for the AFR overlay).
fn filters(app: &XbApp, wideband: bool) -> Filters {
    let a = &app.autotune;
    let mut f = a.settings.filters.clone();
    f.mode = if wideband && logs_have_wideband(app) {
        let target = autotune::TargetAfr::parse(&a.target_text).unwrap_or_default();
        autotune::Mode::Wideband { target, lag: if a.lag > 0.0 { a.lag } else { autotune::DEFAULT_LAG } }
    } else {
        autotune::Mode::Narrowband
    };
    f
}

pub fn open(app: &mut XbApp) {
    let have_wb = logs_have_wideband(app);
    let a = &mut app.autotune;
    if !a.open {
        a.open = true;
        a.proposals = None;
        a.summary.clear();
        if !a.front && !a.rear {
            a.front = true;
            a.rear = true;
        }
        if a.target_text.is_empty() {
            a.target_text = autotune::TargetAfr::default().text();
            a.target_unit = WbUnit::Afr;
            a.lag = autotune::DEFAULT_LAG;
        }
        a.wideband = have_wb;
    }
    let unit = app.settings.bike.wb_unit;
    retarget(&mut app.autotune, unit);
}

/// The user switched AFR/lambda (here, in Connect or in Settings).
pub fn unit_changed(app: &mut XbApp) {
    let unit = app.settings.bike.wb_unit;
    retarget(&mut app.autotune, unit);
    app.autotune.target_error = autotune::TargetAfr::parse(&app.autotune.target_text).err();
}

/// Rewrite the typed targets in `unit` (the user switched AFR/lambda).
fn retarget(a: &mut AutotuneUi, unit: WbUnit) {
    if a.target_unit == unit {
        return;
    }
    if let Ok(t) = autotune::TargetAfr::parse(&a.target_text) {
        a.target_text = match unit {
            WbUnit::Afr => t.text(),
            WbUnit::Lambda => t.lambda_text(),
        };
    }
    a.target_unit = unit;
}

fn logs(app: &XbApp) -> Vec<Log> {
    app.logs.logs.iter().map(|l| (*l.log).clone()).collect()
}

/// The analysis of the loaded logs on `map`, cached for the overlay.
pub fn analysis(app: &mut XbApp, map: &str) -> Option<Arc<Analysis>> {
    if app.logs.logs.is_empty() {
        return None;
    }
    let generation = app.logs.generation;
    let f = filters(app, app.autotune.wideband || app.autotune.overlay == Overlay::Afr);
    if let Some((_, _, _, a)) =
        app.autotune.cache.iter().find(|(m, g, ff, _)| m == map && *g == generation && *ff == f)
    {
        return Some(a.clone());
    }
    let tune = app.tune.as_ref()?;
    let a = Arc::new(autotune::analyze(&logs(app), tune, map, &f).ok()?);
    app.autotune.cache.retain(|(m, g, _, _)| !(m == map) && *g == generation);
    app.autotune.cache.push((map.to_string(), generation, f, a.clone()));
    Some(a)
}

/// UI checks: analyse straight away.
pub fn analyse_now(app: &mut XbApp) {
    run(app);
}

fn run(app: &mut XbApp) {
    let unit = app.settings.bike.wb_unit;
    let mut s = app.autotune.settings.clone();
    s.filters = filters(app, app.autotune.wideband);
    let Some(tune) = &app.tune else { return };
    let logs = logs(app);
    let mut lean = Vec::new();
    let maps: Vec<String> = autotune::fuel_maps(tune)
        .into_iter()
        .filter(|m| (app.autotune.front && m.contains("Front")) || (app.autotune.rear && m.contains("Rear")))
        .collect();
    let mut all = Vec::new();
    let mut summary = Vec::new();
    let mut afv = None;
    for map in &maps {
        match autotune::analyze(&logs, tune, map, &s.filters).and_then(|a| {
            let p = autotune::propose(&a, tune, &s)?;
            Ok((a, p))
        }) {
            Ok((a, p)) => {
                afv = afv.or(a.afv);
                let r = &a.rejected;
                let label = tune.var(map).map(|v| v.label().to_string()).unwrap_or_else(|_| map.clone());
                let first = if a.wideband {
                    format!("{} no wideband reading", r.no_wideband)
                } else {
                    format!("{} open loop", r.open_loop)
                };
                summary.push(format!(
                    "{label}: {} of {} samples used ({first}, {} cold, {} changing, {} enrichment, {} after a gap); {} cell(s) to change",
                    a.used,
                    a.samples,
                    r.cold,
                    r.transient,
                    r.enrichment,
                    r.gap,
                    p.len()
                ));
                if !a.missing_channels.is_empty() {
                    summary.push(format!(
                        "{label}: a log has no {} channel, so its samples can't be used (auto-tune needs RPM, \
                         Load and EGO Corr.)",
                        a.missing_channels.join(", ")
                    ));
                }
                let v = tune.var(map).ok();
                let axis = |n: Option<&String>| n.and_then(|n| tune.elements(n).ok()).unwrap_or_default();
                let (ys, xs) =
                    (axis(v.and_then(|v| v.y_axis.as_ref())), axis(v.and_then(|v| v.x_axis.as_ref())));
                for &(r, c) in &a.lean {
                    lean.push(format!(
                        "{label}: load {} at {} rpm averaged {} {}",
                        ys.get(r).map_or("?".into(), |x| x.to_string()),
                        xs.get(c).map_or("?".into(), |x| x.to_string()),
                        unit.short(),
                        unit.fmt(a.cells[r][c].afr)
                    ));
                }
                all.extend(p.into_iter().map(|p| (p, true)));
            }
            Err(e) => summary.push(format!("{map}: {e}")),
        }
    }
    tracing::info!(cells = all.len(), logs = logs.len(), "auto-tune analysed");
    app.autotune.proposals = Some(all);
    app.autotune.summary = summary;
    app.autotune.afv = afv;
    app.autotune.lean = lean;
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if !app.autotune.open {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let mut analyse = false;
    let mut apply = false;
    let mut close = false;
    let logs_n = app.logs.logs.len();
    let tune_id = app.tune.as_ref().map(|t| t.ecm.id.clone());
    let wrong_ecm: Vec<String> = app
        .logs
        .logs
        .iter()
        .filter(|l| Some(&l.log.ecm.id) != tune_id.as_ref())
        .map(|l| l.log.name())
        .collect();
    let axes = |t: &Option<xbtune_core::tune::Tune>, map: &str| {
        let t = t.as_ref()?;
        let v = t.var(map).ok()?;
        Some((t.elements(v.y_axis.as_ref()?).ok()?, t.elements(v.x_axis.as_ref()?).ok()?))
    };
    let front_axes = axes(&app.tune, "Tab_Fuel_Front");
    let rear_axes = axes(&app.tune, "Tab_Fuel_Rear");
    let wb_why = wideband_missing(app);
    let have_wb = wb_why.is_none();
    let unit = app.settings.bike.wb_unit;
    let mut new_unit = unit;
    // The typed targets follow the unit, however it was changed.
    if app.autotune.target_unit != unit {
        unit_changed(app);
    }
    let modal = egui::Modal::new(egui::Id::new("autotune")).show(ui.ctx(), |ui| {
        ui.set_width(640.0);
        // Taller than a small window once proposals are listed: scroll.
        let max_h = (ui.ctx().content_rect().height() - 80.0).max(200.0);
        egui::ScrollArea::vertical().max_height(max_h).min_scrolled_height(max_h).show(ui, |ui| {
            let a = &mut app.autotune;
            a.wideband &= have_wb;
            let title = if a.wideband {
                format!("Auto-tune (wideband, target {})", unit.word())
            } else {
                "Auto-tune (EGO, narrowband)".into()
            };
            ui.label(RichText::new(title).font(theme::strong_font(19.0)));
            ui.horizontal(|ui| {
                ui.radio_value(&mut a.wideband, false, "Narrowband (the ECM's EGO correction)");
                ui.add_enabled_ui(have_wb, |ui| {
                    ui.radio_value(&mut a.wideband, true, format!("Wideband (target {})", unit.word()))
                        .on_disabled_hover_text(wb_why.clone().unwrap_or_default());
                });
            });
            let how = if a.wideband {
                "Each cell is corrected so the measured mixture would reach the target for its load \
                 (new = old × EGO ÷ 100 × measured λ ÷ target λ), in open and closed loop. Only cells with \
                 enough steady, warm data change. In closed loop the ECM still aims for stoichiometric \
                 (AFR 14.7, λ 1.00), so keep light-load targets there. This edits the open tune only; \
                 burning it is a separate step."
            } else {
                "Where the ECM had to trim the fuel in closed loop, the map is corrected by the same amount \
                 (new = old × EGO ÷ 100). Only cells with enough steady, warm, closed-loop data change. \
                 This edits the open tune only; burning it is a separate step."
            };
            ui.label(RichText::new(how).color(p.muted));
            ui.label(RichText::new(xbtune_core::disclaimer::SHORT).color(p.warn));
            ui.add_space(4.0);
            ui.label(format!("{logs_n} log(s) open"));
            if !wrong_ecm.is_empty() {
                ui.label(
                    RichText::new(format!("These logs are from a different ECM than the tune: {}", wrong_ecm.join(", ")))
                        .color(p.bad),
                );
            }
            ui.add_space(4.0);
            egui::Grid::new("at-settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                let f = &mut a.settings.filters;
                ui.label("Engine at least");
                ui.add(egui::DragValue::new(&mut f.min_clt).range(0.0..=200.0).suffix(" °C"));
                ui.end_row();
                ui.label("Samples per cell at least");
                ui.add(egui::DragValue::new(&mut a.settings.min_hits).range(1.0..=10_000.0))
                    .on_hover_text("About 6.5 per second spent in a cell");
                ui.end_row();
                ui.label("Change a cell by at most");
                let mut pct = a.settings.max_change * 100.0;
                if ui.add(egui::DragValue::new(&mut pct).range(1.0..=30.0).suffix(" %")).changed() {
                    a.settings.max_change = pct / 100.0;
                }
                ui.end_row();
                ui.label("Maps");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut a.front, "Front");
                    ui.checkbox(&mut a.rear, "Rear");
                });
                ui.end_row();
                if a.wideband {
                    ui.label("Show and type as");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut new_unit, WbUnit::Afr, "AFR");
                        ui.radio_value(&mut new_unit, WbUnit::Lambda, "Lambda (λ)");
                    });
                    ui.end_row();
                    ui.label(format!("Target {} by load", unit.word()));
                    if ui
                        .add(egui::TextEdit::singleline(&mut a.target_text).desired_width(320.0))
                        .on_hover_text(format!(
                            "load:{} pairs; straight lines between them, flat beyond the ends",
                            unit.word()
                        ))
                        .changed()
                    {
                        a.target_error = autotune::TargetAfr::parse(&a.target_text).err();
                    }
                    ui.end_row();
                    ui.label("Sensor delay");
                    ui.add(egui::DragValue::new(&mut a.lag).range(0.0..=1.0).speed(0.01).suffix(" s"))
                        .on_hover_text("Time from a sample to the wideband reading that matches it (exhaust travel)");
                    ui.end_row();
                }
            });
            if a.wideband && let Some(e) = &a.target_error {
                ui.colored_label(p.bad, e);
            }
            ui.label(
                RichText::new(
                    "The XB has one O2 sensor (rear cylinder); the ECM applies its EGO correction to both \
                     cylinders, so both maps get the same correction by default.",
                )
                .small()
                .color(p.muted),
            );
            ui.add_space(6.0);
            let target_ok = !a.wideband || a.target_error.is_none();
            if ui.add_enabled(wrong_ecm.is_empty() && logs_n > 0 && target_ok, egui::Button::new("Analyse")).clicked() {
                analyse = true;
            }
            if let Some(props) = &mut a.proposals {
                ui.separator();
                for s in &a.summary {
                    ui.label(RichText::new(s).small());
                }
                if props.is_empty() {
                    ui.label("No cell has enough data and a change to make. Ride longer in steady conditions, or lower the minimum samples.");
                } else {
                    ui.horizontal(|ui| {
                        if ui.small_button("Tick all").clicked() {
                            props.iter_mut().for_each(|(_, on)| *on = true);
                        }
                        if ui.small_button("Untick all").clicked() {
                            props.iter_mut().for_each(|(_, on)| *on = false);
                        }
                    });
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        egui::Grid::new("at-props").num_columns(7).striped(true).spacing([12.0, 3.0]).show(ui, |ui| {
                            let why = if props.first().is_some_and(|(x, _)| x.afr.is_some()) {
                                if unit == WbUnit::Lambda { "λ / target" } else { "AFR / target" }
                            } else {
                                "EGO"
                            };
                            for h in ["", "Map", "Load", "RPM", "Change", why, "Samples"] {
                                ui.label(RichText::new(h).strong());
                            }
                            ui.end_row();
                            for (x, on) in props.iter_mut() {
                                ui.checkbox(on, "");
                                let front = x.map.contains("Front");
                                ui.label(if front { "Front" } else { "Rear" });
                                let ax = if front { &front_axes } else { &rear_axes };
                                let at = |v: Option<&Vec<f64>>, i: usize| {
                                    v.and_then(|v| v.get(i)).map_or("?".into(), |v| format!("{v}"))
                                };
                                ui.label(at(ax.as_ref().map(|a| &a.0), x.row));
                                ui.label(at(ax.as_ref().map(|a| &a.1), x.col));
                                let pct = (x.new / x.old - 1.0) * 100.0;
                                let txt = format!("{:.1} -> {:.1} ({pct:+.1}%){}", x.old, x.new, if x.limited { " capped" } else { "" });
                                ui.label(RichText::new(txt).color(if pct < 0.0 { p.good } else { p.warn }));
                                match x.afr {
                                    Some((afr, target)) => ui.label(format!("{} / {}", unit.fmt(afr), unit.fmt(target))),
                                    None => ui.label(format!("{:.1}%", x.ego)),
                                };
                                ui.label(format!("{:.0}", x.hits));
                                ui.end_row();
                            }
                        });
                    });
                }
                for l in &a.lean {
                    ui.colored_label(p.bad, format!("Lean at high load: {l}. Check this before riding hard."));
                }
                if let Some(v) = a.afv {
                    ui.label(
                        RichText::new(format!(
                            "AFV in the logs: {v:.1}%. These corrections already include it, so after burning \
                             them, reset AFV (Resets) so it isn't counted twice."
                        ))
                        .color(p.warn),
                    );
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let n = a.proposals.as_ref().map_or(0, |v| v.iter().filter(|(_, on)| *on).count());
                let b = theme::primary(format!("Apply {n} change(s) to the tune"));
                if ui.add_enabled(n > 0, b).clicked() {
                    apply = true;
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    });
    if new_unit != unit {
        app.set_wb_unit(new_unit);
    }
    if analyse {
        run(app);
    }
    if apply {
        let chosen: Vec<Proposal> =
            app.autotune.proposals.iter().flatten().filter(|(_, on)| *on).map(|(p, _)| p.clone()).collect();
        if let Some(t) = &mut app.tune {
            match autotune::apply(t, &chosen) {
                Ok(n) => {
                    tracing::info!(cells = n, "auto-tune applied");
                    app.say(
                        format!("Auto-tune changed {n} cell(s) in the tune (Undo reverts it). Nothing was written to the ECM."),
                        false,
                    );
                    if let Some(first) = chosen.first() {
                        app.tune_ui.selected = Some(first.map.clone());
                    }
                    app.autotune.open = false;
                }
                Err(e) => app.say(format!("Auto-tune couldn't change the tune: {e}"), true),
            }
        }
    } else if close || modal.should_close() {
        app.autotune.open = false;
    }
}
