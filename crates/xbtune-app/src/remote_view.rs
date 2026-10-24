// SPDX-License-Identifier: GPL-3.0-or-later
//! The Remote view (phase 8, docs/REMOTE.md): host a session for a tuner
//! or join one with the code; chat; the host's permissions, the tuner's
//! proposals and requests, and the countdown for tuner-started actions.
//! The logic is in `remote_sync.rs`.

use std::collections::HashSet;
use std::time::{Instant, SystemTime};

use egui::{RichText, Ui};
use xbtune_remote::code::{self, Code};
use xbtune_remote::msg::{ReqState, RequestKind};
use xbtune_remote::session::{self, Command, Config, Event, Remote, Start};

use crate::app::{View, XbApp};
use crate::remote_sync::{self as sync, GuestSide, HostSide, Perms};
use crate::theme::{self, Palette};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Connecting,
    Waiting,
    Paired,
    PeerGone,
    Reconnecting(String),
}

struct Line {
    mine: bool,
    text: String,
    at: SystemTime,
}

#[derive(Default)]
pub struct RemoteUi {
    session: Option<Remote>,
    hosting: bool,
    code: Option<Code>,
    status: Option<Status>,
    peer: Option<String>,
    chat: Vec<Line>,
    input: String,
    join_code: String,
    join_error: Option<String>,
    guide_open: bool,
    /// Shown automatically once per app start (until "Don't show again").
    guide_shown: bool,
    guide_dont_show: bool,
    /// UI checks: sent once paired.
    queued: Option<String>,
    /// Host: request prompts put aside with "Later".
    later: HashSet<u64>,
    /// UI checks (`XBTUNE_SHOT_SCRIPT`): steps run in order once possible.
    script: std::collections::VecDeque<String>,
    script_wait: Option<Instant>,
    pub host: HostSide,
    pub guest: GuestSide,
}

impl RemoteUi {
    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    pub fn hosting(&self) -> bool {
        self.hosting
    }

    pub fn paired(&self) -> bool {
        matches!(self.status, Some(Status::Paired))
    }

    pub fn peer(&self) -> Option<String> {
        self.peer.clone()
    }

    pub fn code(&self) -> Option<Code> {
        self.code.clone()
    }

    pub fn session(&self) -> Option<&Remote> {
        self.session.as_ref()
    }
}

