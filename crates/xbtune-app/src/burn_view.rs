// SPDX-License-Identifier: GPL-3.0-or-later
//! The burn dialog (KICKOFF §6): prepare (checks + verified backup), review
//! what differs and choose what to write, confirm, write + verify, and on
//! failure retry or restore the backup. All checks run in the session
//! (`xbtune_core::session`, `xbtune_core::burn`); this is only the UI.

use std::collections::BTreeSet;
use std::path::PathBuf;

use egui::{RichText, Ui};
use xbtune_core::burn::{self, BurnStep, LinkCheck, VarChange};
use xbtune_core::image::Image;
use xbtune_core::session::{BurnFailure, BurnSummary, Command};

use crate::app::XbApp;
use crate::theme::{self, Palette};

/// What is being written.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// The open tune: chosen variables over the ECM's current contents.
    Tune { tune: Box<Image>, opened: Vec<u8> },
    /// Put a backup back, every differing byte.
    Restore { backup: Box<Image>, path: PathBuf },
    /// AFV to 100%.
    Afv,
}

impl Mode {
    fn title(&self) -> &'static str {
        match self {
            Self::Tune { .. } => "Burn tune to ECM",
            Self::Restore { .. } => "Restore backup to ECM",
            Self::Afv => "AFV reset",
        }
    }
}

#[derive(Default)]
pub enum BurnUi {
    #[default]
    Closed,
    Preparing {
        mode: Mode,
        progress: f32,
        then_burn: bool,
    },
    Review {
        mode: Mode,
        backup: Box<Image>,
        path: PathBuf,
        link: LinkCheck,
        changes: Vec<VarChange>,
        chosen: BTreeSet<String>,
        note: String,
        confirmed: bool,
    },
    Burning {
        mode: Mode,
        step: Option<BurnStep>,
        chosen: BTreeSet<String>,
        backup_path: PathBuf,
        backup: Box<Image>,
    },
    Done(BurnSummary),
    Failed {
        mode: Mode,
        failure: BurnFailure,
        chosen: BTreeSet<String>,
        backup_path: Option<PathBuf>,
        backup: Option<Box<Image>>,
    },
}

impl BurnUi {
    pub fn is_open(&self) -> bool {
        !matches!(self, Self::Closed)
    }
}

/// Start a burn of `mode`: ask the session to prepare.
pub fn start(app: &mut XbApp, mode: Mode) {
    // Tuner's side of a remote session: ask the bike owner's XBTune.
    if crate::remote_sync::guest_live(app) {
        match mode {
            Mode::Tune { .. } if !app.remote.guest.following => app.say(
                "Burning goes to the bike owner's ECM: go back to the shared tune first (Remote).",
                true,
            ),
            Mode::Tune { .. } => app.remote.guest.ask_burn = Some((false, String::new())),
            Mode::Afv => app.remote.guest.ask_burn = Some((true, String::new())),
            Mode::Restore { .. } => {
                app.say("Restoring a backup can only be done on the bike owner's side.", true)
            }
        }
        return;
    }
    app.burn = BurnUi::Preparing { mode, progress: 0.0, then_burn: false };
    app.send(Command::PrepareBurn);
}

/// The target image for `mode` over the ECM contents in `backup`.
fn target(app: &XbApp, mode: &Mode, backup: &Image, chosen: &BTreeSet<String>) -> Result<Image, String> {
    let def = app.link.identity.as_ref().map(|i| i.definition.clone()).ok_or("not connected")?;
    let bytes = match mode {
        Mode::Tune { tune, .. } => {
            let names: Vec<String> = chosen.iter().cloned().collect();
            burn::build_target(&def, &backup.bytes, &tune.bytes, backup.page_zero, &names)?
        }
        Mode::Restore { backup: b, .. } => b.bytes.clone(),
        Mode::Afv => burn::afv_reset(&def, &backup.bytes, backup.page_zero)?.0,
    };
    let mut t = backup.clone();
    t.bytes = bytes;
    Ok(t)
}

fn changes_for(app: &XbApp, mode: &Mode, backup: &Image) -> Vec<VarChange> {
    let Some(id) = &app.link.identity else { return Vec::new() };
    let def = &id.definition;
    match mode {
        Mode::Tune { tune, opened } => {
            burn::changes(def, &backup.bytes, &tune.bytes, Some(opened), backup.page_zero && tune.page_zero)
        }
        Mode::Restore { backup: b, .. } => {
            burn::changes(def, &backup.bytes, &b.bytes, None, backup.page_zero && b.page_zero)
        }
        Mode::Afv => match burn::afv_reset(def, &backup.bytes, backup.page_zero) {
            Ok((bytes, _)) => burn::changes(def, &backup.bytes, &bytes, None, backup.page_zero),
            Err(_) => Vec::new(),
        },
    }
}

