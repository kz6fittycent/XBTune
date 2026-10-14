// SPDX-License-Identifier: GPL-3.0-or-later
//! `xbtune-cli`: headless XBTune. Phase 1 is read-only: it identifies the
//! ECM and measures the link. Nothing here writes to the ECM.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use xbtune_core::env;
use xbtune_defs::Catalog;
use xbtune_link::{EcmClient, OpenError, Target, bluetooth};
use xbtune_proto::{EcmAddress, LinkStats, RetryPolicy};

#[derive(Parser)]
#[command(
    name = "xbtune-cli",
    version,
    about = "XBTune: diagnostics and tuning for Buell XB ECMs (DDFI-2/DDFI-3)"
)]
struct Cli {
    /// Show debug logs. Twice (-vv) also shows every frame in hex.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Connect, read the ECM version and state, and report link quality.
    Info(InfoArgs),
    /// List USB serial ports.
    Ports,
    /// List Bluetooth devices (runs discovery first).
    Scan {
        /// How long to discover for, in seconds (0 lists known devices only).
        #[arg(long, default_value_t = 8)]
        seconds: u64,
    },
    /// List the ECM IDs XBTune knows.
    Ecms,
    /// Check the environment: snap interface connections, storage paths.
    Doctor,
}

#[derive(Args)]
struct Connection {
    /// USB serial adapter, e.g. /dev/ttyUSB0.
    #[arg(long, group = "via")]
    serial: Option<String>,
    /// Bluetooth SPP adapter address, e.g. 00:11:22:AA:BB:CC.
    #[arg(long, group = "via")]
    bt: Option<String>,
    /// Use the built-in simulated ECM (BUEIB).
    #[arg(long, group = "via")]
    sim: bool,
    /// RFCOMM channel for --bt.
    #[arg(long, default_value_t = bluetooth::DEFAULT_CHANNEL)]
    channel: u8,
    /// Talk to a factory race ECM (address 0x55, 19200 baud).
    #[arg(long)]
    race: bool,
    /// Override the serial baud rate.
    #[arg(long)]
    baud: Option<u32>,
    /// Reply timeout per attempt, in milliseconds.
    #[arg(long, default_value_t = 1000)]
    timeout_ms: u64,
    /// Attempts per request (1 = no retries).
    #[arg(long, default_value_t = 3)]
    attempts: u32,
}

#[derive(Args)]
struct InfoArgs {
    #[command(flatten)]
    conn: Connection,
    /// Also read the version this many extra times to measure the link.
    #[arg(long, default_value_t = 20)]
    pings: u32,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = match cli.verbose {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(filter)),
        )
        .with_writer(std::io::stderr)
        .init();

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    match rt.block_on(run(cli.cmd)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Info(a) => info(a).await,
        Cmd::Ports => {
            ports();
            Ok(())
        }
        Cmd::Scan { seconds } => scan(seconds).await,
        Cmd::Ecms => ecms(),
        Cmd::Doctor => {
            doctor();
            Ok(())
        }
    }
}

async fn connect(c: &Connection) -> Result<(EcmClient, String)> {
    let address = if c.race { EcmAddress::Race } else { EcmAddress::Stock };
    let policy = RetryPolicy {
        timeout: Duration::from_millis(c.timeout_ms),
        max_attempts: c.attempts.max(1),
        ..RetryPolicy::default()
    };
    let (io, label) = if c.sim {
        let sim = xbtune_sim::shared(xbtune_sim::Simulator::bueib());
        (xbtune_sim::in_process(sim), "simulated ECM (in-process)".to_string())
    } else {
        let target = if let Some(path) = &c.serial {
            Target::Serial { path: path.clone(), baud: c.baud.unwrap_or(address.baud()) }
        } else if let Some(addr) = &c.bt {
            Target::Rfcomm {
                address: xbtune_link::parse_bt_address(addr).map_err(|e| anyhow!(e))?,
                channel: c.channel,
            }
        } else {
            bail!(
                "say how to reach the ECM: --serial PATH, --bt ADDRESS or --sim\n(see `xbtune-cli ports` and `xbtune-cli scan`)"
            );
        };
        let label = target.to_string();
        let io = xbtune_link::open(&target).await.map_err(|e| open_error(&target, e))?;
        (io, label)
    };
    Ok((EcmClient::new(io, address).with_policy(policy), label))
}

