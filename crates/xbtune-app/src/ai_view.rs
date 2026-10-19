// SPDX-License-Identifier: GPL-3.0-or-later
//! The AI advisor (docs/AI.md, stage A1): its settings card, and the
//! "Ask the AI advisor" dialog on a map in the Tune view. Every request is
//! shown first ("What gets sent": where it goes, its size, the exact body)
//! and goes only on Send. Answers are words; nothing here changes the tune
//! or touches the ECM. Requests run on a thread; the UI never waits.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use egui::{RichText, Ui};
use xbtune_ai::client::{self, AiError, Prepared, Reply, Role, Turn};
use xbtune_ai::config::{self, Adapter, AiConfig};
use xbtune_ai::{context, journal::Journal};

use crate::app::XbApp;
use crate::theme::{self, Palette};

/// One line of the conversation as shown.
enum Entry {
    You(String),
    Ai { text: String, footer: String, warnings: Vec<String> },
    Error(String),
}

/// A request on its way.
struct Pending {
    rx: Receiver<Result<Reply, AiError>>,
    prepared: Prepared,
    /// The user turn, added to the history only once answered.
    turn: Turn,
    question: String,
    started: Instant,
}

#[derive(Default)]
pub struct AiUi {
    /// Open the dialog for this map on the next frame (Tune view button).
    pub request: Option<String>,
    // Settings card.
    edit: Option<AiConfig>,
    saved: Option<AiConfig>,
    models: Vec<String>,
    models_rx: Option<Receiver<Result<Vec<String>, AiError>>>,
    test_rx: Option<Receiver<Result<Reply, AiError>>>,
    status: Option<(bool, String)>,
    // The dialog.
    open: Option<String>,
    turns: Vec<Turn>,
    shown: Vec<Entry>,
    question: String,
    include_logs: bool,
    preview: Option<(Prepared, Turn, String)>,
    pending: Option<Pending>,
    journal: Option<Journal>,
}

fn load_config() -> AiConfig {
    AiConfig::path().and_then(|p| AiConfig::load(&p).ok()).unwrap_or_default()
}

fn spawn<T: Send + 'static>(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Receiver<T> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(f());
        ctx.request_repaint();
    });
    rx
}

/// Open the dialog for `map` (Tune view).
pub fn open(app: &mut XbApp, map: &str) {
    let a = &mut app.ai;
    if a.open.as_deref() != Some(map) {
        // A different map: a fresh conversation (its data is in turn 1).
        a.turns.clear();
        a.shown.clear();
        a.journal = None;
        a.preview = None;
    }
    a.open = Some(map.to_string());
    a.include_logs = !app.logs.logs.is_empty();
}

/// UI checks: `XBTUNE_SHOT_AI=guide|ask|preview|answer` on `map`.
pub fn shot(app: &mut XbApp, which: &str, map: &str) {
    open(app, map);
    match which {
        "guide" => app.settings.ai_guide_seen = false,
        "preview" | "ask" | "answer" => {
            app.settings.ai_guide_seen = true;
            app.ai.question =
                "Where is the front map rich at cruise, and what would you change first?".into();
            if which != "ask" {
                prepare(app, &load_config());
            }
            if which == "answer"
                && let Some((p, turn, q)) = app.ai.preview.take()
            {
                app.ai.shown.push(Entry::You(q));
                app.ai.turns.push(turn);
                app.ai.shown.push(Entry::Ai {
                    text:
                        "(made-up answer for a UI check) Load 30 at 2900-3400 rpm: the ECM removed about 5% \
                           fuel there, so the map is rich. Lower those cells by 3-5% and log again."
                            .into(),
                    footer: format!("{} · 3,900 tokens in, 120 out", p.model),
                    warnings: Vec::new(),
                });
            }
        }
        _ => {}
    }
}