/// Session events for the dialog.
pub fn on_prepared(app: &mut XbApp, backup: Image, path: PathBuf, link: LinkCheck) {
    let BurnUi::Preparing { mode, then_burn, .. } = std::mem::take(&mut app.burn) else { return };
    let changes = changes_for(app, &mode, &backup);
    let chosen: BTreeSet<String> = match &mode {
        Mode::Tune { .. } => {
            changes.iter().filter(|c| c.edited && c.blocked.is_none()).map(|c| c.name.clone()).collect()
        }
        _ => changes.iter().filter(|c| c.blocked.is_none()).map(|c| c.name.clone()).collect(),
    };
    if then_burn {
        // Retry/restore already confirmed: write the same choice again.
        let pending = std::mem::take(&mut app.pending_chosen);
        let chosen = if pending.is_empty() { chosen } else { pending };
        burn_now(app, mode, backup, path, chosen, "retry".into());
        return;
    }
    app.burn = BurnUi::Review {
        mode,
        backup: Box::new(backup),
        path,
        link,
        changes,
        chosen,
        note: String::new(),
        confirmed: false,
    };
    crate::remote_sync::on_burn_prepared(app);
}

/// Remote tuning: the changes the review would write, one line each.
pub fn review_lines(app: &XbApp) -> Vec<String> {
    let BurnUi::Review { changes, chosen, .. } = &app.burn else { return Vec::new() };
    changes
        .iter()
        .filter(|c| chosen.contains(&c.name))
        .map(|c| format!("{}: {} -> {}", c.label, c.ecm, c.tune))
        .collect()
}

/// Remote tuning: how many variables the review would write.
pub fn chosen_count(app: &XbApp) -> usize {
    match &app.burn {
        BurnUi::Review { chosen, .. } => chosen.len(),
        _ => 0,
    }
}

/// Remote tuning: the tuner confirmed the review and the host's countdown
/// ran out. Write what the review had chosen, with `note` in burn.txt.
pub fn confirm_remote(app: &mut XbApp, note: String) -> bool {
    if !matches!(app.burn, BurnUi::Review { .. }) {
        return false;
    }
    if let BurnUi::Review { mode, backup, path, chosen, .. } = std::mem::take(&mut app.burn) {
        if chosen.is_empty() {
            return false;
        }
        burn_now(app, mode, *backup, path, chosen, note);
    }
    matches!(app.burn, BurnUi::Burning { .. })
}

fn burn_now(
    app: &mut XbApp,
    mode: Mode,
    backup: Image,
    path: PathBuf,
    chosen: BTreeSet<String>,
    note: String,
) {
    match target(app, &mode, &backup, &chosen) {
        Ok(t) => {
            app.send(Command::Burn { target: Box::new(t), note });
            app.burn =
                BurnUi::Burning { mode, step: None, chosen, backup_path: path, backup: Box::new(backup) };
        }
        Err(e) => {
            app.burn = BurnUi::Failed {
                mode,
                failure: BurnFailure { message: e, mismatched: None, wrote: false },
                chosen,
                backup_path: Some(path),
                backup: Some(Box::new(backup)),
            }
        }
    }
}

pub fn on_progress(app: &mut XbApp, step: BurnStep) {
    if let BurnUi::Burning { step: s, .. } = &mut app.burn {
        *s = Some(step);
    }
}

pub fn on_read_progress(app: &mut XbApp, done: usize, total: usize) -> bool {
    if let BurnUi::Preparing { progress, .. } = &mut app.burn {
        *progress = done as f32 / total.max(1) as f32;
        return true;
    }
    false
}

