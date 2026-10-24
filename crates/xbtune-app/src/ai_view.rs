// SPDX-License-Identifier: GPL-3.0-or-later
//! The AI advisor (docs/AI.md, stage A1): its settings card, and the
//! "Ask the AI advisor" dialog on a map in the Tune view. Every request is
//! shown first ("What gets sent": where it goes, its size, the exact body)
//! and goes only on Send. Answers are words; nothing here changes the tune
//! or touches the ECM. Requests run on a thread; the UI never waits.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use egui::{RichText, Ui};
use xbtune_ai::atmosphere::{self, Place, Weather};
use xbtune_ai::bike::{self, Bikes, Profile};
use xbtune_ai::checker::{self, Accepted, Refused};
use xbtune_ai::client::{self, AiError, Prepared, Reply, Role, Turn};
use xbtune_ai::config::{self, Adapter, AiConfig};
use xbtune_ai::memory;
use xbtune_ai::{context, journal::Journal};
use xbtune_defs::EcmDef;

use crate::app::XbApp;
use crate::theme::{self, Palette};

/// One line of the conversation as shown.
enum Entry {
    You(String),
    Ai {
        text: String,
        footer: String,
        warnings: Vec<String>,
        suggestions: Vec<String>,
        review: Option<Review>,
    },
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
    /// The request about to be sent (between building and sending).
    preview: Option<(Prepared, Turn, String)>,
    /// "What gets sent", when opened: (question it was built for, request).
    peek: Option<(String, Prepared)>,
    /// UI checks: open "What gets sent".
    peek_open: bool,
    pending: Option<Pending>,
    journal: Option<Journal>,
    /// The bike profile for the open tune's ECM: (ECM, as edited).
    bike: Option<(EcmDef, Profile)>,
    bike_saved: Profile,
    bike_open: bool,
    /// The "other mod" being typed.
    new_mod: String,
    /// This request may change cells without logged data (smoothing).
    allow_no_data: bool,
    /// Owner's choice for this session: apply checked changes at once.
    apply_directly: bool,
    /// The warning before turning `apply_directly` on is showing.
    confirm_direct: bool,
    geo: Geo,
}

/// The home-town lookup (Open-Meteo, on a click).
#[derive(Default)]
struct Geo {
    query: String,
    places_rx: Option<Receiver<Result<Vec<Place>, AiError>>>,
    places: Vec<Place>,
    weather_rx: Option<Receiver<Result<Weather, AiError>>>,
    status: Option<(bool, String)>,
}

impl AiUi {
    /// The settings changed elsewhere (the wizard): reload the card.
    pub fn reload_settings(&mut self) {
        self.edit = None;
        self.saved = None;
        self.status = None;
    }
}

fn files_dir() -> std::path::PathBuf {
    xbtune_core::env::Dirs::detect().map(|d| d.files).unwrap_or_default()
}

/// Load the profile for `ecm` if another bike's (or none) is loaded.
fn ensure_bike(a: &mut AiUi, ecm: &EcmDef) {
    if a.bike.as_ref().is_some_and(|(e, _)| e.id == ecm.id) {
        return;
    }
    let all = Bikes::load(&files_dir());
    let p = all.get(ecm);
    // Open the editor until the bike has been described once.
    a.bike_open = !all.bikes.contains_key(&ecm.id);
    a.bike_saved = if a.bike_open { Profile::default() } else { p.clone() };
    a.bike = Some((ecm.clone(), p));
}

fn save_bike(a: &mut AiUi) -> Result<(), String> {
    let Some((ecm, p)) = &a.bike else { return Ok(()) };
    let files = files_dir();
    let mut all = Bikes::load(&files);
    all.set(&ecm.id, p.clone());
    all.save(&files).map_err(|e| e.to_string())?;
    a.bike_saved = p.clone();
    Ok(())
}