/// End the session (app closing): the other side is told at once.
pub fn end(app: &mut XbApp) {
    if let Some(s) = &app.remote.session {
        s.send(Command::End);
        // Give the task a moment to send `bye` before the runtime stops.
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

pub fn open_guide(app: &mut XbApp) {
    app.remote.guide_open = true;
}

/// UI checks: steps separated by `;`, each run once it can be:
/// `wait:<s>`, `perms:edit,burn,tests,nosave`, `set:VAR=VALUE` (tuner edits
/// the shared tune), `fn:<test label>`, `burn[:note]`, `afv`, `confirm`
/// (tuner confirms a burn review), `accept` (host accepts all proposals),
/// `review` (host reviews the next request), `write` (host confirms its
/// burn dialog), `stop` (host stops the countdown), `share-log`.
pub fn set_script(app: &mut XbApp, script: &str) {
    app.remote.script = script.split(';').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
}

fn run_script(app: &mut XbApp) {
    let Some(step) = app.remote.script.front().cloned() else { return };
    if !app.remote.paired() && !step.starts_with("perms:") {
        return;
    }
    let done = match step.split_once(':').unwrap_or((step.as_str(), "")) {
        ("wait", s) => {
            let until = *app.remote.script_wait.get_or_insert_with(|| {
                Instant::now() + std::time::Duration::from_secs_f64(s.parse().unwrap_or(1.0))
            });
            Instant::now() >= until
        }
        ("perms", list) => {
            let mut p = Perms::default();
            for x in list.split(',') {
                match x {
                    "edit" => p.edit = true,
                    "burn" => p.burn = true,
                    "tests" => p.tests = true,
                    "nosave" => p.allow_save = false,
                    _ => {}
                }
            }
            sync::set_perms(app, p);
            true
        }
        ("set", kv) => match (&mut app.tune, kv.split_once('=')) {
            (Some(t), Some((k, v))) if app.remote.guest.following => {
                let _ = t.set(format!("{k} = {v}"), &[(k.to_string(), 0, v.parse().unwrap_or(0.0))]);
                true
            }
            _ => false,
        },
        ("fn", name) if app.link.live() => {
            sync::request(app, RequestKind::Function { name: name.into() }, String::new());
            true
        }
        ("burn", note) if app.link.live() && app.remote.guest.following => {
            sync::request(app, RequestKind::Burn, note.into());
            true
        }
        ("afv", _) if app.link.live() => {
            sync::request(app, RequestKind::AfvReset, String::new());
            true
        }
        ("confirm", _) => match &mut app.remote.guest.review {
            Some((_, _, ticked)) => {
                *ticked = true;
                sync::confirm_review(app);
                true
            }
            None => false,
        },
        ("accept", _) => match app.remote.host.proposals.first().map(|p| p.id) {
            Some(id) => {
                sync::accept(app, id);
                app.remote.host.proposals.is_empty()
            }
            None => false,
        },
        ("review", _) => match app.remote.host.requests.front().map(|r| r.id) {
            Some(id) => {
                sync::review(app, id);
                true
            }
            None => false,
        },
        ("write", _) if matches!(app.burn, crate::burn_view::BurnUi::Review { .. }) => {
            crate::burn_view::auto_confirm(app);
            true
        }
        ("run", _) if app.diag.confirm.is_some() => {
            // As if the host ticked the safety box and pressed Run.
            let f = app.diag.confirm.take().expect("checked");
            app.diag.running = Some((f, None));
            app.diag.last = None;
            app.send(xbtune_core::session::Command::Function(f));
            true
        }
        ("drop", _) => {
            if let Some(s) = &app.remote.session {
                s.send(Command::SimulateDrop);
            }
            true
        }
        ("engine-off", _) => {
            app.set_demo_engine_running(false);
            true
        }
        ("stop", _) if app.remote.host.countdown.is_some() => {
            sync::stop_countdown(app);
            true
        }
        ("share-log", _) => match app.logs.logs.first().map(|l| l.log.path.clone()) {
            Some(p) => {
                sync::share_log(app, &p);
                true
            }
            None => false,
        },
        _ => false,
    };
    if done {
        tracing::info!(%step, "remote script step done");
        app.remote.script.pop_front();
        app.remote.script_wait = None;
    }
}

/// UI checks: a chat line to send as soon as the session is paired.
pub fn queue_chat(app: &mut XbApp, text: String) {
    app.remote.queued = Some(text);
}

fn config(app: &XbApp) -> Config {
    let name = app.settings.remote_name.trim();
    let name = if name.is_empty() { if app.remote.hosting { "Bike owner" } else { "Tuner" } } else { name };
    Config::new(xbtune_remote::relay_url(), name, format!("XBTune {}", crate::report::version()))
}

fn start(app: &mut XbApp, how: Start) {
    app.remote.hosting = matches!(how, Start::Host);
    let cfg = config(app);
    let ctx = app.egui_ctx().clone();
    let notify: session::Notify = std::sync::Arc::new(move || ctx.request_repaint());
    let _guard = app.rt.enter();
    tracing::info!(host = app.remote.hosting, relay = %cfg.relay, "remote session starting");
    app.remote.session = Some(session::start(cfg, how, Some(notify)));
    app.remote.status = Some(Status::Connecting);
    app.remote.code = None;
    app.remote.peer = None;
    app.remote.chat.clear();
    app.remote.join_error = None;
    app.remote.later.clear();
    app.remote.host = HostSide::default();
    app.remote.guest = GuestSide::default();
}

pub fn host(app: &mut XbApp) {
    start(app, Start::Host);
}

pub fn join(app: &mut XbApp, text: &str) {
    match code::parse(text) {
        Ok(c) => start(app, Start::Join(c)),
        Err(e) => app.remote.join_error = Some(e),
    }
}

/// Collect session events and keep both sides in step. Called every frame.
pub fn poll(app: &mut XbApp) {
    run_script(app);
    // Our own edits go out before the other side's updates come in.
    sync::tick(app);
    let Some(s) = &mut app.remote.session else { return };
    let mut events = Vec::new();
    while let Ok(e) = s.events.try_recv() {
        events.push(e);
    }
    for e in events {
        match e {
            Event::Code(c) => {
                if app.is_screenshot() {
                    // UI checks only: the other test instance reads it.
                    println!("XBTUNE_REMOTE_CODE={c}");
                }
                app.remote.code = Some(c)
            }
            Event::Waiting => app.remote.status = Some(Status::Waiting),
            Event::Paired { peer, resumed, .. } => {
                if !resumed {
                    app.say(
                        format!("Connected to {peer}. Everything between you is end-to-end encrypted."),
                        false,
                    );
                }
                app.remote.peer = Some(peer);
                app.remote.status = Some(Status::Paired);
                if let (Some(t), Some(s)) = (app.remote.queued.take(), &app.remote.session) {
                    s.send(Command::Chat(t));
                }
                sync::on_paired(app);
            }
            Event::PeerGone => {
                app.remote.status = Some(Status::PeerGone);
                sync::on_gone(app, false);
            }
            Event::Chat { mine, text } => app.remote.chat.push(Line { mine, text, at: SystemTime::now() }),
            Event::Message(m) => sync::on_message(app, m),
            Event::Reconnecting { reason } => app.remote.status = Some(Status::Reconnecting(reason)),
            Event::Ended { reason, by_peer } => {
                let error = !by_peer && !reason.starts_with("You ended");
                app.say(format!("Remote session ended: {reason}"), error);
                sync::on_gone(app, true);
                app.remote.session = None;
                app.remote.status = None;
                app.remote.code = None;
                app.remote.later.clear();
                return;
            }
        }
    }
}

fn send_chat(app: &mut XbApp) {
    let text = app.remote.input.trim().to_string();
    if text.is_empty() {
        return;
    }
    if let Some(s) = &app.remote.session {
        s.send(Command::Chat(text));
    }
    app.remote.input.clear();
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    if !app.settings.remote_guide_seen && !app.remote.guide_shown && !app.is_screenshot() {
        app.remote.guide_open = true;
        app.remote.guide_shown = true;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Remote tuning");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("How it works").clicked() {
                    app.remote.guide_open = true;
                }
            });
        });
        ui.label(
            RichText::new(
                "Work on a tune with a tuner somewhere else. Everything between the two apps is end-to-end \
                 encrypted.",
            )
            .color(p.muted),
        );
        ui.add_space(8.0);
        if app.remote.session.is_some() {
            session_ui(app, ui, &p);
        } else {
            start_ui(app, ui, &p);
        }
    });
    guide(app, ui);
}

