// SPDX-License-Identifier: GPL-3.0-or-later
//! The app: state, the live session, and the window layout.
//!
//! The ECM link runs as an `xbtune_core::session` task on the tokio runtime.
//! The UI only drains its events each frame and sends commands; it never
//! waits on I/O.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use egui::{Align, Color32, Layout, RichText, Ui};
use tokio::runtime::Handle;
use xbtune_core::env::PlugState;
use xbtune_core::live::{Sample, channels};
use xbtune_core::session::{self, Command, Event, Identity, LinkState, Session, SessionConfig};
use xbtune_defs::{Catalog, RtVar};
use xbtune_link::Target;
use xbtune_proto::{EcmAddress, LinkStats};

use crate::connect::ConnectUi;
use crate::settings::{SavedTarget, Settings};
use crate::theme::{self, Palette, ThemePref};

/// History kept for charts.
const HISTORY: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Connect,
    Dashboard,
    Data,
    Charts,
    Diagnostics,
    Tune,
}

/// What the UI knows about the link.
#[derive(Default)]
pub struct LinkView {
    pub target: Option<SavedTarget>,
    pub state: Option<LinkState>,
    pub identity: Option<Identity>,
    pub channels: Vec<RtVar>,
    pub stats: Option<LinkStats>,
    pub hz: f64,
    pub reconnects: u32,
    pub log: Option<(PathBuf, u64)>,
}

impl LinkView {
    pub fn live(&self) -> bool {
        matches!(self.state, Some(LinkState::Live))
    }

    pub fn channel(&self, name: &str) -> Option<&RtVar> {
        self.channels.iter().find(|v| v.name == name)
    }
}

/// Samples for charts, oldest first.
#[derive(Default)]
pub struct History {
    pub samples: VecDeque<Sample>,
}

