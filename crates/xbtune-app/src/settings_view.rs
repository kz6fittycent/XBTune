// SPDX-License-Identifier: GPL-3.0-or-later
//! The Settings view: where your files go, safety locks (what XBTune may
//! do on a real bike), startup and appearance.
//!
//! The locks live in `hardware.toml` (`xbtune_core::diag::Unlocks`), which
//! the CLI reads too. Unlocking takes a click on a switch and a confirmation
//! naming the risk; locking again is immediate. Both apply to the running
//! connection at once. The demo is always unlocked.

use std::path::PathBuf;

use egui::{RichText, Ui};
use xbtune_core::diag::{self, Unlocks};
use xbtune_core::env::Dirs;

use crate::app::XbApp;
use crate::theme::{self, Palette, ThemePref};

/// Which unlock is waiting for confirmation, and its "I understand" box;
/// the folder picker when open.
#[derive(Default)]
pub struct SettingsUi {
    confirm: Option<(Lock, bool)>,
    folder: Option<FolderPick>,
}

/// The folder picker for the files folder.
struct FolderPick {
    dir: PathBuf,
    new_name: String,
    move_existing: bool,
    error: Option<String>,
}

impl SettingsUi {
    /// UI checks: open the folder picker at `dir`.
    pub fn pick_folder(&mut self, dir: PathBuf) {
        self.folder = Some(FolderPick { dir, new_name: String::new(), move_existing: true, error: None });
    }

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

        files_card(app, ui, &p);
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

        if xbtune_ai::enabled() {
            crate::ai_view::settings_card(app, ui, &p);
            ui.add_space(8.0);
        }

        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Startup").font(theme::strong_font(17.0)));
            let mut changed =
                ui.checkbox(&mut app.settings.show_wizard, "Show the Getting started guide").changed();
            changed |= ui.checkbox(&mut app.settings.show_splash, "Show the splash screen").changed();
            changed |= ui
                .checkbox(&mut app.settings.auto_connect, "Connect to the last adapter when XBTune starts")
                .changed();
            if changed {
                app.save_settings();
            }
        });
        ui.add_space(8.0);

        if crate::update::applies() {
            updates_card(app, ui);
            ui.add_space(8.0);
        }

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
    folder_dialog(app, ui, &p);
}

fn files_card(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    let Some(d) = Dirs::detect() else { return };
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Your files").font(theme::strong_font(17.0)));
        ui.label(
            RichText::new("Tunes read from the ECM, backups taken before burns, data logs and burn history.")
                .color(p.muted),
        );
        ui.add_space(4.0);
        let mut shown = d.files.display().to_string();
        ui.add(
            egui::TextEdit::singleline(&mut shown)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY),
        );
        ui.label(
            RichText::new(format!(
                "In it: {}, {}, {}, {}",
                xbtune_core::env::TUNES,
                xbtune_core::env::BACKUPS,
                xbtune_core::env::LOGS,
                xbtune_core::env::HISTORY
            ))
            .small()
            .color(p.muted),
        );
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.button("Change folder…").clicked() {
                app.settings_ui.folder = Some(FolderPick {
                    dir: d.files.clone(),
                    new_name: String::new(),
                    move_existing: true,
                    error: None,
                });
            }
            if let Some(default) = d.default_files()
                && default != d.files
                && ui.button(format!("Use the default ({})", default.display())).clicked()
                && let Err(e) = app.set_files_folder(None, true)
            {
                app.say(e, true);
            }
        });
    });
}

fn folder_dialog(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    let Some(pick) = &mut app.settings_ui.folder else { return };
    let mut chosen = None;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("files-folder")).show(ui.ctx(), |ui| {
        ui.set_width(540.0);
        ui.label(RichText::new("Choose a folder for your files").font(theme::strong_font(18.0)));
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Home").clicked()
                && let Some(h) = xbtune_core::env::real_home()
            {
                pick.dir = h;
            }
            if ui.small_button("Up").clicked()
                && let Some(parent) = pick.dir.parent()
            {
                pick.dir = parent.to_path_buf();
            }
            ui.label(RichText::new(pick.dir.display().to_string()).small().color(p.muted));
        });
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&pick.dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'))
                    })
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
            if dirs.is_empty() {
                ui.label(RichText::new("No folders here.").color(p.muted));
            }
            for d in dirs {
                let name = d.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if ui.selectable_label(false, format!("{name}/")).clicked() {
                    pick.dir = d;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut pick.new_name)
                    .hint_text("New folder name")
                    .desired_width(220.0),
            );
            let name = pick.new_name.trim().to_string();
            let valid = !name.is_empty() && !name.contains('/') && !name.starts_with('.');
            if ui.add_enabled(valid, egui::Button::new("Create")).clicked() {
                let d = pick.dir.join(&name);
                match std::fs::create_dir_all(&d) {
                    Ok(()) => {
                        pick.dir = d;
                        pick.new_name.clear();
                        pick.error = None;
                    }
                    Err(e) => pick.error = Some(format!("Couldn't create {name}: {e}")),
                }
            }
        });
        ui.separator();
        ui.checkbox(&mut pick.move_existing, "Move my existing files there");
        if xbtune_core::env::in_snap() {
            ui.label(
                RichText::new(
                    "XBTune can use folders in your home (not hidden ones), and on USB drives once \
                     removable-media is allowed.",
                )
                .small()
                .color(p.muted),
            );
        }
        if let Some(e) = &pick.error {
            ui.label(RichText::new(e).color(p.bad));
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let b = egui::Button::new(RichText::new("Use this folder").color(egui::Color32::WHITE))
                .fill(p.accent);
            if ui.add(b).clicked() {
                chosen = Some((pick.dir.clone(), pick.move_existing));
            }
            if ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
    });
    if let Some((dir, move_existing)) = chosen {
        match app.set_files_folder(Some(dir), move_existing) {
            Ok(()) => app.settings_ui.folder = None,
            Err(e) => {
                if let Some(pick) = &mut app.settings_ui.folder {
                    pick.error = Some(e);
                }
            }
        }
    } else if cancel || modal.should_close() {
        app.settings_ui.folder = None;
    }
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

/// New versions: a daily look at GitHub's latest release (never in the snap).
fn updates_card(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Updates").font(theme::strong_font(17.0)));
        ui.label(
            RichText::new(
                "Once a day XBTune asks GitHub whether a newer version is out, and if so shows a link to it. \
                 It never downloads or installs anything by itself.",
            )
            .color(p.muted),
        );
        if ui.checkbox(&mut app.settings.update_check, "Tell me when a new version is out").changed() {
            app.save_settings();
        }
        ui.horizontal(|ui| {
            ui.label(format!("This is version {}.", env!("CARGO_PKG_VERSION")));
            if crate::update::checking(app) {
                ui.spinner();
            } else if ui.button("Check now").clicked() {
                crate::update::check_now(app);
            }
            match &app.settings.update_latest {
                Some(r) if crate::update::is_newer(&r.version, env!("CARGO_PKG_VERSION")) => {
                    ui.hyperlink_to(format!("{} is available", r.version), &r.url);
                }
                Some(_) if app.settings.update_checked_at > 0 => {
                    ui.label(RichText::new("You have the latest.").color(p.good));
                }
                _ => {}
            }
        });
        ui.hyperlink_to("All releases", crate::update::RELEASES_PAGE);
    });
}
