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
    } else if e.is_busy() && matches!(target, Target::Rfcomm { .. }) {
        format!(
            "{e}\n\nBluetooth is still closing the last connection (usually because the adapter lost \
             power: ignition off). XBTune keeps trying; it clears within about 20 seconds once the \
             ignition is on."
        )
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

/// EcmDroid's "wrong Protocol?" hint (`ECM.java`, 3c189be): a factory race
/// ECM answers only on address 0x55, every other ECM only on 0x42, so the
/// wrong choice looks like a silent ECM.
pub fn wrong_type_hint(address: EcmAddress) -> &'static str {
    match address {
        EcmAddress::Stock => {
            "If this is a factory race ECM (RACE USE ONLY engraved on the case), choose \"Factory race\" (CLI: --race)."
        }
        EcmAddress::Race => {
            "Factory race was chosen: if this is a stock or P&A race ECM (RACE USE ONLY printed, not engraved), \
             choose \"Stock\" (CLI: no --race). Over Bluetooth, a factory race ECM needs the adapter set to 19200 baud."
        }
    }
}

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
    /// Reply timeout during an EEPROM read (the read default, 1 s).
    pub read_timeout: Duration,
    /// Allows burns; `None` refuses them.
    pub write_permit: Option<crate::diag::WritePermit>,
    /// The files folder: burn backups go in `Backups/`, records in `History/`.
    pub store: Option<PathBuf>,
    pub link_limits: crate::burn::LinkLimits,
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
            read_timeout: RetryPolicy::default().timeout,
            write_permit: None,
            store: crate::env::Dirs::detect().map(|d| d.files),
            link_limits: crate::burn::LinkLimits::default(),
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
    /// Read the whole EEPROM (verified, resumed after reconnects), then go
    /// back to polling. Read-only.
    ReadEeprom,
    CancelRead,
    /// Get ready to write: checks, then a verified full read saved as a
    /// backup. Answered by `BurnPrepared` or `BurnDone(Err)`.
    PrepareBurn,
    /// Write `target` (built from the prepared backup) and verify it.
    Burn {
        target: Box<crate::image::Image>,
        note: String,
    },
    /// The files folder changed (app Settings): backups and burn history
    /// go to the new one from now on.
    SetStore(PathBuf),
    /// The owner changed the unlocks (app Settings): replace both permits.
    SetPermits {
        permit: Option<crate::diag::Permit>,
        write_permit: Option<crate::diag::WritePermit>,
    },
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
    /// EEPROM read progress.
    ReadProgress(crate::read::Progress),
    /// The EEPROM was read and verified.
    EepromRead(Box<crate::image::Image>),
    ReadFailed(String),
    BurnPrepared {
        backup: Box<crate::image::Image>,
        path: PathBuf,
        link: crate::burn::LinkCheck,
    },
    BurnProgress(crate::burn::BurnStep),
    /// A burn (or its preparation) ended.
    BurnDone(Result<BurnSummary, BurnFailure>),
    /// Something worth showing that doesn't stop the session.
    Warning(String),
}

/// A successful burn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnSummary {
    pub report: crate::burn::BurnReport,
    /// Before/after images and the note, if they could be saved.
    pub history: Option<PathBuf>,
}

/// Why a burn (or its preparation) stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurnFailure {
    pub message: String,
    /// Pages known to differ from the target; `None` if nothing was
    /// written or the state is unknown.
    pub mismatched: Option<Vec<crate::burn::PageMismatch>>,
    /// True once a write was attempted (the ECM may be partly written).
    pub wrote: bool,
}

