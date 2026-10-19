// SPDX-License-Identifier: GPL-3.0-or-later
//! `live` and `log`: runtime data from the ECM through a session
//! (`xbtune_core::session`). Both are read-only.

use std::io::{IsTerminal, Write as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, bail};
use clap::Args;
use xbtune_core::live::{DEFAULT_CHANNELS, channels, format_value, set_flags, units};
use xbtune_core::session::{self, Command, Event, Identity, LinkState, SessionConfig};
use xbtune_core::wideband::{self, WbLink};
use xbtune_defs::{Catalog, RtVar};
use xbtune_proto::LinkStats;

use crate::{Connection, Endpoint, env};

#[derive(Args)]
pub struct LiveArgs {
    #[command(flatten)]
    conn: Connection,
    /// Channels to show, by name (see --list). Default: the main engine channels.
    #[arg(long, value_delimiter = ',')]
    channels: Vec<String>,
    /// Show every channel.
    #[arg(long)]
    all: bool,
    /// List the channels and exit after the first frame.
    #[arg(long)]
    list: bool,
    /// Stop after this many frames (default: run until Ctrl+C).
    #[arg(long)]
    count: Option<u64>,
    /// Shortest time between requests, in milliseconds (0: as fast as the link allows).
    #[arg(long, default_value_t = 0)]
    interval_ms: u64,
    /// Wideband controller (Innovate LC-2 or other ISP2 device) on this
    /// serial port, e.g. /dev/ttyUSB1; "sim" for a simulated one.
    #[arg(long, value_name = "PORT")]
    wideband: Option<String>,
}

#[derive(Args)]
pub struct LogArgs {
    #[command(flatten)]
    conn: Connection,
    /// CSV file to write. Default: the XBTune logs folder, named
    /// <ECM ID>_<UTC time>.csv.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Stop after this many seconds (default: run until Ctrl+C).
    #[arg(long)]
    duration: Option<f64>,
    /// Note for the log header.
    #[arg(long)]
    note: Option<String>,
    /// Shortest time between requests, in milliseconds (0: as fast as the link allows).
    #[arg(long, default_value_t = 0)]
    interval_ms: u64,
    /// Give up after this many reconnects in a row without data.
    #[arg(long, default_value_t = 20)]
    reconnects: u32,
    /// Wideband controller (Innovate LC-2 or other ISP2 device) on this
    /// serial port, e.g. /dev/ttyUSB1; "sim" for a simulated one. Its
    /// readings are logged as wb_lambda, wb_afr and wb_status.
    #[arg(long, value_name = "PORT")]
    wideband: Option<String>,
}

fn start(
    conn: &mut Connection,
    interval_ms: u64,
    max_reconnects: Option<u32>,
    wideband: Option<&str>,
) -> Result<(session::Session, String)> {
    // The demo ECM behaves like a BuellTooth: ~35 ms latency plus the
    // K-line's 9600-baud wire time.
    conn.sim_latency_ms.get_or_insert(35);
    conn.sim_baud.get_or_insert(9600);
    let ep = Endpoint::from_args(conn)?;
    let label = ep.label();
    let mut cfg = SessionConfig::new(label.clone(), conn.address());
    cfg.policy = conn.policy_with(session::LIVE_TIMEOUT);
    cfg.min_interval = Duration::from_millis(interval_ms);
    cfg.max_reconnects = max_reconnects;
    cfg.wideband = match wideband {
        None => None,
        Some("sim") => {
            // Follows the simulated ECM's ride when there is one.
            let sim = match &ep {
                Endpoint::Sim(s) => s.clone(),
                Endpoint::Real(_) => xbtune_sim::shared(xbtune_sim::Simulator::bueib_running()),
            };
            Some(WbLink {
                label: "simulated Innovate LC-2".into(),
                opener: Arc::new(move || {
                    let sim = sim.clone();
                    Box::pin(async move { Ok(xbtune_sim::wideband::in_process(sim)) })
                }),
            })
        }
        Some(path) => Some(WbLink::serial(path)),
    };
    if let Some(w) = &cfg.wideband {
        eprintln!("Wideband: {}", w.label);
    }
    let s = session::start(cfg, ep.opener(), Arc::new(Catalog::builtin()?), None);
    Ok((s, label))
}