fn open_error(target: &Target, e: OpenError) -> anyhow::Error {
    let plug = match target {
        Target::Serial { .. } => "raw-usb",
        Target::Rfcomm { .. } => "bluez",
    };
    if e.is_permission_denied() {
        anyhow!("{e}\n\n{}", env::permission_hint(plug))
    } else if let Target::Rfcomm { address, .. } = target {
        anyhow!(
            "{e}\n\nIs the adapter powered (ignition on) and paired? To pair:\n    \
             bluetoothctl pair {address}   (BuellTooth PIN is usually 1234)\n    \
             bluetoothctl trust {address}"
        )
    } else {
        anyhow!(e)
    }
}

async fn info(a: InfoArgs) -> Result<()> {
    let (mut client, label) = connect(&a.conn).await?;
    println!("Link:        {label}");
    println!("ECM address: {:#04x}", client.ecm().id());

    let version = client.version().await.context(
        "no answer to the version request. Is the ignition on and the adapter on the diagnostic connector?",
    )?;
    println!("Version:     {version}");

    let catalog = Catalog::builtin()?;
    match catalog.lookup(&version) {
        Some(def) => {
            println!("ECM ID:      {} ({})", def.id, def.family);
            if let Some(bikes) = &def.bikes {
                println!("Fitted to:   {bikes}");
            }
            println!(
                "Definition:  {} - EEPROM {} bytes ({} with page 0), {} pages",
                def.definition,
                def.size,
                def.xsize,
                def.pages.len()
            );
            if !def.family.in_scope() {
                println!("Support:     not supported (XBTune covers DDFI-2 and DDFI-3 XBs only)");
            }
            if def.is_race() != a.conn.race {
                println!(
                    "Note:        definition says race={}, connected with --race={}",
                    def.is_race(),
                    a.conn.race
                );
            }
            check_runtime(&mut client, Some(def.runtime_len)).await;
        }
        None => {
            println!("ECM ID:      UNKNOWN - read-only. Writes will be refused for this ECM.");
            check_runtime(&mut client, None).await;
        }
    }
    match client.current_state().await {
        Ok(0) => println!("State:       idle"),
        Ok(s) => println!("State:       busy ({s:#04x})"),
        Err(e) => println!("State:       could not read ({e})"),
    }
    println!("Writes:      disabled (XBTune phase 1 is read-only)");

    if a.pings > 0 {
        let mut failed = 0;
        for _ in 0..a.pings {
            if client.version().await.is_err() {
                failed += 1;
            }
        }
        println!("Ping:        {} version reads, {failed} failed", a.pings);
    }
    print_stats(client.stats());
    Ok(())
}

async fn check_runtime(client: &mut EcmClient, expected: Option<usize>) {
    match client.runtime_data().await {
        Ok(pdu) => {
            let n = pdu.as_bytes().len();
            match expected {
                Some(e) if e != n => {
                    println!("Runtime:     {n}-byte frame - MISMATCH, definition expects {e}")
                }
                Some(_) => println!("Runtime:     {n}-byte frame (matches definition)"),
                None => println!("Runtime:     {n}-byte frame"),
            }
        }
        Err(e) => println!("Runtime:     could not read ({e})"),
    }
}

