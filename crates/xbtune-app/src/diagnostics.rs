// SPDX-License-Identifier: GPL-3.0-or-later
//! The Diagnostics view: trouble codes, active tests, TPS reset.
//!
//! Everything that changes the ECM goes through a confirmation dialog with
//! the safety notice, then `Command::Function` on the session, which checks
//! the permit, that the engine is stopped and that the ECM is idle
//! (`xbtune_core::diag`).

use egui::{RichText, Ui};
use xbtune_core::diag::{self, CodeKind, Outcome, Stage, TroubleCode};
use xbtune_core::session::Command;
use xbtune_proto::Function;

use crate::app::XbApp;
use crate::theme::{self, Palette};

#[derive(Default)]
pub struct DiagUi {
    /// Function waiting for the user to confirm.
    pub confirm: Option<Function>,
    /// "The bike is safe" box in the dialog.
    pub safe: bool,
    /// Function in progress and its last stage.
    pub running: Option<(Function, Option<Stage>)>,
    pub last: Option<(Function, Result<Outcome, String>)>,
}

impl DiagUi {
    /// Open the confirmation dialog for `f` (once).
    pub fn ask(&mut self, f: Function) {
        if self.confirm.is_none() && self.last.is_none() {
            self.confirm = Some(f);
            self.safe = false;
        }
    }
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let Some(id) = app.link.identity.clone() else {
        crate::table::not_connected(app, ui);
        return;
    };
    let p = Palette::for_visuals(ui.visuals());
    let frame = app.history.latest().map(|s| s.frame.clone());
    let busy = app.diag.running.is_some();
    let can_command = app.link.live() && app.session_permitted() && !busy;
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Diagnostics");
        ui.add_space(6.0);
        status(app, ui, &p);
        ui.add_space(6.0);

        if !app.session_permitted() {
            locked_card(
                app,
                ui,
                "ECM commands are locked",
                "Active tests and clearing codes are locked on a real bike. Reading trouble codes works \
                 without unlocking.",
            );
        }

        demo_engine_card(app, ui, &p);

        // Trouble codes
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Trouble codes").font(theme::strong_font(16.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(can_command, egui::Button::new("Clear stored codes"))
                        .on_disabled_hover_text("Needs a live connection and unlocked ECM commands")
                        .clicked()
                    {
                        app.diag.confirm = Some(Function::ClearCodes);
                        app.diag.safe = false;
                    }
                });
            });
            match &frame {
                None => {
                    ui.label(RichText::new("Waiting for live data…").color(p.muted));
                }
                Some(f) => {
                    codes_table(ui, &p, "Current (active now)", &diag::codes_from_runtime(&id.definition, f, CodeKind::Current));
                    ui.add_space(4.0);
                    codes_table(ui, &p, "Stored (history)", &diag::codes_from_runtime(&id.definition, f, CodeKind::Stored));
                }
            }
        });
        ui.add_space(8.0);

        // Active tests
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Active tests").font(theme::strong_font(16.0)));
            ui.label(
                RichText::new(
                    "Engine stopped, ignition ON, kill switch RUN. The ECM refuses tests for hardware that isn't fitted.",
                )
                .color(p.muted),
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                for f in diag::TESTS {
                    if ui.add_enabled(can_command, egui::Button::new(f.label())).clicked() {
                        app.diag.confirm = Some(*f);
                        app.diag.safe = false;
                    }
                }
            });
        });

    });

    confirm_dialog(app, ui);
}

/// Progress and the last result of a test or reset, shown at the top of
/// the views that run them.
pub fn status(app: &XbApp, ui: &mut Ui, p: &Palette) {
    if let Some((f, stage)) = &app.diag.running {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(match stage {
                None | Some(Stage::Checking) => format!("{}: checking the bike is safe…", f.label()),
                Some(Stage::Sent) => format!("{}: sent, waiting for the ECM…", f.label()),
                Some(Stage::Running { seconds }) => format!("{}: running {seconds:.0} s…", f.label()),
            });
        });
    } else if let Some((f, r)) = &app.diag.last {
        match r {
            Ok(o) => ui.colored_label(
                p.good,
                format!(
                    "{}: done after {:.1} s{}",
                    f.label(),
                    o.busy_for.as_secs_f64(),
                    if o.acknowledged { "" } else { " (reply lost; the busy state showed it ran)" }
                ),
            ),
            Err(e) => ui.colored_label(p.bad, format!("{}: {e}", f.label())),
        };
    }
}

