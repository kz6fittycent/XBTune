// SPDX-License-Identifier: GPL-3.0-or-later
//! Bike profiles: one per bike a rider tunes. Each has its own folder,
//! `<files>/Bikes/<name>/` with Tunes, Backups, Logs, History (and Remote),
//! and a `bike.toml` holding the bike's details and its [`BikeState`]: how
//! it's reached, the gauges and charts, the tune, map, compare file and
//! logs that were open, the auto-tune choices. Switching profiles saves
//! the one in use and opens the other as it was left.
//!
//! The active profile is named in `<config>/bike.toml`
//! (`xbtune_core::env::active_bike`), so the session's backups and burn
//! history and the CLI follow it too. Without a profile everything works
//! as before, from settings.toml and the files folder itself.
//!
//! On connect the ECM is checked against the profile: a profile without
//! an ECM learns it; a different ECM model gets an offer to switch to a
//! profile for it or make one. Bikes with the same ECM model can't be told
//! apart (the version string is shared), so they never trigger an offer.

use std::path::{Path, PathBuf};

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};
use xbtune_core::env::{self, Dirs};
use xbtune_core::session::Identity;

use crate::app::{View, XbApp};
use crate::settings::BikeState;
use crate::theme::{self, Palette};

/// The profile file in each bike folder.
pub const FILE: &str = "bike.toml";

/// What the rider tells us about the bike, and the ECM it was seen with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BikeInfo {
    pub name: String,
    pub model: String,
    pub year: String,
    pub notes: String,
    /// The ECM's ID and version string, learned on the first connection.
    pub ecm_id: Option<String>,
    pub ecm_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct BikeFile {
    bike: BikeInfo,
    state: BikeState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The folder's name under `Bikes/` (stays when the name changes).
    pub folder: String,
    pub info: BikeInfo,
}

impl Profile {
    pub fn dir(&self, dirs: &Dirs) -> PathBuf {
        dirs.bikes().join(&self.folder)
    }
}

/// Every profile in `bikes`, by name.
pub fn list(bikes: &Path) -> Vec<Profile> {
    let mut out: Vec<Profile> = std::fs::read_dir(bikes)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let folder = e.file_name().to_str()?.to_string();
            let (info, _) = read(&e.path())?;
            Some(Profile { folder, info })
        })
        .collect();
    out.sort_by_key(|p| p.info.name.to_lowercase());
    out
}

fn read(dir: &Path) -> Option<(BikeInfo, BikeState)> {
    let text = std::fs::read_to_string(dir.join(FILE)).ok()?;
    match toml::from_str::<BikeFile>(&text) {
        Ok(f) => Some((f.bike, f.state)),
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "bike profile unreadable");
            None
        }
    }
}

/// Load one profile and its state.
pub fn load(bikes: &Path, folder: &str) -> Option<(Profile, BikeState)> {
    let (info, state) = read(&bikes.join(folder))?;
    Some((Profile { folder: folder.into(), info }, state))
}

/// Save a profile (temp file + rename).
pub fn save(bikes: &Path, p: &Profile, state: &BikeState) -> std::io::Result<()> {
    let dir = bikes.join(&p.folder);
    std::fs::create_dir_all(&dir)?;
    let file = BikeFile { bike: p.info.clone(), state: state.clone() };
    let text = toml::to_string_pretty(&file).map_err(std::io::Error::other)?;
    let path = dir.join(FILE);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, format!("# XBTune bike profile: {}\n{text}", p.info.name))?;
    std::fs::rename(&tmp, &path)
}

/// A folder name for `name` that isn't taken: characters Windows and the
/// shell dislike become '-', and " (2)", " (3)"... make it unique.
pub fn folder_for(bikes: &Path, name: &str) -> String {
    let clean: String =
        name.chars().map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { '-' } else { c }).collect();
    let clean = clean.trim().trim_matches('.').trim();
    let base = if clean.is_empty() { "Bike".to_string() } else { clean.chars().take(60).collect() };
    let mut folder = base.clone();
    let mut n = 2;
    while bikes.join(&folder).exists() {
        folder = format!("{base} ({n})");
        n += 1;
    }
    folder
}