/// "Your bike": model (from the models the ECM fits, or typed), year, mods
/// and notes. Returns true when Save was clicked.
fn bike_editor(
    ui: &mut Ui,
    p: &Palette,
    ecm: &EcmDef,
    b: &mut Profile,
    new_mod: &mut String,
    geo: &mut Geo,
    dirty: bool,
) -> bool {
    let ctx = ui.ctx().clone();
    let mut save = false;
    egui::Grid::new("ai-bike").num_columns(2).spacing([12.0, 5.0]).show(ui, |ui| {
        ui.label("Model");
        ui.horizontal(|ui| {
            let fits = bike::models_for(ecm);
            egui::ComboBox::from_id_salt("ai-bike-model")
                .selected_text(if b.model.is_empty() { "Choose…" } else { b.model.as_str() })
                .width(240.0)
                .show_ui(ui, |ui| {
                    for m in &fits {
                        ui.selectable_value(
                            &mut b.model,
                            m.name.to_string(),
                            format!("{} ({} cc)", m.name, m.cc),
                        );
                    }
                    ui.separator();
                    for m in bike::MODELS.iter().filter(|m| !fits.contains(m)) {
                        ui.selectable_value(
                            &mut b.model,
                            m.name.to_string(),
                            format!("{} ({} cc, ECM swapped)", m.name, m.cc),
                        );
                    }
                });
            ui.add(egui::TextEdit::singleline(&mut b.model).desired_width(160.0).hint_text("or type it"));
        });
        ui.end_row();
        ui.label("Year");
        ui.horizontal(|ui| {
            let (lo, hi) = bike::years_for(ecm).unwrap_or((1999, 2010));
            egui::ComboBox::from_id_salt("ai-bike-year")
                .selected_text(b.year.map_or("Choose…".to_string(), |y| y.to_string()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut b.year, None, "not set");
                    for y in (1999..=2010).rev() {
                        let hint = if (lo..=hi).contains(&y) { "" } else { " (outside this ECM's years)" };
                        ui.selectable_value(&mut b.year, Some(y), format!("{y}{hint}"));
                    }
                });
            ui.label(
                RichText::new(format!("This {} fits {}", ecm.id, ecm.bikes.as_deref().unwrap_or("?")))
                    .small()
                    .color(p.muted),
            );
        });
        ui.end_row();
        let num = |ui: &mut Ui, v: &mut Option<u32>, on: &str, unit: &str, default: u32| {
            let mut set = v.is_some();
            ui.horizontal(|ui| {
                if ui.checkbox(&mut set, on).changed() {
                    *v = set.then_some(v.unwrap_or(default));
                }
                if let Some(x) = v {
                    ui.add(egui::DragValue::new(x).range(1..=99_999).suffix(unit));
                }
            });
        };
        let stock_cc = bike::model(&b.model).map_or(1203, |m| m.cc);
        ui.label("Engine");
        num(ui, &mut b.mods.big_bore_cc, "Big bore kit", " cc", stock_cc + 100);
        ui.end_row();
        let text = |ui: &mut Ui, label: &str, v: &mut String, hint: &str| {
            ui.label(label);
            ui.add(egui::TextEdit::singleline(v).desired_width(320.0).hint_text(hint));
            ui.end_row();
        };
        text(ui, "Wideband O2", &mut b.mods.wideband, "none (e.g. Innovate LC-2)");
        text(ui, "Exhaust", &mut b.mods.exhaust, "stock (or brand / type)");
        text(ui, "Intake / air filter", &mut b.mods.intake, "stock");
        text(ui, "Cams", &mut b.mods.cams, "stock");
        ui.label("Heads");
        ui.checkbox(&mut b.mods.ported_heads, "Ported");
        ui.end_row();
        ui.label("Final drive");
        ui.horizontal(|ui| {
            num(ui, &mut b.mods.front_pulley, "Front pulley", " teeth", 27);
            num(ui, &mut b.mods.rear_pulley, "Rear pulley", " teeth", 64);
        });
        ui.end_row();
        ui.label("Fuel");
        num(ui, &mut b.mods.fuel_octane, "Octane", "", 91);
        ui.end_row();
        text(ui, "ECM / tune", &mut b.mods.ecm_tune, "stock (or race ECM, someone's tune)");
        ui.label("Home town");
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if let Some(pl) = &b.place {
                    ui.label(format!("{} ({:.0} m)", pl.label(), pl.elevation_m));
                }
                ui.add(egui::TextEdit::singleline(&mut geo.query).desired_width(160.0).hint_text("town name"));
                let busy = geo.places_rx.is_some() || geo.weather_rx.is_some();
                if ui.add_enabled(!busy && !geo.query.trim().is_empty(), egui::Button::new("Look up")).clicked() {
                    let q = geo.query.clone();
                    geo.places_rx = Some(spawn(&ctx, move || atmosphere::lookup(&q)));
                    geo.status = None;
                }
                if let Some(pl) = b.place.clone()
                    && ui.add_enabled(!busy, egui::Button::new("Current weather")).clicked()
                {
                    geo.weather_rx = Some(spawn(&ctx, move || atmosphere::current(&pl)));
                }
                if busy {
                    ui.spinner();
                }
            });
            if !geo.places.is_empty() {
                let mut chosen = None;
                egui::ComboBox::from_id_salt("ai-geo").selected_text("Pick the right place…").width(320.0).show_ui(
                    ui,
                    |ui| {
                        for pl in &geo.places {
                            if ui.selectable_label(false, format!("{} ({:.0} m)", pl.label(), pl.elevation_m)).clicked() {
                                chosen = Some(pl.clone());
                            }
                        }
                    },
                );
                if let Some(pl) = chosen {
                    b.place = Some(pl);
                    b.weather = None;
                    geo.places.clear();
                    geo.status = None;
                }
            }
            if let Some(w) = &b.weather {
                ui.label(
                    RichText::new(format!(
                        "Measured {:.1} kPa, {:.1} °C at {}",
                        w.surface_pressure_hpa / 10.0,
                        w.temperature_c,
                        w.time
                    ))
                    .small(),
                );
            }
            if let Some((good, s)) = &geo.status {
                ui.colored_label(if *good { p.good } else { p.bad }, s);
            }
            ui.label(
                RichText::new(
                    "Look up sends the town name, and Current weather its coordinates (rounded to about 1 km), to \
                     open-meteo.com. The advisor gets only the altitude and air numbers, never the place.",
                )
                .small()
                .color(p.muted),
            );
        });
        ui.end_row();
        ui.label("Usual altitude");
        ui.horizontal(|ui| {
            num(ui, &mut b.altitude_m, "Set", " m", 500);
            if let Some(alt) = b.altitude() {
                let kpa = atmosphere::standard_pressure_kpa(alt);
                let d = atmosphere::density_ratio(kpa, atmosphere::standard_temp_c(alt));
                ui.label(
                    RichText::new(format!("{alt:.0} m: standard {kpa:.1} kPa, {:.0}% of sea-level air density", d * 100.0))
                        .small()
                        .color(p.muted),
                );
            }
        });
        ui.end_row();
        ui.label("Other mods");
        ui.vertical(|ui| {
            let mut remove = None;
            for (i, m) in b.mods.other.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(m);
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                b.mods.other.remove(i);
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(new_mod)
                        .desired_width(240.0)
                        .hint_text("e.g. velocity stacks, 10.5:1 pistons"),
                );
                if ui.add_enabled(!new_mod.trim().is_empty(), egui::Button::new("Add")).clicked() {
                    b.mods.other.push(new_mod.trim().to_string());
                    new_mod.clear();
                }
            });
        });
        ui.end_row();
    });
    ui.label("Notes");
    ui.add(
        egui::TextEdit::multiline(&mut b.notes)
            .hint_text("Anything else the advisor should always know (how you ride, goals, quirks)")
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    ui.horizontal(|ui| {
        if ui.add_enabled(dirty, egui::Button::new("Save bike")).clicked() {
            save = true;
        }
        ui.label(
            RichText::new("Sent with every question; kept in the AI folder (bikes.toml).")
                .small()
                .color(p.muted),
        );
    });
    save
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
            if which == "preview" {
                app.ai.peek_open = true;
            }
            if which == "answer" {
                prepare(app, &load_config());
            }
            if which == "answer"
                && let Some((p, turn, q)) = app.ai.preview.take()
            {
                app.ai.shown.push(Entry::You(q));
                app.ai.turns.push(turn);
                let text = "(made-up answer for a UI check) Load 30 at 2900-3400 rpm: the ECM removed about 5% \
                            fuel there, so the map is rich. Lower those cells a little and log again.\n\n\
                            Remember: Rear cylinder runs hotter (air-cooled)\n\n```json\n{\"changes\": [\
                            {\"map\": \"Tab_Fuel_Front\", \"load\": 30, \"rpm\": 2900, \"value\": 54, \"why\": \"EGO 94%, rich\"},\
                            {\"map\": \"Tab_Fuel_Front\", \"load\": 30, \"rpm\": 3400, \"value\": 49, \"why\": \"EGO 94%, rich\"},\
                            {\"map\": \"Tab_Fuel_Front\", \"load\": 125, \"rpm\": 3400, \"value\": 100, \"why\": \"lean it out\"}]}\n```";
                let reply = Reply { text: text.into(), model: Some(p.model.clone()), ..Default::default() };
                let review = app.tune.as_ref().and_then(|t| {
                    let logs: Vec<_> = app.logs.logs.iter().map(|l| (*l.log).clone()).collect();
                    review_of(&reply, t, &logs, &app.catalog, false, &p.model)
                });
                app.ai.shown.push(Entry::Ai {
                    text: text.split("```json").next().unwrap_or_default().trim().into(),
                    footer: format!("{} · 3,900 tokens in, 120 out", p.model),
                    warnings: Vec::new(),
                    suggestions: context::suggested_notes(text),
                    review,
                });
            }
        }
        "direct" => {
            app.settings.ai_guide_seen = true;
            app.ai.confirm_direct = true;
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
        let mut data = context::data_block(tune, &map, &logs);
        let profile = match &app.ai.bike {
            Some((e, b)) if e.id == tune.ecm.id => b.clone(),
            _ => Bikes::load(&files_dir()).get(&tune.ecm),
        };
        let recent = memory::recent(&files_dir(), &tune.ecm.id, None);
        data["memory"] = memory::block(profile.to_json(&tune.ecm), &recent);
        context::user_message(&question, &data)
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
    if let Some(rx) = &a.geo.places_rx {
        match rx.try_recv() {
            Ok(Ok(places)) => {
                a.geo.status = Some(if places.is_empty() {
                    (false, "No place by that name found".to_string())
                } else {
                    (true, format!("{} place(s) found: pick one", places.len()))
                });
                a.geo.places = places;
                a.geo.places_rx = None;
            }
            Ok(Err(e)) => {
                a.geo.status = Some((false, e.to_string()));
                a.geo.places_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => a.geo.places_rx = None,
        }
    }
    if let Some(rx) = &a.geo.weather_rx {
        match rx.try_recv() {
            Ok(Ok(w)) => {
                if let Some((_, b)) = &mut a.bike {
                    b.weather = Some(w);
                }
                a.geo.status = None;
                a.geo.weather_rx = None;
            }
            Ok(Err(e)) => {
                a.geo.status = Some((false, e.to_string()));
                a.geo.weather_rx = None;
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => a.geo.weather_rx = None,
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
    // The changes in the answer, through the checker (A2).
    let review = match (&r, &app.tune) {
        (Ok(reply), Some(tune)) => {
            let logs: Vec<_> = if app.ai.include_logs {
                app.logs.logs.iter().map(|l| (*l.log).clone()).collect()
            } else {
                Vec::new()
            };
            review_of(reply, tune, &logs, &app.catalog, app.ai.allow_no_data, &pending.prepared.model)
        }
        _ => None,
    };
    let a = &mut app.ai;
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
            let suggestions = context::suggested_notes(&reply.text);
            // The JSON block shows as the review table, not as text.
            let words = reply.text.split("```json").next().unwrap_or(&reply.text).trim().to_string();
            a.shown.push(Entry::Ai {
                text: words,
                footer: format!("{model} · {usage} · {secs:.0} s"),
                warnings,
                suggestions,
                review,
            });
            if a.apply_directly {
                let i = a.shown.len() - 1;
                apply_review(app, i, true);
            }
        }
        Err(e) => a.shown.push(Entry::Error(e.to_string())),
    }
}

/// The checked changes of an answer, ready for the review table.
struct Review {
    ok: Vec<(Accepted, bool)>,
    refused: Vec<Refused>,
    error: Option<String>,
    base_from: String,
    model: String,
    applied: bool,
}

fn review_of(
    reply: &Reply,
    tune: &xbtune_core::tune::Tune,
    logs: &[xbtune_core::logfile::Log],
    catalog: &xbtune_defs::Catalog,
    allow_no_data: bool,
    model: &str,
) -> Option<Review> {
    let model = reply.model.clone().unwrap_or_else(|| model.to_string());
    let suggestions = match checker::parse_changes(&reply.text) {
        Ok(s) if s.is_empty() => return None,
        Ok(s) => s,
        Err(e) => {
            return Some(Review {
                ok: Vec::new(),
                refused: Vec::new(),
                error: Some(e),
                base_from: String::new(),
                model,
                applied: false,
            });
        }
    };
    let (baseline, base_from) = checker::baseline(&files_dir(), tune, catalog);
    let cx = checker::Context { tune, baseline: &baseline, logs, allow_no_data };
    let (ok, refused) = checker::check(&suggestions, &cx, &checker::Limits::default());
    Some(Review {
        ok: ok.into_iter().map(|c| (c, true)).collect(),
        refused,
        error: None,
        base_from,
        model,
        applied: false,
    })
}

/// The review table under an answer. Returns true when Apply was clicked.
fn review_table(ui: &mut Ui, p: &Palette, r: &mut Review, i: usize) -> bool {
    let mut apply = false;
    if let Some(e) = &r.error {
        ui.colored_label(p.bad, format!("The suggested changes couldn't be read: {e}"));
        return false;
    }
    ui.add_space(4.0);
    ui.label(RichText::new(format!("Suggested changes that pass XBTune's checks ({})", r.ok.len())).strong());
    if !r.ok.is_empty() {
        egui::Grid::new(("ai-review", i)).num_columns(6).striped(true).spacing([10.0, 3.0]).show(ui, |ui| {
            for h in ["", "Map", "Cell", "Change", "Why", ""] {
                ui.label(RichText::new(h).small().strong());
            }
            ui.end_row();
            for (c, on) in &mut r.ok {
                ui.add_enabled_ui(!r.applied, |ui| ui.checkbox(on, ""));
                let map = if c.map.contains("Front") { "Front" } else { "Rear" };
                let kind = if c.map.starts_with("Tab_Spark") { "spark" } else { "fuel" };
                ui.label(format!("{kind} {map}"));
                ui.label(match (c.load, c.rpm) {
                    (Some(l), Some(rpm)) => format!("load {l}, {rpm} rpm"),
                    _ => format!("row {}, col {}", c.row, c.col),
                });
                let change = if kind == "spark" {
                    format!("{} -> {} ({:+.1}°)", c.old, c.new, c.new - c.old)
                } else {
                    format!("{} -> {} ({:+.1}%)", c.old, c.new, (c.new / c.old - 1.0) * 100.0)
                };
                ui.label(RichText::new(change).color(if c.new < c.old { p.good } else { p.warn }));
                ui.scope(|ui| {
                    ui.set_min_width(200.0);
                    ui.set_max_width(260.0);
                    ui.add(egui::Label::new(RichText::new(&c.why).small()).wrap());
                });
                if c.flags.is_empty() {
                    ui.label("");
                } else {
                    ui.colored_label(p.warn, "check").on_hover_text(c.flags.join("\n"));
                }
                ui.end_row();
            }
        });
        let n = r.ok.iter().filter(|(_, on)| *on).count();
        ui.horizontal(|ui| {
            if r.applied {
                ui.colored_label(p.good, "Applied to the tune (Undo reverts it).");
            } else {
                let b = theme::primary(format!("Apply {n} change(s) to the tune"));
                if ui.add_enabled(n > 0, b).clicked() {
                    apply = true;
                }
            }
            ui.label(
                RichText::new(format!("Drift is measured from {}.", r.base_from)).small().color(p.muted),
            );
        });
    }
    if !r.refused.is_empty() {
        egui::CollapsingHeader::new(format!("Refused by XBTune's checks ({})", r.refused.len()))
            .id_salt(("ai-refused", i))
            .show(ui, |ui| {
                for x in &r.refused {
                    ui.label(RichText::new(format!("{}: {}", x.what, x.reason)).small());
                }
            });
    }
    apply
}

/// Apply the ticked changes of answer `i` as one undo step.
fn apply_review(app: &mut XbApp, i: usize, direct: bool) {
    let Some(Entry::Ai { review: Some(r), .. }) = app.ai.shown.get_mut(i) else { return };
    if r.applied {
        return;
    }
    let chosen: Vec<Accepted> = r.ok.iter().filter(|(_, on)| *on).map(|(c, _)| c.clone()).collect();
    if chosen.is_empty() {
        return;
    }
    let model = r.model.clone();
    let Some(tune) = &mut app.tune else { return };
    match checker::apply(tune, &chosen, &model) {
        Ok(n) => {
            r.applied = true;
            tracing::info!(cells = n, direct, %model, "AI changes applied");
            let how = if direct { "applied directly (your setting)" } else { "applied" };
            app.say(
                format!("AI changes {how}: {n} cell(s) in the tune (Undo reverts them). Nothing was written to the ECM."),
                false,
            );
        }
        Err(e) => app.say(format!("The AI changes couldn't be applied: {e}"), true),
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
    if let Some(e) = app.tune.as_ref().map(|t| t.ecm.clone()) {
        ensure_bike(&mut app.ai, &e);
    }
    let mut add_note: Option<String> = None;
    let mut apply_now: Option<usize> = None;
    let mut save_bike_now = false;
    let mut close = false;
    let mut ask = false;
    let mut want_peek = false;
    let mut cancel = false;
    let mut fresh = false;
    let mut to_settings = false;
    let mut to_wizard = false;
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
                ui.horizontal(|ui| {
                    if ui.button("Set up with the guide…").clicked() {
                        to_wizard = true;
                    }
                    if ui.button("Open Settings").clicked() {
                        to_settings = true;
                    }
                });
                return;
            }
            for (i, e) in a.shown.iter_mut().enumerate() {
                match e {
                    Entry::You(q) => {
                        ui.label(RichText::new("You").strong());
                        ui.label(q.as_str());
                    }
                    Entry::Ai { text, footer, warnings, suggestions, review } => {
                        ui.label(RichText::new("Advisor").strong().color(p.accent));
                        ui.add(egui::Label::new(text.as_str()).wrap().selectable(true));
                        if let Some(r) = review
                            && review_table(ui, &p, r, i)
                        {
                            apply_now = Some(i);
                        }
                        for w in warnings {
                            ui.colored_label(p.warn, w);
                        }
                        let have = a.bike.as_ref().map_or("", |(_, b)| b.notes.as_str());
                        for s in suggestions.iter().filter(|s| !have.contains(s.as_str())) {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(format!("Suggested note: {s}")).small());
                                if ui.small_button("Add to your bike's notes").clicked() {
                                    add_note = Some(s.clone());
                                }
                            });
                        }
                        ui.label(RichText::new(footer.as_str()).small().color(p.muted));
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
            let dirty = a.bike.as_ref().is_some_and(|(_, b)| *b != a.bike_saved);
            if let Some((ecm, b)) = &mut a.bike {
                let title = b.title();
                let header = if title.is_empty() {
                    "Your bike: not described yet (the advisor uses this every time)".to_string()
                } else {
                    let mods = b.mod_lines().len();
                    format!("Your bike: {title}{}", if mods > 0 { format!(", {mods} mod(s)") } else { ", stock".into() })
                };
                let open = std::mem::take(&mut a.bike_open);
                egui::CollapsingHeader::new(header)
                    .id_salt("ai-bike")
                    .open(open.then_some(true))
                    .show(ui, |ui| {
                        if bike_editor(ui, &p, ecm, b, &mut a.new_mod, &mut a.geo, dirty) {
                            save_bike_now = true;
                        }
                    });
            }
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
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut a.allow_no_data, "Allow changes to cells without logged data")
                    .on_hover_text("For requests like smoothing; otherwise only cells with enough logged samples may change");
                let mut direct = a.apply_directly;
                if ui
                    .checkbox(&mut direct, "Apply checked changes directly")
                    .on_hover_text("This session only. Changes that pass XBTune's checks go into the tune without the Apply click. Never to the ECM.")
                    .changed()
                {
                    if direct {
                        a.confirm_direct = true;
                    } else {
                        a.apply_directly = false;
                    }
                }
            });
            ui.add(
                egui::TextEdit::multiline(&mut a.question)
                    .hint_text("Ask about this map, e.g. \"Where is it rich at cruise?\"")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                let b = theme::primary("Send");
                if ui.add_enabled(!a.question.trim().is_empty(), b).clicked() {
                    ask = true;
                }
                if !a.shown.is_empty() && ui.button("New conversation").clicked() {
                    fresh = true;
                }
                if let Some(j) = &a.journal {
                    ui.label(RichText::new(format!("Saved in {}", j.path.display())).small().color(p.muted));
                }
            });
            let dest = if cfg.is_local() {
                "Goes to the model on this computer; nothing leaves it.".to_string()
            } else {
                format!("Goes to {} (hosted). No VIN, names, file paths or Bluetooth addresses.", cfg.host())
            };
            ui.label(RichText::new(dest).small().color(p.muted));
            // The exact request, for those who want to look (collapsed).
            let open_peek = std::mem::take(&mut a.peek_open);
            let r = egui::CollapsingHeader::new(RichText::new("What gets sent").small())
                .id_salt("ai-peek")
                .open(open_peek.then_some(true))
                .show(ui, |ui| match &a.peek {
                    Some((q, prep)) if *q == a.question => {
                        ui.label(
                            RichText::new(format!("{} bytes, about {} tokens", prep.body.len(), prep.approx_tokens))
                                .small()
                                .color(p.muted),
                        );
                        let mut body = prep.body.clone();
                        egui::ScrollArea::vertical().id_salt("ai-body").max_height(260.0).show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::multiline(&mut body)
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                    }
                    _ => {
                        want_peek = true;
                    }
                });
            if r.body_returned.is_none() {
                a.peek = None;
            }
        });
        ui.separator();
        if ui.button("Close").clicked() {
            close = true;
        }
    });
    if let Some(i) = apply_now {
        apply_review(app, i, false);
    }
    if app.ai.confirm_direct {
        direct_warning(app, ui, &p);
    }
    if let Some(line) = add_note
        && let Some((_, b)) = &mut app.ai.bike
    {
        b.add_note(&line);
        save_bike_now = true;
    }
    if save_bike_now && let Err(e) = save_bike(&mut app.ai) {
        app.say(format!("Couldn't save your bike: {e}"), true);
    }
    if guide_done {
        app.settings.ai_guide_seen = true;
        app.save_settings();
    }
    if to_wizard {
        crate::ai_wizard::open(app);
    }
    if to_settings {
        app.view = crate::app::View::Settings;
        close = true;
    }
    if ask {
        // Sent at once; "What gets sent" shows the same request on demand.
        prepare(app, &cfg);
        send(app, &ctx);
        app.ai.peek = None;
    } else if want_peek {
        prepare(app, &cfg);
        if let Some((p, _, q)) = app.ai.preview.take() {
            app.ai.peek = Some((q, p));
        }
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
    if close || modal.should_close() {
        app.ai.open = None;
        app.ai.preview = None;
    }
}