fn prepare(app: &mut XbApp, cfg: &AiConfig) {
    let Some(map) = app.ai.open.clone() else { return };
    let Some(tune) = &app.tune else { return };
    let question = app.ai.question.trim().to_string();
    // The data goes with the first question of a conversation.
    let text = if app.ai.turns.is_empty() {
        let logs: Vec<_> = if app.ai.include_logs {
            app.logs.logs.iter().map(|l| (*l.log).clone()).collect()
        } else {
            Vec::new()
        };
        context::user_message(&question, &context::data_block(tune, &map, &logs))
    } else {
        question.clone()
    };
    let turn = Turn { role: Role::User, text };
    let mut turns = app.ai.turns.clone();
    turns.push(turn.clone());
    match client::prepare(cfg, context::SYSTEM_PROMPT, &turns) {
        Ok(p) => app.ai.preview = Some((p, turn, question)),
        Err(e) => app.ai.shown.push(Entry::Error(e.to_string())),
    }
}

fn send(app: &mut XbApp, ctx: &egui::Context) {
    let Some((p, turn, question)) = app.ai.preview.take() else { return };
    let cfg = load_config();
    let journal = app.ai.journal.get_or_insert_with(|| {
        Journal::new(&xbtune_core::env::Dirs::detect().map(|d| d.files).unwrap_or_default())
    });
    if let Err(e) = journal.request(&p) {
        tracing::warn!(error = %e, "AI journal not written");
    }
    tracing::info!(host = %p.host, model = %p.model, bytes = p.body.len(), "AI request sent");
    let prepared = p.clone();
    let rx = spawn(ctx, move || client::send(&cfg, &prepared));
    app.ai.pending = Some(Pending { rx, prepared: p, turn, question, started: Instant::now() });
    app.ai.question.clear();
}

