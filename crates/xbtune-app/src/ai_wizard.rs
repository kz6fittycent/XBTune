// SPDX-License-Identifier: GPL-3.0-or-later
//! "Set up the AI advisor": a guided alternative to the settings form,
//! after Desktop Scriptures' AI Setup Wizard (owner's request, 2026-10-19).
//!
//! 1. Welcome: what the advisor does, local vs hosted, privacy.
//! 2. Where: model servers found running on this computer (localhost,
//!    default ports only), the hosted providers, or another address.
//! 3. Model: picked from that server's own list (hosted: after the API key
//!    is pasted); typed only for "another address" without a list.
//! 4. Test and finish: one tiny request, then the settings are saved.
//!
//! Choices are cards rather than radio buttons, as in Desktop Scriptures.

use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{RichText, Ui};
use xbtune_ai::client::{self, AiError, Reply};
use xbtune_ai::config::{self, Adapter, AiConfig, Preset};
use xbtune_ai::discover::{self, Found};

use crate::app::XbApp;
use crate::theme::{self, Palette};

#[derive(Debug, Clone, PartialEq)]
enum Where {
    Local(Found),
    Hosted(&'static Preset),
    Other,
}

#[derive(Default)]
pub struct WizardUi {
    pub open: bool,
    step: usize,
    scan_rx: Option<Receiver<Vec<Found>>>,
    found: Option<Vec<Found>>,
    choice: Option<Where>,
    cfg: AiConfig,
    models: Vec<String>,
    models_rx: Option<Receiver<Result<Vec<String>, AiError>>>,
    test_rx: Option<Receiver<Result<Reply, AiError>>>,
    result: Option<Result<String, String>>,
    error: Option<String>,
}

pub fn open(app: &mut XbApp) {
    let w = &mut app.ai_wizard;
    *w = WizardUi { open: true, ..Default::default() };
    w.cfg = AiConfig::path().and_then(|p| AiConfig::load(&p).ok()).unwrap_or_default();
}

fn spawn<T: Send + 'static>(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
    rx
}