/// Paths under `from` moved to `to` (tune and log paths in a state, after
/// its files moved into a new bike folder).
fn rebase(state: &mut BikeState, from: &Path, to: &Path) {
    let fix = |p: &mut PathBuf| {
        if let Ok(rest) = p.strip_prefix(from)
            && !rest.starts_with(env::BIKES)
        {
            *p = to.join(rest);
        }
    };
    state.last_tune.iter_mut().for_each(fix);
    state.compare.iter_mut().for_each(fix);
    state.logs.iter_mut().for_each(fix);
}

/// The dialog to create or edit a profile.
pub struct Dialog {
    /// `None`: a new profile.
    editing: Option<String>,
    info: BikeInfo,
    /// New profile: start from the current gauges, connection, tune...
    copy_state: bool,
    /// New profile: move the files folder's own Tunes/Backups/Logs/History in.
    move_files: bool,
    /// Files there to move (shown only when some exist).
    root_files: bool,
    /// New profile: keep the live connection (made from the connect offer).
    keep_session: bool,
    /// Placeholder for Model (the bikes the connected ECM shipped on).
    model_hint: String,
    error: Option<String>,
}

/// After connecting: the ECM doesn't fit the profile in use.
pub struct Offer {
    ecm_id: String,
    version: String,
    /// Profiles for this ECM model: (folder, name).
    matches: Vec<(String, String)>,
    /// The profile in use is for another ECM model (else: none in use).
    mismatch: Option<(String, String)>,
}

#[derive(Default)]
pub struct ProfilesUi {
    pub dialog: Option<Dialog>,
    offer: Option<Offer>,
    /// ECM versions already offered this run.
    offered: Vec<String>,
}

/// Open the "new bike profile" dialog.
pub fn new_profile(app: &mut XbApp, keep_session: bool) {
    let Some(d) = Dirs::detect() else { return };
    let mut info = BikeInfo::default();
    if let Some(id) = app.link.identity.as_ref().filter(|_| !app.is_demo()) {
        info.ecm_id = Some(id.ecm.id.clone());
        info.ecm_version = Some(id.version.trim().to_string());
    }
    let model_hint = app
        .link
        .identity
        .as_ref()
        .filter(|_| !app.is_demo())
        .and_then(|id| id.ecm.bikes.clone())
        .map(|b| format!("this ECM shipped on: {b}"))
        .unwrap_or_else(|| "e.g. XB9SX".into());
    let first = list(&d.bikes()).is_empty();
    let root_files = [env::TUNES, env::BACKUPS, env::LOGS, env::HISTORY]
        .iter()
        .any(|f| std::fs::read_dir(d.root.join(f)).is_ok_and(|mut r| r.next().is_some()));
    app.profiles.dialog = Some(Dialog {
        editing: None,
        info,
        copy_state: true,
        move_files: first && root_files && app.profile.is_none(),
        root_files: root_files && app.profile.is_none(),
        keep_session,
        model_hint,
        error: None,
    });
}

fn edit_profile(app: &mut XbApp) {
    let Some(p) = &app.profile else { return };
    app.profiles.dialog = Some(Dialog {
        editing: Some(p.folder.clone()),
        info: p.info.clone(),
        copy_state: false,
        move_files: false,
        root_files: false,
        keep_session: false,
        model_hint: "e.g. XB9SX".into(),
        error: None,
    });
}

/// Why switching now isn't possible, if it isn't.
fn switch_blocked(app: &XbApp) -> Option<&'static str> {
    if app.remote.active() {
        Some("End the remote session first (Remote).")
    } else if app.burn.is_open() {
        Some("Finish the burn first.")
    } else if app.tune_ui.read_progress.is_some() {
        Some("Wait for the ECM read to finish.")
    } else {
        None
    }
}