fn start_ui(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("Your name").font(theme::strong_font(15.0)));
            let r = ui.add(
                egui::TextEdit::singleline(&mut app.settings.remote_name)
                    .hint_text("shown to the other side")
                    .char_limit(xbtune_remote::msg::MAX_NAME)
                    .desired_width(220.0),
            );
            if r.lost_focus() {
                app.save_settings();
            }
        });
    });
    ui.add_space(8.0);
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new("I have the bike").font(theme::strong_font(16.0)));
                ui.label(
                    RichText::new("Start a session, then read the code to your tuner over the phone.")
                        .color(p.muted),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Start session").clicked() {
                    host(app);
                }
            });
        });
    });
    ui.add_space(8.0);
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("I'm the tuner").font(theme::strong_font(16.0)));
        ui.label(RichText::new("Type the code the bike owner reads to you.").color(p.muted));
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut app.remote.join_code)
                    .hint_text("47-381926")
                    .font(theme::strong_font(18.0))
                    .desired_width(160.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if r.changed() {
                app.remote.join_error = None;
            }
            if ui.button("Join").clicked() || enter {
                let c = app.remote.join_code.clone();
                join(app, &c);
            }
        });
        if let Some(e) = &app.remote.join_error {
            ui.colored_label(p.bad, e);
        }
    });
    if let Some(dir) = sync::last_journal() {
        ui.add_space(8.0);
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Last remote session").font(theme::strong_font(15.0)));
                    ui.label(
                        RichText::new(format!(
                            "Every edit is kept in {}. Open it to get the tune as it was at the end (for example \
                             after a crash), then save it.",
                            dir.display()
                        ))
                        .small()
                        .color(p.muted),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Open that tune").clicked() {
                        match sync::rebuild_journal(&dir, &app.catalog) {
                            Ok((img, n)) => match xbtune_core::tune::Tune::new(img, &app.catalog) {
                                Ok(t) => {
                                    app.save_recovery_now();
                                    app.tune = Some(t);
                                    app.view = View::Tune;
                                    app.say(format!("Opened the remote session's tune ({n} edits). Save it to keep it."), false);
                                }
                                Err(e) => app.say(format!("Couldn't open it: {e}"), true),
                            },
                            Err(e) => app.say(format!("Couldn't read the journal: {e}"), true),
                        }
                    }
                });
            });
        });
    }
}