/// The demo's engine switch: tests and resets refuse while it runs.
pub fn demo_engine_card(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    if app.is_demo() {
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Demo engine").font(theme::strong_font(15.0)));
                let mut running = app.demo_engine_running();
                if ui.checkbox(&mut running, "running").changed() {
                    app.set_demo_engine_running(running);
                }
                ui.label(
                    RichText::new("Tests and resets refuse to run while the engine is running.")
                        .color(p.muted),
                );
            });
        });
        ui.add_space(8.0);
    }
}

/// "Locked" card with a button to the Settings, where the owner unlocks.
pub fn locked_card(app: &mut XbApp, ui: &mut Ui, title: &str, text: &str) {
    let p = Palette::for_visuals(ui.visuals());
    theme::card(ui).stroke(egui::Stroke::new(1.0, p.warn)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).font(theme::strong_font(15.0)).color(p.warn));
        ui.label(text);
        ui.label(RichText::new("Everything can be tried in the demo without unlocking.").color(p.muted));
        if ui.button("Unlock in Settings…").clicked() {
            app.view = crate::app::View::Settings;
        }
    });
    ui.add_space(8.0);
}

fn codes_table(ui: &mut Ui, p: &Palette, title: &str, codes: &[TroubleCode]) {
    ui.label(RichText::new(title).color(p.muted));
    if codes.is_empty() {
        ui.label(RichText::new("None").color(p.good));
        return;
    }
    egui::Grid::new(title).num_columns(3).spacing([16.0, 4.0]).show(ui, |ui| {
        for c in codes {
            let code = if c.code.is_empty() || c.code == "0" { "--" } else { &c.code };
            ui.label(RichText::new(code).font(theme::strong_font(15.0)).color(p.bad));
            ui.label(RichText::new(&c.name).strong());
            ui.label(&c.description);
            ui.end_row();
        }
    });
}

pub fn confirm_dialog(app: &mut XbApp, ui: &mut Ui) {
    let Some(f) = app.diag.confirm else { return };
    let p = Palette::for_visuals(ui.visuals());
    let mut go = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("confirm-function")).show(ui.ctx(), |ui| {
        ui.set_width(440.0);
        ui.label(RichText::new(f.label()).font(theme::strong_font(18.0)));
        ui.add_space(4.0);
        ui.label(diag::safety_notice(f));
        ui.add_space(6.0);
        let check = if f == Function::ClearCodes {
            "I want to permanently clear the stored codes"
        } else {
            "The engine is stopped and the bike is safe for this test"
        };
        ui.checkbox(&mut app.diag.safe, check);
        let remote = crate::remote_sync::guest_live(app);
        let owner = app.remote.peer().unwrap_or_else(|| "the bike owner".into());
        let direct = app.remote.guest.perms.tests;
        if remote {
            ui.label(
                RichText::new(if direct {
                    format!(
                        "This runs on {owner}'s bike after a 10 s countdown on their side; they can stop it."
                    )
                } else {
                    format!("This asks {owner}: they check the bike and confirm it on their side.")
                })
                .color(p.warn),
            );
        }
        ui.add_space(8.0);
        let label = if remote && !direct { "Ask" } else { "Run" };
        (go, cancel) = theme::dialog_buttons(ui, theme::primary(label), app.diag.safe, "Cancel");
    });
    if modal.should_close() || cancel {
        app.diag.confirm = None;
    } else if go && crate::remote_sync::guest_live(app) {
        app.diag.confirm = None;
        let kind = xbtune_remote::msg::RequestKind::Function { name: f.label().into() };
        crate::remote_sync::request(app, kind, String::new());
    } else if go {
        app.diag.confirm = None;
        app.diag.running = Some((f, None));
        app.diag.last = None;
        app.send(Command::Function(f));
    }
}