/// Switch to the profile in `folder` (`None`: no profile). The one in use
/// is saved first; unsaved tune edits go to a recovery file in its folder.
/// `keep_session`: the live connection stays (it's this bike).
pub fn switch(app: &mut XbApp, folder: Option<&str>, keep_session: bool) {
    if let Some(why) = switch_blocked(app) {
        return app.say(why, true);
    }
    let Some(d) = Dirs::detect() else { return };
    if folder == app.profile.as_ref().map(|p| p.folder.as_str()) {
        return;
    }
    if app.tune.as_ref().is_some_and(|t| t.is_modified()) {
        app.save_recovery_now();
        app.say(
            format!("Unsaved tune edits were kept in a \"recovered_\" file in {}.", d.tunes().display()),
            false,
        );
    }
    app.save_settings();
    let next = match folder {
        Some(f) => match load(&d.bikes(), f) {
            Some(x) => Some(x),
            None => {
                return app.say(
                    format!("The bike profile in {} can't be read.", d.bikes().join(f).display()),
                    true,
                );
            }
        },
        None => None,
    };
    if let Err(e) = env::set_active_bike(&d.config, folder) {
        return app.say(format!("Couldn't switch the bike profile: {e}"), true);
    }
    if !keep_session {
        app.stop();
    }
    let target = app.link.target.clone();
    let state = match next {
        Some((p, state)) => {
            if app.profile.is_none() {
                app.general_bike = app.settings.bike.clone();
            }
            app.profile = Some(p);
            state
        }
        None => {
            app.profile = None;
            app.general_bike.clone()
        }
    };
    app.settings.bike = state;
    if keep_session {
        app.settings.bike.last = target;
    }
    reset_views(app);
    restore(app);
    if keep_session {
        record_ecm(app);
        let files = Dirs::detect().map(|d| d.files);
        if let Some(files) = files {
            app.send(xbtune_core::session::Command::SetStore(files));
        }
    }
    app.save_settings();
    let what = match &app.profile {
        Some(p) => format!("Bike profile: {}", p.info.name),
        None => "No bike profile: using the files folder itself.".into(),
    };
    tracing::info!("{what}");
    app.say(what, false);
}

/// Forget what the views showed for the previous bike.
fn reset_views(app: &mut XbApp) {
    crate::logs_view::stop_playback(app);
    app.tune = None;
    app.compare = None;
    app.tune_ui = Default::default();
    app.logs = Default::default();
    app.autotune = Default::default();
    app.setup_ui = Default::default();
    app.connect = crate::connect::ConnectUi::new(app.settings.bike.last.clone());
}

/// Open what the bike state says was open: tune, map, compare file, logs,
/// overlay, auto-tune choices and the view.
pub fn restore(app: &mut XbApp) {
    let s = app.settings.bike.clone();
    if app.tune.is_none()
        && let Some(p) = s.last_tune.as_ref().filter(|p| p.exists())
    {
        crate::tune_view::open(app, p, false);
        app.tune_ui.status = None;
    }
    if let Some(v) = &s.tune_var
        && app.tune.as_ref().is_some_and(|t| t.var(v).is_ok())
    {
        app.tune_ui.selected = Some(v.clone());
    }
    if let Some(p) = s.compare.as_ref().filter(|p| p.exists())
        && app.tune.is_some()
    {
        crate::tune_view::open(app, p, true);
        app.tune_ui.status = None;
    }
    for p in s.logs.iter().filter(|p| p.exists()) {
        crate::logs_view::add(app, p);
    }
    app.autotune.restore(&s.autotune);
    app.autotune.overlay = crate::autotune_view::Overlay::from_key(&s.overlay);
    if let Some(v) = View::from_key(&s.view)
        && !matches!(v, View::Remote | View::Settings)
    {
        app.view = v;
    }
}

/// The views' state now, into `b` (before saving).
pub fn capture(app: &XbApp, b: &mut BikeState) {
    b.view = app.view.key().into();
    b.tune_var = app.tune_ui.selected.clone();
    b.compare = app.compare.as_ref().map(|(p, _)| p.clone());
    b.overlay = app.autotune.overlay.key().into();
    b.logs = app.logs.logs.iter().map(|l| l.log.path.clone()).collect();
    b.autotune = app.autotune.state();
}

