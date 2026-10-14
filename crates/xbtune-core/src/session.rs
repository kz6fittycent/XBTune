// SPDX-License-Identifier: GPL-3.0-or-later
//! A live-data session: one tokio task that owns the ECM link.
//!
//! It connects, reads the version, maps it to a definition, then requests
//! runtime frames back to back (or no faster than `min_interval`). After a
//! dropout it reconnects and carries on. Logging to CSV happens inside the
//! task, so the log rate never depends on the UI. The UI (or the CLI) sends
//! [`Command`]s and receives [`Event`]s; it never blocks on I/O.
//!
//! Polling is read-only (version and runtime-data requests). The only
//! requests that change the ECM are [`Command::Function`] (active tests,
//! clearing codes, TPS reset), which run through `diag::run_function` and
//! need the [`crate::diag::Permit`] given in the config.

use std::fs::File;
use std::future::Future;
use std::io::BufWriter;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info};
use xbtune_defs::{Catalog, Definition, EcmDef};
use xbtune_link::{BoxIo, EcmClient, OpenError, Target};
use xbtune_proto::{EcmAddress, LinkStats, RetryPolicy, TransactionError};

use crate::live::{CsvLogger, LogMeta, RateMeter, Sample, channels};

/// Opens the transport; called again for every reconnect.
pub type Opener = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<BoxIo, String>> + Send>> + Send + Sync>;

/// Called after every event is queued (e.g. to wake the UI).
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// An [`Opener`] for a real transport. Errors carry advice (snap
/// connection, dialout group, pairing).
pub fn target_opener(target: Target) -> Opener {
    Arc::new(move || {
        let target = target.clone();
        Box::pin(async move { xbtune_link::open(&target).await.map_err(|e| open_error_text(&target, &e)) })
    })
}

/// A failure to open `target`, with what to do about it.
pub fn open_error_text(target: &Target, e: &OpenError) -> String {
    let plug = match target {
        Target::Serial { .. } => "raw-usb",
        Target::Rfcomm { .. } => "bluez",
    };
    if e.is_permission_denied() {
        format!("{e}\n\n{}", crate::env::permission_hint(plug))
    } else if let Target::Rfcomm { address, .. } = target {
        format!(
            "{e}\n\nIs the adapter powered (ignition on) and paired? To pair:\n    \
             bluetoothctl pair {address}   (BuellTooth PIN is usually 1234)\n    \
             bluetoothctl trust {address}"
        )
    } else {
        e.to_string()
    }
}

/// Reply timeout for live polling. A runtime reply takes ~146 ms on a
/// BuellTooth (111 ms of it is 107 bytes at 9600 baud), and the owner's logs
/// show lost replies never arrive late, so waiting the read default of 1 s
/// only stretches every dropout. EEPROM reads keep the 1 s default.
pub const LIVE_TIMEOUT: Duration = Duration::from_millis(400);

#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Human-readable target, for logs and the UI.
    pub target: String,
    pub address: EcmAddress,
    pub policy: RetryPolicy,
    /// Shortest time between runtime requests (zero: as fast as the link goes).
    pub min_interval: Duration,
    /// Longest pause before a reconnect. The first pause is an eighth of
    /// it, doubling with each failed reconnect in a row.
    pub reconnect_delay: Duration,
    /// Give up after this many reconnects in a row without a good frame
    /// (`None`: keep trying until stopped).
    pub max_reconnects: Option<u32>,
    /// Failed requests in a row (each after its retries) before reconnecting.
    pub fails_before_reconnect: u32,
    /// Allows [`Command::Function`]; `None` refuses them.
    pub permit: Option<crate::diag::Permit>,
    pub function_timing: crate::diag::FunctionTiming,
}

