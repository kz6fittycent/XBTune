// SPDX-License-Identifier: GPL-3.0-or-later
//! "Fold AFV into the maps": multiply the fuel maps by the learned AFV and
//! set AFV to 100% in the open tune, as one undoable step
//! (`xbtune_core::autotune::fold_afv`). Nothing here talks to the ECM:
//! burning the result is the normal Burn dialog, maps and AFV together.

use egui::{RichText, Ui};
use xbtune_core::autotune;

use crate::app::XbApp;
use crate::theme::{self, Palette};

#[derive(Default)]
pub struct FoldUi {
    pub open: bool,
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if !app.afv_fold.open {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let Some(tune) = &app.tune else {
        app.afv_fold.open = false;
        return;
    };
    let plan = autotune::afv_fold_plan(tune);
    let tuned = autotune::has_autotune_step(tune);
    let neutral = plan.as_ref().is_ok_and(|pl| pl.iter().all(|f| (f.afv - 100.0).abs() < 1e-9));
    let mut apply = false;
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("afv-fold")).show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new("Fold AFV into the fuel maps").font(theme::strong_font(19.0)));
        ui.label(
            RichText::new(
                "AFV is the correction the ECM has learned in closed loop. This multiplies each fuel map by \
                 its AFV and sets AFV to 100%, so the bike fuels the same but the learning starts from a \
                 corrected map. It edits the open tune only (Undo reverts it); burn the maps and AFV together \
                 afterwards.",
            )
            .color(p.muted),
        );
        ui.label(RichText::new(xbtune_core::disclaimer::SHORT).color(p.warn));
        ui.add_space(6.0);
        match &plan {
            Ok(plan) => {
                egui::Grid::new("fold-plan").num_columns(3).spacing([16.0, 4.0]).show(ui, |ui| {
                    for h in ["Map", "AFV", "Multiply by"] {
                        ui.label(RichText::new(h).strong());
                    }
                    ui.end_row();
                    for f in plan {
                        let map = tune.var(&f.map).map(|v| v.label().to_string()).unwrap_or_else(|_| f.map.clone());
                        ui.label(map);
                        let afv = tune.var(&f.afv_var).map(|v| v.label().to_string()).unwrap_or_default();
                        ui.label(format!("{:.1}% ({afv})", f.afv));
                        ui.label(format!("{:.3}", f.afv / 100.0));
                        ui.end_row();
                    }
                });
            }
            Err(e) => {
                ui.label(RichText::new(e).color(p.bad));
            }
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Use a fresh read of the ECM made after a ride and an ignition off/on: the ECM stores AFV \
                 every so often, not at every change, so an older read has an older AFV. Cells are stored \
                 at the map's resolution, so small cells may not change for a small AFV.",
            )
            .small()
            .color(p.muted),
        );
        if tuned {
            ui.label(
                RichText::new(
                    "This tune has auto-tune corrections. They already include AFV (EGO starts from it), so \
                     folding AFV in too would count it twice. Burn them and reset AFV instead (Resets), or \
                     undo the auto-tune first.",
                )
                .color(p.bad),
            );
        } else if neutral {
            ui.label("AFV is already 100%: nothing to fold.");
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ok = plan.is_ok() && !tuned && !neutral;
            let b = egui::Button::new(RichText::new("Fold into the maps").color(egui::Color32::WHITE)).fill(p.accent);
            if ui.add_enabled(ok, b).clicked() {
                apply = true;
            }
            if ui.button("Close").clicked() {
                close = true;
            }
        });
    });
    if apply {
        let plan = plan.expect("enabled only with a plan");
        if let Some(t) = &mut app.tune {
            match autotune::fold_afv(t, &plan) {
                Ok(a) => {
                    tracing::info!(cells = a.changed, clamped = a.clamped, "AFV folded into the fuel maps");
                    let mut msg = format!(
                        "AFV folded into the fuel maps ({} value(s) changed{}). Nothing was written to the ECM: \
                         burn the maps and AFV together.",
                        a.changed,
                        if a.clamped > 0 {
                            format!(", {} at the map's limit", a.clamped)
                        } else {
                            String::new()
                        }
                    );
                    msg.push_str(" Undo reverts it.");
                    app.say(msg, a.clamped > 0);
                    app.tune_ui.selected = plan.first().map(|f| f.map.clone());
                    app.afv_fold.open = false;
                }
                Err(e) => app.say(format!("Couldn't fold AFV into the maps: {e}"), true),
            }
        }
    } else if close || modal.should_close() {
        app.afv_fold.open = false;
    }
}
