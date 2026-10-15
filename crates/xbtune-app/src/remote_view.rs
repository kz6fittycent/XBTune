// SPDX-License-Identifier: GPL-3.0-or-later
//! The Remote view (phase 8, docs/REMOTE.md): host a session for a tuner
//! or join one with the code, then chat. Stage R1: pairing and encrypted
//! chat; live data, the shared tune and requests come in R2-R5.

use std::time::SystemTime;

use egui::{RichText, Ui};
use xbtune_remote::code::{self, Code};
use xbtune_remote::session::{self, Command, Config, Event, Remote, Start};

use crate::app::XbApp;
use crate::theme::{self, Palette};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Status {
    Connecting,
    Waiting,
    Paired { peer: String },
    PeerGone { peer: String },
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
}

impl RemoteUi {
    pub fn active(&self) -> bool {
        self.session.is_some()
    }
}

/// End the session (app closing): the other side is told at once.
pub fn end(app: &mut XbApp) {
    if app.remote.active()
        && let Some(s) = &app.remote.session
    {
        s.send(Command::End);
        // Give the task a moment to send `bye` before the runtime stops.
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
}

pub fn open_guide(app: &mut XbApp) {
    app.remote.guide_open = true;
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
    app.remote.chat.clear();
    app.remote.join_error = None;
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

/// Collect session events. Called every frame.
pub fn poll(app: &mut XbApp) {
    let Some(s) = &mut app.remote.session else { return };
    let mut events = Vec::new();
    while let Ok(e) = s.events.try_recv() {
        events.push(e);
    }
    for e in events {
        let peer = || match &app.remote.status {
            Some(Status::Paired { peer } | Status::PeerGone { peer }) => peer.clone(),
            _ => "The other side".into(),
        };
        match e {
            Event::Code(c) => app.remote.code = Some(c),
            Event::Waiting => app.remote.status = Some(Status::Waiting),
            Event::Paired { peer, resumed, .. } => {
                if !resumed {
                    app.say(
                        format!("Connected to {peer}. Everything between you is end-to-end encrypted."),
                        false,
                    );
                }
                app.remote.status = Some(Status::Paired { peer });
                if let (Some(t), Some(s)) = (app.remote.queued.take(), &app.remote.session) {
                    s.send(Command::Chat(t));
                }
            }
            Event::PeerGone => app.remote.status = Some(Status::PeerGone { peer: peer() }),
            Event::Chat { mine, text } => app.remote.chat.push(Line { mine, text, at: SystemTime::now() }),
            Event::Reconnecting { reason } => app.remote.status = Some(Status::Reconnecting(reason)),
            Event::Ended { reason, by_peer } => {
                let error = !by_peer && !reason.starts_with("You ended");
                app.say(format!("Remote session ended: {reason}"), error);
                app.remote.session = None;
                app.remote.status = None;
                app.remote.code = None;
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
}

/// Local wall-clock time, "14:05".
fn hhmm(t: SystemTime) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
    // SAFETY: localtime_r only writes the `tm` we pass; zeroed is a valid tm.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&secs, &mut tm).is_null() };
    if ok { format!("{:02}:{:02}", tm.tm_hour, tm.tm_min) } else { String::new() }
}

fn dot(ui: &mut Ui, color: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(r.center(), 5.0, color);
}

fn session_ui(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    let status = app.remote.status.clone();
    let paired = matches!(status, Some(Status::Paired { .. }));
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                let (color, text) = match &status {
                    Some(Status::Paired { peer }) => (p.good, format!("Connected to {peer} · end-to-end encrypted")),
                    Some(Status::PeerGone { peer }) => {
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
                    && !paired
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
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Chat").font(theme::strong_font(16.0)));
        let peer = match &status {
            Some(Status::Paired { peer } | Status::PeerGone { peer }) => peer.clone(),
            _ => "Them".into(),
        };
        egui::ScrollArea::vertical().max_height(300.0).stick_to_bottom(true).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if app.remote.chat.is_empty() {
                ui.label(RichText::new("No messages yet.").color(p.muted));
            }
            for l in &app.remote.chat {
                let who = if l.mine { "You" } else { peer.as_str() };
                let t = hhmm(l.at);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(t).small().color(p.muted));
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
    ui.add_space(8.0);
    ui.label(
        RichText::new(
            "Coming in the next stages: the tuner sees your live data and logs, works on the tune with you, and \
             asks for burns and tests. Nothing reaches the ECM without your OK unless you give permission.",
        )
        .small()
        .color(p.muted),
    );
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
            "You stay in charge: the tuner sees what you share and asks for burns and tests. You can let them do more, \
             and take that back at any time.",
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