pub fn on_done(app: &mut XbApp, result: Result<BurnSummary, BurnFailure>) {
    let text = match &result {
        Ok(s) => format!("written and verified, {} write(s)", s.report.writes),
        Err(f) => f.message.clone(),
    };
    crate::remote_sync::on_burn_done(app, result.is_ok(), &text);
    let prev = std::mem::take(&mut app.burn);
    app.burn = match (result, prev) {
        (Ok(s), _) => BurnUi::Done(s),
        (Err(failure), BurnUi::Burning { mode, chosen, backup_path, backup, .. }) => {
            BurnUi::Failed { mode, failure, chosen, backup_path: Some(backup_path), backup: Some(backup) }
        }
        (Err(failure), BurnUi::Preparing { mode, .. }) => {
            BurnUi::Failed { mode, failure, chosen: BTreeSet::new(), backup_path: None, backup: None }
        }
        (Err(failure), _) => BurnUi::Failed {
            mode: Mode::Afv,
            failure,
            chosen: BTreeSet::new(),
            backup_path: None,
            backup: None,
        },
    };
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if !app.burn.is_open() {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let mut action: Option<Action> = None;
    let state = std::mem::take(&mut app.burn);
    let title = match &state {
        BurnUi::Preparing { mode, .. }
        | BurnUi::Review { mode, .. }
        | BurnUi::Burning { mode, .. }
        | BurnUi::Failed { mode, .. } => mode.title(),
        BurnUi::Done(_) => "Burn complete",
        BurnUi::Closed => "",
    };
    let mut state = state;
    let app_family_ddfi3 =
        app.link.identity.as_ref().is_some_and(|i| i.ecm.family == xbtune_defs::Family::Ddfi3);
    egui::Modal::new(egui::Id::new("burn")).show(ui.ctx(), |ui| {
        ui.set_width(640.0);
        ui.label(RichText::new(title).font(theme::strong_font(19.0)));
        ui.add_space(6.0);
        match &mut state {
            BurnUi::Closed => {}
            BurnUi::Preparing { progress, .. } => {
                ui.label("Checking the engine is stopped and the link is good, then reading and saving a backup of the whole EEPROM.");
                ui.add(egui::ProgressBar::new(*progress).show_percentage());
                ui.label(RichText::new("Nothing is written until you confirm.").color(p.muted));
            }
            BurnUi::Review { mode, path, link, changes, chosen, note, confirmed, .. } => {
                ui.label(RichText::new(format!("Backup saved: {}", path.display())).color(p.good));
                ui.label(
                    RichText::new(format!(
                        "Link: {} of {} test requests OK, average reply {} ms",
                        link.attempts - link.failures,
                        link.attempts,
                        link.rtt_avg.map_or(0, |d| d.as_millis())
                    ))
                    .color(p.muted),
                );
                ui.add_space(6.0);
                if changes.is_empty() {
                    ui.label("The ECM already holds these values. Nothing to write.");
                    if ui.button("Close").clicked() {
                        action = Some(Action::Close);
                    }
                    return;
                }
                let free = matches!(mode, Mode::Tune { .. });
                if free {
                    ui.label(
                        "Variables that differ between the ECM and the tune. Your edits are ticked. \
                         Unticked ones may be values the ECM changes itself (AFV, stored codes, counters): \
                         writing them would put back the tune file's older values.",
                    );
                } else {
                    ui.label("These will be written:");
                }
                egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                    egui::Grid::new("changes").num_columns(4).spacing([12.0, 4.0]).striped(true).show(ui, |ui| {
                        ui.label("");
                        ui.label(RichText::new("Variable").color(p.muted));
                        ui.label(RichText::new("ECM now").color(p.muted));
                        ui.label(RichText::new("Will be").color(p.muted));
                        ui.end_row();
                        for c in changes.iter() {
                            let mut on = chosen.contains(&c.name);
                            ui.add_enabled_ui(free && c.blocked.is_none(), |ui| {
                                if ui.checkbox(&mut on, "").changed() {
                                    if on {
                                        chosen.insert(c.name.clone());
                                    } else {
                                        chosen.remove(&c.name);
                                    }
                                }
                            });
                            let mut l = RichText::new(&c.label);
                            if c.edited && free {
                                l = l.strong();
                            }
                            ui.label(l).on_hover_text(&c.name);
                            ui.label(&c.ecm);
                            match &c.blocked {
                                Some(b) => ui.colored_label(p.bad, format!("can't write: {b}")),
                                None => ui.label(&c.tune),
                            };
                            ui.end_row();
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Note");
                    ui.add(egui::TextEdit::singleline(note).desired_width(400.0).hint_text("what changed and why (kept in the history)"));
                });
                ui.add_space(4.0);
                ui.colored_label(
                    p.warn,
                    "Engine OFF, ignition ON, battery healthy. Don't switch off or disconnect while writing.",
                );
                if app_family_ddfi3 {
                    ui.colored_label(
                        p.warn,
                        "DDFI-3: if the ECM's keep-alive power supply is faulty, written changes can be lost. \
                         Check that the values stick after an ignition cycle.",
                    );
                }
                ui.checkbox(confirmed, format!("Write {} variable(s) to the ECM", chosen.len()));
                ui.horizontal(|ui| {
                    let ok = *confirmed && !chosen.is_empty();
                    let b = egui::Button::new(RichText::new("Write to ECM").color(egui::Color32::WHITE)).fill(p.bad);
                    if ui.add_enabled(ok, b).clicked() {
                        action = Some(Action::Burn);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(Action::Close);
                    }
                });
            }
            BurnUi::Burning { step, .. } => {
                let (f, text) = match step {
                    None => (0.0, "starting…".to_string()),
                    Some(BurnStep::Writing { done, total, page }) => {
                        (*done as f32 / (*total).max(1) as f32 * 0.8, format!("writing {done}/{total} (page {page})"))
                    }
                    Some(BurnStep::Verifying { page }) => (0.9, format!("reading back page {page}")),
                };
                ui.add(egui::ProgressBar::new(f).text(text));
                ui.colored_label(p.warn, "Don't switch off the ignition or move away from the bike.");
            }
            BurnUi::Done(s) => {
                if s.report.writes == 0 {
                    ui.label("Nothing needed writing: the ECM already held these values.");
                } else {
                    ui.colored_label(
                        p.good,
                        format!(
                            "Written and verified: {} write(s), {} bytes, page(s) {:?}.",
                            s.report.writes, s.report.bytes, s.report.pages
                        ),
                    );
                    if let Some(h) = &s.history {
                        ui.label(RichText::new(format!("Before/after saved in {}", h.display())).color(p.muted));
                    }
                    ui.label("Turn the ignition off and on before riding, so the ECM starts from the new values.");
                }
                if ui.button("Close").clicked() {
                    action = Some(Action::Close);
                }
            }
            BurnUi::Failed { failure, backup_path, backup, .. } => {
                ui.colored_label(p.bad, &failure.message);
                if failure.wrote {
                    match &failure.mismatched {
                        Some(m) if m.is_empty() => {
                            ui.label("Every page checked holds the new data.");
                        }
                        Some(m) => {
                            ui.label("Pages that don't hold the new data:");
                            for pm in m {
                                let r: Vec<String> = pm.ranges.iter().map(|(a, b)| format!("{a:#06x}..{b:#06x}")).collect();
                                ui.label(format!("  page {}: {}", pm.page, r.join(", ")));
                            }
                        }
                        None => {
                            ui.label("The ECM's state couldn't be read back.");
                        }
                    }
                    ui.colored_label(p.warn, "Keep the ignition ON. Retry, or put the backup back.");
                } else {
                    ui.label("Nothing was written.");
                }
                if let Some(bp) = backup_path {
                    ui.label(RichText::new(format!("Backup: {}", bp.display())).color(p.muted));
                }
                ui.horizontal(|ui| {
                    if ui.button("Retry").clicked() {
                        action = Some(Action::Retry);
                    }
                    if failure.wrote && backup.is_some() && ui.button("Restore backup").clicked() {
                        action = Some(Action::Restore);
                    }
                    if ui.button("Close").clicked() {
                        action = Some(Action::Close);
                    }
                });
            }
        }
    });
    app.burn = state;
    match action {
        None => {}
        Some(Action::Close) => app.burn = BurnUi::Closed,
        Some(Action::Burn) => {
            if let BurnUi::Review { mode, backup, path, chosen, note, .. } = std::mem::take(&mut app.burn) {
                burn_now(app, mode, *backup, path, chosen, note);
            }
        }
        Some(Action::Retry) => {
            if let BurnUi::Failed { mode, chosen, failure, .. } = std::mem::take(&mut app.burn) {
                // Nothing written yet: go back through review. Written: the
                // user already confirmed; prepare and write the same choice.
                app.burn =
                    BurnUi::Preparing { mode, progress: 0.0, then_burn: failure.wrote && !chosen.is_empty() };
                if let BurnUi::Preparing { .. } = app.burn {
                    app.pending_chosen = chosen;
                }
                app.send(Command::PrepareBurn);
            }
        }
        Some(Action::Restore) => {
            if let BurnUi::Failed { backup: Some(b), backup_path: Some(path), .. } =
                std::mem::take(&mut app.burn)
            {
                app.burn = BurnUi::Preparing {
                    mode: Mode::Restore { backup: b, path },
                    progress: 0.0,
                    then_burn: true,
                };
                app.pending_chosen = BTreeSet::new();
                app.send(Command::PrepareBurn);
            }
        }
    }
}

/// Screenshot hook: confirm a review and write.
pub fn auto_confirm(app: &mut XbApp) {
    if let BurnUi::Review { confirmed, note, .. } = &mut app.burn {
        *confirmed = true;
        *note = "screenshot test".into();
        if let BurnUi::Review { mode, backup, path, chosen, note, .. } = std::mem::take(&mut app.burn) {
            burn_now(app, mode, *backup, path, chosen, note);
        }
    }
}

enum Action {
    Burn,
    Retry,
    Restore,
    Close,
}