/// Wait for the session to identify the ECM, printing progress.
async fn identify(s: &mut session::Session) -> Result<Identity> {
    loop {
        tokio::select! {
            e = s.events.recv() => match e {
                Some(Event::Identified(id)) => return Ok(id),
                Some(Event::State(LinkState::Reconnecting { reason, .. })) => {
                    eprintln!("  {}; retrying...", reason.lines().next().unwrap_or_default())
                }
                Some(Event::State(LinkState::Stopped { reason })) => {
                    bail!("{}", reason.unwrap_or_else(|| "stopped".into()))
                }
                Some(_) => {}
                None => bail!("session ended"),
            },
            _ = tokio::signal::ctrl_c() => {
                s.send(Command::Stop);
                bail!("interrupted");
            }
        }
    }
}

fn pick(all: &[RtVar], names: &[String], every: bool) -> Result<Vec<RtVar>> {
    if every {
        return Ok(all.to_vec());
    }
    if names.is_empty() {
        return Ok(DEFAULT_CHANNELS
            .iter()
            .filter_map(|n| all.iter().find(|v| v.name == *n).cloned())
            .collect());
    }
    names
        .iter()
        .map(|n| {
            all.iter()
                .find(|v| v.name.eq_ignore_ascii_case(n) || v.varname.eq_ignore_ascii_case(n))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no channel '{n}' (see `xbtune-cli live --list`)"))
        })
        .collect()
}

pub async fn live(mut a: LiveArgs) -> Result<()> {
    let (mut s, label) = start(&mut a.conn, a.interval_ms, Some(2), a.wideband.as_deref())?;
    eprintln!("Connecting: {label}");
    let id = identify(&mut s).await?;
    eprintln!("ECM: {} ({}, {})", id.version, id.ecm.id, id.ecm.family);
    let mut all = channels(&id.definition);
    if a.wideband.is_some() {
        all.extend(wideband::channels());
    }
    if a.list {
        println!("{:<14} {:<16} {:<6} {:>4}  Label", "Channel", "Variable", "Units", "Off");
        for v in &all {
            println!("{:<14} {:<16} {:<6} {:>4}  {}", v.name, v.varname, units(v), v.offset, v.label());
        }
        s.send(Command::Stop);
        return Ok(());
    }
    let mut vars = pick(&all, &a.channels, a.all)?;
    if a.wideband.is_some() && a.channels.is_empty() && !a.all {
        vars.extend(wideband::channels());
    }
    let tty = std::io::stdout().is_terminal() && a.count.is_none();
    let mut frames = 0u64;
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut status = Status::default();
    loop {
        tokio::select! {
            e = s.events.recv() => match e {
                Some(Event::Sample(sample)) => {
                    frames += 1;
                    let done = a.count.is_some_and(|c| frames >= c);
                    if !tty || done || last_draw.elapsed() >= Duration::from_millis(200) {
                        last_draw = Instant::now();
                        let mut out = String::new();
                        if tty {
                            out.push_str("\x1b[H\x1b[2J");
                            out.push_str(&format!("{}  {}  ({})\n{}\n\n", id.version, label, status.line(), "Ctrl+C to stop"));
                        } else {
                            out.push_str(&format!("t={:.2}s\n", sample.t));
                        }
                        for v in &vars {
                            let val = sample.value(v).map(|x| format_value(v, x)).unwrap_or_else(|| "-".into());
                            let flags = sample.value(v).map(|x| set_flags(v, x).join(", ")).unwrap_or_default();
                            out.push_str(&format!("{:<34} {:>10} {:<6} {}\n", v.label(), val, units(v), flags));
                        }
                        print!("{out}");
                        let _ = std::io::stdout().flush();
                    }
                    if done {
                        s.send(Command::Stop);
                        break;
                    }
                }
                Some(Event::Stats { stats, hz, reconnects, .. }) => {
                    status.stats = Some(stats);
                    status.hz = hz;
                    status.reconnects = reconnects;
                }
                Some(Event::State(LinkState::Reconnecting { reason, .. })) => eprintln!("link lost ({}); reconnecting...", reason.lines().next().unwrap_or_default()),
                Some(Event::State(LinkState::Stopped { reason })) => {
                    if let Some(r) = reason {
                        bail!("{r}");
                    }
                    break;
                }
                Some(Event::Warning(w)) => eprintln!("warning: {w}"),
                Some(Event::Wideband(w)) if !tty => eprintln!("wideband: {}", w.text()),
                Some(Event::Wideband(w)) => status.wideband = Some(w),
                Some(_) => {}
                None => break,
            },
            _ = tokio::signal::ctrl_c() => s.send(Command::Stop),
        }
    }
    eprintln!("{frames} frames. {}", status.line());
    Ok(())
}

