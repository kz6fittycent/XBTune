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
    Resets,
    Tune,
    Logs,
    Remote,
    Settings,
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
    /// Remote tuning: the bike owner whose bike this is (tuner's side).
    pub remote: Option<String>,
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
    pub fn push(&mut self, s: Sample) {
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
    pub settings_ui: crate::settings_view::SettingsUi,
    pub logs: crate::logs_view::LogsUi,
    pub autotune: crate::autotune_view::AutotuneUi,
    pub afv_fold: crate::afv_fold_view::FoldUi,
    pub report: crate::report_view::ReportUi,
    pub remote: crate::remote_view::RemoteUi,
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
            settings_ui: Default::default(),
            logs: Default::default(),
            autotune: Default::default(),
            afv_fold: Default::default(),
            report: Default::default(),
            remote: Default::default(),
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
            let race = std::env::var_os("XBTUNE_SHOT_RACE").is_some();
            app.connect.race = race;
            if let Ok(path) = std::env::var("XBTUNE_SHOT_SERIAL") {
                // UI checks against a pty simulator (`xbtune-sim --ecm ID`).
                app.start(SavedTarget::Serial { path, race });
            } else if (view != "connect"
                && view != "remote"
                && std::env::var_os("XBTUNE_SHOT_LOGS").is_none())
                || std::env::var_os("XBTUNE_SHOT_DEMO").is_some()
            {
                app.start(SavedTarget::Demo { race });
            }
            // `XBTUNE_SHOT_REMOTE=host|join:<code>` (with XBTUNE_REMOTE_URL
            // pointing at a local relay): start a remote session.
            match std::env::var("XBTUNE_SHOT_REMOTE").as_deref() {
                Ok("host") => crate::remote_view::host(&mut app),
                Ok("guide") => crate::remote_view::open_guide(&mut app),
                Ok(j) if j.starts_with("join:") => crate::remote_view::join(&mut app, &j[5..]),
                _ => {}
            }
            if let Ok(script) = std::env::var("XBTUNE_SHOT_SCRIPT") {
                crate::remote_view::set_script(&mut app, &script);
            }
            if let Ok(chat) = std::env::var("XBTUNE_SHOT_REMOTE_CHAT") {
                crate::remote_view::queue_chat(&mut app, chat);
            }
            app.view = match view.as_str() {
                "data" => View::Data,
                "charts" => View::Charts,
                "connect" => View::Connect,
                "diag" => View::Diagnostics,
                "resets" => View::Resets,
                "settings" => View::Settings,
                "remote" => View::Remote,
                "logs" => View::Logs,
                "tune" => View::Tune,
                _ => View::Dashboard,
            };
        }
        // Files from before the visible files folder existed move there once.
        if let Some(d) = xbtune_core::env::Dirs::detect() {
            let moved = xbtune_core::env::move_old_layout(&d);
            if moved.files > 0 {
                tracing::info!(files = moved.files, to = %d.files.display(), "moved earlier files");
                app.say(format!("Your tunes, backups and logs now live in {}", d.files.display()), false);
            }
            for (path, why) in &moved.skipped {
                tracing::warn!(path = %path.display(), why, "file not moved");
            }
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
            // XBTUNE_SHOT_PLUGS=1: made-up snap permission states (outside a
            // snap there are none to show).
            if std::env::var_os("XBTUNE_SHOT_PLUGS").is_some() {
                app.plugs = xbtune_core::env::MANUAL_PLUGS
                    .iter()
                    .zip([Some(true), Some(false), None])
                    .map(|(&(plug, purpose), connected)| PlugState { plug, purpose, connected })
                    .collect();
            }
            // XBTUNE_SHOT_LOGS=a.csv:b.csv opens logs; XBTUNE_SHOT_PLAY=<s>
            // plays the first one from <s> seconds in (paused there);
            // XBTUNE_SHOT_OVERLAY=hits|ego; XBTUNE_SHOT_AUTOTUNE=1 opens the
            // auto-tune dialog, analysed.
            if let Ok(list) = std::env::var("XBTUNE_SHOT_LOGS") {
                for p in list.split(':').filter(|p| !p.is_empty()) {
                    crate::logs_view::add(&mut app, std::path::Path::new(p));
                }
                if let Some(t) = std::env::var("XBTUNE_SHOT_PLAY").ok().and_then(|t| t.parse::<f64>().ok()) {
                    crate::logs_view::start_playback(&mut app, 0);
                    let t0 = app.logs.logs[0].log.samples.first().map_or(0.0, |s| s.t);
                    if let Some(pb) = &mut app.logs.play {
                        pb.playing = false;
                    }
                    crate::logs_view::seek(&mut app, t0 + t);
                }
                app.autotune.overlay = match std::env::var("XBTUNE_SHOT_OVERLAY").as_deref() {
                    Ok("hits") => crate::autotune_view::Overlay::Hits,
                    Ok("ego") => crate::autotune_view::Overlay::Ego,
                    _ => Default::default(),
                };
                if std::env::var_os("XBTUNE_SHOT_AUTOTUNE").is_some() {
                    crate::autotune_view::open(&mut app);
                    crate::autotune_view::analyse_now(&mut app);
                }
            }
            // XBTUNE_SHOT_FOLD=1: the "fold AFV into the maps" dialog.
            if std::env::var_os("XBTUNE_SHOT_FOLD").is_some() {
                app.afv_fold.open = true;
            }
            // XBTUNE_SHOT_FOLDER=<dir>: the files-folder picker there.
            if let Some(dir) = std::env::var_os("XBTUNE_SHOT_FOLDER") {
                app.settings_ui.pick_folder(dir.into());
            }
            // XBTUNE_SHOT_UNLOCK=commands|writes: the unlock confirmation.
            if let Ok(which) = std::env::var("XBTUNE_SHOT_UNLOCK") {
                app.settings_ui.ask(&which);
            }
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

    pub fn egui_ctx(&self) -> &egui::Context {
        &self.egui_ctx
    }

    /// Start a live session to `target`.
    pub fn start(&mut self, target: SavedTarget) {
        if crate::remote_sync::is_guest(self) {
            let owner = self.remote.peer().unwrap_or_else(|| "the bike owner".into());
            return self.say(
                format!("You're in a remote session with {owner}: end it (Remote) before connecting to a bike here."),
                true,
            );
        }
        crate::logs_view::stop_playback(self);
        self.stop();
        let (opener, label): (session::Opener, String) = match &target {
            SavedTarget::Demo { race } => {
                let mut sim = if *race {
                    xbtune_sim::Simulator::b2rib_running()
                } else {
                    xbtune_sim::Simulator::bueib_running()
                };
                // Like a BuellTooth: ~35 ms latency plus K-line wire time.
                sim.set_rates(xbtune_sim::FaultRates {
                    delay: 1.0,
                    delay_by: Duration::from_millis(35),
                    ..Default::default()
                });
                sim.set_wire_baud(Some(target_address(&target).baud()));
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
            SavedTarget::Serial { path, .. } => {
                // Factory race ECMs talk at 19200 (EcmDroid 2f17d29).
                let t = Target::Serial { path: path.clone(), baud: target_address(&target).baud() };
                (session::target_opener(t.clone()), t.to_string())
            }
        };
        // The ECM type goes into the label, so app.log and the log file's
        // `target:` line show which address was used.
        let label = if target.race() && !target.is_demo() {
            format!("{label}, factory race ECM (0x55)")
        } else {
            label
        };
        tracing::info!("connecting to {label}");
        let mut cfg = SessionConfig::new(label, target_address(&target));
        cfg.permit = xbtune_core::diag::permit(target.is_demo()).ok();
        self.permitted = cfg.permit.is_some();
        cfg.write_permit = xbtune_core::diag::write_permit(target.is_demo()).ok();
        self.write_permitted = cfg.write_permit.is_some();
        if !target.is_demo() {
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
        let path = crate::tune_view::tunes_dir().join(format!(
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
        if self.shot.is_some() || crate::remote_sync::save_blocked(self) {
            return;
        }
        let Some(t) = &mut self.tune else { return };
        if !t.is_modified() {
            return;
        }
        let path = crate::tune_view::tunes_dir().join(format!(
            "recovered_{}_{}.xbt",
            t.ecm.id,
            xbtune_core::time::file_stamp(SystemTime::now())
        ));
        let catalog = self.catalog.clone();
        if let Err(e) = t.save(&path, &catalog) {
            tracing::warn!(error = %e, "could not save recovery file");
        }
    }

    /// Use `dir` for the user's files (`None`: the default), moving what's
    /// in the current folder there if asked. Errors are for the dialog.
    pub fn set_files_folder(&mut self, dir: Option<PathBuf>, move_existing: bool) -> Result<(), String> {
        use xbtune_core::env;
        let d = env::Dirs::detect().ok_or("no home folder")?;
        let target = dir.clone().or_else(|| d.default_files()).ok_or("no home folder")?;
        if target == d.files {
            return Ok(());
        }
        if move_existing && (target.starts_with(&d.files) || d.files.starts_with(&target)) {
            return Err("Pick a folder that isn't inside the current one (or the other way round), \
                        or untick moving the files."
                .into());
        }
        // The default is stored as "no choice", so it follows the home folder.
        let store = if Some(&target) == d.default_files().as_ref() { None } else { Some(target.as_path()) };
        env::check_writable(&target)
            .map_err(|e| format!("XBTune can't write to {}: {e}", target.display()))?;
        env::set_files_dir(&d.config, store).map_err(|e| format!("Couldn't save the setting: {e}"))?;
        let mut note = String::new();
        if move_existing {
            let moved = env::move_files(&env::folder_pairs(&d.files, &target));
            for (path, why) in &moved.skipped {
                tracing::warn!(path = %path.display(), why, "file not moved");
            }
            note = format!(" (moved {} file(s)", moved.files);
            if !moved.skipped.is_empty() {
                note.push_str(&format!(", {} left in the old folder: see the log", moved.skipped.len()));
            }
            note.push(')');
        }
        tracing::info!(from = %d.files.display(), to = %target.display(), "files folder changed");
        self.say(format!("Your files now go to {}{note}", target.display()), false);
        self.tune_ui.dir = target.join(env::TUNES);
        self.send(Command::SetStore(target));
        Ok(())
    }

    /// The owner changed a safety lock in Settings: save it and apply it to
    /// the running connection at once.
    pub fn apply_unlocks(&mut self, u: xbtune_core::diag::Unlocks) {
        // UI checks never change the real file (they normally run with
        // XBTUNE_HOME anyway).
        if self.shot.is_none() {
            let before = xbtune_core::diag::unlocks();
            match xbtune_core::diag::set_unlocks(u) {
                Ok(path) => {
                    tracing::info!(?u, path = %path.display(), "safety locks changed");
                    for (was, now, what) in [
                        (before.ecm_commands, u.ecm_commands, "ECM commands"),
                        (before.eeprom_writes, u.eeprom_writes, "Writing to the ECM"),
                    ] {
                        if was != now {
                            self.say(format!("{what} {}.", if now { "unlocked" } else { "locked" }), false);
                        }
                    }
                }
                Err(e) => return self.say(format!("Couldn't save the safety locks: {e}"), true),
            }
        }
        let demo = self.link.target.as_ref().is_some_and(SavedTarget::is_demo);
        let permit = xbtune_core::diag::permit(demo).ok();
        let write_permit = xbtune_core::diag::write_permit(demo).ok();
        self.permitted = permit.is_some();
        self.write_permitted = write_permit.is_some();
        self.send(Command::SetPermits { permit, write_permit });
    }

    /// Burn buttons usable. On the tuner's side of a remote session they
    /// send requests instead (the bike owner's side decides).
    pub fn writes_permitted(&self) -> bool {
        self.write_permitted || crate::remote_sync::guest_live(self)
    }

    /// Test/reset buttons usable (requests on the tuner's side).
    pub fn session_permitted(&self) -> bool {
        self.permitted || crate::remote_sync::guest_live(self)
    }

    pub fn session_active(&self) -> bool {
        self.session.is_some()
    }

    /// Keep unsaved edits before something replaces the open tune.
    pub fn save_recovery_now(&mut self) {
        self.save_recovery();
    }

    pub fn is_demo(&self) -> bool {
        self.demo_sim.is_some() && self.link.target.as_ref().is_some_and(SavedTarget::is_demo)
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
                    // Reconnects are logged by the session, with their reason.
                    if !matches!(st, LinkState::Connecting { .. } | LinkState::Reconnecting { .. }) {
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
                    let race = self.link.target.as_ref().is_some_and(SavedTarget::race);
                    if race && !id.ecm.is_race() {
                        // Not expected (EcmDroid: only factory race ECMs use
                        // 0x55); worth knowing if a street ECM does.
                        self.say(
                            format!(
                                "Connected as \"Factory race\" (address 0x55), but {} is a street ECM and answered \
                                 anyway. \"Stock\" works for it too.",
                                id.ecm.id
                            ),
                            false,
                        );
                    }
                    self.link.channels = channels(&id.definition);
                    self.link.identity = Some(id);
                }
                Event::Sample(s) => {
                    crate::remote_sync::on_sample(self, &s);
                    self.history.push(s)
                }
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
                    let text = match &result {
                        Ok(o) if o.acknowledged => String::new(),
                        Ok(_) => format!("{}: ran (reply lost, the ECM showed it busy)", function.label()),
                        Err(e) => e.clone(),
                    };
                    crate::remote_sync::on_function_done(self, result.is_ok(), text);
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
            let (dot, text) = match (self.logs.playing_name(), &self.logs.play) {
                (Some(name), Some(pb)) => {
                    let t0 =
                        self.logs.logs.get(pb.log).and_then(|l| l.log.samples.first()).map_or(0.0, |s| s.t);
                    (p.accent, format!("Log playback · {name} · {:.0} s", pb.t - t0))
                }
                _ => status_text(&self.link, &p),
            };
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
            (View::Resets, "Resets"),
            (View::Tune, "Tune"),
            (View::Logs, "Logs"),
            (View::Remote, "Remote"),
            (View::Settings, "Settings"),
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
            ui.label(RichText::new(format!("v{}", crate::report::version())).small().weak());
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
                    // The text wraps (long paths) and leaves room for Dismiss.
                    ui.horizontal_top(|ui| {
                        let w = (ui.available_width() - 90.0).max(100.0);
                        ui.allocate_ui(egui::vec2(w, 0.0), |ui| {
                            ui.set_width(w);
                            ui.add(egui::Label::new(&m.text).wrap());
                        });
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
    if let Some(owner) = &link.remote {
        let id = link
            .identity
            .as_ref()
            .map(|i| format!(" · {} · {}", i.version.trim(), i.ecm.family))
            .unwrap_or_default();
        return match &link.state {
            Some(LinkState::Live) => (p.good, format!("Remote · {owner}'s bike{id}")),
            Some(_) => (p.warn, format!("Remote · {owner}'s bike: link down{id}")),
            None => (p.muted, format!("Remote · {owner} isn't connected to the bike")),
        };
    }
    let target = link.target.as_ref().map(|t| t.label()).unwrap_or_default();
    match &link.state {
        None | Some(LinkState::Stopped { .. }) => (p.muted, "Not connected".into()),
        Some(LinkState::Connecting { attempt }) if *attempt > 1 => {
            (p.warn, format!("Connecting to {target} (attempt {attempt})…"))
        }
        Some(LinkState::Connecting { .. }) => (p.warn, format!("Connecting to {target}…")),
        // Never answered yet: retrying a few times, then the reason shows.
        Some(LinkState::Reconnecting { .. }) if link.identity.is_none() => {
            (p.warn, format!("No answer from {target}, trying again…"))
        }
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
        crate::remote_view::poll(self);
        crate::logs_view::tick(self);
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
                    View::Resets => crate::resets::show(self, ui),
                    View::Settings => crate::settings_view::show(self, ui),
                    View::Remote => crate::remote_view::show(self, ui),
                    View::Logs => crate::logs_view::show(self, ui),
                    View::Tune => crate::tune_view::show(self, ui),
                }
            });
        crate::burn_view::show(self, ui);
        crate::autotune_view::show(self, ui);
        crate::afv_fold_view::show(self, ui);
        crate::remote_view::dialogs(self, ui);
        // One dialog at a time: a report (or crash offer) before the guide.
        if self.report.is_open() {
            crate::report_view::show(self, ui);
        } else {
            crate::wizard::show(self, ui);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        crate::remote_view::end(self);
        self.save_recovery();
        self.stop();
        self.save_settings();
    }
}

/// The ECM address for a saved target: stock (0x42, 9600 baud) or factory
/// race (0x55, 19200 baud).
fn target_address(t: &SavedTarget) -> EcmAddress {
    if t.race() { EcmAddress::Race } else { EcmAddress::Stock }
}
