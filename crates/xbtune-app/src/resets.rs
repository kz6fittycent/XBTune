// SPDX-License-Identifier: GPL-3.0-or-later
//! The Resets view: TPS reset and AFV reset, the two jobs done most often
//! after work on the bike. TPS reset is an ECM command (confirm dialog and
//! checks in `crate::diagnostics` / `xbtune_core::diag`); AFV reset is an
//! EEPROM write through the burn dialog (backup, review, write, read back).

use egui::{RichText, Ui};
use xbtune_core::diag;
use xbtune_core::live::{format_value, units};
use xbtune_proto::Function;

use crate::app::XbApp;
use crate::theme::{self, Palette};

/// A live channel as "4.2 °", or "–" without data.
fn live(app: &XbApp, name: &str) -> String {
    let Some(v) = app.link.channel(name) else { return "–".into() };
    match app.history.latest().and_then(|s| s.value(v)) {
        Some(x) => format!("{} {}", format_value(v, x), units(v)).trim().to_string(),
        None => "–".into(),
    }
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let Some(id) = app.link.identity.clone() else {
        crate::table::not_connected(app, ui);
        return;
    };
    let p = Palette::for_visuals(ui.visuals());
    let busy = app.diag.running.is_some();
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Resets");
        ui.label(
            RichText::new(
                "Engine OFF, ignition ON, kill switch RUN for both. Turn the ignition off and on afterwards.",
            )
            .color(p.muted),
        );
        ui.add_space(6.0);
        crate::diagnostics::status(app, ui, &p);
        ui.add_space(6.0);
        crate::diagnostics::demo_engine_card(app, ui, &p);

        if !app.session_permitted() || !app.writes_permitted() {
            let what = match (app.session_permitted(), app.writes_permitted()) {
                (false, false) => "TPS reset (an ECM command) and AFV reset (a write to the ECM) are",
                (false, true) => "TPS reset (an ECM command) is",
                _ => "AFV reset (a write to the ECM) is",
            };
            crate::diagnostics::locked_card(
                app,
                ui,
                "Locked on a real bike",
                &format!("{what} locked until you unlock them in Settings."),
            );
        }

        // TPS
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("TPS reset").font(theme::strong_font(17.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("Now: {} · {}", live(app, "TPD"), live(app, "TPS Voltg.")))
                            .color(p.muted),
                    );
                });
            });
            ui.label(
                "Teaches the ECM where the throttle is fully closed. Do it after work on the throttle body, \
                 the TPS or the idle screw, or when idle or tip-in feels off.",
            );
            ui.add_space(4.0);
            if diag::tps_reset_supported(&id.ecm) {
                ui.label(RichText::new(diag::safety_notice(Function::TpsReset)).color(p.muted));
                ui.add_space(4.0);
                let can = app.link.live() && app.session_permitted() && !busy;
                if ui.add_enabled(can, egui::Button::new("TPS reset…")).clicked() {
                    app.diag.confirm = Some(Function::TpsReset);
                    app.diag.safe = false;
                }
            } else {
                ui.label(format!(
                    "{} is a {} ECM: follow the service manual for its TPS reset.",
                    id.ecm.id, id.ecm.family
                ));
            }
        });
        ui.add_space(8.0);

        // AFV
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("AFV reset").font(theme::strong_font(17.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("Now: {}", live(app, "AFV"))).color(p.muted));
                });
            });
            ui.label(
                "Sets the adaptive fuel value back to 100%, so the ECM learns again from scratch. Do it \
                 after changing the fuel maps, the exhaust or the intake.",
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Goes through the same checks as a burn: a verified backup first, then you review and \
                     confirm, then it's written and read back.",
                )
                .color(p.muted),
            );
            ui.add_space(4.0);
            let can = app.link.live() && app.writes_permitted() && !app.burn.is_open() && !busy;
            if ui.add_enabled(can, egui::Button::new("AFV reset…")).clicked() {
                crate::burn_view::start(app, crate::burn_view::Mode::Afv);
            }
        });
    });
    crate::diagnostics::confirm_dialog(app, ui);
}
