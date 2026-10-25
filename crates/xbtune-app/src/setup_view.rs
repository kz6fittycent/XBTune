// SPDX-License-Identifier: GPL-3.0-or-later
//! The Setup view: EcmDroid's named ECM settings (`xbtune_core::setup`) for
//! the open tune, grouped as on EcmDroid's Setup screen. Switches and
//! numbers edit the tune (one undo step each); Burn writes them to the ECM
//! through the normal review. The airbox pressure group also shows what the
//! Baro ADC reads live (255: no sensor fitted).

use egui::{RichText, Ui};
use xbtune_core::logmode;
use xbtune_core::setup::{self, Group, State};

use crate::app::{View, XbApp};
use crate::theme::{self, Palette};

/// The Baro ADC reads this or more with nothing connected (the owner's
/// XB9SX logs: 254-255 throughout, 2026-10-14/15).
pub const BARO_NO_SENSOR: f64 = 254.0;

#[derive(Default)]
pub struct SetupUi {
    show_missing: bool,
    /// A number being typed: (variable, text).
    editing: Option<(String, String)>,
}

enum Edit {
    Switch(setup::Switch, bool),
    Number(String, f64),
    LogModeOn,
    LogModeOff,
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    let Some(tune) = &app.tune else {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No tune open").font(theme::strong_font(20.0)));
            ui.label(
                RichText::new(
                    "Setup shows the ECM's settings from a tune: read the ECM or open a backup in Tune.",
                )
                .color(p.muted),
            );
            ui.add_space(10.0);
            if ui.button("Go to Tune").clicked() {
                app.view = View::Tune;
            }
        });
        return;
    };
    let groups = setup::screen(tune);
    let saved = |var: &str| tune.saved_elements(var).ok().and_then(|v| v.first().copied());
    let now = |var: &str| tune.elements(var).ok().and_then(|v| v.first().copied());
    let baro =
        app.link.channel("Baro ADC").cloned().and_then(|v| app.history.latest().and_then(|s| s.value(&v)));
    let live = app.link.live();
    let permitted = app.writes_permitted();
    let can_burn = live && permitted && !app.burn.is_open();
    let file = tune.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
    let title = format!("{} · {}", tune.ecm.id, file.unwrap_or_else(|| "unsaved tune".into()));
    let mut edit: Option<Edit> = None;
    let mut burn = false;
    let mut show_missing = app.setup_ui.show_missing;
    let log_mode = logmode::changes(tune);
    let restore = app
        .settings
        .bike
        .log_mode
        .as_ref()
        .and_then(|s| logmode::restore_changes(tune, s).ok())
        .filter(|c| !c.is_empty());
    ui.horizontal(|ui| {
        ui.heading("Setup");
        ui.label(RichText::new(title).color(p.muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let b = egui::Button::new(RichText::new("Burn to ECM…").color(if can_burn {
                p.bad
            } else {
                p.muted
            }));
            let r = ui.add_enabled(can_burn, b);
            let r = if permitted {
                r.on_disabled_hover_text("Connect to the ECM first")
            } else {
                r.on_disabled_hover_text(xbtune_core::diag::write_locked_message())
            };
            if r.clicked() {
                burn = true;
            }
            ui.checkbox(&mut show_missing, "Show settings this ECM doesn't have");
        });
    });
    app.setup_ui.show_missing = show_missing;
    ui.label(
        RichText::new(
            "EcmDroid's Setup screen. Changes edit the open tune (Undo works); Burn writes them to the ECM after the \
             usual backup and review.",
        )
        .color(p.muted),
    );
    ui.add_space(6.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if let Some(e) = log_mode_card(ui, &p, &log_mode, restore.as_deref()) {
            edit = Some(e);
        }
        let mut top = String::new();
        for g in &groups {
            let shown: Vec<_> =
                g.items.iter().filter(|i| app.setup_ui.show_missing || i.state != State::Missing).collect();
            if shown.is_empty() {
                continue;
            }
            if g.path.first() != Some(&top) {
                top = g.path.first().cloned().unwrap_or_default();
                ui.add_space(6.0);
                ui.label(RichText::new(&top).font(theme::strong_font(18.0)));
            }
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let sub = g.path.iter().skip(1).cloned().collect::<Vec<_>>().join(" · ");
                if !sub.is_empty() {
                    ui.label(RichText::new(sub).font(theme::strong_font(15.0)));
                }
                if is_airbox(g) {
                    baro_note(ui, &p, baro, live);
                }
                egui::Grid::new(("setup", g.path.join("/"))).num_columns(3).spacing([16.0, 4.0]).show(
                    ui,
                    |ui| {
                        for i in shown {
                            let tip = i.keys.join(" or ");
                            match &i.state {
                                State::Switch { switch, on } => {
                                    let mut v = *on;
                                    let changed = saved(&switch.var) != now(&switch.var);
                                    let mut text = RichText::new(&i.title);
                                    if changed {
                                        text = text.color(p.accent);
                                    }
                                    if ui.checkbox(&mut v, text).on_hover_text(&tip).changed() {
                                        edit = Some(Edit::Switch(switch.clone(), v));
                                    }
                                    ui.label("");
                                }
                                State::Odd { .. } => {
                                    ui.label(RichText::new(&i.title).color(p.muted)).on_hover_text(&tip);
                                    ui.label(
                                        RichText::new("bits disagree: edit in Tune").small().color(p.warn),
                                    );
                                }
                                State::Number { var, value, units, decimals } => {
                                    let changed = saved(var).is_some_and(|s| (s - *value).abs() > 1e-9);
                                    let mut text = RichText::new(&i.title);
                                    if changed {
                                        text = text.color(p.accent);
                                    }
                                    ui.label(text).on_hover_text(&tip);
                                    let shown = xbtune_defs::format_number(*value, *decimals);
                                    let editing =
                                        app.setup_ui.editing.as_ref().is_some_and(|(v, _)| v == var);
                                    let mut s = if editing {
                                        app.setup_ui
                                            .editing
                                            .as_ref()
                                            .map(|(_, t)| t.clone())
                                            .unwrap_or_default()
                                    } else {
                                        shown.clone()
                                    };
                                    // Typed text is kept while the field has focus;
                                    // the value changes when it loses it (one undo step).
                                    let r = ui.add(egui::TextEdit::singleline(&mut s).desired_width(90.0));
                                    if r.has_focus() {
                                        app.setup_ui.editing = Some((var.clone(), s.clone()));
                                    }
                                    if r.lost_focus() {
                                        app.setup_ui.editing = None;
                                        if let Ok(x) = s.trim().parse::<f64>()
                                            && (x - value).abs() > 1e-9
                                        {
                                            edit = Some(Edit::Number(var.clone(), x));
                                        }
                                    }
                                    ui.label(
                                        RichText::new(units.as_deref().unwrap_or_default())
                                            .small()
                                            .color(p.muted),
                                    );
                                }
                                State::Missing => {
                                    ui.label(RichText::new(&i.title).color(p.muted)).on_hover_text(&tip);
                                    ui.label(RichText::new("not on this ECM").small().color(p.muted));
                                }
                            }
                            ui.end_row();
                        }
                    },
                );
            });
        }
    });
    if burn {
        crate::tune_view::start_burn(app);
    }
    let Some(e) = edit else { return };
    let Some(t) = &mut app.tune else { return };
    let r = match e {
        Edit::Switch(s, on) => setup::set_switch(t, &s, on),
        Edit::Number(var, x) => setup::set_number(t, &var, x),
        Edit::LogModeOn => {
            match logmode::switch_on(t, app.settings.bike.log_mode.as_ref()) {
                Ok(saved) => {
                    tracing::info!(?saved, "wideband logging mode set in the tune");
                    app.settings.bike.log_mode = Some(saved);
                    app.save_settings();
                    app.say(
                        "Wideband logging mode is set in the tune (Undo reverts it). Burn to write it to the ECM;                          the values it replaced are kept for Restore.",
                        false,
                    );
                }
                Err(e) => app.say(e, true),
            }
            return;
        }
        Edit::LogModeOff => {
            let Some(saved) = app.settings.bike.log_mode.clone() else { return };
            match logmode::restore(t, &saved) {
                Ok(c) => {
                    tracing::info!(changes = c.len(), "normal mode restored in the tune");
                    app.say(
                        "Closed loop and AFV limits are back as they were, AFV stays at 100% (the maps changed). \
                         Burn to write it to the ECM.",
                        false,
                    );
                }
                Err(e) => app.say(e, true),
            }
            return;
        }
    };
    match r {
        Ok(a) if a.clamped > 0 => {
            app.say("That value is outside what the ECM can store; the nearest limit was used.", true)
        }
        Ok(_) => {}
        Err(e) => app.say(format!("Couldn't change the setting: {e}"), true),
    }
}