/// Local wall-clock time, "14:05".
pub fn hhmm(t: SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: localtime_r/_s only write the `tm` we pass; zeroed is a valid tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    #[cfg(unix)]
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    #[cfg(windows)]
    let ok = unsafe { libc::localtime_s(&mut tm, &secs) == 0 };
    if ok { format!("{:02}:{:02}", tm.tm_hour, tm.tm_min) } else { String::new() }
}

fn dot(ui: &mut Ui, color: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), 5.0, color);
}

fn session_ui(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    let status = app.remote.status.clone();
    let paired = matches!(status, Some(Status::Paired));
    let peer = app.remote.peer.clone().unwrap_or_else(|| "Them".into());
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let (color, text) = match &status {
                    Some(Status::Paired) => (p.good, format!("Connected to {peer} · end-to-end encrypted")),
                    Some(Status::PeerGone) => {
                        (p.warn, format!("{peer} dropped out. Waiting for them to come back (up to 30 minutes)…"))
                    }
                    Some(Status::Reconnecting(r)) => (p.warn, format!("Connection lost, reconnecting… ({r})")),
                    Some(Status::Waiting) if app.remote.hosting => (p.warn, "Waiting for your tuner to join…".into()),
                    Some(Status::Waiting) => (p.warn, "Connecting to the bike owner…".into()),
                    Some(Status::Connecting) | None => (p.muted, "Connecting to the relay…".into()),
                };
                ui.horizontal(|ui| {
                    dot(ui, color);
                    ui.label(RichText::new(text).font(theme::strong_font(15.0)));
                });
                if app.remote.hosting
                    && let Some(c) = &app.remote.code
                    && app.remote.peer.is_none()
                {
                    ui.add_space(6.0);
                    ui.label("Read this code to your tuner:");
                    ui.label(RichText::new(c.to_string()).font(theme::strong_font(34.0)).color(p.accent));
                    ui.label(
                        RichText::new(
                            "Only to your tuner, by phone or in person. A wrong code ends the session; then start \
                             a new one.",
                        )
                        .small()
                        .color(p.muted),
                    );
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                if ui.button("End session").clicked()
                    && let Some(s) = &app.remote.session
                {
                    s.send(Command::End);
                }
            });
        });
    });
    ui.add_space(8.0);
    if app.remote.peer.is_some() {
        if app.remote.hosting {
            host_cards(app, ui, p, &peer);
        } else {
            guest_cards(app, ui, p, &peer);
        }
    }
    chat_card(app, ui, p, &peer, paired);
}