impl History {
    fn push(&mut self, s: Sample) {
        while self.samples.front().is_some_and(|f| s.t - f.t > HISTORY.as_secs_f64()) {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }

    pub fn latest(&self) -> Option<&Sample> {
        self.samples.back()
    }
}

pub struct Message {
    pub text: String,
    pub error: bool,
    pub at: Instant,
}

pub struct XbApp {
    pub rt: Handle,
    pub catalog: Arc<Catalog>,
    pub settings: Settings,
    settings_path: Option<PathBuf>,
    pub view: View,
    pub connect: ConnectUi,
    session: Option<Session>,
    pub link: LinkView,
    pub history: History,
    pub plugs: Vec<PlugState>,
    pub messages: Vec<Message>,
    pub chart_paused: Option<f64>,
    pub diag: crate::diagnostics::DiagUi,
    /// The tune being edited, and an image to compare it with.
    pub tune: Option<xbtune_core::tune::Tune>,
    pub compare: Option<(PathBuf, xbtune_core::image::Image)>,
    pub tune_ui: crate::tune_view::TuneUi,
    pub burn: crate::burn_view::BurnUi,
    pub wizard: crate::wizard::WizardUi,
    pub report: crate::report_view::ReportUi,
    /// Variables to write again on a retry.
    pub pending_chosen: std::collections::BTreeSet<String>,
    /// EEPROM writes allowed for the current session.
    write_permitted: bool,
    /// The demo ECM, to start and stop its engine.
    demo_sim: Option<xbtune_sim::SharedSim>,
    /// ECM commands allowed for the current session.
    permitted: bool,
    egui_ctx: egui::Context,
    shot: Option<Shot>,
}

/// Test hook: `XBTUNE_SCREENSHOT=out.ppm [XBTUNE_VIEW=charts]
/// [XBTUNE_THEME=light] [XBTUNE_SHOT_AFTER=5] xbtune` starts the demo, opens
/// the view, saves a screenshot and quits. Settings are not saved.
struct Shot {
    path: PathBuf,
    after: Duration,
    started: Instant,
    requested: bool,
    /// `XBTUNE_SHOT_RUN=fan`: stop the demo engine and start this test first.
    run: Option<xbtune_proto::Function>,
}

fn write_ppm(path: &std::path::Path, img: &egui::ColorImage) -> std::io::Result<()> {
    let mut out = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
    for px in &img.pixels {
        out.extend_from_slice(&[px.r(), px.g(), px.b()]);
    }
    std::fs::write(path, out)
}

impl XbApp {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: Handle, crash: Option<crate::report::Crash>) -> Self {
        theme::install(&cc.egui_ctx);
        let shot = std::env::var_os("XBTUNE_SCREENSHOT").map(|p| Shot {
            path: p.into(),
            after: Duration::from_secs_f64(
                std::env::var("XBTUNE_SHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(5.0),
            ),
            started: Instant::now(),
            requested: false,
            run: std::env::var("XBTUNE_SHOT_RUN").ok().and_then(|n| xbtune_core::diag::function_by_name(&n)),
        });
        let settings_path = if shot.is_some() { None } else { Settings::path() };
        let mut settings = Settings::load(settings_path.as_deref());
        if shot.is_some() {
            settings.theme = match std::env::var("XBTUNE_THEME").as_deref() {
                Ok("light") => ThemePref::Light,
                _ => ThemePref::Dark,
            };
        }
        theme::apply(&cc.egui_ctx, settings.theme);
        let catalog = Arc::new(Catalog::builtin().expect("built-in definitions load"));
        let mut app = Self {
            rt,
            catalog,
            connect: ConnectUi::new(settings.last.clone()),
            settings,
            settings_path,
            view: View::Connect,
            session: None,
            link: LinkView::default(),
            history: History::default(),
            plugs: xbtune_core::env::plug_states(),
            messages: Vec::new(),
            chart_paused: None,
            diag: Default::default(),
            tune: None,
            compare: None,
            tune_ui: Default::default(),
            burn: Default::default(),
            wizard: Default::default(),
            report: Default::default(),
            pending_chosen: Default::default(),
            write_permitted: false,
            demo_sim: None,
            permitted: false,
            egui_ctx: cc.egui_ctx.clone(),
            shot,
        };
        if let Some(p) =
            std::env::var_os("XBTUNE_TUNE").map(PathBuf::from).or_else(|| app.settings.last_tune.clone())
            && p.exists()
        {
            crate::tune_view::open(&mut app, &p, false);
            app.tune_ui.status = None;
        }
        if app.shot.is_some()
            && let Ok(script) = std::env::var("XBTUNE_SHOT_TUNE")
        {
            crate::tune_view::scripted(&mut app, &script);
        }
        if app.shot.is_some() {
            let view = std::env::var("XBTUNE_VIEW").unwrap_or_default();
            if view != "connect" {
                app.start(SavedTarget::Demo);
            }
            app.view = match view.as_str() {
                "data" => View::Data,
                "charts" => View::Charts,
                "connect" => View::Connect,
                "diag" => View::Diagnostics,
                "tune" => View::Tune,
                _ => View::Dashboard,
            };
        }
        // A crash last time comes first; the guide waits for the next start.
        if let Some(c) = crash.clone()
            && app.shot.is_none()
        {
            app.report.offer_crash(c, true);
        } else if app.shot.is_none() && app.settings.show_wizard {
            app.wizard.open();
        }
        if app.shot.is_some() {
            // XBTUNE_SHOT_WIZARD=N: guide step N; XBTUNE_SHOT_REPORT=1|crash
            // (the previous session's crash if there is one, else a made-up one).
            if let Some(n) = std::env::var("XBTUNE_SHOT_WIZARD").ok().and_then(|n| n.parse::<usize>().ok()) {
                app.wizard.step = Some(n.min(crate::wizard::STEPS - 1));
            }
            match std::env::var("XBTUNE_SHOT_REPORT").as_deref() {
                Ok("crash") => app.report.offer_crash(
                    crash.unwrap_or_else(|| crate::report::Crash {
                        summary: "panic in tune_view::show".into(),
                        message: "index out of bounds: the len is 16 but the index is 16".into(),
                        fingerprint: crate::report::fingerprint("shot"),
                        version: crate::report::version(),
                        when: 0,
                        details: String::new(),
                    }),
                    true,
                ),
                Ok(_) => app.report.open(),
                Err(_) => {}
            }
        } else {
            // Reports that couldn't be sent before.
            crate::report::send_pending_later(Duration::from_secs(15));
        }
        if app.settings.auto_connect
            && let Some(t) = app.settings.last.clone()
        {
            app.start(t);
        }
        app
    }

