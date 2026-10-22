// SPDX-License-Identifier: GPL-3.0-or-later
//! The engine-damage warning (`xbtune_core::disclaimer`): shown at the first
//! start, and again when its version goes up, until the user accepts it.
//! Nothing else can be used meanwhile; "Quit" closes the app.

use egui::{RichText, Ui};
use xbtune_core::disclaimer;

use crate::app::XbApp;
use crate::theme::{self, Palette};

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if !app.disclaimer_open {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let mut accept = false;
    let mut quit = false;
    egui::Modal::new(egui::Id::new("disclaimer")).show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new(disclaimer::TITLE).font(theme::strong_font(19.0)).color(p.bad));
        ui.add_space(8.0);
        for (i, point) in disclaimer::POINTS.iter().enumerate() {
            crate::wizard::bullet(ui, point);
            ui.add_space(4.0);
            if i == 1 && xbtune_ai::enabled() {
                crate::wizard::bullet(ui, disclaimer::AI_POINT);
                ui.add_space(4.0);
            }
        }
        ui.add_space(4.0);
        ui.checkbox(&mut app.disclaimer_ticked, "I have read this and accept the risk");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let b = egui::Button::new(RichText::new("Continue").color(egui::Color32::WHITE)).fill(p.accent);
            if ui.add_enabled(app.disclaimer_ticked, b).clicked() {
                accept = true;
            }
            if ui.button("Quit").clicked() {
                quit = true;
            }
        });
    });
    if accept {
        app.disclaimer_open = false;
        app.settings.disclaimer_accepted = disclaimer::VERSION;
        app.save_settings();
        if app.settings.show_wizard {
            app.wizard.open();
        }
    } else if quit {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
    }
}