/// Save the profile in use with `state`.
pub fn save_active(app: &XbApp, state: &BikeState) {
    let (Some(p), Some(d)) = (&app.profile, Dirs::detect()) else { return };
    if let Err(e) = save(&d.bikes(), p, state) {
        tracing::warn!(error = %e, "could not save the bike profile");
    }
}

/// The profile in use at start (as `<config>/bike.toml` names it).
pub fn load_active() -> Option<(Profile, BikeState)> {
    let d = Dirs::detect()?;
    let folder = env::active_bike(&d.config)?;
    load(&d.bikes(), &folder)
}

/// The profile in use has no ECM yet: this one is it.
fn record_ecm(app: &mut XbApp) {
    let Some(id) = app.link.identity.as_ref() else { return };
    let (ecm, version) = (id.ecm.id.clone(), id.version.trim().to_string());
    let Some(p) = &mut app.profile else { return };
    if p.info.ecm_id.is_none() || p.info.ecm_id.as_deref() == Some(ecm.as_str()) {
        p.info.ecm_id = Some(ecm);
        p.info.ecm_version = Some(version);
    }
}

/// The ECM answered: check it against the profile in use.
pub fn on_identified(app: &mut XbApp, id: &Identity) {
    if app.is_screenshot() || app.link.target.as_ref().is_some_and(|t| t.is_demo()) {
        return;
    }
    let Some(d) = Dirs::detect() else { return };
    let version = id.version.trim().to_string();
    let ecm_id = id.ecm.id.clone();
    let others: Vec<(String, String)> = list(&d.bikes())
        .into_iter()
        .filter(|p| p.info.ecm_id.as_deref() == Some(ecm_id.as_str()))
        .filter(|p| app.profile.as_ref().is_none_or(|a| a.folder != p.folder))
        .map(|p| (p.folder, p.info.name))
        .collect();
    let mismatch = match &app.profile {
        Some(p) => match p.info.ecm_id.as_deref() {
            None => {
                record_ecm(app);
                app.save_settings();
                return;
            }
            Some(e) if e == ecm_id => {
                if p.info.ecm_version.as_deref() != Some(version.as_str()) {
                    record_ecm(app);
                    app.save_settings();
                }
                return;
            }
            Some(e) => Some((p.info.name.clone(), e.to_string())),
        },
        None if others.is_empty() && !app.settings.profile_offer => return,
        None => None,
    };
    if app.profiles.offered.contains(&version) {
        return;
    }
    app.profiles.offered.push(version.clone());
    app.profiles.offer = Some(Offer { ecm_id, version, matches: others, mismatch });
}

/// UI checks: the offer as a BUEIB with a profile for a BUEGC in use.
pub fn shot_offer(app: &mut XbApp) {
    app.profiles.offer = Some(Offer {
        ecm_id: "BUEIB".into(),
        version: "BUEIB310 12-11-03".into(),
        matches: vec![("XB9SX".into(), "Jim's XB9SX".into())],
        mismatch: Some(("Tube frame X1".into(), "BUEGC".into())),
    });
}

