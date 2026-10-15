// SPDX-License-Identifier: GPL-3.0-or-later
//! The "Report a problem" dialog, and the same dialog offered after a
//! crash. The user can say what happened (optional) and leave a contact
//! (optional); the app's log goes with it and can be looked at first under
//! "What gets sent". Nothing leaves the machine until Send is clicked.
//! Sending and the files involved are in `crate::report`.

use std::sync::mpsc;

use egui::{RichText, Ui};

use crate::app::XbApp;
use crate::report::{self, Crash, Outcome};
use crate::theme::{self, Palette};

#[derive(Default)]
pub enum ReportUi {
    #[default]
    Closed,
    Editing {
        /// Set when this report is about a crash.
        crash: Option<Crash>,
        /// The crash happened in an earlier session (vs. just now).
        earlier: bool,
        description: String,
        contact: String,
        /// The report as it would be sent, for "What gets sent".
        preview: Option<Box<report::Report>>,
    },
    Sending {
        rx: mpsc::Receiver<Outcome>,
        crash: bool,
    },
    Done(Outcome),
}

impl ReportUi {
    pub fn is_open(&self) -> bool {
        !matches!(self, Self::Closed)
    }

    pub fn open(&mut self) {
        *self = Self::Editing {
            crash: None,
            earlier: false,
            description: String::new(),
            contact: String::new(),
            preview: None,
        };
    }

    pub fn offer_crash(&mut self, crash: Crash, earlier: bool) {
        *self = Self::Editing {
            crash: Some(crash),
            earlier,
            description: String::new(),
            contact: String::new(),
            preview: None,
        };
    }
}

fn ecm_label(app: &XbApp) -> Option<String> {
    app.link.identity.as_ref().map(|i| format!("{} ({}, {})", i.version.trim(), i.ecm.id, i.ecm.family))
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if !app.report.is_open() {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let ecm = ecm_label(app);
    let mut close = false;
    let mut send = false;
    let mut discard = false;
    if let ReportUi::Sending { rx, crash } = &app.report
        && let Ok(o) = rx.try_recv()
    {
        if *crash {
            report::forget_crash();
        }
        app.report = ReportUi::Done(o);
    }

    let modal = egui::Modal::new(egui::Id::new("report")).show(ui.ctx(), |ui| {
        ui.set_width(540.0);
        match &mut app.report {
            ReportUi::Closed => {}
            ReportUi::Editing { crash, earlier, description, contact, preview } => {
                let title = match (&crash, *earlier) {
                    (None, _) => "Report a problem",
                    (Some(_), true) => "XBTune closed unexpectedly",
                    (Some(_), false) => "XBTune hit an internal error",
                };
                ui.label(RichText::new(title).font(theme::strong_font(19.0)));
                ui.add_space(4.0);
                match &crash {
                    None => {
                        ui.label(
                            "Something not working right? Say what happened below if you like, but you \
                             don't have to: the app's log goes with the report and usually shows enough.",
                        );
                    }
                    Some(c) => {
                        let when = if *earlier { "Last time it ran, XBTune" } else { "XBTune just" };
                        ui.label(format!(
                            "{when} hit a bug ({}). Sending a report lets it get fixed. If you \
                             remember what you were doing, add it below (optional).",
                            c.summary
                        ));
                        if !*earlier {
                            ui.label(
                                RichText::new(
                                    "Part of the app may have stopped working. Your files are safe; \
                                     restarting XBTune is a good idea.",
                                )
                                .color(p.warn),
                            );
                        }
                    }
                }
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::multiline(description)
                        .hint_text("What were you doing, and what went wrong? (optional)")
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .char_limit(report::MAX_DESCRIPTION_CHARS),
                );
                ui.add(
                    egui::TextEdit::singleline(contact)
                        .hint_text("Email or name, if you'd like a reply (optional)")
                        .desired_width(f32::INFINITY)
                        .char_limit(report::MAX_CONTACT_CHARS),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "The report goes to the XBTune developer as an issue on the project's GitHub page; \
                         you don't need an account. It includes the app version, your Linux version, the \
                         ECM's version string if connected, and the app's recent log, with your home \
                         folder and Bluetooth addresses masked. Tunes and data logs are never sent.",
                    )
                    .small()
                    .color(p.muted),
                );
                let header =
                    egui::CollapsingHeader::new("What gets sent").id_salt("report-preview").show(ui, |ui| {
                        let r = preview.get_or_insert_with(|| {
                            Box::new(report::build(description, contact, ecm.clone(), crash.as_ref()))
                        });
                        let mut text = serde_json::to_string_pretty(r).unwrap_or_default();
                        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut text)
                                    .font(egui::TextStyle::Monospace)
                                    .interactive(false)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    });
                if header.body_returned.is_none() {
                    // Rebuilt from the current text when opened again.
                    *preview = None;
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let b = egui::Button::new(RichText::new("Send report").color(egui::Color32::WHITE))
                        .fill(p.accent);
                    if ui.add(b).clicked() {
                        send = true;
                    }
                    let cancel = if crash.is_some() { "Don't send" } else { "Cancel" };
                    if ui.button(cancel).clicked() {
                        discard = true;
                    }
                });
            }
            ReportUi::Sending { .. } => {
                ui.label(RichText::new("Sending…").font(theme::strong_font(19.0)));
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Sending the report.");
                });
            }
            ReportUi::Done(o) => {
                let (head, text, good) = match o {
                    Outcome::Sent => ("Thank you", "Your report was sent.".to_string(), true),
                    Outcome::Duplicate => (
                        "Thank you",
                        "Your report was sent. Someone already hit this one, so it was added to \
                         the existing report."
                            .to_string(),
                        true,
                    ),
                    Outcome::Failed(_) => (
                        "Saved for later",
                        "The report couldn't be sent right now (no internet?), so it was saved. \
                         XBTune sends it automatically the next time it starts."
                            .to_string(),
                        false,
                    ),
                    Outcome::Rejected(e) => {
                        ("Not sent", format!("Sorry, the report couldn't be sent: {e}"), false)
                    }
                };
                ui.label(RichText::new(head).font(theme::strong_font(19.0)));
                ui.add_space(4.0);
                ui.label(RichText::new(text).color(if good { p.good } else { p.warn }));
                ui.add_space(8.0);
                if ui.button("Close").clicked() {
                    close = true;
                }
            }
        }
    });
    if send && let ReportUi::Editing { crash, description, contact, .. } = &app.report {
        if app.is_screenshot() {
            // UI checks never send anything.
            app.report = ReportUi::Done(Outcome::Sent);
            return;
        }
        let r = report::build(description, contact, ecm, crash.as_ref());
        let ctx = ui.ctx().clone();
        let rx = report::send(r, move || ctx.request_repaint());
        app.report = ReportUi::Sending { rx, crash: crash.is_some() };
    } else if discard {
        if matches!(&app.report, ReportUi::Editing { crash: Some(_), .. }) {
            tracing::info!("crash report not sent (user's choice)");
            report::forget_crash();
        }
        app.report = ReportUi::Closed;
    } else if close
        // Esc / clicking outside: fine for a plain report or the result; a
        // crash offer needs an answer, and a report on its way can't stop.
        || (modal.should_close()
            && matches!(app.report, ReportUi::Editing { crash: None, .. } | ReportUi::Done(_)))
    {
        app.report = ReportUi::Closed;
    }
    if matches!(app.report, ReportUi::Sending { .. }) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
    }
}