    pub fn is_screenshot(&self) -> bool {
        self.shot.is_some()
    }

    pub fn save_settings(&self) {
        self.settings.save(self.settings_path.as_deref());
    }

    pub fn say(&mut self, text: impl Into<String>, error: bool) {
        let text = text.into();
        if error {
            tracing::warn!("shown to user: {text}");
        } else {
            tracing::info!("shown to user: {text}");
        }
        self.messages.push(Message { text, error, at: Instant::now() });
    }

    /// Start a live session to `target`.
    pub fn start(&mut self, target: SavedTarget) {
        self.stop();
        let (opener, label): (session::Opener, String) = match &target {
            SavedTarget::Demo => {
                let mut sim = xbtune_sim::Simulator::bueib_running();
                // Like a BuellTooth: ~35 ms latency plus 9600-baud wire time.
                sim.set_rates(xbtune_sim::FaultRates {
                    delay: 1.0,
                    delay_by: Duration::from_millis(35),
                    ..Default::default()
                });
                sim.set_wire_baud(Some(9600));
                let sim = xbtune_sim::shared(sim);
                self.demo_sim = Some(sim.clone());
                let opener: session::Opener = Arc::new(move || {
                    let sim = sim.clone();
                    Box::pin(async move { Ok(xbtune_sim::in_process(sim)) })
                });
                (opener, "simulated ECM".into())
            }
            SavedTarget::Bluetooth { address, channel, .. } => match xbtune_link::parse_bt_address(address) {
                Ok(a) => {
                    let t = Target::Rfcomm { address: a, channel: *channel };
                    (session::target_opener(t.clone()), t.to_string())
                }
                Err(e) => return self.say(e, true),
            },
            SavedTarget::Serial { path } => {
                let t = Target::Serial { path: path.clone(), baud: EcmAddress::Stock.baud() };
                (session::target_opener(t.clone()), t.to_string())
            }
        };
        tracing::info!("connecting to {label}");
        let mut cfg = SessionConfig::new(label, EcmAddress::Stock);
        cfg.permit = xbtune_core::diag::permit(target == SavedTarget::Demo).ok();
        self.permitted = cfg.permit.is_some();
        cfg.write_permit = xbtune_core::diag::write_permit(target == SavedTarget::Demo).ok();
        self.write_permitted = cfg.write_permit.is_some();
        if target != SavedTarget::Demo {
            self.demo_sim = None;
        }
        let ctx = self.egui_ctx.clone();
        let notify: session::Notify = Arc::new(move || ctx.request_repaint());
        let catalog = self.catalog.clone();
        let _guard = self.rt.enter();
        self.session = Some(session::start(cfg, opener, catalog, Some(notify)));
        self.link = LinkView { target: Some(target.clone()), ..Default::default() };
        self.history = History::default();
        self.chart_paused = None;
        self.diag = Default::default();
        self.settings.last = Some(target);
        self.save_settings();
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.session.take() {
            s.send(Command::Stop);
        }
        if let Some(st) = &mut self.link.state
            && !matches!(st, LinkState::Stopped { .. })
        {
            *st = LinkState::Stopped { reason: None };
        }
        self.link.log = None;
    }