/// The offer above every view after connecting.
pub fn banner(app: &mut XbApp, ui: &mut Ui) {
    let Some(o) = &app.profiles.offer else { return };
    let p = Palette::for_visuals(ui.visuals());
    let mut action: Option<Action> = None;
    theme::card(ui).stroke(egui::Stroke::new(1.0, p.accent)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let title = match &o.mismatch {
            Some((name, theirs)) => format!(
                "This ECM is a {} ({}), but the bike profile \u{201c}{name}\u{201d} is for a {theirs}.",
                o.ecm_id, o.version
            ),
            None if o.matches.is_empty() => {
                format!("Connected to a {} ({}). Save this bike as a profile?", o.ecm_id, o.version)
            }
            None => format!("Connected to a {} ({}). Use its bike profile?", o.ecm_id, o.version),
        };
        ui.label(RichText::new(title).font(theme::strong_font(15.0)));
        ui.label(
            RichText::new(
                "A bike profile keeps this bike's tunes, backups and logs in their own folder and \
                 reopens everything as you left it.",
            )
            .color(p.muted),
        );
        ui.horizontal_wrapped(|ui| {
            for (folder, name) in o.matches.iter().take(4) {
                if ui
                    .add(theme::primary(crate::icons::with(crate::icons::BIKE, &format!("Use {name}"))))
                    .clicked()
                {
                    action = Some(Action::Use(folder.clone()));
                }
            }
            let new = crate::icons::with(crate::icons::PLUS, "New bike profile…");
            let b = if o.matches.is_empty() { theme::primary(new) } else { egui::Button::new(new) };
            if ui.add(b).clicked() {
                action = Some(Action::New);
            }
            let keep = match &o.mismatch {
                Some((name, _)) => format!("Keep \u{201c}{name}\u{201d}"),
                None => "Not now".into(),
            };
            if ui.button(keep).clicked() {
                action = Some(Action::Dismiss);
            }
            if o.mismatch.is_none()
                && o.matches.is_empty()
                && ui.button("Don't ask again").on_hover_text("Bike profiles stay in the side bar").clicked()
            {
                action = Some(Action::Never);
            }
        });
    });
    ui.add_space(8.0);
    match action {
        Some(Action::Use(folder)) => {
            app.profiles.offer = None;
            switch(app, Some(&folder), true);
        }
        Some(Action::New) => {
            app.profiles.offer = None;
            new_profile(app, true);
        }
        Some(Action::Dismiss) => app.profiles.offer = None,
        Some(Action::Never) => {
            app.profiles.offer = None;
            app.settings.profile_offer = false;
            app.save_settings();
        }
        None => {}
    }
}

enum Action {
    Use(String),
    New,
    Dismiss,
    Never,
}

/// The profile picker at the top of the side bar.
pub fn picker(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    let bikes = Dirs::detect().map(|d| list(&d.bikes())).unwrap_or_default();
    let current = app.profile.as_ref().map(|p| p.info.name.clone());
    let label = current.clone().unwrap_or_else(|| "No bike profile".into());
    let mut choice: Option<Option<String>> = None;
    let mut new = false;
    let mut edit = false;
    ui.horizontal(|ui| {
        ui.add_space(4.0);
        let text = RichText::new(crate::icons::with(crate::icons::BIKE, &label))
            .color(if current.is_some() { ui.visuals().strong_text_color() } else { p.muted });
        ui.menu_button(text, |ui| {
            ui.set_min_width(220.0);
            for b in &bikes {
                let on = app.profile.as_ref().is_some_and(|a| a.folder == b.folder);
                let mut text = b.info.name.clone();
                if let Some(e) = &b.info.ecm_id {
                    text.push_str(&format!("  ·  {e}"));
                }
                if ui.selectable_label(on, text).clicked() {
                    choice = Some(Some(b.folder.clone()));
                }
            }
            if !bikes.is_empty() {
                ui.separator();
            }
            if ui.button(crate::icons::with(crate::icons::PLUS, "New bike profile…")).clicked() {
                new = true;
            }
            if app.profile.is_some() {
                if ui.button(crate::icons::with(crate::icons::EDIT, "Edit this profile…")).clicked() {
                    edit = true;
                }
                if ui
                    .button("No profile")
                    .on_hover_text("Use the files folder itself, as before profiles")
                    .clicked()
                {
                    choice = Some(None);
                }
            }
        })
        .response
        .on_hover_text(match &app.profile {
            Some(b) => profile_tip(b),
            None => "Bike profiles keep each bike's files and views apart".into(),
        });
    });
    if let Some(c) = choice {
        // Same bike's ECM still connected? Keep the link only when it fits.
        switch(app, c.as_deref(), false);
    }
    if new {
        let live = app.link.live();
        new_profile(app, live);
    }
    if edit {
        edit_profile(app);
    }
}