/// The warning before the AI may change the tune without a click.
fn direct_warning(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    let mut yes = false;
    let mut no = false;
    egui::Modal::new(egui::Id::new("ai-direct")).show(ui.ctx(), |ui| {
        ui.set_width(480.0);
        ui.label(
            RichText::new("Let the AI change the tune directly?").font(theme::strong_font(18.0)).color(p.bad),
        );
        for line in [
            "Changes that pass XBTune's checks go into the open tune as soon as the answer arrives, without \
             the Apply click. Each answer is one undo step (\"AI: ...\").",
            "The checks stop the dangerous moves (big steps, leaning at high load, more spark at high load, \
             more than 15% from your last ECM read). They can't make a wrong suggestion right: models are \
             confidently wrong, and an allowed change can still be wrong for your engine.",
            "Nothing is ever written to the ECM by the AI. Burning stays your step, with the normal review.",
            "This lasts until you close XBTune.",
        ] {
            crate::wizard::bullet(ui, line);
        }
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new("Allow for this session").color(egui::Color32::WHITE))
                        .fill(p.warn),
                )
                .clicked()
            {
                yes = true;
            }
            if ui.button("Cancel").clicked() {
                no = true;
            }
        });
    });
    if yes {
        app.ai.apply_directly = true;
        app.ai.confirm_direct = false;
        tracing::info!("AI apply-directly allowed for this session");
    } else if no {
        app.ai.confirm_direct = false;
    }
}

fn guide_text(ui: &mut Ui, p: &Palette) {
    ui.label(RichText::new("Before you start").font(theme::strong_font(16.0)));
    for line in [
        "The advisor is a language model. You ask about a map; it answers in words. It never changes the \
         tune or writes to the ECM. You make any changes yourself, and they go through the normal Burn review.",
        "You choose the model in Settings: one on this computer (Ollama, LM Studio, ...), where nothing \
         leaves the machine, or a hosted one with your own API key, where you pay the provider.",
        "Each question goes with the map and its axes, AFV, your bike profile and statistics from your \
         open logs; no VIN, names, file paths or Bluetooth addresses. \"What gets sent\" under the question \
         shows the exact request if you want to look.",
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
    let mut wizard = false;
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
        if ui
            .button("Set up with the guide…")
            .on_hover_text("Finds model servers on this computer, or walks you through a hosted provider")
            .clicked()
        {
            wizard = true;
        }
        ui.add_space(4.0);
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
    if wizard {
        crate::ai_wizard::open(app);
    }
}