    /// A verified read from the ECM: save it as a backup, then open it (or,
    /// with unsaved edits open, offer it for comparison).
    fn got_eeprom(&mut self, img: xbtune_core::image::Image) {
        let path = crate::tune_view::images_dir().join(format!(
            "{}_{}.xbt",
            img.ecm_id,
            xbtune_core::time::file_stamp(SystemTime::now())
        ));
        if let Err(e) = img.save(&path, &self.catalog) {
            self.say(format!("Read OK, but saving the backup failed: {e}"), true);
            return;
        }
        if self.tune.as_ref().is_some_and(|t| t.is_modified()) {
            self.compare = Some((path.clone(), img));
            self.say(
                format!(
                    "EEPROM saved to {}. Your unsaved edits stay open; it's loaded for comparison.",
                    path.display()
                ),
                false,
            );
        } else {
            crate::tune_view::open(self, &path, false);
            self.say(format!("EEPROM read, verified and saved to {}", path.display()), false);
        }
        self.view = View::Tune;
    }

    /// Unsaved edits at exit go to a recovery file.
    fn save_recovery(&mut self) {
        if self.shot.is_some() {
            return;
        }
        let Some(t) = &mut self.tune else { return };
        if !t.is_modified() {
            return;
        }
        let path = crate::tune_view::images_dir().join(format!(
            "recovered_{}_{}.xbt",
            t.ecm.id,
            xbtune_core::time::file_stamp(SystemTime::now())
        ));
        let catalog = self.catalog.clone();
        if let Err(e) = t.save(&path, &catalog) {
            tracing::warn!(error = %e, "could not save recovery file");
        }
    }

    pub fn writes_permitted(&self) -> bool {
        self.write_permitted
    }

    pub fn session_permitted(&self) -> bool {
        self.permitted
    }

    pub fn is_demo(&self) -> bool {
        self.demo_sim.is_some() && self.link.target == Some(SavedTarget::Demo)
    }

    pub fn demo_engine_running(&self) -> bool {
        self.demo_sim.as_ref().is_some_and(|s| s.lock().map(|s| s.engine_running()).unwrap_or(false))
    }

    pub fn set_demo_engine_running(&self, running: bool) {
        if let Some(s) = &self.demo_sim
            && let Ok(mut s) = s.lock()
        {
            s.set_engine_running(running);
        }
    }

    pub fn send(&self, c: Command) {
        if let Some(s) = &self.session {
            s.send(c);
        }
    }

    pub fn start_log(&mut self) {
        let Some(id) = &self.link.identity else { return };
        let dir = xbtune_core::env::Dirs::detect().map(|d| d.logs()).unwrap_or_else(|| PathBuf::from("."));
        let path =
            dir.join(format!("{}_{}.csv", id.ecm.id, xbtune_core::time::file_stamp(SystemTime::now())));
        self.send(Command::StartLog { path, note: None });
    }