fn print_stats(s: &LinkStats) {
    let ms =
        |d: Option<Duration>| d.map_or("-".to_string(), |d| format!("{:.0} ms", d.as_secs_f64() * 1000.0));
    println!("Link stats:");
    println!("  requests {}  attempts {}  ok {}", s.requests, s.attempts, s.ok);
    println!(
        "  retries {}  timeouts {}  corrupt {}  naks {}  io errors {}",
        s.retries, s.timeouts, s.corrupt, s.naks, s.io_errors
    );
    println!("  RTT avg {}  min {}  max {}", ms(s.rtt_avg), ms(s.rtt_min), ms(s.rtt_max));
    println!(
        "  recent error rate {:.1}% over {} attempts",
        s.recent_error_rate() * 100.0,
        s.recent_attempts()
    );
}

fn ports() {
    let ports = xbtune_link::serial::list_ports();
    if ports.is_empty() {
        println!("No USB serial ports found (/dev/ttyUSB*, /dev/ttyACM*).");
        if env::in_snap() {
            println!("If an adapter is plugged in, check: {}", env::permission_hint("raw-usb"));
        }
    }
    for p in ports {
        println!("{}  {}", p.path, p.description);
    }
}

async fn scan(seconds: u64) -> Result<()> {
    if seconds > 0 {
        eprintln!("Scanning for {seconds} s... (turn the ignition on so the adapter is powered)");
    }
    let devices = bluetooth::scan(Duration::from_secs(seconds)).await.map_err(|e| {
        let mut msg = format!("Bluetooth: {e}");
        if env::in_snap() {
            msg.push_str(&format!("\n\n{}", env::permission_hint("bluez")));
        }
        anyhow!(msg)
    })?;
    if devices.is_empty() {
        println!("No Bluetooth devices found.");
    }
    println!("{:<18} {:<24} {:<6} {:<7} {:<5} RSSI", "ADDRESS", "NAME", "PAIRED", "TRUSTED", "SPP");
    for d in devices {
        println!(
            "{:<18} {:<24} {:<6} {:<7} {:<5} {}",
            d.address.to_string(),
            d.name.unwrap_or_else(|| "?".into()),
            yes(d.paired),
            yes(d.trusted),
            yes(d.has_spp),
            d.rssi.map_or("-".into(), |r| r.to_string())
        );
    }
    Ok(())
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

fn ecms() -> Result<()> {
    let c = Catalog::builtin()?;
    println!(
        "{:<6} {:<7} {:<6} {:>5} {:>6}  {:<36} REMARK",
        "ID", "FAMILY", "DEF", "SIZE", "XSIZE", "FITTED TO"
    );
    for d in c.iter() {
        println!(
            "{:<6} {:<7} {:<6} {:>5} {:>6}  {:<36} {}{}",
            d.id,
            d.family.to_string(),
            d.definition,
            d.size,
            d.xsize,
            d.bikes.as_deref().unwrap_or("-"),
            d.remark,
            if d.family.in_scope() { "" } else { " [out of scope]" }
        );
    }
    Ok(())
}

fn doctor() {
    println!("XBTune {}", env!("CARGO_PKG_VERSION"));
    if env::in_snap() {
        println!("Running inside the snap '{}'.", env::snap_name().unwrap_or_default());
        let mut missing = Vec::new();
        for p in env::plug_states() {
            let state = match p.connected {
                Some(true) => "connected",
                Some(false) => {
                    missing.push(p.connect_command());
                    "NOT connected"
                }
                None => "unknown",
            };
            println!("  {:<16} {:<14} {}", p.plug, state, p.purpose);
        }
        if !missing.is_empty() {
            println!("\nTo connect the missing interfaces, run:");
            for m in missing {
                println!("    {m}");
            }
        }
    } else {
        println!("Not running in a snap.");
    }
    match env::Dirs::detect() {
        Some(d) => {
            println!("Config dir:  {}", d.config.display());
            println!("Data dir:    {}", d.data.display());
        }
        None => println!("Could not determine storage directories (no $HOME?)."),
    }
}