impl BurnFailure {
    fn before(message: impl Into<String>) -> Self {
        Self { message: message.into(), mismatched: None, wrote: false }
    }
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
            read: None,
            prepared: None,
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
    /// An EEPROM read in progress; survives reconnects.
    read: Option<crate::read::EepromRead>,
    /// ECM contents from the last PrepareBurn (cleared by each burn).
    prepared: Option<crate::image::Image>,
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
            Command::ReadEeprom => self.start_read(),
            Command::PrepareBurn | Command::Burn { .. } => {
                self.out.send(Event::BurnDone(Err(BurnFailure::before("not connected to the ECM right now"))))
            }
            Command::CancelRead => {
                if self.read.take().is_some() {
                    self.out.send(Event::ReadFailed("cancelled".into()));
                }
            }
            Command::SetStore(dir) => {
                info!(dir = %dir.display(), "files folder changed");
                self.cfg.store = Some(dir);
            }
            Command::SetPermits { permit, write_permit } => {
                info!(commands = permit.is_some(), writes = write_permit.is_some(), "permits changed");
                self.cfg.permit = permit;
                self.cfg.write_permit = write_permit;
            }
            Command::Stop => {}
        }
    }

    async fn connected(&mut self, mut client: EcmClient, failed_in_a_row: &mut u32) -> End {
        let version = match client.version().await {
            Ok(v) => v,
            Err(e) => {
                return End::Retry(format!(
                    "no answer to the version request: {e}. {}",
                    wrong_type_hint(self.cfg.address)
                ));
            }
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
                    Command::PrepareBurn => {
                        let r = self.prepare_burn(&mut client).await;
                        match r {
                            Ok(ev) => self.out.send(ev),
                            Err(e) => self.out.send(Event::BurnDone(Err(BurnFailure::before(e)))),
                        }
                    }
                    Command::Burn { target, note } => {
                        let r = self.burn(&mut client, *target, note).await;
                        self.out.send(Event::BurnDone(r));
                    }
                    c => self.handle(c),
                }
            }
            if let Some(end) = self.continue_read(&mut client).await {
                return end;
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

    async fn prepare_burn(&mut self, client: &mut EcmClient) -> Result<Event, String> {
        self.prepared = None;
        if self.cfg.write_permit.is_none() {
            return Err(crate::diag::write_locked_message());
        }
        let id = self.identity.clone().ok_or("the ECM hasn't been identified yet")?;
        crate::burn::engine_stopped(client, &id.definition).await?;
        let link = crate::burn::check_link(client, &self.cfg.link_limits).await;
        link.verdict(&self.cfg.link_limits)?;
        self.log_event("burn: reading the EEPROM for the backup");
        let layout = id.ecm.layout().map_err(|e| e.to_string())?;
        let mut read = crate::read::EepromRead::new(&layout, true);
        let live = client.policy();
        client.set_policy(RetryPolicy { timeout: self.cfg.read_timeout, ..live });
        let out = &self.out;
        let r = read.run(client, |p| out.send(Event::ReadProgress(p))).await;
        client.set_policy(live);
        r.map_err(|e| format!("backup read failed, nothing was written: {e}"))?;
        let mut img = crate::image::Image::from_raw(&id.ecm, &read.bytes().expect("complete"))
            .map_err(|e| e.to_string())?;
        img.page_zero = read.has_page_zero();
        img.ecm_version = Some(id.version.clone());
        let now = SystemTime::now();
        img.meta.created = crate::time::rfc3339(now);
        img.meta.source = format!(
            "read from ECM over {} before a burn, every chunk read twice and matched",
            self.cfg.target
        );
        let dir = self
            .cfg
            .store
            .clone()
            .ok_or("no folder to keep the backup in, so nothing will be written")?
            .join(crate::env::BACKUPS);
        let path = crate::time::unique_path(
            &dir,
            &format!("{}_{}_before-burn", id.ecm.id, crate::time::file_stamp(now)),
            ".xbt",
        );
        let catalog = self.catalog.clone();
        img.save(&path, &catalog)
            .map_err(|e| format!("couldn't save the backup ({e}), so nothing will be written"))?;
        self.log_event(&format!("burn: backup saved to {}", path.display()));
        self.prepared = Some(img.clone());
        Ok(Event::BurnPrepared { backup: Box::new(img), path, link })
    }

    async fn burn(
        &mut self,
        client: &mut EcmClient,
        target: crate::image::Image,
        note: String,
    ) -> Result<BurnSummary, BurnFailure> {
        let Some(before) = self.prepared.take() else {
            return Err(BurnFailure::before("no fresh backup; prepare the burn first"));
        };
        if self.cfg.write_permit.is_none() {
            return Err(BurnFailure::before(crate::diag::write_locked_message()));
        }
        let id =
            self.identity.clone().ok_or_else(|| BurnFailure::before("the ECM hasn't been identified yet"))?;
        // BurnTask: the connected ECM's version must start with the image ID.
        let version = client
            .version()
            .await
            .map_err(|e| BurnFailure::before(format!("can't read the ECM version: {e}")))?;
        crate::burn::check_identity(&version, &target.ecm_id, &id.ecm).map_err(BurnFailure::before)?;
        if version != id.version || before.ecm_id != target.ecm_id {
            return Err(BurnFailure::before(format!(
                "the ECM ({version}) isn't the one the backup came from"
            )));
        }
        crate::burn::engine_stopped(client, &id.definition).await.map_err(BurnFailure::before)?;
        let limits = crate::burn::LinkLimits { pings: 10, ..self.cfg.link_limits };
        crate::burn::check_link(client, &limits).await.verdict(&limits).map_err(BurnFailure::before)?;
        let layout = id.ecm.layout().map_err(|e| BurnFailure::before(e.to_string()))?;
        let plan = crate::burn::plan(&layout, &id.definition, &before.bytes, &target.bytes)
            .map_err(BurnFailure::before)?;
        if plan.is_empty() {
            return Ok(BurnSummary {
                report: crate::burn::BurnReport { writes: 0, bytes: 0, pages: Vec::new() },
                history: None,
            });
        }
        let live = client.policy();
        client.set_policy(RetryPolicy { timeout: self.cfg.read_timeout, ..live });
        let changed = crate::burn::still_matches(client, &layout, &plan, &before.bytes).await;
        match changed {
            Ok(m) if m.is_empty() => {}
            Ok(m) => {
                client.set_policy(live);
                let pages: Vec<String> = m.iter().map(|p| p.page.to_string()).collect();
                return Err(BurnFailure::before(format!(
                    "the ECM changed page(s) {} since the backup was read; nothing was written. Prepare again.",
                    pages.join(", ")
                )));
            }
            Err(e) => {
                client.set_policy(live);
                return Err(BurnFailure::before(format!("couldn't re-check the ECM before writing: {e}")));
            }
        }
        self.log_event(&format!("burn: writing {} chunk(s) to page(s) {:?}", plan.writes.len(), plan.pages));
        let out = &self.out;
        let result =
            crate::burn::execute(client, &layout, &plan, &target.bytes, |s| out.send(Event::BurnProgress(s)))
                .await;
        client.set_policy(live);
        match result {
            Ok(report) => {
                self.log_event("burn: written and verified");
                let history = self.save_history(&before, &target, &note, &report);
                Ok(BurnSummary { report, history })
            }
            Err(e) => {
                self.log_event(&format!("burn FAILED: {e}"));
                Err(BurnFailure {
                    message: e.to_string(),
                    mismatched: e.mismatched().map(<[_]>::to_vec),
                    wrote: true,
                })
            }
        }
    }

    /// Keep the before and after images and the note of a burn.
    fn save_history(
        &self,
        before: &crate::image::Image,
        after: &crate::image::Image,
        note: &str,
        report: &crate::burn::BurnReport,
    ) -> Option<PathBuf> {
        let now = SystemTime::now();
        let dir = crate::time::unique_path(
            &self.cfg.store.as_ref()?.join(crate::env::HISTORY),
            &format!("{}_{}", after.ecm_id, crate::time::file_stamp(now)),
            "",
        );
        let mut after = after.clone();
        after.meta.created = crate::time::rfc3339(now);
        after.meta.source = format!("burned to the ECM over {}", self.cfg.target);
        let r = before
            .save(&dir.join("before.xbt"), &self.catalog)
            .and_then(|_| after.save(&dir.join("after.xbt"), &self.catalog));
        let text = format!(
            "burned: {}\ntarget: {}\nwrites: {} ({} bytes), pages {:?}\nnote: {note}\n",
            crate::time::rfc3339(now),
            self.cfg.target,
            report.writes,
            report.bytes,
            report.pages
        );
        match r
            .map_err(|e| e.to_string())
            .and_then(|_| std::fs::write(dir.join("burn.txt"), text).map_err(|e| e.to_string()))
        {
            Ok(()) => Some(dir),
            Err(e) => {
                tracing::warn!(error = %e, "couldn't save burn history");
                None
            }
        }
    }

    fn start_read(&mut self) {
        let Some(id) = &self.identity else {
            self.out.send(Event::ReadFailed("the ECM hasn't been identified yet".into()));
            return;
        };
        match id.ecm.layout() {
            Ok(layout) => self.read = Some(crate::read::EepromRead::new(&layout, true)),
            Err(e) => self.out.send(Event::ReadFailed(e.to_string())),
        }
    }

    /// Run a pending EEPROM read to the end. `Some` ends the connection
    /// (link trouble: reconnect and resume).
    async fn continue_read(&mut self, client: &mut EcmClient) -> Option<End> {
        let mut read = self.read.take()?;
        // EEPROM reads use the full timeout, not the live-data one.
        let live_policy = client.policy();
        client.set_policy(RetryPolicy { timeout: self.cfg.read_timeout, ..live_policy });
        let out = &self.out;
        let result = read.run(client, |p| out.send(Event::ReadProgress(p))).await;
        client.set_policy(live_policy);
        match result {
            Ok(()) => {
                let id = self.identity.as_ref().expect("identified before reading");
                let bytes = read.bytes().expect("complete");
                match crate::image::Image::from_raw(&id.ecm, &bytes) {
                    Ok(mut img) => {
                        img.page_zero = read.has_page_zero();
                        img.ecm_version = Some(id.version.clone());
                        img.meta.created = crate::time::rfc3339(SystemTime::now());
                        img.meta.source = format!(
                            "read from ECM over {}, every chunk read twice and matched ({} re-read)",
                            self.cfg.target,
                            read.mismatches()
                        );
                        self.log_event("EEPROM read complete");
                        self.out.send(Event::EepromRead(Box::new(img)));
                    }
                    Err(e) => self.out.send(Event::ReadFailed(e.to_string())),
                }
                None
            }
            Err(crate::read::ReadError::Link(e)) => {
                self.read = Some(read);
                Some(End::Retry(format!("EEPROM read interrupted ({e}); will resume")))
            }
            Err(e) => {
                self.out.send(Event::ReadFailed(e.to_string()));
                None
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
    /// A note for the app log, and for the CSV data log when recording.
    fn log_event(&mut self, what: &str) {
        info!("{what}");
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
        // Unlocked in the app's Settings while connected: works at once.
        s.send(Command::SetPermits { permit: Some(crate::diag::test_permit()), write_permit: None });
        s.send(Command::Function(xbtune_proto::Function::FuelPump));
        let ev = collect_until(&mut s, |e| matches!(e, Event::FunctionDone { .. })).await;
        assert!(matches!(ev.last(), Some(Event::FunctionDone { result: Ok(_), .. })), "{ev:?}");
        assert_eq!(sim.lock().unwrap().journal.functions.len(), 1);
        // Locked again: refused again.
        s.send(Command::SetPermits { permit: None, write_permit: None });
        s.send(Command::Function(xbtune_proto::Function::FuelPump));
        let ev = collect_until(&mut s, |e| matches!(e, Event::FunctionDone { .. })).await;
        assert!(matches!(ev.last(), Some(Event::FunctionDone { result: Err(_), .. })), "{ev:?}");
        assert_eq!(sim.lock().unwrap().journal.functions.len(), 1);
        s.send(Command::Stop);
    }

    #[tokio::test]
    async fn eeprom_read_through_dropouts_matches_the_image() {
        let mut sim = Simulator::bueib_running();
        sim.set_rates(FaultRates { drop: 0.15, ..Default::default() });
        let image = sim.image().to_vec();
        let sim = shared(sim);
        let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut cfg);
        cfg.policy.timeout = Duration::from_millis(40);
        cfg.policy.settle = Duration::from_millis(5);
        cfg.policy.max_attempts = 1;
        cfg.read_timeout = Duration::from_millis(40);
        cfg.fails_before_reconnect = 1;
        let mut s = start(cfg, sim_opener(sim), Arc::new(Catalog::builtin().unwrap()), None);
        collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        s.send(Command::ReadEeprom);
        let ev = collect_until(&mut s, |e| matches!(e, Event::EepromRead(_) | Event::ReadFailed(_))).await;
        let Some(Event::EepromRead(img)) = ev.last() else { panic!("{:?}", ev.last()) };
        assert_eq!(img.bytes, image);
        assert!(img.page_zero);
        assert!(ev.iter().any(|e| matches!(e, Event::ReadProgress(_))));
        // Polling resumes.
        let ev = collect_until(&mut s, |e| matches!(e, Event::Sample(_))).await;
        assert!(matches!(ev.last(), Some(Event::Sample(_))));
        s.send(Command::Stop);
    }

    fn burn_session(sim: xbtune_sim::SharedSim, permit: bool, store: &std::path::Path) -> Session {
        let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut cfg);
        cfg.read_timeout = Duration::from_millis(200);
        cfg.store = Some(store.to_path_buf());
        cfg.write_permit = permit.then(crate::diag::test_write_permit);
        start(cfg, sim_opener(sim), Arc::new(Catalog::builtin().unwrap()), None)
    }

    fn stopped_bueib() -> xbtune_sim::SharedSim {
        let mut sim = Simulator::bueib_running();
        sim.set_engine_running(false);
        let c = Catalog::builtin().unwrap();
        let e = c.lookup("BUEIB").unwrap();
        let p0 = e.layout().unwrap().page(0).unwrap().start;
        sim.poke(p0, &vec![0x64; e.xsize - p0]);
        shared(sim)
    }

    async fn prepared(s: &mut Session) -> (crate::image::Image, PathBuf) {
        s.send(Command::PrepareBurn);
        let ev = collect_until(s, |e| matches!(e, Event::BurnPrepared { .. } | Event::BurnDone(_))).await;
        match ev.last() {
            Some(Event::BurnPrepared { backup, path, .. }) => ((**backup).clone(), path.clone()),
            other => panic!("{other:?}"),
        }
    }

    /// Factory race ECMs (EcmDroid 3c189be): the stock address gets no
    /// answer and a hint; the race address reads, polls and burns.
    #[tokio::test]
    async fn factory_race_ecm_needs_the_race_address() {
        let mut sim = Simulator::b2rib_running();
        sim.set_engine_running(false);
        let sim = shared(sim);
        let catalog = Arc::new(Catalog::builtin().unwrap());
        let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
        fast(&mut cfg);
        let mut s = start(cfg, sim_opener(sim.clone()), catalog.clone(), None);
        let ev = collect_until(&mut s, |e| matches!(e, Event::State(LinkState::Reconnecting { .. }))).await;
        let Some(Event::State(LinkState::Reconnecting { reason, .. })) = ev.last() else { panic!("{ev:?}") };
        assert!(reason.contains("Factory race") && reason.contains("--race"), "{reason}");
        s.send(Command::Stop);
        s.task.await.unwrap();

        let store = std::env::temp_dir().join(format!("xbtune-race-{}", std::process::id()));
        let mut cfg = SessionConfig::new("sim", EcmAddress::Race);
        fast(&mut cfg);
        cfg.read_timeout = Duration::from_millis(200);
        cfg.store = Some(store.clone());
        cfg.write_permit = Some(crate::diag::test_write_permit());
        let mut s = start(cfg, sim_opener(sim.clone()), catalog.clone(), None);
        let ev = collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        let Some(Event::Identified(id)) = ev.last() else { panic!("{ev:?}") };
        assert_eq!(id.ecm.id, "B2RIB");
        assert_eq!(id.ecm.definition, "BUEIB");
        let ev = collect_until(&mut s, |e| matches!(e, Event::Sample(_))).await;
        assert!(matches!(ev.last(), Some(Event::Sample(_))));
        let (backup, _) = prepared(&mut s).await;
        assert_eq!(backup.ecm_id, "B2RIB");
        let mut t = crate::tune::Tune::new(backup, &catalog).unwrap();
        t.set("rev", &[("KRPM_Hard_Hi".into(), 0, 7000.0)]).unwrap();
        s.send(Command::Burn { target: Box::new(t.image.clone()), note: String::new() });
        let ev = collect_until(&mut s, |e| matches!(e, Event::BurnDone(_))).await;
        let Some(Event::BurnDone(Ok(sum))) = ev.last() else { panic!("{:?}", ev.last()) };
        assert_eq!(sum.report.writes, 1);
        assert_eq!(sim.lock().unwrap().image(), &t.image.bytes[..]);
        s.send(Command::Stop);
        let _ = std::fs::remove_dir_all(&store);
    }

    /// Tube-frame DDFI ECMs (no page 0, own runtime frame): identify, poll,
    /// read the EEPROM, read codes, AFV reset and burn, all on the sim.
    #[tokio::test]
    async fn tube_frame_ecms_end_to_end() {
        let catalog = Arc::new(Catalog::builtin().unwrap());
        for id in ["BUEGC", "BUEIA", "BUEJA", "BUEKA"] {
            let ecm = catalog.lookup(id).unwrap().clone();
            assert_eq!(ecm.family, xbtune_defs::Family::Ddfi1);
            assert!(ecm.family.in_scope() && crate::diag::tps_reset_supported(&ecm), "{id}");
            let def = catalog.definition(&ecm).unwrap().clone();
            let mut image = vec![0u8; ecm.xsize];
            // A stored code in the EEPROM's HDiag1 (BUEGC has no HDiag0)
            // and AFV at 100%.
            let hd1 = &def.eeprom["HDiag1"];
            image[hd1.offset as usize] = 0x01;
            let lfuel = &def.eeprom["LFuel"];
            image[lfuel.offset as usize..lfuel.offset as usize + 2].copy_from_slice(&1000u16.to_le_bytes());
            let mut sim = Simulator::new(xbtune_sim::SimConfig::for_ecm(&ecm, "000 01-01-26"), &image)
                .with_engine(def.clone());
            sim.set_engine_running(false);
            let sim = shared(sim);
            let store = std::env::temp_dir().join(format!("xbtune-tube-{id}-{}", std::process::id()));
            let mut cfg = SessionConfig::new("sim", EcmAddress::Stock);
            fast(&mut cfg);
            cfg.read_timeout = Duration::from_millis(200);
            cfg.store = Some(store.clone());
            cfg.write_permit = Some(crate::diag::test_write_permit());
            let mut s = start(cfg, sim_opener(sim.clone()), catalog.clone(), None);
            let ev = collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
            let Some(Event::Identified(ident)) = ev.last() else { panic!("{id}: {ev:?}") };
            assert_eq!(ident.ecm.id, id);
            let ev = collect_until(&mut s, |e| matches!(e, Event::Sample(_))).await;
            assert!(matches!(ev.last(), Some(Event::Sample(_))), "{id}");

            s.send(Command::ReadEeprom);
            let ev =
                collect_until(&mut s, |e| matches!(e, Event::EepromRead(_) | Event::ReadFailed(_))).await;
            let Some(Event::EepromRead(img)) = ev.last() else { panic!("{id}: {:?}", ev.last()) };
            assert_eq!(img.bytes, image, "{id}");
            assert!(!img.page_zero, "{id}: no page 0 on DDFI-1");
            let view = xbtune_defs::ImageView { bytes: &img.bytes, page_zero: false };
            let codes = crate::diag::stored_codes_from_image(&def, &view);
            assert!(!codes.is_empty(), "{id}: stored code in HDiag1");

            // AFV reset finds LFuel (page 1 here, not page 0 as on XBs).
            let mut ecm_bytes = img.bytes.clone();
            ecm_bytes[lfuel.offset as usize..lfuel.offset as usize + 2]
                .copy_from_slice(&950u16.to_le_bytes());
            let (reset, names) = crate::burn::afv_reset(&def, &ecm_bytes, false).unwrap();
            assert_eq!(names, ["LFuel"], "{id}");
            assert_eq!(&reset[lfuel.offset as usize..lfuel.offset as usize + 2], &1000u16.to_le_bytes());

            // Burn a fuel-map cell.
            let (backup, _) = prepared(&mut s).await;
            let mut t = crate::tune::Tune::new(backup, &catalog).unwrap();
            t.set("cell", &[("Tab_Fuel_Front".into(), 0, 42.0)]).unwrap();
            s.send(Command::Burn { target: Box::new(t.image.clone()), note: String::new() });
            let ev = collect_until(&mut s, |e| matches!(e, Event::BurnDone(_))).await;
            let Some(Event::BurnDone(Ok(sum))) = ev.last() else { panic!("{id}: {:?}", ev.last()) };
            assert_eq!(sum.report.writes, 1, "{id}");
            assert_eq!(sim.lock().unwrap().image(), &t.image.bytes[..], "{id}");
            s.send(Command::Stop);
            let _ = std::fs::remove_dir_all(&store);
        }
    }

    #[tokio::test]
    async fn burn_end_to_end_with_backup_and_history() {
        let sim = stopped_bueib();
        let store = std::env::temp_dir().join(format!("xbtune-burn-{}", std::process::id()));
        let mut s = burn_session(sim.clone(), true, &store);
        collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        let (backup, path) = prepared(&mut s).await;
        assert!(path.exists() && path.starts_with(store.join(crate::env::BACKUPS)));
        assert_eq!(backup.bytes, sim.lock().unwrap().image());
        // Edit the rev limit over the backup.
        let c = Catalog::builtin().unwrap();
        let mut t = crate::tune::Tune::new(backup.clone(), &c).unwrap();
        t.set("rev", &[("KRPM_Hard_Hi".into(), 0, 7000.0)]).unwrap();
        s.send(Command::Burn { target: Box::new(t.image.clone()), note: "test burn".into() });
        let ev = collect_until(&mut s, |e| matches!(e, Event::BurnDone(_))).await;
        let Some(Event::BurnDone(Ok(sum))) = ev.last() else { panic!("{:?}", ev.last()) };
        assert_eq!(sum.report.writes, 1);
        assert_eq!(sim.lock().unwrap().image(), &t.image.bytes[..]);
        let h = sum.history.clone().unwrap();
        assert!(h.join("before.xbt").exists() && h.join("after.xbt").exists());
        assert!(std::fs::read_to_string(h.join("burn.txt")).unwrap().contains("note: test burn"));
        // A second burn needs a fresh preparation.
        s.send(Command::Burn { target: Box::new(t.image.clone()), note: String::new() });
        let ev = collect_until(&mut s, |e| matches!(e, Event::BurnDone(_))).await;
        let Some(Event::BurnDone(Err(f))) = ev.last() else { panic!() };
        assert!(f.message.contains("prepare") && !f.wrote, "{f:?}");
        s.send(Command::Stop);
        let _ = std::fs::remove_dir_all(&store);
    }

    #[tokio::test]
    async fn burn_is_refused_without_permit_engine_off_or_unchanged_ecm() {
        let store = std::env::temp_dir().join(format!("xbtune-burn-r-{}", std::process::id()));
        // No permit.
        let sim = stopped_bueib();
        let mut s = burn_session(sim.clone(), false, &store);
        collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        s.send(Command::PrepareBurn);
        let ev = collect_until(&mut s, |e| matches!(e, Event::BurnDone(_))).await;
        let Some(Event::BurnDone(Err(f))) = ev.last() else { panic!() };
        assert!(f.message.contains("locked"), "{f:?}");
        s.send(Command::Stop);
        // Engine running.
        let sim = stopped_bueib();
        sim.lock().unwrap().set_engine_running(true);
        let mut s = burn_session(sim.clone(), true, &store);
        collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        s.send(Command::PrepareBurn);
        let ev = collect_until(&mut s, |e| matches!(e, Event::BurnDone(_))).await;
        let Some(Event::BurnDone(Err(f))) = ev.last() else { panic!() };
        assert!(f.message.contains("engine is running"), "{f:?}");
        s.send(Command::Stop);
        // ECM changed between the backup and the burn.
        let sim = stopped_bueib();
        let mut s = burn_session(sim.clone(), true, &store);
        collect_until(&mut s, |e| matches!(e, Event::Identified(_))).await;
        let (backup, _) = prepared(&mut s).await;
        let c = Catalog::builtin().unwrap();
        let mut t = crate::tune::Tune::new(backup, &c).unwrap();
        t.set("rev", &[("KRPM_Hard_Hi".into(), 0, 7000.0)]).unwrap();
        let at = t.def.eeprom["KRPM_Hard_Hi"].offset as usize;
        sim.lock().unwrap().poke(at + 1, &[0x77]);
        s.send(Command::Burn { target: Box::new(t.image.clone()), note: String::new() });
        let ev = collect_until(&mut s, |e| matches!(e, Event::BurnDone(_))).await;
        let Some(Event::BurnDone(Err(f))) = ev.last() else { panic!() };
        assert!(f.message.contains("changed") && !f.wrote, "{f:?}");
        assert!(sim.lock().unwrap().journal.writes.is_empty());
        s.send(Command::Stop);
        let _ = std::fs::remove_dir_all(&store);
    }
}