    fn drain_events(&mut self) {
        let Some(s) = &mut self.session else { return };
        let mut events = Vec::new();
        while let Ok(e) = s.events.try_recv() {
            events.push(e);
        }
        for e in events {
            match e {
                Event::State(st) => {
                    if !matches!(st, LinkState::Connecting { .. }) {
                        tracing::info!(state = ?st, "link");
                    }
                    match &st {
                        LinkState::Live if self.view == View::Connect && self.shot.is_none() => {
                            self.view = View::Dashboard
                        }
                        LinkState::Stopped { reason: Some(r) } => self.say(r.clone(), true),
                        _ => {}
                    }
                    if matches!(st, LinkState::Stopped { .. }) {
                        self.session = None;
                        self.link.log = None;
                        self.diag.running = None;
                    }
                    self.link.state = Some(st);
                }
                Event::Identified(id) => {
                    tracing::info!("ECM {} ({}, {})", id.version.trim(), id.ecm.id, id.ecm.family);
                    self.link.channels = channels(&id.definition);
                    self.link.identity = Some(id);
                }
                Event::Sample(s) => self.history.push(s),
                Event::Stats { stats, hz, reconnects, log_rows } => {
                    self.link.stats = Some(stats);
                    self.link.hz = hz;
                    self.link.reconnects = reconnects;
                    if let (Some(log), Some(rows)) = (&mut self.link.log, log_rows) {
                        log.1 = rows;
                    }
                }
                Event::LogStarted(p) => self.link.log = Some((p, 0)),
                Event::LogStopped { path, rows, error } => {
                    self.link.log = None;
                    match error {
                        Some(e) => self.say(format!("Logging stopped: {e}"), true),
                        None => self.say(format!("Saved {rows} rows to {}", path.display()), false),
                    }
                }
                Event::FunctionProgress { function, stage } => {
                    self.diag.running = Some((function, Some(stage)))
                }
                Event::FunctionDone { function, result } => {
                    self.diag.running = None;
                    self.diag.last = Some((function, result));
                }
                Event::ReadProgress(p) => {
                    if !crate::burn_view::on_read_progress(self, p.done, p.total) {
                        self.tune_ui.read_progress = Some((p.done, p.total));
                    }
                }
                Event::BurnPrepared { backup, path, link } => {
                    crate::burn_view::on_prepared(self, *backup, path, link)
                }
                Event::BurnProgress(s) => crate::burn_view::on_progress(self, s),
                Event::BurnDone(r) => crate::burn_view::on_done(self, r),
                Event::ReadFailed(e) => {
                    self.tune_ui.read_progress = None;
                    self.say(format!("EEPROM read failed: {e}"), true);
                }
                Event::EepromRead(img) => {
                    self.tune_ui.read_progress = None;
                    self.got_eeprom(*img);
                }
                Event::Warning(w) => self.say(w, true),
            }
            if self.session.is_none() {
                break;
            }
        }
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        let p = Palette::for_visuals(ui.visuals());
        ui.horizontal(|ui| {
            ui.label(RichText::new("XBTune").font(theme::strong_font(18.0)).color(p.accent));
            ui.add_space(12.0);
            let (dot, text) = status_text(&self.link, &p);
            let (r, painter) = ui.allocate_painter(egui::vec2(10.0, 10.0), egui::Sense::hover());
            painter.circle_filled(r.rect.center(), 5.0, dot);
            ui.label(text);
            if let (Some(id), Some(s)) = (&self.link.identity, self.history.latest()) {
                let codes = xbtune_core::diag::codes_from_runtime(
                    &id.definition,
                    &s.frame,
                    xbtune_core::diag::CodeKind::Current,
                );
                if !codes.is_empty() {
                    let list: Vec<&str> = codes.iter().map(|c| c.code.as_str()).collect();
                    let b = egui::Button::new(
                        RichText::new(format!("Code {}", list.join(", "))).color(Color32::WHITE),
                    )
                    .fill(p.bad)
                    .small();
                    if ui.add(b).on_hover_text("Current trouble codes: open Diagnostics").clicked() {
                        self.view = View::Diagnostics;
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let next = match self.settings.theme {
                    ThemePref::System => ThemePref::Dark,
                    ThemePref::Dark => ThemePref::Light,
                    ThemePref::Light => ThemePref::System,
                };
                let label = match self.settings.theme {
                    ThemePref::System => "Theme: system",
                    ThemePref::Dark => "Theme: dark",
                    ThemePref::Light => "Theme: light",
                };
                if ui.button(label).clicked() {
                    self.settings.theme = next;
                    theme::apply(ui.ctx(), next);
                    self.save_settings();
                }
                if self.session.is_some() {
                    if ui.button("Disconnect").clicked() {
                        self.stop();
                    }
                    if self.link.live() || self.link.log.is_some() {
                        match &self.link.log {
                            Some((_, rows)) => {
                                let b = egui::Button::new(
                                    RichText::new(format!("■ Stop log ({rows})")).color(Color32::WHITE),
                                )
                                .fill(p.bad);
                                if ui.add(b).clicked() {
                                    self.send(Command::StopLog);
                                }
                            }
                            None => {
                                if ui
                                    .button(RichText::new("● Record").color(p.bad))
                                    .on_hover_text("Log every frame to CSV")
                                    .clicked()
                                {
                                    self.start_log();
                                }
                            }
                        }
                    }
                }
                if let Some(st) = &self.link.stats {
                    let rtt =
                        st.rtt_avg.map_or("-".into(), |d| format!("{:.0} ms", d.as_secs_f64() * 1000.0));
                    let err = st.recent_error_rate() * 100.0;
                    let color = if err >= 10.0 {
                        p.bad
                    } else if err > 0.0 {
                        p.warn
                    } else {
                        p.muted
                    };
                    ui.label(
                        RichText::new(format!(
                            "{:.1} Hz · RTT {rtt} · errors {err:.0}% · retries {} · reconnects {}",
                            self.link.hz, st.retries, self.link.reconnects
                        ))
                        .color(color)
                        .size(12.5),
                    );
                }
            });
        });
    }

    fn nav(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        for (v, label) in [
            (View::Connect, "Connect"),
            (View::Dashboard, "Dashboard"),
            (View::Data, "Live data"),
            (View::Charts, "Charts"),
            (View::Diagnostics, "Diagnostics"),
            (View::Tune, "Tune"),
        ] {
            let selected = self.view == v;
            let text = RichText::new(label).size(15.0);
            let text = if selected { text.font(theme::strong_font(15.0)) } else { text };
            if ui.add_sized([ui.available_width(), 34.0], egui::Button::selectable(selected, text)).clicked()
            {
                self.view = v;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).small().weak());
            ui.label(RichText::new("EEPROM writes only via Burn").small().weak());
            ui.add_space(6.0);
            let w = ui.available_width();
            if ui.add_sized([w, 28.0], egui::Button::new("Report a problem")).clicked() {
                self.report.open();
            }
            if ui.add_sized([w, 28.0], egui::Button::new("Getting started")).clicked() {
                self.wizard.open();
            }
        });
    }