fn profile_tip(b: &Profile) -> String {
    let mut lines = vec![b.info.name.clone()];
    let bike = [b.info.year.as_str(), b.info.model.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>();
    if !bike.is_empty() {
        lines.push(bike.join(" "));
    }
    if let Some(v) = &b.info.ecm_version {
        lines.push(format!("ECM {v}"));
    }
    if let Some(d) = Dirs::detect() {
        lines.push(b.dir(&d).display().to_string());
    }
    lines.join("\n")
}

/// The create/edit dialog.
pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let Some(dlg) = &mut app.profiles.dialog else { return };
    let p = Palette::for_visuals(ui.visuals());
    let Some(d) = Dirs::detect() else { return };
    let mut ok = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("bike-profile")).show(ui.ctx(), |ui| {
        ui.set_width(500.0);
        let title = if dlg.editing.is_some() { "Edit bike profile" } else { "New bike profile" };
        ui.label(RichText::new(title).font(theme::strong_font(19.0)));
        ui.label(
            RichText::new(
                "Each bike gets its own folder for tunes, backups, logs and burn history, and XBTune \
                 reopens it as you left it: the tune and map, logs, gauges, connection.",
            )
            .color(p.muted),
        );
        ui.add_space(6.0);
        egui::Grid::new("bike-fields").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut dlg.info.name)
                    .hint_text("e.g. Dave's XB12R")
                    .desired_width(300.0),
            );
            ui.end_row();
            ui.label("Model");
            ui.add(
                egui::TextEdit::singleline(&mut dlg.info.model)
                    .hint_text(dlg.model_hint.as_str())
                    .desired_width(300.0),
            );
            ui.end_row();
            ui.label("Year");
            ui.add(egui::TextEdit::singleline(&mut dlg.info.year).desired_width(80.0));
            ui.end_row();
            ui.label("Notes");
            ui.add(
                egui::TextEdit::multiline(&mut dlg.info.notes)
                    .hint_text("exhaust, intake, owner, ...")
                    .desired_rows(3)
                    .desired_width(300.0),
            );
            ui.end_row();
            ui.label("ECM");
            let ecm = match (&dlg.info.ecm_id, &dlg.info.ecm_version) {
                (_, Some(v)) => v.clone(),
                (Some(id), None) => id.clone(),
                _ => "learned when you connect to it".into(),
            };
            ui.label(RichText::new(ecm).color(p.muted));
            ui.end_row();
        });
        if dlg.editing.is_none() {
            ui.add_space(6.0);
            ui.checkbox(
                &mut dlg.copy_state,
                "Start with the current settings (connection, gauges, open tune and logs)",
            );
            if dlg.root_files {
                ui.checkbox(
                    &mut dlg.move_files,
                    format!(
                        "Move the tunes, backups, logs and history in {} into this bike's folder",
                        d.root.display()
                    ),
                )
                .on_hover_text("Leave it unticked if those files are from other bikes");
            }
        }
        if let Some(e) = &dlg.error {
            ui.label(RichText::new(e).color(p.bad));
        }
        ui.add_space(8.0);
        let main = theme::primary(if dlg.editing.is_some() { "Save" } else { "Create" });
        let (m, c) = theme::dialog_buttons(ui, main, !dlg.info.name.trim().is_empty(), "Cancel");
        ok = m;
        cancel = c;
    });
    if ok {
        finish(app);
    } else if cancel || modal.should_close() {
        app.profiles.dialog = None;
    }
}

