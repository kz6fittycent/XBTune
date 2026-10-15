// SPDX-License-Identifier: GPL-3.0-or-later
//! The Settings view: safety locks (what XBTune may do on a real bike),
//! startup and appearance.
//!
//! The locks live in `hardware.toml` (`xbtune_core::diag::Unlocks`), which
//! the CLI reads too. Unlocking takes a click on a switch and a confirmation
//! naming the risk; locking again is immediate. Both apply to the running
//! connection at once. The demo is always unlocked.

use egui::{RichText, Ui};
use xbtune_core::diag::{self, Unlocks};

use crate::app::XbApp;
use crate::theme::{self, Palette, ThemePref};

/// Which unlock is waiting for confirmation, and its "I understand" box.
#[derive(Default)]
pub struct SettingsUi {
    confirm: Option<(Lock, bool)>,
}

impl SettingsUi {
    /// UI checks: open the unlock confirmation ("commands" or "writes").
    pub fn ask(&mut self, which: &str) {
        self.confirm = Some((if which == "writes" { Lock::Writes } else { Lock::Commands }, false));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lock {
    Commands,
    Writes,
}

impl Lock {
    fn title(self) -> &'static str {
        match self {
            Self::Commands => "Unlock ECM commands?",
            Self::Writes => "Unlock writing to the ECM?",
        }
    }

    fn risk(self) -> &'static str {
        match self {
            Self::Commands => {
                "Active tests switch real parts on (fuel pump, injectors, coils, fan). Clearing codes \
                 erases the stored trouble codes, and a TPS reset changes the stored throttle zero. \
                 XBTune checks the engine is stopped and asks before each one."
            }
            Self::Writes => {
                "Writing changes how the engine runs. A wrong tune can make it run badly or damage it. \
                 Before every write XBTune saves a verified backup, shows exactly what will change and \
                 asks you to confirm; afterwards it reads everything back to check it, and the backup \
                 can be restored."
            }
        }
    }

    fn check(self) -> &'static str {
        match self {
            Self::Commands => "I've tried these in the demo and understand what they do",
            Self::Writes => "I've tried burning in the demo and I keep my backups",
        }
    }
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    let current = diag::unlocks();
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Settings");
        ui.add_space(8.0);

        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Safety locks").font(theme::strong_font(17.0)));
            ui.label(
                RichText::new(
                    "What XBTune may do on a real bike. Reading, live data and logging never change the \
                     bike and need no unlocking. The demo is always unlocked, so you can try everything \
                     there first.",
                )
                .color(p.muted),
            );
            ui.add_space(6.0);
            lock_row(
                app,
                ui,
                &p,
                Lock::Commands,
                current.ecm_commands,
                "Allow ECM commands",
                "Active tests, clearing stored codes, TPS reset",
            );
            ui.add_space(4.0);
            lock_row(
                app,
                ui,
                &p,
                Lock::Writes,
                current.eeprom_writes,
                "Allow writing to the ECM",
                "Burning tunes, restoring a backup, AFV reset",
            );
            if let Some(path) = diag::permit_file() {
                ui.add_space(4.0);
                ui.label(RichText::new(format!("Saved in {}", path.display())).small().color(p.muted));
            }
        });
        ui.add_space(8.0);

        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Startup").font(theme::strong_font(17.0)));
            let mut changed =
                ui.checkbox(&mut app.settings.show_wizard, "Show the Getting started guide").changed();
            changed |= ui
                .checkbox(&mut app.settings.auto_connect, "Connect to the last adapter when XBTune starts")
                .changed();
            if changed {
                app.save_settings();
            }
        });
        ui.add_space(8.0);

        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Appearance").font(theme::strong_font(17.0)));
            ui.horizontal(|ui| {
                for (pref, label) in [
                    (ThemePref::System, "Follow the system"),
                    (ThemePref::Dark, "Dark"),
                    (ThemePref::Light, "Light"),
                ] {
                    if ui.radio(app.settings.theme == pref, label).clicked() && app.settings.theme != pref {
                        app.settings.theme = pref;
                        theme::apply(ui.ctx(), pref);
                        app.save_settings();
                    }
                }
            });
        });
    });
    confirm(app, ui, &p, current);
}

fn lock_row(app: &mut XbApp, ui: &mut Ui, p: &Palette, lock: Lock, on: bool, label: &str, what: &str) {
    ui.horizontal(|ui| {
        let mut want = on;
        if ui.checkbox(&mut want, RichText::new(label).strong()).changed() {
            if want {
                app.settings_ui.confirm = Some((lock, false));
            } else {
                let mut u = diag::unlocks();
                match lock {
                    Lock::Commands => u.ecm_commands = false,
                    Lock::Writes => u.eeprom_writes = false,
                }
                app.apply_unlocks(u);
            }
        }
        let (state, color) = if on { ("unlocked", p.warn) } else { ("locked", p.good) };
        ui.label(RichText::new(state).color(color));
    });
    ui.label(RichText::new(what).color(p.muted));
}

fn confirm(app: &mut XbApp, ui: &mut Ui, p: &Palette, current: Unlocks) {
    let Some((lock, mut understood)) = app.settings_ui.confirm else { return };
    let mut unlock = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("unlock")).show(ui.ctx(), |ui| {
        ui.set_width(460.0);
        ui.label(RichText::new(lock.title()).font(theme::strong_font(18.0)));
        ui.add_space(4.0);
        ui.label(lock.risk());
        ui.add_space(6.0);
        ui.label(RichText::new("You can lock it again here at any time.").color(p.muted));
        ui.add_space(6.0);
        ui.checkbox(&mut understood, lock.check());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let b = egui::Button::new(RichText::new("Unlock").color(egui::Color32::WHITE)).fill(p.warn);
            if ui.add_enabled(understood, b).clicked() {
                unlock = true;
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
    });
    if unlock {
        let mut u = current;
        match lock {
            Lock::Commands => u.ecm_commands = true,
            Lock::Writes => u.eeprom_writes = true,
        }
        app.settings_ui.confirm = None;
        app.apply_unlocks(u);
    } else if cancel || modal.should_close() {
        app.settings_ui.confirm = None;
    } else {
        app.settings_ui.confirm = Some((lock, understood));
    }
}