#[derive(Default)]
struct Status {
    stats: Option<LinkStats>,
    hz: f64,
    reconnects: u32,
    wideband: Option<wideband::WbState>,
}

impl Status {
    fn line(&self) -> String {
        let Some(st) = &self.stats else { return "starting".into() };
        let mut s = format!(
            "{:.1} Hz  RTT {}  retries {}  errors {:.0}%  reconnects {}",
            self.hz,
            st.rtt_avg.map_or("-".into(), |d| format!("{:.0} ms", d.as_secs_f64() * 1000.0)),
            st.retries,
            st.recent_error_rate() * 100.0,
            self.reconnects
        );
        if let Some(w) = &self.wideband {
            s.push_str(&format!("  wideband {}", w.text()));
        }
        s
    }
}

fn logs_dir() -> PathBuf {
    env::Dirs::detect().map(|d| d.logs()).unwrap_or_else(|| PathBuf::from("."))
}

pub async fn log(mut a: LogArgs) -> Result<()> {
    let (mut s, label) = start(&mut a.conn, a.interval_ms, Some(a.reconnects), a.wideband.as_deref())?;
    eprintln!("Connecting: {label}");
    let id = identify(&mut s).await?;
    eprintln!("ECM: {} ({}, {})", id.version, id.ecm.id, id.ecm.family);
    let path = a.output.clone().unwrap_or_else(|| {
        logs_dir().join(format!("{}_{}.csv", id.ecm.id, xbtune_core::time::file_stamp(SystemTime::now())))
    });
    s.send(Command::StartLog { path: path.clone(), note: a.note.clone() });
    let mut started: Option<Instant> = None;
    let mut status = Status::default();
    let mut rows = 0;
    let mut stopping = false;
    let mut result = Ok(());
    let deadline = a.duration.map(Duration::from_secs_f64);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            e = s.events.recv() => match e {
                Some(Event::LogStarted(p)) => {
                    started = Some(Instant::now());
                    eprintln!("Logging to {}  (Ctrl+C to stop)", p.display());
                }
                Some(Event::Wideband(w)) => eprintln!("\n  wideband: {}", w.text()),
                Some(Event::Stats { stats, hz, reconnects, log_rows }) => {
                    rows = log_rows.unwrap_or(rows);
                    status.stats = Some(stats);
                    status.hz = hz;
                    status.reconnects = reconnects;
                    if let Some(t) = started {
                        eprint!("\r  {:>6.1} s  {rows} rows  {}   ", t.elapsed().as_secs_f64(), status.line());
                        let _ = std::io::stderr().flush();
                    }
                }
                Some(Event::State(LinkState::Reconnecting { reason, .. })) => eprintln!("\n  link lost ({}); reconnecting...", reason.lines().next().unwrap_or_default()),
                Some(Event::LogStopped { rows: r, error, .. }) => {
                    rows = r;
                    if let Some(e) = error {
                        result = Err(anyhow::anyhow!("log stopped: {e}"));
                    }
                    s.send(Command::Stop);
                    stopping = true;
                }
                Some(Event::State(LinkState::Stopped { reason })) => {
                    if let (Some(r), Ok(())) = (reason, &result) {
                        result = Err(anyhow::anyhow!("{r}"));
                    }
                    break;
                }
                Some(_) => {}
                None => break,
            },
            _ = tick.tick() => {
                if !stopping && let (Some(t), Some(d)) = (started, deadline) && t.elapsed() >= d {
                    stopping = true;
                    s.send(Command::StopLog);
                }
            }
            _ = tokio::signal::ctrl_c(), if !stopping => {
                stopping = true;
                s.send(Command::StopLog);
            }
        }
    }
    eprintln!();
    let secs = started.map_or(0.0, |t| t.elapsed().as_secs_f64());
    println!("Saved:       {}", path.display());
    println!(
        "Rows:        {rows} in {secs:.1} s ({:.1} rows/s)",
        if secs > 0.0 { rows as f64 / secs } else { 0.0 }
    );
    println!("Link:        {}", status.line());
    result
}