fn host_cards(app: &mut XbApp, ui: &mut Ui, p: &Palette, peer: &str) {
    // Permissions.
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(format!("What {peer} may do")).font(theme::strong_font(16.0)));
        ui.label(
            RichText::new(
                "Off: they ask, you review in the normal dialog. On: they do it themselves; burns, tests and resets \
                 still show here with a 10 s countdown you can stop. Everything is off again in the next session.",
            )
            .small()
            .color(p.muted),
        );
        let mut perms: Perms = app.remote.host.perms;
        ui.checkbox(&mut perms.edit, "Edit the tune directly (still undoable here)");
        let can_burn = app.writes_permitted();
        ui.add_enabled(can_burn, egui::Checkbox::new(&mut perms.burn, "Burn to the ECM (incl. AFV reset)"))
            .on_disabled_hover_text("Unlock writing to the ECM in Settings first");
        ui.checkbox(&mut perms.tests, "Run tests and resets, clear codes");
        ui.checkbox(&mut perms.allow_save, "Save a copy of the tune on their computer");
        if perms != app.remote.host.perms {
            sync::set_perms(app, perms);
        }
    });
    ui.add_space(8.0);
    // What's shared.
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Shared with them").font(theme::strong_font(16.0)));
        let live = match (&app.link.identity, app.link.live()) {
            (Some(id), true) => format!("Live data: {} at {:.1} Hz", id.version.trim(), app.link.hz),
            _ => "Live data: connect to the bike (Connect) to share it".into(),
        };
        ui.label(live);
        let tune = match &app.tune {
            Some(t) => format!(
                "Tune: {} (the one open in the Tune view)",
                t.path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map_or("unsaved tune".into(), |n| n.to_string_lossy().into_owned())
            ),
            None => "Tune: open one in the Tune view (or Read from ECM) to share it".into(),
        };
        ui.label(tune);
        ui.label(RichText::new("Logs: Logs view, Send to tuner").color(p.muted));
    });
    // Proposals.
    if !app.remote.host.proposals.is_empty() {
        ui.add_space(8.0);
        let mut act: Vec<(u64, bool)> = Vec::new();
        theme::card(ui).stroke(egui::Stroke::new(1.0, p.accent)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{peer}'s proposed edits")).font(theme::strong_font(16.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Reject all").clicked() {
                        act.extend(app.remote.host.proposals.iter().map(|x| (x.id, false)));
                    }
                    if ui.button("Accept all").clicked() {
                        act.extend(app.remote.host.proposals.iter().map(|x| (x.id, true)));
                    }
                });
            });
            ui.label(
                RichText::new(
                    "Accepted edits change your open tune only (undo works); nothing goes to the ECM.",
                )
                .small()
                .color(p.muted),
            );
            for x in &app.remote.host.proposals {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&x.label).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Reject").clicked() {
                            act.push((x.id, false));
                        }
                        if ui.small_button("Accept").clicked() {
                            act.push((x.id, true));
                        }
                    });
                });
                for l in x.lines.iter().take(8) {
                    ui.label(RichText::new(l).small());
                }
                if x.lines.len() > 8 {
                    ui.label(
                        RichText::new(format!("… and {} more", x.lines.len() - 8)).small().color(p.muted),
                    );
                }
            }
        });
        for (id, yes) in act {
            if yes { sync::accept(app, id) } else { sync::reject(app, id) }
        }
    }
    // Requests waiting.
    if !app.remote.host.requests.is_empty() {
        ui.add_space(8.0);
        let mut act: Vec<(u64, bool)> = Vec::new();
        theme::card(ui).stroke(egui::Stroke::new(1.0, p.warn)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("{peer} asks you to")).font(theme::strong_font(16.0)));
            for r in &app.remote.host.requests {
                ui.horizontal(|ui| {
                    ui.label(sync::describe(&r.kind));
                    if !r.note.is_empty() {
                        ui.label(RichText::new(format!("\"{}\"", r.note)).color(p.muted));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Decline").clicked() {
                            act.push((r.id, false));
                        }
                        if ui.small_button(review_label(&r.kind)).clicked() {
                            act.push((r.id, true));
                        }
                    });
                });
            }
        });
        for (id, yes) in act {
            if yes { sync::review(app, id) } else { sync::decline(app, id) }
        }
    }
    ui.add_space(8.0);
}