fn poll(app: &mut XbApp) {
    let a = &mut app.ai;
    if let Some(rx) = &a.models_rx {
        match rx.try_recv() {
            Ok(Ok(m)) => {
                a.status = Some((true, format!("{} model(s) listed", m.len())));
                a.models = m;
                a.models_rx = None;
            }
            Ok(Err(e)) => {
                a.status = Some((false, e.to_string()));
                a.models_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => a.models_rx = None,
        }
    }
    if let Some(rx) = &a.test_rx {
        match rx.try_recv() {
            Ok(r) => {
                a.status = Some(match r {
                    Ok(r) => (
                        true,
                        format!("Connected: {} answered \"{}\"", r.model.unwrap_or_default(), r.text.trim()),
                    ),
                    Err(e) => (false, e.to_string()),
                });
                a.test_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => a.test_rx = None,
        }
    }
    let Some(pending) = &a.pending else { return };
    let r = match pending.rx.try_recv() {
        Ok(r) => r,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => {
            Err(AiError::BadReply { host: String::new(), message: "lost".into() })
        }
    };
    let Some(pending) = a.pending.take() else { return };
    if let Some(j) = &a.journal {
        j.reply(&r).ok();
    }
    a.shown.push(Entry::You(pending.question));
    match r {
        Ok(reply) => {
            let mut warnings = Vec::new();
            if reply.truncated() {
                warnings.push("The answer was cut off by the output limit.".to_string());
            }
            if reply.input_cut(&pending.prepared) {
                warnings.push(client::INPUT_CUT_HINT.to_string());
            }
            let usage = match (reply.input_tokens, reply.output_tokens) {
                (Some(i), Some(o)) => format!("{i} tokens in, {o} out"),
                _ => "token use not reported".into(),
            };
            let model = reply.model.clone().unwrap_or_else(|| pending.prepared.model.clone());
            let secs = pending.started.elapsed().as_secs_f64();
            a.turns.push(pending.turn);
            a.turns.push(Turn { role: Role::Assistant, text: reply.text.clone() });
            a.shown.push(Entry::Ai {
                text: reply.text,
                footer: format!("{model} · {usage} · {secs:.0} s"),
                warnings,
            });
        }
        Err(e) => a.shown.push(Entry::Error(e.to_string())),
    }
}

/// The dialog, when open. Called every frame.
pub fn show(app: &mut XbApp, ui: &mut Ui) {
    poll(app);
    if let Some(m) = app.ai.request.take() {
        open(app, &m);
    }
    let Some(map) = app.ai.open.clone() else { return };
    let p = Palette::for_visuals(ui.visuals());
    let cfg = load_config();
    let ctx = ui.ctx().clone();
    let map_label =
        app.tune.as_ref().and_then(|t| t.var(&map).ok()).map_or(map.clone(), |v| v.label().to_string());
    let logs_n = app.logs.logs.len();
    let mut close = false;
    let mut ask = false;
    let mut do_send = false;
    let mut back = false;
    let mut cancel = false;
    let mut fresh = false;
    let mut to_settings = false;
    let guide = !app.settings.ai_guide_seen;
    let mut guide_done = false;
    let modal = egui::Modal::new(egui::Id::new("ai-advisor")).show(ui.ctx(), |ui| {
        ui.set_width(680.0);
        let max_h = (ui.ctx().content_rect().height() - 80.0).max(200.0);
        egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
            let a = &mut app.ai;
            ui.label(RichText::new(format!("AI advisor · {map_label}")).font(theme::strong_font(19.0)));
            let place = if cfg.base_url.is_empty() {
                "not set up".to_string()
            } else if cfg.is_local() {
                format!("{} on this computer", cfg.model)
            } else {
                format!("{} at {}", cfg.model, cfg.host())
            };
            ui.label(RichText::new(format!("Model: {place}")).color(p.muted));
            ui.label(RichText::new(xbtune_core::disclaimer::AI).color(p.warn));
            ui.add_space(6.0);
            if guide {
                guide_text(ui, &p);
                ui.add_space(6.0);
                if ui.button("Got it").clicked() {
                    guide_done = true;
                }
                return;
            }
            if let Some(m) = cfg.missing() {
                ui.colored_label(p.bad, format!("Set up the AI advisor first: {m}."));
                if ui.button("Open Settings").clicked() {
                    to_settings = true;
                }
                return;
            }
            if let Some((prep, _, _)) = &a.preview {
                ui.label(RichText::new("What gets sent").font(theme::strong_font(16.0)));
                let dest = if prep.local {
                    "to this computer (the model runs here; nothing leaves it)".to_string()
                } else {
                    format!("to {} (a hosted provider: it leaves this computer)", prep.host)
                };
                ui.label(format!(
                    "{} bytes, about {} tokens, {dest}. No VIN, names, file paths or Bluetooth addresses.",
                    prep.body.len(),
                    prep.approx_tokens
                ));
                let mut body = prep.body.clone();
                egui::ScrollArea::vertical().id_salt("ai-body").max_height(300.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut body)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .interactive(true),
                    );
                });
                ui.horizontal(|ui| {
                    let b = egui::Button::new(RichText::new("Send").color(egui::Color32::WHITE)).fill(p.accent);
                    if ui.add(b).clicked() {
                        do_send = true;
                    }
                    if ui.button("Back").clicked() {
                        back = true;
                    }
                });
                return;
            }
            for e in &a.shown {
                match e {
                    Entry::You(q) => {
                        ui.label(RichText::new("You").strong());
                        ui.label(q);
                    }
                    Entry::Ai { text, footer, warnings } => {
                        ui.label(RichText::new("Advisor").strong().color(p.accent));
                        ui.add(egui::Label::new(text.as_str()).wrap().selectable(true));
                        for w in warnings {
                            ui.colored_label(p.warn, w);
                        }
                        ui.label(RichText::new(footer).small().color(p.muted));
                    }
                    Entry::Error(e) => {
                        ui.colored_label(p.bad, e);
                    }
                }
                ui.add_space(6.0);
            }
            if let Some(pending) = &a.pending {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Waiting for the answer… {:.0} s", pending.started.elapsed().as_secs_f64()));
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                return;
            }
            ui.separator();
            if a.turns.is_empty() {
                ui.add_enabled_ui(logs_n > 0, |ui| {
                    ui.checkbox(&mut a.include_logs, format!("Include statistics from the open logs ({logs_n})"))
                        .on_disabled_hover_text("Open logs in the Logs view to give the advisor data");
                });
            } else {
                ui.label(
                    RichText::new("Follow-up questions go with the conversation so far (the data was sent with the first).")
                        .small()
                        .color(p.muted),
                );
            }
            ui.add(
                egui::TextEdit::multiline(&mut a.question)
                    .hint_text("Ask about this map, e.g. \"Where is it rich at cruise?\"")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                if ui.add_enabled(!a.question.trim().is_empty(), egui::Button::new("Review and send…")).clicked() {
                    ask = true;
                }
                if !a.shown.is_empty() && ui.button("New conversation").clicked() {
                    fresh = true;
                }
                if let Some(j) = &a.journal {
                    ui.label(RichText::new(format!("Saved in {}", j.path.display())).small().color(p.muted));
                }
            });
        });
        ui.separator();
        if ui.button("Close").clicked() {
            close = true;
        }
    });
    if guide_done {
        app.settings.ai_guide_seen = true;
        app.save_settings();
    }
    if to_settings {
        app.view = crate::app::View::Settings;
        close = true;
    }
    if ask {
        prepare(app, &cfg);
    }
    if back {
        app.ai.preview = None;
    }
    if do_send {
        send(app, &ctx);
    }
    if cancel {
        // The thread finishes on its own; its answer is dropped.
        app.ai.pending = None;
    }
    if fresh {
        app.ai.turns.clear();
        app.ai.shown.clear();
        app.ai.journal = None;
    }
    if close || (modal.should_close() && app.ai.preview.is_none()) {
        app.ai.open = None;
        app.ai.preview = None;
    }
}