    fn messages(&mut self, ui: &mut Ui) {
        self.messages.retain(|m| m.at.elapsed() < Duration::from_secs(if m.error { 20 } else { 8 }));
        if self.messages.is_empty() {
            return;
        }
        let p = Palette::for_visuals(ui.visuals());
        let mut dismiss = None;
        for (i, m) in self.messages.iter().enumerate() {
            let color = if m.error { p.bad } else { p.good };
            egui::Frame::new()
                .fill(color.gamma_multiply(0.12))
                .stroke(egui::Stroke::new(1.0, color))
                .corner_radius(8)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(&m.text);
                        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                            if ui.small_button("Dismiss").clicked() {
                                dismiss = Some(i);
                            }
                        });
                    });
                });
        }
        if let Some(i) = dismiss {
            self.messages.remove(i);
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
    }
}

impl XbApp {
    fn screenshot(&mut self, ui: &mut Ui) {
        let Some(shot) = &mut self.shot else { return };
        ui.ctx().request_repaint_after(Duration::from_millis(100));
        if self.link.live()
            && let Some(f) = shot.run.take()
        {
            self.set_demo_engine_running(false);
            self.diag.running = Some((f, None));
            self.send(Command::Function(f));
        }
        // XBTUNE_SHOT_BURN=NAME=VALUE: edit the open tune, start a burn;
        // XBTUNE_SHOT_BURN_GO=1 also confirms it.
        if self.link.live()
            && !self.burn.is_open()
            && self.tune.as_ref().is_some_and(|t| !t.is_modified())
            && let Ok(spec) = std::env::var("XBTUNE_SHOT_BURN")
            && let Some((name, v)) = spec.split_once('=')
            && let Ok(v) = v.parse::<f64>()
            && let Some(t) = &mut self.tune
        {
            let _ = t.set("shot", &[(name.to_string(), 0, v)]);
            let mode = crate::burn_view::Mode::Tune {
                tune: Box::new(t.image.clone()),
                opened: t.opened_bytes().to_vec(),
            };
            self.set_demo_engine_running(false);
            crate::burn_view::start(self, mode);
        }
        if std::env::var_os("XBTUNE_SHOT_BURN_GO").is_some() {
            crate::burn_view::auto_confirm(self);
        }
        if self.link.live()
            && std::env::var_os("XBTUNE_SHOT_READ").is_some()
            && self.tune_ui.read_progress.is_none()
            && self.tune.is_none()
        {
            self.tune_ui.read_progress = Some((0, 1));
            self.send(Command::ReadEeprom);
        }
        if self.link.live()
            && self.diag.running.is_none()
            && let Some(f) = std::env::var("XBTUNE_SHOT_CONFIRM")
                .ok()
                .and_then(|n| xbtune_core::diag::function_by_name(&n))
        {
            self.diag.ask(f);
        }
        let Some(shot) = &mut self.shot else { return };
        if !shot.requested && shot.started.elapsed() >= shot.after {
            shot.requested = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
        let image = ui.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            if let Err(e) = write_ppm(&shot.path, &img) {
                eprintln!("screenshot: {e}");
            }
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn status_text(link: &LinkView, p: &Palette) -> (Color32, String) {
    let target = link.target.as_ref().map(|t| t.label()).unwrap_or_default();
    match &link.state {
        None | Some(LinkState::Stopped { .. }) => (p.muted, "Not connected".into()),
        Some(LinkState::Connecting { attempt }) if *attempt > 1 => {
            (p.warn, format!("Connecting to {target} (attempt {attempt})…"))
        }
        Some(LinkState::Connecting { .. }) => (p.warn, format!("Connecting to {target}…")),
        Some(LinkState::Reconnecting { .. }) => (p.warn, format!("Link lost, reconnecting to {target}…")),
        Some(LinkState::Live) => {
            let id = link
                .identity
                .as_ref()
                .map(|i| format!("{} · {}", i.version, i.ecm.family))
                .unwrap_or_default();
            (p.good, format!("Live · {id}"))
        }
    }
}

impl eframe::App for XbApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        self.connect.poll();
        // A panic the app survived (a background task): offer a report now.
        if !self.report.is_open()
            && let Some(c) = crate::report::take_new_crash()
        {
            self.report.offer_crash(c, false);
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.screenshot(ui);
        let panel = ui.visuals().panel_fill;
        egui::Panel::top("top")
            .frame(egui::Frame::new().fill(panel).inner_margin(egui::Margin::symmetric(16, 10)))
            .show(ui, |ui| self.top_bar(ui));
        egui::Panel::left("nav")
            .resizable(false)
            .exact_size(170.0)
            .frame(egui::Frame::new().fill(panel).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| self.nav(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(ui.visuals().extreme_bg_color).inner_margin(18))
            .show(ui, |ui| {
                self.messages(ui);
                match self.view {
                    View::Connect => crate::connect::show(self, ui),
                    View::Dashboard => crate::dashboard::show(self, ui),
                    View::Data => crate::table::show(self, ui),
                    View::Charts => crate::charts::show(self, ui),
                    View::Diagnostics => crate::diagnostics::show(self, ui),
                    View::Tune => crate::tune_view::show(self, ui),
                }
            });
        crate::burn_view::show(self, ui);
        // One dialog at a time: a report (or crash offer) before the guide.
        if self.report.is_open() {
            crate::report_view::show(self, ui);
        } else {
            crate::wizard::show(self, ui);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_recovery();
        self.stop();
        self.save_settings();
    }
}