impl SessionConfig {
    pub fn new(target: impl Into<String>, address: EcmAddress) -> Self {
        Self {
            target: target.into(),
            address,
            policy: RetryPolicy { timeout: LIVE_TIMEOUT, ..RetryPolicy::default() },
            min_interval: Duration::ZERO,
            reconnect_delay: Duration::from_secs(2),
            max_reconnects: None,
            fails_before_reconnect: 2,
            permit: None,
            function_timing: crate::diag::FunctionTiming::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    StartLog {
        path: PathBuf,
        note: Option<String>,
    },
    StopLog,
    /// Run an active test, clear codes or reset the TPS (between polls).
    Function(xbtune_proto::Function),
    Stop,
}

/// The ECM the session is talking to.
#[derive(Debug, Clone)]
pub struct Identity {
    pub version: String,
    pub ecm: EcmDef,
    pub definition: Arc<Definition>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LinkState {
    Connecting {
        attempt: u32,
    },
    Live,
    /// Waiting before the next connect attempt.
    Reconnecting {
        reason: String,
        delay: Duration,
    },
    Stopped {
        reason: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub enum Event {
    State(LinkState),
    Identified(Identity),
    Sample(Sample),
    /// About twice a second while live.
    Stats {
        stats: LinkStats,
        hz: f64,
        reconnects: u32,
        log_rows: Option<u64>,
    },
    LogStarted(PathBuf),
    LogStopped {
        path: PathBuf,
        rows: u64,
        error: Option<String>,
    },
    /// A function is under way.
    FunctionProgress {
        function: xbtune_proto::Function,
        stage: crate::diag::Stage,
    },
    /// A function finished (or was refused).
    FunctionDone {
        function: xbtune_proto::Function,
        result: Result<crate::diag::Outcome, String>,
    },
    /// Something worth showing that doesn't stop the session.
    Warning(String),
}

/// Handle to a running session.
pub struct Session {
    pub commands: mpsc::UnboundedSender<Command>,
    pub events: mpsc::UnboundedReceiver<Event>,
    pub task: JoinHandle<()>,
}

impl Session {
    pub fn send(&self, c: Command) {
        let _ = self.commands.send(c);
    }
}

/// Start a session on the current tokio runtime.
pub fn start(cfg: SessionConfig, opener: Opener, catalog: Arc<Catalog>, notify: Option<Notify>) -> Session {
    let (ctx, crx) = mpsc::unbounded_channel();
    let (etx, erx) = mpsc::unbounded_channel();
    let out = Out { tx: etx, notify };
    let task = tokio::spawn(async move {
        let mut w = Worker {
            cfg,
            opener,
            catalog,
            out,
            commands: crx,
            identity: None,
            log: None,
            started: Instant::now(),
            reconnects: 0,
        };
        let reason = w.run().await;
        w.stop_log(None);
        info!(?reason, "session stopped");
        w.out.send(Event::State(LinkState::Stopped { reason }));
    });
    Session { commands: ctx, events: erx, task }
}

struct Out {
    tx: mpsc::UnboundedSender<Event>,
    notify: Option<Notify>,
}

impl Out {
    fn send(&self, e: Event) {
        if self.tx.send(e).is_ok()
            && let Some(n) = &self.notify
        {
            n();
        }
    }
}

struct Log {
    path: PathBuf,
    writer: CsvLogger<BufWriter<File>>,
    last_flush: Instant,
}

struct Worker {
    cfg: SessionConfig,
    opener: Opener,
    catalog: Arc<Catalog>,
    out: Out,
    commands: mpsc::UnboundedReceiver<Command>,
    identity: Option<Identity>,
    log: Option<Log>,
    started: Instant,
    reconnects: u32,
}

/// Why a connection ended.
enum End {
    /// Stop requested, or a fatal problem: the session ends.
    Stop(Option<String>),
    /// Link trouble: reconnect.
    Retry(String),
}

impl Worker {
    /// Returns why the session ended (`None`: stopped on request).
    async fn run(&mut self) -> Option<String> {
        let mut attempt = 0;
        let mut failed_in_a_row = 0;
        loop {
            attempt += 1;
            self.out.send(Event::State(LinkState::Connecting { attempt }));
            let opened = tokio::select! {
                r = (self.opener)() => r,
                stop = self.wait_for_stop() => return stop,
            };
            let end = match opened {
                Ok(io) => {
                    let client = EcmClient::new(io, self.cfg.address).with_policy(self.cfg.policy);
                    self.connected(client, &mut failed_in_a_row).await
                }
                Err(e) => End::Retry(e),
            };
            let reason = match end {
                End::Stop(r) => return r,
                End::Retry(r) => r,
            };
            failed_in_a_row += 1;
            if self.cfg.max_reconnects.is_some_and(|m| failed_in_a_row > m) {
                return Some(format!("gave up after {} failed reconnects: {reason}", failed_in_a_row - 1));
            }
            info!(%reason, "link lost; reconnecting");
            self.reconnects += 1;
            let max = self.cfg.reconnect_delay;
            let delay = (max / 8).saturating_mul(1 << (failed_in_a_row - 1).min(3)).min(max);
            self.log_event(&format!("reconnecting in {} ms: {reason}", delay.as_millis()));
            self.out.send(Event::State(LinkState::Reconnecting { reason, delay }));
            if let Ok(stop) = tokio::time::timeout(delay, self.wait_for_stop()).await {
                return stop;
            }
        }
    }

    /// Resolves when a Stop arrives (handling other commands meanwhile).
    async fn wait_for_stop(&mut self) -> Option<String> {
        loop {
            match self.commands.recv().await {
                None | Some(Command::Stop) => return None,
                Some(c) => self.handle(c),
            }
        }
    }

    fn handle(&mut self, c: Command) {
        match c {
            Command::StartLog { path, note } => self.start_log(path, note),
            Command::StopLog => self.stop_log(None),
            Command::Function(function) => self.out.send(Event::FunctionDone {
                function,
                result: Err("not connected to the ECM right now".into()),
            }),
            Command::Stop => {}
        }
    }

    async fn connected(&mut self, mut client: EcmClient, failed_in_a_row: &mut u32) -> End {
        let version = match client.version().await {
            Ok(v) => v,
            Err(e) => return End::Retry(format!("no answer to the version request: {e}")),
        };
        let Some(ecm) = self.catalog.lookup(&version).cloned() else {
            return End::Stop(Some(format!(
                "unknown ECM version '{version}': no definition, so no live data"
            )));
        };
        if let Some(prev) = &self.identity
            && prev.version != version
        {
            return End::Stop(Some(format!(
                "a different ECM answered after reconnecting ({version}, was {})",
                prev.version
            )));
        }
        let Some(definition) = self.catalog.definition(&ecm).cloned() else {
            return End::Stop(Some(format!("{}: no definition data", ecm.id)));
        };
        if self.identity.is_none() {
            let id = Identity { version, ecm, definition };
            self.identity = Some(id.clone());
            self.out.send(Event::Identified(id));
        }
        let expected = self.identity.as_ref().map(|i| i.ecm.runtime_len).unwrap_or(0);
        self.out.send(Event::State(LinkState::Live));

        let mut rate = RateMeter::default();
        let mut last_stats = Instant::now();
        let mut fails = 0;
        loop {
            while let Ok(c) = self.commands.try_recv() {
                match c {
                    Command::Stop => return End::Stop(None),
                    Command::Function(f) => self.function(&mut client, f).await,
                    c => self.handle(c),
                }
            }
            let asked = Instant::now();
            let before = client.stats().clone();
            let result = client.runtime_data().await;
            if let Some(what) = failed_attempts(&before, client.stats()) {
                self.log_event(&what);
            }
            match result {
                Ok(pdu) => {
                    fails = 0;
                    *failed_in_a_row = 0;
                    let frame = pdu.into_bytes();
                    if frame.len() != expected {
                        return End::Stop(Some(format!(
                            "runtime frame is {} bytes but the definition expects {expected}; not decoding it",
                            frame.len()
                        )));
                    }
                    let now = Instant::now();
                    rate.tick(now);
                    let sample = Sample {
                        t: now.duration_since(self.started).as_secs_f64(),
                        wall: SystemTime::now(),
                        frame,
                    };
                    self.write_log(&sample);
                    self.out.send(Event::Sample(sample));
                }
                Err(e) => {
                    fails += 1;
                    debug!(error = %e, fails, "runtime request failed");
                    self.log_event(&format!("request gave up: {e}"));
                    if matches!(e, TransactionError::Io(_)) || fails >= self.cfg.fails_before_reconnect {
                        return End::Retry(e.to_string());
                    }
                }
            }
            if last_stats.elapsed() >= Duration::from_millis(500) {
                last_stats = Instant::now();
                self.out.send(Event::Stats {
                    stats: client.stats().clone(),
                    hz: rate.hz(),
                    reconnects: self.reconnects,
                    log_rows: self.log.as_ref().map(|l| l.writer.rows()),
                });
            }
            let spent = asked.elapsed();
            if spent < self.cfg.min_interval {
                tokio::time::sleep(self.cfg.min_interval - spent).await;
            }
        }
    }

    /// Run one function on the connected ECM and report it.
    async fn function(&mut self, client: &mut EcmClient, f: xbtune_proto::Function) {
        self.log_event(&format!("{}: starting", f.label()));
        let result = match (&self.cfg.permit, &self.identity) {
            (None, _) => Err(crate::diag::locked_message()),
            (_, None) => Err("the ECM hasn't been identified yet".into()),
            (Some(permit), Some(id)) => {
                let (ecm, def) = (id.ecm.clone(), id.definition.clone());
                let out = &self.out;
                crate::diag::run_function(client, &ecm, &def, f, permit, self.cfg.function_timing, |stage| {
                    out.send(Event::FunctionProgress { function: f, stage })
                })
                .await
                .map_err(|e| e.to_string())
            }
        };
        let what = match &result {
            Ok(o) => format!("{}: done after {:.1} s", f.label(), o.busy_for.as_secs_f64()),
            Err(e) => format!("{}: {e}", f.label()),
        };
        tracing::info!("{what}");
        self.log_event(&what);
        self.out.send(Event::FunctionDone { function: f, result });
    }

    fn start_log(&mut self, path: PathBuf, note: Option<String>) {
        self.stop_log(None);
        let Some(id) = &self.identity else {
            self.out.send(Event::LogStopped {
                path,
                rows: 0,
                error: Some("not connected to an ECM yet".into()),
            });
            return;
        };
        let meta = LogMeta {
            ecm_id: id.ecm.id.clone(),
            version: id.version.clone(),
            target: self.cfg.target.clone(),
            started: SystemTime::now(),
            note,
        };
        let opened = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| File::create(&path))
            .and_then(|f| CsvLogger::new(BufWriter::new(f), &meta, channels(&id.definition)));
        match opened {
            Ok(writer) => {
                info!(path = %path.display(), "logging");
                self.out.send(Event::LogStarted(path.clone()));
                self.log = Some(Log { path, writer, last_flush: Instant::now() });
            }
            Err(e) => self.out.send(Event::LogStopped { path, rows: 0, error: Some(e.to_string()) }),
        }
    }

    fn write_log(&mut self, s: &Sample) {
        let Some(log) = &mut self.log else { return };
        let mut r = log.writer.write(s);
        if r.is_ok() && log.last_flush.elapsed() >= Duration::from_secs(1) {
            log.last_flush = Instant::now();
            r = log.writer.flush();
        }
        if let Err(e) = r {
            self.stop_log(Some(e.to_string()));
        }
    }

    /// Note a link event in the log as a `# event:` line.
    fn log_event(&mut self, what: &str) {
        let t = self.started.elapsed().as_secs_f64();
        let Some(log) = &mut self.log else { return };
        if let Err(e) = log.writer.event(t, what) {
            self.stop_log(Some(e.to_string()));
        }
    }

    fn stop_log(&mut self, error: Option<String>) {
        let Some(mut log) = self.log.take() else { return };
        let flushed = log.writer.flush().err().map(|e| e.to_string());
        let rows = log.writer.rows();
        self.out.send(Event::LogStopped { path: log.path, rows, error: error.or(flushed) });
    }
}

/// Failed attempts between two stats snapshots, e.g. "2 failed attempts
/// (timeout 2)", or `None` if every attempt succeeded.
fn failed_attempts(before: &LinkStats, after: &LinkStats) -> Option<String> {
    let kinds = [
        ("timeout", after.timeouts - before.timeouts),
        ("corrupt", after.corrupt - before.corrupt),
        ("nak", after.naks - before.naks),
        ("io", after.io_errors - before.io_errors),
    ];
    let total: u64 = kinds.iter().map(|k| k.1).sum();
    if total == 0 {
        return None;
    }
    let detail: Vec<String> = kinds.iter().filter(|k| k.1 > 0).map(|(n, c)| format!("{n} {c}")).collect();
    Some(format!("{total} failed attempt(s) ({})", detail.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_sim::{FaultRates, Simulator, shared};

    fn sim_opener(sim: xbtune_sim::SharedSim) -> Opener {
        Arc::new(move || {
            let sim = sim.clone();
            Box::pin(async move { Ok(xbtune_sim::in_process(sim)) })
        })
    }

    fn fast(cfg: &mut SessionConfig) {
        cfg.policy = xbtune_link::client::fast_policy();
        cfg.reconnect_delay = Duration::from_millis(10);
    }

    async fn collect_until(s: &mut Session, mut pred: impl FnMut(&Event) -> bool) -> Vec<Event> {
        let mut seen = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while let Ok(Some(e)) = tokio::time::timeout_at(deadline, s.events.recv()).await {
            let done = pred(&e);
            seen.push(e);
            if done {
                break;
            }
        }
        seen
    }

    #[tokio::test]
    async fn polls_logs_and_stops() {
        let sim = shared(Simulator::bueib_running());
        let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut cfg);
        let mut s = start(cfg, sim_opener(sim), Arc::new(Catalog::builtin().unwrap()), None);
        let ev = collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        let Some(Event::Identified(id)) = ev.last() else { panic!("{ev:?}") };
        assert_eq!(id.ecm.id, "BUEIB");

        let dir = std::env::temp_dir().join(format!("xbtune-session-{}", std::process::id()));
        let path = dir.join("log.csv");
        s.send(Command::StartLog { path: path.clone(), note: None });
        let (mut logging, mut samples) = (false, 0);
        collect_until(&mut s, |e| {
            match e {
                Event::LogStarted(_) => logging = true,
                Event::Sample(_) if logging => samples += 1,
                _ => {}
            }
            samples >= 20
        })
        .await;
        s.send(Command::StopLog);
        let ev = collect_until(&mut s, |e| matches!(e, Event::LogStopped { .. })).await;
        let Some(Event::LogStopped { rows, error, .. }) = ev.last() else { panic!() };
        assert!(*rows >= 15, "{rows}");
        assert_eq!(error, &None);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().filter(|l| !l.starts_with('#')).count() as u64, rows + 1);
        let _ = std::fs::remove_dir_all(&dir);

        s.send(Command::Stop);
        let ev = collect_until(&mut s, |e| matches!(e, Event::State(LinkState::Stopped { .. }))).await;
        assert!(matches!(ev.last(), Some(Event::State(LinkState::Stopped { reason: None }))), "{ev:?}");
        s.task.await.unwrap();
    }

    #[tokio::test]
    async fn reconnects_through_dropouts() {
        let mut sim = Simulator::bueib_running();
        sim.set_rates(FaultRates { drop: 0.5, ..Default::default() });
        let sim = shared(sim);
        let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut cfg);
        cfg.policy.max_attempts = 1;
        cfg.policy.timeout = Duration::from_millis(30);
        cfg.fails_before_reconnect = 1;
        let mut s = start(cfg, sim_opener(sim.clone()), Arc::new(Catalog::builtin().unwrap()), None);
        let (mut samples, mut reconnecting) = (0, 0);
        collect_until(&mut s, |e| {
            match e {
                Event::Sample(_) => samples += 1,
                Event::State(LinkState::Reconnecting { .. }) => reconnecting += 1,
                _ => {}
            }
            samples >= 30 && reconnecting >= 2
        })
        .await;
        assert!(samples >= 30 && reconnecting >= 2, "{samples} samples, {reconnecting} reconnects");
        s.send(Command::Stop);
        s.task.await.unwrap();
    }

    #[tokio::test]
    async fn unknown_ecm_stops_with_reason() {
        let mut cfg = xbtune_sim::SimConfig::bueib();
        cfg.version = "ZZZZZ999".into();
        let sim = shared(Simulator::new(cfg, xbtune_sim::BUEIB_EEPROM));
        let mut sc = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut sc);
        let mut s = start(sc, sim_opener(sim), Arc::new(Catalog::builtin().unwrap()), None);
        let ev = collect_until(&mut s, |e| matches!(e, Event::State(LinkState::Stopped { .. }))).await;
        let Some(Event::State(LinkState::Stopped { reason: Some(r) })) = ev.last() else { panic!("{ev:?}") };
        assert!(r.contains("unknown ECM"), "{r}");
    }

    #[tokio::test]
    async fn gives_up_when_nothing_answers() {
        let opener: Opener = Arc::new(|| Box::pin(async { Err("Host is down".to_string()) }));
        let mut cfg = SessionConfig::new("nowhere", EcmAddress::Stock);
        fast(&mut cfg);
        cfg.max_reconnects = Some(2);
        let mut s = start(cfg, opener, Arc::new(Catalog::builtin().unwrap()), None);
        let ev = collect_until(&mut s, |e| matches!(e, Event::State(LinkState::Stopped { .. }))).await;
        let Some(Event::State(LinkState::Stopped { reason: Some(r) })) = ev.last() else { panic!("{ev:?}") };
        assert!(r.contains("gave up after 2") && r.contains("Host is down"), "{r}");
    }

    fn stopped_sim() -> xbtune_sim::SharedSim {
        let mut sim = Simulator::bueib_running();
        sim.set_engine_running(false);
        sim.set_test_duration(Duration::from_millis(100));
        shared(sim)
    }

    #[tokio::test]
    async fn function_runs_then_polling_resumes() {
        let sim = stopped_sim();
        let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut cfg);
        cfg.permit = Some(crate::diag::test_permit());
        cfg.function_timing.poll = Duration::from_millis(20);
        let mut s = start(cfg, sim_opener(sim.clone()), Arc::new(Catalog::builtin().unwrap()), None);
        collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        s.send(Command::Function(xbtune_proto::Function::Fan));
        let ev = collect_until(&mut s, |e| matches!(e, Event::FunctionDone { .. })).await;
        let Some(Event::FunctionDone { result, .. }) = ev.last() else { panic!("{ev:?}") };
        assert!(result.as_ref().is_ok_and(|o| o.acknowledged), "{result:?}");
        assert!(
            ev.iter().any(|e| matches!(
                e,
                Event::FunctionProgress { stage: crate::diag::Stage::Running { .. }, .. }
            ))
        );
        assert_eq!(sim.lock().unwrap().journal.functions, vec![xbtune_proto::Function::Fan.code()]);
        // Live data carries on afterwards.
        let ev = collect_until(&mut s, |e| matches!(e, Event::Sample(_))).await;
        assert!(matches!(ev.last(), Some(Event::Sample(_))));
        s.send(Command::Stop);
        s.task.await.unwrap();
    }

    #[tokio::test]
    async fn function_without_permit_is_refused() {
        let sim = stopped_sim();
        let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut cfg);
        let mut s = start(cfg, sim_opener(sim.clone()), Arc::new(Catalog::builtin().unwrap()), None);
        collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        s.send(Command::Function(xbtune_proto::Function::FuelPump));
        let ev = collect_until(&mut s, |e| matches!(e, Event::FunctionDone { .. })).await;
        let Some(Event::FunctionDone { result: Err(e), .. }) = ev.last() else { panic!("{ev:?}") };
        assert!(e.contains("locked"), "{e}");
        assert!(sim.lock().unwrap().journal.functions.is_empty());
        s.send(Command::Stop);
    }
}