fn guide_text(ui: &mut Ui, p: &Palette) {
    ui.label(RichText::new("Before you start").font(theme::strong_font(16.0)));
    for line in [
        "The advisor is a language model. You ask about a map; it answers in words. It never changes the \
         tune or writes to the ECM. You make any changes yourself, and they go through the normal Burn review.",
        "You choose the model in Settings: one on this computer (Ollama, LM Studio, ...), where nothing \
         leaves the machine, or a hosted one with your own API key, where you pay the provider.",
        "Each question shows exactly what will be sent before anything goes: the map and its axes, AFV, \
         and statistics from your open logs. No VIN, names, file paths or Bluetooth addresses.",
        "Models can be confidently wrong. Compare what it says with the numbers, change a little at a \
         time, and keep your backup.",
        "Conversations are saved in the AI folder next to your tunes, so you have a record.",
    ] {
        crate::wizard::bullet(ui, line);
    }
    ui.label(
        RichText::new(
            "Local models need a context window of 16k tokens or more (Ollama: OLLAMA_CONTEXT_LENGTH=16384).",
        )
        .small()
        .color(p.muted),
    );
}

/// The Settings card.
pub fn settings_card(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    poll(app);
    let a = &mut app.ai;
    if a.edit.is_none() {
        let c = load_config();
        a.saved = Some(c.clone());
        a.edit = Some(c);
    }
    let ctx = ui.ctx().clone();
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("AI advisor").font(theme::strong_font(17.0)));
        ui.label(
            RichText::new(
                "Any model: one running on this computer (nothing leaves it), or a hosted provider with your \
                 own API key. The advisor answers questions about a map in the Tune view; it never changes \
                 anything by itself.",
            )
            .color(p.muted),
        );
        ui.add_space(6.0);
        let Some(c) = a.edit.as_mut() else { return };
        egui::Grid::new("ai-settings").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Provider");
            egui::ComboBox::from_id_salt("ai-preset")
                .selected_text(if c.preset.is_empty() { "Choose…" } else { c.preset.as_str() })
                .width(280.0)
                .show_ui(ui, |ui| {
                    for pr in config::PRESETS {
                        let where_ = if pr.local { "on this computer" } else { "hosted" };
                        if ui
                            .selectable_label(c.preset == pr.name, format!("{} ({where_})", pr.name))
                            .clicked()
                        {
                            c.apply_preset(pr);
                        }
                    }
                });
            ui.end_row();
            if let Some(pr) = config::preset(&c.preset)
                && !pr.note.is_empty()
            {
                ui.label("");
                ui.label(RichText::new(pr.note).small().color(p.muted));
                ui.end_row();
            }
            ui.label("Request format");
            ui.horizontal(|ui| {
                ui.radio_value(&mut c.adapter, Adapter::OpenAi, Adapter::OpenAi.label());
                ui.radio_value(&mut c.adapter, Adapter::Anthropic, Adapter::Anthropic.label());
            });
            ui.end_row();
            ui.label("URL");
            ui.add(egui::TextEdit::singleline(&mut c.base_url).desired_width(360.0));
            ui.end_row();
            ui.label("Model");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut c.model)
                        .desired_width(260.0)
                        .hint_text("as the provider spells it"),
                );
                if !a.models.is_empty() {
                    egui::ComboBox::from_id_salt("ai-models").selected_text("Pick").width(80.0).show_ui(
                        ui,
                        |ui| {
                            for m in &a.models {
                                if ui.selectable_label(c.model == *m, m).clicked() {
                                    c.model = m.clone();
                                }
                            }
                        },
                    );
                }
                if ui
                    .add_enabled(
                        a.models_rx.is_none() && !c.base_url.is_empty(),
                        egui::Button::new("List models"),
                    )
                    .clicked()
                {
                    let cfg = c.clone();
                    a.models_rx = Some(spawn(&ctx, move || client::list_models(&cfg)));
                    a.status = None;
                }
            });
            ui.end_row();
            ui.label("API key");
            ui.add(egui::TextEdit::singleline(&mut c.api_key).password(true).desired_width(360.0).hint_text(
                if std::env::var_os("XBTUNE_AI_KEY").is_some() {
                    "from XBTUNE_AI_KEY"
                } else {
                    "not needed for most local servers"
                },
            ));
            ui.end_row();
            ui.label("Wait for an answer");
            ui.add(egui::DragValue::new(&mut c.timeout_s).range(10..=900).suffix(" s"));
            ui.end_row();
            if c.preset == "Custom" || !c.header_name.is_empty() {
                ui.label("Extra header");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut c.header_name).desired_width(120.0).hint_text("name"),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut c.header_value)
                            .password(true)
                            .desired_width(220.0)
                            .hint_text("value"),
                    );
                });
                ui.end_row();
            }
        });
        let changed = a.saved.as_ref() != Some(c);
        ui.horizontal(|ui| {
            if ui.add_enabled(changed, egui::Button::new("Save")).clicked()
                && let Some(path) = AiConfig::path()
            {
                match c.save(&path) {
                    Ok(()) => {
                        a.saved = Some(c.clone());
                        a.status = Some((true, format!("Saved (readable only by you): {}", path.display())));
                    }
                    Err(e) => a.status = Some((false, e.to_string())),
                }
            }
            let ok = c.missing().is_none() && a.test_rx.is_none();
            if ui
                .add_enabled(ok, egui::Button::new("Test connection"))
                .on_hover_text("Sends a one-word request")
                .clicked()
            {
                let cfg = c.clone();
                a.test_rx = Some(spawn(&ctx, move || client::test_connection(&cfg)));
                a.status = Some((true, "Testing… (a local model may take a minute to load)".into()));
            }
            if a.test_rx.is_some() || a.models_rx.is_some() {
                ui.spinner();
            }
        });
        if changed {
            ui.label(
                RichText::new("Unsaved changes. Test connection uses what's shown here.")
                    .small()
                    .color(p.warn),
            );
        }
        if let Some(m) = c.missing() {
            ui.label(RichText::new(format!("Still needed: {m}.")).small().color(p.muted));
        }
        let where_ = if c.base_url.is_empty() {
            None
        } else if c.is_local() {
            Some("Requests go to this computer only.".to_string())
        } else {
            Some(format!("Requests go to {} when you click Send.", c.host()))
        };
        if let Some(w) = where_ {
            ui.label(RichText::new(w).small().color(p.muted));
        }
        if let Some((good, s)) = &a.status {
            ui.colored_label(if *good { p.good } else { p.bad }, s);
        }
    });
}