/// Wideband logging mode (`xbtune_core::logmode`): closed loop and AFV
/// locked, so the ECM doesn't correct what a wideband log should capture.
fn log_mode_card(
    ui: &mut Ui,
    p: &Palette,
    plan: &Result<Vec<logmode::Change>, String>,
    restore: Option<&[logmode::Change]>,
) -> Option<Edit> {
    let mut out = None;
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("Wideband logging mode").font(theme::strong_font(17.0)));
            let (text, color) = match plan {
                Ok(c) if c.is_empty() => ("on", p.warn),
                Ok(_) => ("off", p.muted),
                Err(_) => ("not available", p.muted),
            };
            ui.label(RichText::new(text).color(color).strong());
        });
        ui.label(
            RichText::new(
                "For logging with a wideband the way it's done in ECMSpy: closed loop off (EGO correction \
                 locked at 100%) and AFV locked at 100%, so the ECM doesn't correct the mixture you're \
                 measuring. XBTune's wideband auto-tune also works without it: it takes EGO out of the result.",
            )
            .color(p.muted),
        );
        match plan {
            Err(e) => {
                ui.label(RichText::new(e).color(p.muted));
            }
            Ok(changes) if changes.is_empty() => {
                ui.label(
                    "The tune has closed loop and AFV locked. After burning it, log your ride: EGO Corr. should \
                     read 100% throughout. Put the normal settings back when you're done tuning.",
                );
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(restore.is_some(), egui::Button::new("Restore normal mode"));
                    let r = r.on_disabled_hover_text(
                        "No saved values for this ECM: set closed loop and the AFV limits by hand below.",
                    );
                    if let Some(c) = restore {
                        let lines: Vec<String> = c.iter().map(logmode::Change::line).collect();
                        if r.on_hover_text(lines.join("\n")).clicked() {
                            out = Some(Edit::LogModeOff);
                        }
                    }
                });
            }
            Ok(changes) => {
                for c in changes {
                    ui.label(RichText::new(format!("•  {}", c.line())).small());
                }
                ui.horizontal(|ui| {
                    if ui.button("Switch on in the tune").clicked() {
                        out = Some(Edit::LogModeOn);
                    }
                    if let Some(c) = restore {
                        let lines: Vec<String> = c.iter().map(logmode::Change::line).collect();
                        if ui.button("Restore normal mode").on_hover_text(lines.join("\n")).clicked() {
                            out = Some(Edit::LogModeOff);
                        }
                    }
                });
            }
        }
    });
    ui.add_space(6.0);
    out
}

fn is_airbox(g: &Group) -> bool {
    g.path.last().is_some_and(|p| p == "Airbox Pressure Sensor")
}

/// What the Baro ADC says live, if connected.
fn baro_note(ui: &mut Ui, p: &Palette, baro: Option<f64>, live: bool) {
    let text = match (baro, live) {
        (Some(v), true) if v >= BARO_NO_SENSOR => {
            format!(
                "Live: the Baro ADC reads {v:.0}, which means no baro sensor is fitted (the input floats high)."
            )
        }
        (Some(v), true) => format!("Live: the Baro ADC reads {v:.0}."),
        _ => "Connect to the ECM to see whether a baro sensor is fitted (Baro ADC 255 = none).".into(),
    };
    ui.label(RichText::new(text).small().color(p.muted));
}