fn finish(app: &mut XbApp) {
    let Some(d) = Dirs::detect() else { return };
    let Some(mut dlg) = app.profiles.dialog.take() else { return };
    dlg.info.name = dlg.info.name.trim().to_string();
    if let Some(folder) = &dlg.editing {
        if let Some(p) = app.profile.as_mut().filter(|p| &p.folder == folder) {
            p.info = dlg.info;
        }
        app.save_settings();
        return;
    }
    if let Some(why) = switch_blocked(app) {
        dlg.error = Some(why.into());
        app.profiles.dialog = Some(dlg);
        return;
    }
    let folder = folder_for(&d.bikes(), &dlg.info.name);
    let profile = Profile { folder: folder.clone(), info: dlg.info.clone() };
    let mut state = if dlg.copy_state {
        let mut s = app.settings.bike.clone();
        capture(app, &mut s);
        s
    } else {
        BikeState { last: app.link.target.clone().filter(|_| dlg.keep_session), ..Default::default() }
    };
    let dir = profile.dir(&d);
    if dlg.move_files {
        let moved = env::move_files(
            &[env::TUNES, env::BACKUPS, env::LOGS, env::HISTORY]
                .iter()
                .map(|f| (d.root.join(f), dir.join(f)))
                .collect::<Vec<_>>(),
        );
        for (path, why) in &moved.skipped {
            tracing::warn!(path = %path.display(), why, "file not moved");
        }
        rebase(&mut state, &d.root, &dir);
        app.say(format!("Moved {} file(s) into {}", moved.files, dir.display()), !moved.skipped.is_empty());
    }
    if let Err(e) = save(&d.bikes(), &profile, &state) {
        dlg.error = Some(format!("Couldn't create {}: {e}", dir.display()));
        app.profiles.dialog = Some(dlg);
        return;
    }
    tracing::info!(folder, "bike profile created");
    switch(app, Some(&folder), dlg.keep_session && app.link.live());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xbtune-bikes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn folder_names_are_safe_and_unique() {
        let bikes = scratch("names");
        assert_eq!(folder_for(&bikes, "Dave's XB12R"), "Dave's XB12R");
        assert_eq!(folder_for(&bikes, "a/b:c*?"), "a-b-c--");
        assert_eq!(folder_for(&bikes, "  ..  "), "Bike");
        std::fs::create_dir_all(bikes.join("XB9")).unwrap();
        std::fs::create_dir_all(bikes.join("XB9 (2)")).unwrap();
        assert_eq!(folder_for(&bikes, "XB9"), "XB9 (3)");
        let _ = std::fs::remove_dir_all(&bikes);
    }

    #[test]
    fn profiles_round_trip_and_list_by_name() {
        let bikes = scratch("list");
        let mut state = BikeState { tune_var: Some("Tab_Spark_Rear".into()), ..Default::default() };
        state.logs.push("/x/Logs/a.csv".into());
        for name in ["zed", "Alpha"] {
            let p =
                Profile { folder: name.into(), info: BikeInfo { name: name.into(), ..Default::default() } };
            save(&bikes, &p, &state).unwrap();
        }
        std::fs::create_dir_all(bikes.join("not a profile")).unwrap();
        let names: Vec<_> = list(&bikes).into_iter().map(|p| p.info.name).collect();
        assert_eq!(names, ["Alpha", "zed"]);
        let (p, back) = load(&bikes, "zed").unwrap();
        assert_eq!(p.info.name, "zed");
        assert_eq!(back, state);
        let _ = std::fs::remove_dir_all(&bikes);
    }

    #[test]
    fn moved_paths_follow_into_the_bike_folder() {
        let mut s = BikeState {
            last_tune: Some("/r/Tunes/a.xbt".into()),
            compare: Some("/elsewhere/b.xbt".into()),
            logs: vec!["/r/Logs/l.csv".into(), "/r/Bikes/Other/Logs/m.csv".into()],
            ..Default::default()
        };
        rebase(&mut s, Path::new("/r"), Path::new("/r/Bikes/Mine"));
        assert_eq!(s.last_tune.unwrap(), Path::new("/r/Bikes/Mine/Tunes/a.xbt"));
        assert_eq!(s.compare.unwrap(), Path::new("/elsewhere/b.xbt"));
        assert_eq!(s.logs[0], Path::new("/r/Bikes/Mine/Logs/l.csv"));
        assert_eq!(s.logs[1], Path::new("/r/Bikes/Other/Logs/m.csv"));
    }
}