fn review_label(kind: &RequestKind) -> &'static str {
    match kind {
        RequestKind::Record { .. } => "Allow",
        _ => "Review…",
    }
}

fn guest_cards(app: &mut XbApp, ui: &mut Ui, p: &Palette, peer: &str) {
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(format!("{peer}'s bike")).font(theme::strong_font(16.0)));
        let g = &app.remote.guest;
        match (&g.version, app.link.live()) {
            (Some(v), true) => ui.label(format!(
                "Live: {} at {:.1} Hz. Dashboard, Live data, Charts and the Tune view's live point show it.",
                v.trim(),
                app.link.hz
            )),
            (Some(v), false) => ui.label(format!("{}: their link to the bike is down right now.", v.trim())),
            (None, _) => ui.label(RichText::new("Not connected to the bike right now.").color(p.muted)),
        };
        if g.following {
            ui.label(format!(
                "Tune: {} (Tune view). {}",
                g.tune_name,
                if g.allow_save { "You may save a copy." } else { "Saving a copy is off." }
            ));
        } else if g.tune_name.is_empty() {
            ui.label(RichText::new("No tune shared yet.").color(p.muted));
        }
        let perms = g.perms;
        let mut can = Vec::new();
        if perms.edit {
            can.push("edit the tune directly");
        }
        if perms.burn {
            can.push("burn");
        }
        if perms.tests {
            can.push("run tests and resets");
        }
        ui.label(
            RichText::new(if can.is_empty() {
                format!("{peer} reviews your edits and requests.")
            } else {
                format!("{peer} lets you {}; they see a 10 s countdown and can stop it.", can.join(", "))
            })
            .color(p.muted),
        );
        let recording = g.host_recording;
        let back = !g.following && !g.tune_name.is_empty();
        ui.horizontal(|ui| {
            if back && ui.button("Back to the shared tune").clicked() {
                sync::back_to_shared(app);
            }
            let (label, start) =
                if recording { ("Ask to stop recording", false) } else { ("Ask to record a log", true) };
            if app.link.identity.is_some() && ui.button(label).clicked() {
                sync::request(app, RequestKind::Record { start }, String::new());
            }
        });
    });
    if !app.remote.guest.requests.is_empty() {
        ui.add_space(8.0);
        let mut cancel = None;
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Your requests").font(theme::strong_font(16.0)));
            for r in app.remote.guest.requests.iter().rev().take(8) {
                ui.horizontal(|ui| {
                    let text = match r.state {
                        None => "sent".into(),
                        Some(s) => sync::state_text(s, &r.text),
                    };
                    ui.label(RichText::new(&r.what).strong());
                    ui.label(RichText::new(text).color(p.muted));
                    let open = matches!(
                        r.state,
                        None | Some(ReqState::Shown | ReqState::Countdown | ReqState::Preparing)
                    );
                    if open && ui.small_button("Cancel").clicked() {
                        cancel = Some(r.id);
                    }
                });
            }
        });
        if let Some(id) = cancel {
            sync::cancel_request(app, id);
        }
    }
    ui.add_space(8.0);
}

