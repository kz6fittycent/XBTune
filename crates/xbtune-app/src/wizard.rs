// SPDX-License-Identifier: GPL-3.0-or-later
//! "Getting started": a short walk-through shown when XBTune starts, until
//! the user ticks "Don't show this at startup" (or reaches the end). It's
//! always reachable from the side bar. Closing it at any step is fine.
//!
//! Steps: what XBTune is and what it won't do on its own; the adapter
//! (and snap permissions); connecting to the bike; the demo; backing up
//! the ECM first; reporting problems.

use egui::{RichText, Ui};

use crate::app::{View, XbApp};
use crate::settings::SavedTarget;
use crate::theme::{self, Palette};

pub const STEPS: usize = 6;

#[derive(Default)]
pub struct WizardUi {
    /// The step shown, or `None` when closed.
    pub step: Option<usize>,
}

impl WizardUi {
    pub fn open(&mut self) {
        self.step = Some(0);
    }
}

enum Action {
    Back,
    Next,
    Finish,
    Close,
    StartDemo,
    OpenConnect,
    OpenReport,
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let Some(step) = app.wizard.step else { return };
    let p = Palette::for_visuals(ui.visuals());
    let mut action = None;
    let mut dont_show = !app.settings.show_wizard;
    let modal = egui::Modal::new(egui::Id::new("wizard")).show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Getting started").font(theme::strong_font(19.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{} of {STEPS}", step + 1)).color(p.muted));
            });
        });
        ui.add_space(8.0);
        // Same height for every step, so the buttons don't jump.
        ui.vertical(|ui| {
            ui.set_min_height(230.0);
            match step {
                0 => {
                    heading(ui, "Welcome to XBTune");
                    ui.label(
                        "XBTune talks to the ECM on Buell DDFI-2 and DDFI-3 bikes: live data and logging, \
                     trouble codes and tests, and editing and burning tunes.",
                    );
                    ui.add_space(6.0);
                    ui.label("It's safe by default:");
                    bullet(ui, "Connecting, live data, logging and reading the ECM never change the bike.");
                    bullet(
                        ui,
                        "Tests, resets, clearing codes and writing to the ECM are locked on a real bike until \
                     you switch them on in Settings. Try them all in the demo first.",
                    );
                    bullet(ui, "Before every write, XBTune saves a verified backup of the ECM.");
                }
                1 => {
                    heading(ui, "Your adapter");
                    ui.label(
                        "XBTune needs an adapter on the bike's diagnostic connector: a Bluetooth one \
                     (like the BuellTooth) or a USB cable.",
                    );
                    bullet(
                        ui,
                        "Bluetooth: pair the adapter once in your system's Bluetooth settings, with \
                     the PIN from the adapter's instructions. XBTune then lists it under Paired devices.",
                    );
                    bullet(ui, "USB: plug it in; it shows under USB serial ports.");
                    if !app.plugs.is_empty() {
                        ui.add_space(6.0);
                        ui.label("The snap also needs permission to use them:");
                        for s in &app.plugs {
                            ui.horizontal(|ui| {
                                // ✓/✗: Inter has these, not the heavy ✔/✘.
                                let (mark, color) = match s.connected {
                                    Some(true) => ("✓ allowed", p.good),
                                    Some(false) => ("✗ not allowed", p.bad),
                                    None => ("unknown", p.muted),
                                };
                                ui.add_sized([110.0, 18.0], egui::Label::new(RichText::new(mark).color(color)));
                                ui.label(s.purpose);
                            });
                            if s.connected == Some(false) {
                                let mut cmd = s.connect_command();
                                ui.add(
                                    egui::TextEdit::singleline(&mut cmd)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(f32::INFINITY),
                                );
                            }
                        }
                        if app.plugs.iter().any(|s| s.connected == Some(false)) {
                            ui.label(
                                RichText::new("Run the commands above in a terminal, then restart XBTune.")
                                    .color(p.warn),
                            );
                        }
                    }
                }
                2 => {
                    heading(ui, "Connecting to the bike");
                    bullet(ui, "Ignition ON, engine OFF, kill switch on RUN.");
                    bullet(ui, "Open Connect and pick your adapter. The dashboard opens once it's live.");
                    bullet(
                        ui,
                        "Keep the laptop close to a Bluetooth adapter. If the link drops, XBTune reconnects \
                     by itself.",
                    );
                    ui.add_space(8.0);
                    if ui.button("Open Connect").clicked() {
                        action = Some(Action::OpenConnect);
                    }
                }
                3 => {
                    heading(ui, "No bike handy? Try the demo");
                    ui.label(
                        "The demo is a simulated ECM with a made-up ride, so every screen moves. \
                     Tests, burning and resets can be tried here safely.",
                    );
                    ui.add_space(8.0);
                    let demo = app.is_demo();
                    if ui.add_enabled(!demo, egui::Button::new("Start the demo")).clicked() {
                        action = Some(Action::StartDemo);
                    }
                    if demo {
                        ui.label(RichText::new("The demo is running.").color(p.good));
                    }
                }
                4 => {
                    heading(ui, "First time on the bike: back it up");
                    ui.label(
                        "Once connected, open Tune and click Read from ECM. XBTune reads the whole EEPROM \
                     twice to check it, then saves it in:",
                    );
                    let mut dir = crate::tune_view::images_dir().display().to_string();
                    ui.add(
                        egui::TextEdit::singleline(&mut dir)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                    ui.label(
                        "Keep that file somewhere safe as well (a USB stick, cloud storage). It's your way \
                     back to how the bike is today.",
                    );
                }
                _ => {
                    heading(ui, "If something goes wrong");
                    ui.label(
                        "Report a problem (bottom of the side bar) sends a report straight to the developer, \
                     with the app's recent log. No GitHub account needed.",
                    );
                    bullet(ui, "If XBTune crashes, it offers to send a report the next time it starts.");
                    bullet(ui, "Nothing is ever sent without you clicking Send.");
                    ui.add_space(8.0);
                    if ui.button("Report a problem…").clicked() {
                        action = Some(Action::OpenReport);
                    }
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("This guide is under Getting started in the side bar.").color(p.muted),
                    );
                }
            }
        });
        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.checkbox(&mut dont_show, "Don't show this at startup");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if step + 1 == STEPS {
                    let b =
                        egui::Button::new(RichText::new("Finish").color(egui::Color32::WHITE)).fill(p.accent);
                    if ui.add(b).clicked() {
                        action = Some(Action::Finish);
                    }
                } else {
                    let b =
                        egui::Button::new(RichText::new("Next").color(egui::Color32::WHITE)).fill(p.accent);
                    if ui.add(b).clicked() {
                        action = Some(Action::Next);
                    }
                    if ui.button("Skip").clicked() {
                        action = Some(Action::Close);
                    }
                }
                if step > 0 && ui.button("Back").clicked() {
                    action = Some(Action::Back);
                }
            });
        });
    });
    if dont_show == app.settings.show_wizard {
        app.settings.show_wizard = !dont_show;
        app.save_settings();
    }
    if modal.should_close() && action.is_none() {
        action = Some(Action::Close);
    }
    match action {
        None => {}
        Some(Action::Back) => app.wizard.step = Some(step.saturating_sub(1)),
        Some(Action::Next) => app.wizard.step = Some((step + 1).min(STEPS - 1)),
        Some(Action::Close) => app.wizard.step = None,
        Some(Action::Finish) => {
            // Seen it all: don't show at startup again (the box can undo it
            // next time the guide is opened).
            app.wizard.step = None;
            if app.settings.show_wizard {
                app.settings.show_wizard = false;
                app.save_settings();
            }
        }
        Some(Action::OpenConnect) => {
            app.view = View::Connect;
            app.wizard.step = None;
        }
        Some(Action::StartDemo) => app.start(SavedTarget::Demo),
        Some(Action::OpenReport) => {
            app.wizard.step = None;
            app.report.open();
        }
    }
}

fn heading(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::strong_font(16.0)));
    ui.add_space(4.0);
}

fn bullet(ui: &mut Ui, text: &str) {
    ui.horizontal_top(|ui| {
        ui.label("•");
        ui.add(egui::Label::new(text).wrap());
    });
}