fn poll(w: &mut WizardUi) {
    if let Some(rx) = &w.scan_rx
        && let Ok(f) = rx.try_recv()
    {
        w.found = Some(f);
        w.scan_rx = None;
    }
    if let Some(rx) = &w.models_rx {
        match rx.try_recv() {
            Ok(Ok(m)) => {
                w.models = discover::chat_models(&m);
                w.error =
                    if w.models.is_empty() { Some("The server lists no chat models.".into()) } else { None };
                if w.cfg.model.is_empty() || !w.models.contains(&w.cfg.model) {
                    w.cfg.model = w.models.first().cloned().unwrap_or_default();
                }
                w.models_rx = None;
            }
            Ok(Err(e)) => {
                w.error = Some(e.to_string());
                w.models_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => w.models_rx = None,
        }
    }
    if let Some(rx) = &w.test_rx {
        match rx.try_recv() {
            Ok(r) => {
                w.result = Some(match r {
                    Ok(r) => Ok(format!(
                        "{} answered \"{}\"",
                        r.model.unwrap_or_else(|| w.cfg.model.clone()),
                        r.text.trim()
                    )),
                    Err(e) => Err(e.to_string()),
                });
                w.test_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => w.test_rx = None,
        }
    }
}

/// A selectable card. Returns true when clicked.
fn card(ui: &mut Ui, p: &Palette, selected: bool, title: &str, detail: &str) -> bool {
    let stroke = if selected {
        egui::Stroke::new(2.0, p.accent)
    } else {
        egui::Stroke::new(1.0, p.muted.gamma_multiply(0.4))
    };
    let r = egui::Frame::new()
        .stroke(stroke)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong());
            if !detail.is_empty() {
                ui.label(RichText::new(detail).small().color(p.muted));
            }
        })
        .response
        .interact(egui::Sense::click());
    ui.add_space(3.0);
    r.clicked()
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if !app.ai_wizard.open {
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let ctx = ui.ctx().clone();
    let w = &mut app.ai_wizard;
    poll(w);
    let mut finish = false;
    let mut close = false;
    egui::Modal::new(egui::Id::new("ai-wizard")).show(ui.ctx(), |ui| {
        ui.set_width(600.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Set up the AI advisor").font(theme::strong_font(19.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("{} of 4", w.step + 1)).color(p.muted));
            });
        });
        ui.add_space(6.0);
        let max_h = (ui.ctx().content_rect().height() - 160.0).max(200.0);
        egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| match w.step {
            0 => {
                for line in [
                    "The advisor is a language model you ask about a fuel or spark map. It answers, and can suggest \
                     changes that XBTune checks and you review. It never writes to the ECM.",
                    "On this computer: a model server such as Ollama, LM Studio or an Ubuntu inference snap. Nothing \
                     leaves the machine and it costs nothing, but small models make more mistakes.",
                    "Hosted: a provider such as OpenAI, Anthropic or Google with your own API key. Usually better \
                     answers; you pay the provider, and each question goes to them (you see exactly what first).",
                ] {
                    crate::wizard::bullet(ui, line);
                }
            }
            1 => {
                if w.found.is_none() && w.scan_rx.is_none() {
                    w.scan_rx = Some(spawn(&ctx, discover::scan));
                }
                ui.label(RichText::new("On this computer").strong());
                match &w.found {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Looking for model servers on this computer…");
                        });
                    }
                    Some(f) if f.is_empty() => {
                        ui.label(
                            RichText::new(
                                "None found. Install one (e.g. Ollama: then `ollama pull qwen3:14b`), start it, and \
                                 look again, or use a hosted provider below.",
                            )
                            .color(p.muted),
                        );
                    }
                    Some(f) => {
                        for s in f.clone() {
                            let chat = discover::chat_models(&s.models);
                            let detail = if chat.is_empty() {
                                "no chat models installed".to_string()
                            } else {
                                format!("{} chat model(s): {}", chat.len(), chat.join(", "))
                            };
                            let selected = w.choice == Some(Where::Local(s.clone()));
                            if card(ui, &p, selected, &format!("{} ({})", s.name, s.base_url), &detail) {
                                w.cfg.preset = s.name.clone();
                                w.cfg.adapter = Adapter::OpenAi;
                                w.cfg.base_url = s.base_url.clone();
                                w.models = chat;
                                if !w.models.contains(&w.cfg.model) {
                                    w.cfg.model = w.models.first().cloned().unwrap_or_default();
                                }
                                w.choice = Some(Where::Local(s));
                            }
                        }
                    }
                }
                if w.found.is_some() && ui.small_button("Look again").clicked() {
                    w.found = None;
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Hosted (your own API key; billed by the provider)").strong());
                for pr in config::PRESETS.iter().filter(|p| !p.local && !p.base_url.is_empty()) {
                    let selected = w.choice == Some(Where::Hosted(pr));
                    if card(ui, &p, selected, pr.name, pr.note) {
                        w.cfg.apply_preset(pr);
                        w.models.clear();
                        w.cfg.model.clear();
                        w.choice = Some(Where::Hosted(pr));
                    }
                }
                ui.add_space(6.0);
                if card(ui, &p, w.choice == Some(Where::Other), "Another address", "any OpenAI-compatible server, or a model server elsewhere on your network") {
                    w.cfg.preset = "Custom".into();
                    w.choice = Some(Where::Other);
                }
            }
            2 => match w.choice.clone() {
                Some(Where::Local(s)) => {
                    ui.label(format!("Models on {}:", s.name));
                    for m in w.models.clone() {
                        let detail = if discover::is_specialist(&m) { "made for a special purpose (e.g. code)" } else { "" };
                        if card(ui, &p, w.cfg.model == m, &m, detail) {
                            w.cfg.model = m;
                        }
                    }
                    if w.models.is_empty() {
                        ui.label(RichText::new("No chat models: install one (e.g. `ollama pull qwen3:14b`), then go back and look again.").color(p.warn));
                    }
                    ui.label(
                        RichText::new("Local models need a context window of 16k or more (Ollama: OLLAMA_CONTEXT_LENGTH=16384).")
                            .small()
                            .color(p.muted),
                    );
                }
                Some(Where::Hosted(pr)) => {
                    ui.label(format!("Your {} API key (from the provider's website; stored readable only by you):", pr.name));
                    ui.add(egui::TextEdit::singleline(&mut w.cfg.api_key).password(true).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        let can = !w.cfg.key().is_empty() && w.models_rx.is_none();
                        if ui.add_enabled(can, egui::Button::new("Get the model list")).clicked() {
                            let c = w.cfg.clone();
                            w.models_rx = Some(spawn(&ctx, move || client::list_models(&c)));
                            w.error = None;
                        }
                        if w.models_rx.is_some() {
                            ui.spinner();
                        }
                    });
                    for m in w.models.clone() {
                        if card(ui, &p, w.cfg.model == m, &m, "") {
                            w.cfg.model = m;
                        }
                    }
                }
                Some(Where::Other) => {
                    egui::Grid::new("aiw-other").num_columns(2).show(ui, |ui| {
                        ui.label("URL");
                        ui.add(egui::TextEdit::singleline(&mut w.cfg.base_url).hint_text("http://host:port/v1").desired_width(340.0));
                        ui.end_row();
                        ui.label("Format");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut w.cfg.adapter, Adapter::OpenAi, Adapter::OpenAi.label());
                            ui.radio_value(&mut w.cfg.adapter, Adapter::Anthropic, Adapter::Anthropic.label());
                        });
                        ui.end_row();
                        ui.label("API key");
                        ui.add(egui::TextEdit::singleline(&mut w.cfg.api_key).password(true).hint_text("if it needs one").desired_width(340.0));
                        ui.end_row();
                        ui.label("Model");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut w.cfg.model).desired_width(220.0));
                            if ui.add_enabled(!w.cfg.base_url.is_empty() && w.models_rx.is_none(), egui::Button::new("List")).clicked() {
                                let c = w.cfg.clone();
                                w.models_rx = Some(spawn(&ctx, move || client::list_models(&c)));
                            }
                        });
                        ui.end_row();
                    });
                    for m in w.models.clone() {
                        if card(ui, &p, w.cfg.model == m, &m, "") {
                            w.cfg.model = m;
                        }
                    }
                }
                None => {}
            },
            _ => {
                ui.label(format!("{} · {}", w.cfg.model, if w.cfg.is_local() { "on this computer".to_string() } else { w.cfg.host() }));
                if w.result.is_none() && w.test_rx.is_none() {
                    let c = w.cfg.clone();
                    w.test_rx = Some(spawn(&ctx, move || client::test_connection(&c)));
                }
                match &w.result {
                    None => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Sending a one-word test (a local model may take a minute to load)…");
                        });
                    }
                    Some(Ok(s)) => {
                        ui.colored_label(p.good, format!("It works: {s}."));
                        ui.label("Finish saves these settings. You can change them later in Settings → AI advisor.");
                    }
                    Some(Err(e)) => {
                        ui.colored_label(p.bad, e);
                        if ui.button("Try again").clicked() {
                            w.result = None;
                        }
                    }
                }
            }
        });
        if let Some(e) = &w.error {
            ui.colored_label(p.bad, e);
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.add_enabled(w.step > 0, egui::Button::new("Back")).clicked() {
                w.step -= 1;
                w.result = None;
                w.error = None;
            }
            let can_next = match w.step {
                0 => true,
                1 => w.choice.is_some(),
                2 => w.cfg.missing().is_none(),
                _ => false,
            };
            if w.step < 3 {
                let b = egui::Button::new(RichText::new("Next").color(egui::Color32::WHITE)).fill(p.accent);
                if ui.add_enabled(can_next, b).clicked() {
                    w.step += 1;
                    w.error = None;
                }
            } else {
                let ok = matches!(w.result, Some(Ok(_)));
                let b = egui::Button::new(RichText::new("Finish").color(egui::Color32::WHITE)).fill(p.accent);
                if ui.add_enabled(ok, b).clicked() {
                    finish = true;
                }
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
            if w.step == 2
                && let Some(m) = w.cfg.missing()
            {
                ui.label(RichText::new(format!("Still needed: {m}")).small().color(p.muted));
            }
        });
    });
    if finish {
        let cfg = app.ai_wizard.cfg.clone();
        match AiConfig::path().map(|p| cfg.save(&p)) {
            Some(Ok(())) => {
                app.say(format!("AI advisor set up: {}", cfg.model), false);
                app.ai.reload_settings();
                app.ai_wizard.open = false;
            }
            Some(Err(e)) => app.ai_wizard.error = Some(e.to_string()),
            None => app.ai_wizard.error = Some("no config folder".into()),
        }
    } else if close {
        app.ai_wizard.open = false;
    }
}

/// UI checks: jump to a step (1 scans this computer first, blocking).
pub fn shot(app: &mut XbApp, step: usize) {
    let w = &mut app.ai_wizard;
    w.step = step.min(1);
    if w.step == 1 {
        w.found = Some(discover::scan());
    }
}