fn chat_card(app: &mut XbApp, ui: &mut Ui, p: &Palette, peer: &str, paired: bool) {
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Chat").font(theme::strong_font(16.0)));
        egui::ScrollArea::vertical().max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if app.remote.chat.is_empty() {
                ui.label(RichText::new("No messages yet.").color(p.muted));
            }
            for l in &app.remote.chat {
                let who = if l.mine { "You" } else { peer };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(hhmm(l.at)).small().color(p.muted));
                    ui.label(RichText::new(format!("{who}:")).strong().color(if l.mine {
                        p.muted
                    } else {
                        p.accent
                    }));
                    ui.label(&l.text);
                });
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let r = ui.add_enabled(
                paired,
                egui::TextEdit::singleline(&mut app.remote.input)
                    .hint_text(if paired { "Message" } else { "You can chat once you're connected" })
                    .char_limit(xbtune_remote::msg::MAX_CHAT)
                    .desired_width(ui.available_width() - 70.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add_enabled(paired, egui::Button::new("Send")).clicked() || (enter && paired) {
                send_chat(app);
                r.request_focus();
            }
        });
    });
}

/// Dialogs that show over any view: the host's countdown and request
/// prompt, the tuner's burn request and review.
pub fn dialogs(app: &mut XbApp, ui: &mut Ui) {
    if !app.remote.active() {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let peer = app.remote.peer.clone().unwrap_or_else(|| "The tuner".into());
    if app.remote.hosting {
        if let Some(c) = &app.remote.host.countdown {
            let left = c.until.saturating_duration_since(Instant::now()).as_secs_f32().ceil();
            let what = c.what.clone();
            let mut stop = false;
            egui::Modal::new(egui::Id::new("remote-countdown")).show(ui.ctx(), |ui| {
                ui.set_width(460.0);
                ui.label(RichText::new(format!("{peer} is about to {what}")).font(theme::strong_font(19.0)).color(p.warn));
                ui.label(RichText::new(format!("{left:.0}")).font(theme::strong_font(48.0)).color(p.accent));
                ui.label("They have your permission for this. Stop it if the bike isn't ready (engine off, ignition on, nobody near it).");
                ui.add_space(8.0);
                let b = egui::Button::new(RichText::new("Stop").font(theme::strong_font(18.0)).color(egui::Color32::WHITE))
                    .fill(p.bad)
                    .min_size(egui::vec2(140.0, 40.0));
                if ui.add(b).clicked() {
                    stop = true;
                }
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            if stop {
                sync::stop_countdown(app);
            }
            return;
        }
        // One request prompt at a time, when nothing else is open.
        let free = !app.burn.is_open() && app.diag.confirm.is_none();
        let next = app.remote.host.requests.iter().find(|r| !app.remote.later.contains(&r.id));
        if free && let Some(r) = next {
            let (id, what, note, label) =
                (r.id, sync::describe(&r.kind), r.note.clone(), review_label(&r.kind));
            let mut act = None;
            egui::Modal::new(egui::Id::new("remote-request")).show(ui.ctx(), |ui| {
                ui.set_width(440.0);
                ui.label(RichText::new(format!("{peer} asks you to {what}")).font(theme::strong_font(18.0)));
                if !note.is_empty() {
                    ui.label(RichText::new(format!("\"{note}\"")).color(p.muted));
                }
                ui.label(
                    RichText::new("Review opens the normal dialog: you check and confirm everything there.")
                        .small()
                        .color(p.muted),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(label).clicked() {
                        act = Some(1);
                    }
                    if ui.button("Decline").clicked() {
                        act = Some(2);
                    }
                    if ui.button("Later").clicked() {
                        act = Some(3);
                    }
                });
            });
            match act {
                Some(1) => sync::review(app, id),
                Some(2) => sync::decline(app, id),
                Some(3) => {
                    app.remote.later.insert(id);
                }
                _ => {}
            }
        }
        return;
    }
    // Tuner: the burn / AFV request dialog.
    if let Some((afv, mut note)) = app.remote.guest.ask_burn.take() {
        let direct = app.remote.guest.perms.burn;
        let mut act = None;
        egui::Modal::new(egui::Id::new("remote-ask-burn")).show(ui.ctx(), |ui| {
            ui.set_width(460.0);
            let what = if afv { "Reset AFV" } else { "Burn the shared tune" };
            ui.label(RichText::new(format!("{what} on {peer}'s ECM")).font(theme::strong_font(18.0)));
            ui.label(if direct {
                format!(
                    "You have permission: {peer}'s XBTune makes a verified backup, you review the changes, then \
                     {peer} gets a 10 s countdown and can stop it."
                )
            } else {
                format!("{peer} reviews and confirms it in their burn dialog.")
            });
            ui.add(egui::TextEdit::singleline(&mut note).hint_text("Note (goes into the burn history)").desired_width(420.0));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(if direct { "Start" } else { "Ask" }).clicked() {
                    act = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    act = Some(false);
                }
            });
        });
        match act {
            Some(true) => {
                let kind = if afv { RequestKind::AfvReset } else { RequestKind::Burn };
                sync::request(app, kind, note);
            }
            Some(false) => {}
            None => app.remote.guest.ask_burn = Some((afv, note)),
        }
        return;
    }
    // Tuner: review a direct burn.
    if let Some((id, lines, mut ticked)) = app.remote.guest.review.take() {
        let mut act = None;
        egui::Modal::new(egui::Id::new("remote-review")).show(ui.ctx(), |ui| {
            ui.set_width(560.0);
            ui.label(RichText::new(format!("Review: burn to {peer}'s ECM")).font(theme::strong_font(18.0)));
            ui.label(
                RichText::new("Backup made on their side. These are written, then read back:").color(p.muted),
            );
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                if lines.is_empty() {
                    ui.label("Nothing differs from the ECM.");
                }
                for l in &lines {
                    ui.label(l);
                }
            });
            ui.checkbox(&mut ticked, "I've checked these changes");
            ui.horizontal(|ui| {
                let b = egui::Button::new(RichText::new("Burn").color(egui::Color32::WHITE)).fill(p.bad);
                if ui.add_enabled(ticked && !lines.is_empty(), b).clicked() {
                    act = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    act = Some(false);
                }
            });
        });
        app.remote.guest.review = Some((id, lines, ticked));
        match act {
            Some(true) => sync::confirm_review(app),
            Some(false) => sync::cancel_request(app, id),
            None => {}
        }
    }
}

fn guide(app: &mut XbApp, ui: &mut Ui) {
    if !app.remote.guide_open {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("remote-guide")).show(ui.ctx(), |ui| {
        ui.set_width(560.0);
        ui.label(RichText::new("Remote tuning").font(theme::strong_font(19.0)));
        ui.add_space(4.0);
        let bullet = |ui: &mut Ui, s: &str| {
            ui.horizontal_wrapped(|ui| {
                ui.label("•");
                ui.label(s);
            });
        };
        ui.label("A tuner somewhere else helps with your bike, as if they were in the garage.");
        bullet(ui, "The bike owner presses Start session and reads the code (like 47-381926) to the tuner by phone.");
        bullet(ui, "The tuner types it in. The two apps then talk end-to-end encrypted: only you two can read it.");
        bullet(
            ui,
            "The tuner sees your live data and the tune you have open, proposes edits and asks for burns, tests and \
             resets. You review each one in the normal dialog.",
        );
        bullet(
            ui,
            "You can let them edit, burn or run tests themselves, and take that back at any time. Anything they \
             start on the bike gives you 10 seconds to stop it.",
        );
        bullet(ui, "Give the code only to your tuner. Anyone who has it could try to join first.");
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "The connection goes through XBTune's relay on Cloudflare. The relay can't read anything; like any \
                 website it sees your IP address and when you're connected.",
            )
            .small()
            .color(p.muted),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.remote.guide_dont_show, "Don't show this again");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
    });
    if close || modal.should_close() {
        app.remote.guide_open = false;
        app.remote.guide_shown = true;
        if app.remote.guide_dont_show && !app.settings.remote_guide_seen {
            app.settings.remote_guide_seen = true;
            app.save_settings();
        }
    }
}
