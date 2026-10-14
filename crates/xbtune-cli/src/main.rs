// SPDX-License-Identifier: GPL-3.0-or-later
//! `xbtune-cli`: headless XBTune. Read-only: it identifies the ECM, measures
//! the link, reads and saves the EEPROM, and decodes images. Nothing here
//! writes to the ECM.

mod live;
mod render;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand};
use xbtune_core::env;
use xbtune_core::image::{Image, crc32, diff_ranges};
use xbtune_core::read::EepromRead;
use xbtune_defs::{Catalog, EeKind, Shape};
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
    /// Read the whole EEPROM and save it (read-only; never writes the ECM).
    Dump(DumpArgs),
    /// Show an EEPROM image: summary, one or more variables, or a list.
    Show(ShowArgs),
    /// Compare two EEPROM images and name the variables that differ.
    Diff {
        a: PathBuf,
        b: PathBuf,
        /// ECM ID for .xpr files whose name doesn't start with it.
        #[arg(long)]
        id: Option<String>,
    },
    /// Convert between XBTune (.xbt) and EcmDroid (.xpr) image files.
    Convert {
        input: PathBuf,
        output: PathBuf,
        /// ECM ID for an .xpr input whose name doesn't start with it.
        #[arg(long)]
        id: Option<String>,
    },
    /// Show live data from the ECM (read-only).
    Live(live::LiveArgs),
    /// Record live data to a CSV log (read-only).
    Log(live::LogArgs),
    /// Show what XBTune knows about an ECM ID's definition, and data issues.
    Defs {
        /// ECM ID, e.g. BUEIB.
        id: String,
        /// List every variable.
        #[arg(long)]
        list: bool,
    },
}

#[derive(Args)]
struct DumpArgs {
    #[command(flatten)]
    conn: Connection,
    /// Output file (.xbt, or .xpr for EcmDroid). Default: the XBTune data
    /// folder, named <ECM ID>_<UTC time>.xbt.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Also write an EcmDroid-compatible .xpr copy here.
    #[arg(long)]
    xpr: Option<PathBuf>,
    /// Read each chunk once instead of twice. Faster, but a corrupted reply
    /// that passes the checksum would go unnoticed. Don't use for backups.
    #[arg(long)]
    no_verify: bool,
    /// Note to store with the image.
    #[arg(long, default_value = "")]
    notes: String,
    /// VIN to store with the image.
    #[arg(long, default_value = "")]
    vin: String,
    /// How many times to reconnect after a dropout before giving up.
    #[arg(long, default_value_t = 5)]
    reconnects: u32,
    /// Pause before each reconnect, in milliseconds.
    #[arg(long, hide = true, default_value_t = 2000)]
    reconnect_delay_ms: u64,
}

#[derive(Args)]
struct ShowArgs {
    /// Image file (.xbt or .xpr).
    file: PathBuf,
    /// Variables to show (names as in `--list`, e.g. Tab_Fuel_Front).
    vars: Vec<String>,
    /// List every variable with its value or shape.
    #[arg(long)]
    list: bool,
    /// Include variables the original database marks hidden.
    #[arg(long)]
    hidden: bool,
    /// ECM ID for an .xpr whose name doesn't start with it.
    #[arg(long)]
    id: Option<String>,
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
    /// Reply timeout per attempt, in milliseconds (default 1000; 400 for
    /// live and log).
    #[arg(long)]
    timeout_ms: Option<u64>,
    /// Attempts per request (1 = no retries).
    #[arg(long, default_value_t = 3)]
    attempts: u32,
    /// Testing: make the simulator drop this fraction of replies.
    #[arg(long, hide = true, default_value_t = 0.0)]
    sim_drop: f64,
    /// Testing: delay every simulator reply, in milliseconds (live and log
    /// default to 35, plus 9600-baud wire time).
    #[arg(long, hide = true)]
    sim_latency_ms: Option<u64>,
    /// Testing: simulated K-line baud rate (0 = instant).
    #[arg(long, hide = true)]
    sim_baud: Option<u32>,
}

impl Connection {
    fn address(&self) -> EcmAddress {
        if self.race { EcmAddress::Race } else { EcmAddress::Stock }
    }

    fn policy(&self) -> RetryPolicy {
        self.policy_with(RetryPolicy::default().timeout)
    }

    /// The policy, with `timeout` unless --timeout-ms was given.
    fn policy_with(&self, timeout: Duration) -> RetryPolicy {
        RetryPolicy {
            timeout: self.timeout_ms.map(Duration::from_millis).unwrap_or(timeout),
            max_attempts: self.attempts.max(1),
            ..RetryPolicy::default()
        }
    }
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
        Cmd::Dump(a) => dump(a).await,
        Cmd::Show(a) => show(a),
        Cmd::Diff { a, b, id } => diff(&a, &b, id.as_deref()),
        Cmd::Convert { input, output, id } => convert(&input, &output, id.as_deref()),
        Cmd::Defs { id, list } => defs(&id, list),
        Cmd::Live(a) => live::live(a).await,
        Cmd::Log(a) => live::log(a).await,
    }
}

/// Where the ECM is, kept so the link can be reopened after a dropout.
enum Endpoint {
    Sim(xbtune_sim::SharedSim),
    Real(Target),
}

impl Endpoint {
    fn from_args(c: &Connection) -> Result<Self> {
        let address = if c.race { EcmAddress::Race } else { EcmAddress::Stock };
        Ok(if c.sim {
            let mut sim = xbtune_sim::Simulator::bueib_running();
            let latency = c.sim_latency_ms.unwrap_or(0);
            sim.set_rates(xbtune_sim::FaultRates {
                drop: c.sim_drop,
                delay: if latency > 0 { 1.0 } else { 0.0 },
                delay_by: Duration::from_millis(latency),
                ..Default::default()
            });
            sim.set_wire_baud(c.sim_baud.filter(|b| *b > 0));
            Endpoint::Sim(xbtune_sim::shared(sim))
        } else if let Some(path) = &c.serial {
            Endpoint::Real(Target::Serial { path: path.clone(), baud: c.baud.unwrap_or(address.baud()) })
        } else if let Some(addr) = &c.bt {
            Endpoint::Real(Target::Rfcomm {
                address: xbtune_link::parse_bt_address(addr).map_err(|e| anyhow!(e))?,
                channel: c.channel,
            })
        } else {
            bail!(
                "say how to reach the ECM: --serial PATH, --bt ADDRESS or --sim\n(see `xbtune-cli ports` and `xbtune-cli scan`)"
            );
        })
    }

    fn label(&self) -> String {
        match self {
            Endpoint::Sim(_) => "simulated ECM (in-process)".into(),
            Endpoint::Real(t) => t.to_string(),
        }
    }

    async fn open_io(&self) -> Result<xbtune_link::BoxIo> {
        Ok(match self {
            Endpoint::Sim(sim) => xbtune_sim::in_process(sim.clone()),
            Endpoint::Real(target) => xbtune_link::open(target).await.map_err(|e| open_error(target, e))?,
        })
    }

    async fn open(&self, c: &Connection) -> Result<EcmClient> {
        Ok(EcmClient::new(self.open_io().await?, c.address()).with_policy(c.policy()))
    }

    /// The endpoint as a reopenable transport for a live session.
    fn opener(self) -> xbtune_core::session::Opener {
        let ep = std::sync::Arc::new(self);
        std::sync::Arc::new(move || {
            let ep = ep.clone();
            Box::pin(async move { ep.open_io().await.map_err(|e| format!("{e:#}")) })
        })
    }
}

async fn connect(c: &Connection) -> Result<(EcmClient, String)> {
    let ep = Endpoint::from_args(c)?;
    Ok((ep.open(c).await?, ep.label()))
}

fn open_error(target: &Target, e: OpenError) -> anyhow::Error {
    anyhow!(xbtune_core::session::open_error_text(target, &e))
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

/// Default place for images read from the ECM.
fn images_dir() -> PathBuf {
    env::Dirs::detect().map(|d| d.data.join("images")).unwrap_or_else(|| PathBuf::from("."))
}

async fn dump(a: DumpArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let ep = Endpoint::from_args(&a.conn)?;
    eprintln!("Connecting: {}", ep.label());
    let mut client = ep.open(&a.conn).await?;
    // The first request can be lost like any other; retry a few times.
    let mut tries = 0;
    let version = loop {
        match client.version().await {
            Ok(v) => break v,
            Err(e) if tries < 3 => {
                tries += 1;
                eprintln!("  no answer to the version request ({e}); trying again");
            }
            Err(e) => return Err(anyhow!(e).context("no answer to the version request")),
        }
    };
    let Some(ecm) = catalog.lookup(&version).cloned() else {
        bail!("ECM reports '{version}', which XBTune doesn't know. Can't read its EEPROM safely.");
    };
    eprintln!("ECM: {version} ({}, {})", ecm.id, ecm.family);
    if !ecm.family.in_scope() {
        eprintln!("Note: {} is outside XBTune's scope; reading anyway.", ecm.family);
    }
    let layout = ecm.layout()?;
    let verify = !a.no_verify;
    let mut read = EepromRead::new(&layout, verify);
    let started = Instant::now();
    let mut reconnects = 0;
    loop {
        let result = read
            .run(&mut client, |p| {
                eprint!(
                    "\r  reading: {:>3}%  ({}/{} reads, page {:>2}){}   ",
                    p.done * 100 / p.total.max(1),
                    p.done,
                    p.total,
                    p.page,
                    if p.mismatches > 0 { format!(", {} re-read", p.mismatches) } else { String::new() }
                );
                let _ = std::io::stderr().flush();
            })
            .await;
        match result {
            Ok(()) => break,
            Err(xbtune_core::read::ReadError::Link(e)) if reconnects < a.reconnects => {
                reconnects += 1;
                eprintln!("\n  link lost ({e}); reconnecting ({reconnects}/{})...", a.reconnects);
                tokio::time::sleep(Duration::from_millis(a.reconnect_delay_ms)).await;
                match ep.open(&a.conn).await {
                    Ok(c) => client = c,
                    Err(e) => {
                        eprintln!("  reconnect failed: {e:#}");
                        continue;
                    }
                }
                // Make sure it is still the same ECM before continuing.
                match client.version().await {
                    Ok(again) if again != version => {
                        bail!("after reconnecting the ECM reports '{again}', not '{version}'. Stopping.")
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("  no answer after reconnecting ({e})"),
                }
            }
            Err(e) => {
                let p = read.progress(0);
                bail!("\nread stopped at {}/{}: {e}", p.done, p.total);
            }
        }
    }
    eprintln!();
    let bytes = read.bytes().expect("complete");
    let mut img = Image::from_raw(&ecm, &bytes)?;
    img.page_zero = read.has_page_zero();
    img.ecm_version = Some(version.clone());
    let now = SystemTime::now();
    img.meta.created = xbtune_core::time::rfc3339(now);
    img.meta.source = format!(
        "read from ECM over {}{}",
        ep.label(),
        if verify { ", every chunk read twice and matched" } else { ", single read (not verified)" }
    );
    img.meta.notes = a.notes;
    img.meta.vin = a.vin;
    let out = a.output.unwrap_or_else(|| {
        images_dir().join(format!("{}_{}.xbt", ecm.id, xbtune_core::time::file_stamp(now)))
    });
    img.save(&out, &catalog).with_context(|| format!("saving {}", out.display()))?;
    println!("Saved:       {}", out.display());
    if let Some(x) = &a.xpr {
        img.save(x, &catalog).with_context(|| format!("saving {}", x.display()))?;
        println!("EcmDroid:    {}", x.display());
    }
    println!(
        "Image:       {} bytes{}, CRC-32 {:08x}",
        img.bytes.len(),
        if img.page_zero { " (incl. page 0)" } else { "" },
        crc32(&img.bytes)
    );
    println!(
        "Verified:    {}",
        if verify { format!("yes, {} chunk(s) needed re-reading", read.mismatches()) } else { "no".into() }
    );
    println!("Took:        {:.1} s, {reconnects} reconnect(s)", started.elapsed().as_secs_f64());
    print_stats(client.stats());
    Ok(())
}

fn load_image(path: &Path, id: Option<&str>, catalog: &Catalog) -> Result<Image> {
    Image::load(path, id, catalog).with_context(|| format!("opening {}", path.display()))
}

fn show(a: ShowArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let img = load_image(&a.file, a.id.as_deref(), &catalog)?;
    let ecm = catalog.lookup(&img.ecm_id).expect("checked on load");
    let Some(def) = catalog.definition(ecm) else {
        bail!("{} ({}) is out of scope; no variable definitions", ecm.id, ecm.family);
    };
    let view = img.view();
    if !a.vars.is_empty() {
        for name in &a.vars {
            let var = def.eeprom.get(name).or_else(|| {
                def.eeprom
                    .values()
                    .find(|v| v.name.eq_ignore_ascii_case(name) || v.label().eq_ignore_ascii_case(name))
            });
            match var {
                Some(v) => println!("{}", render::render(v, def, &img)),
                None => println!("{name}: no such variable (see --list)\n"),
            }
        }
        return Ok(());
    }
    println!("File:        {}", a.file.display());
    println!(
        "ECM:         {} ({}){}",
        ecm.id,
        ecm.family,
        ecm.bikes.as_ref().map_or(String::new(), |b| format!(", {b}"))
    );
    if let Some(v) = &img.ecm_version {
        println!("Version:     {v}");
    }
    println!(
        "Bytes:       {}{}  CRC-32 {:08x}",
        img.bytes.len(),
        if img.page_zero { " (incl. page 0)" } else { " (page 0 not included)" },
        crc32(&img.bytes)
    );
    for (k, v) in [
        ("Created", &img.meta.created),
        ("Source", &img.meta.source),
        ("VIN", &img.meta.vin),
        ("Notes", &img.meta.notes),
    ] {
        if !v.is_empty() {
            println!("{:<12} {v}", format!("{k}:"));
        }
    }
    let get = |n: &str| def.eeprom.get(n).map(|v| render::inline_value(v, &view));
    for (label, name) in [
        ("Serial", "KMFG_Serial"),
        ("Mfg year", "KMFG_Year"),
        ("Mfg day", "KMFG_Day"),
        ("Calibration", "Cal_ID"),
        ("Country ID", "Country_ID"),
    ] {
        if let Some(v) = get(name) {
            println!("{:<12} {v}", format!("{label}:"));
        }
    }
    if !a.list {
        let maps: Vec<&str> = def
            .eeprom
            .values()
            .filter(|v| v.kind == EeKind::Map && !v.secret)
            .map(|v| v.name.as_str())
            .collect();
        println!("\nMaps: {}", maps.join(", "));
        println!("Show one with: xbtune-cli show {} Tab_Fuel_Front", a.file.display());
        println!("Everything:    xbtune-cli show {} --list", a.file.display());
        return Ok(());
    }
    println!();
    println!("{:<6} {:<24} {:<6} {:<40} VALUE", "OFFSET", "NAME", "KIND", "LABEL");
    for v in def.eeprom_by_offset(img.bytes.len()) {
        if v.secret && !a.hidden {
            continue;
        }
        let at = v.start(&view).map(|s| format!("{s:04x}")).unwrap_or_else(|_| "p0".into());
        let label: String = v.label().chars().take(40).collect();
        let kind =
            if matches!(v.shape, Shape::Invalid(_)) { "?".to_string() } else { format!("{:?}", v.kind) };
        println!("{at:<6} {:<24} {kind:<6} {label:<40} {}", v.name, render::inline_value(v, &view));
    }
    Ok(())
}

fn diff(a: &Path, b: &Path, id: Option<&str>) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let ia = load_image(a, id, &catalog)?;
    let ib = load_image(b, id, &catalog)?;
    if ia.ecm_id != ib.ecm_id {
        let (da, db) = (catalog.lookup(&ia.ecm_id).unwrap(), catalog.lookup(&ib.ecm_id).unwrap());
        if da.definition != db.definition {
            bail!("different ECM types: {} vs {}", ia.ecm_id, ib.ecm_id);
        }
    }
    // Only compare page 0 if both images have it.
    let ecm = catalog.lookup(&ia.ecm_id).unwrap();
    let both_p0 = ia.page_zero && ib.page_zero;
    let n = if both_p0 { ecm.xsize } else { ecm.size };
    if ia.page_zero != ib.page_zero {
        println!("Note: only one image has page 0; comparing the first {n} bytes.");
    }
    let ranges = diff_ranges(&ia.bytes[..n], &ib.bytes[..n]);
    if ranges.is_empty() {
        println!("Identical ({n} bytes compared).");
        return Ok(());
    }
    let changed: usize = ranges.iter().map(|(s, e)| e - s).sum();
    println!("{changed} byte(s) differ in {} range(s):", ranges.len());
    let def = catalog.definition(ecm);
    let (va, vb) = (ia.view(), ib.view());
    let mut seen = std::collections::BTreeSet::new();
    for (s, e) in &ranges {
        let names: Vec<String> = (*s..*e)
            .filter_map(|at| def.and_then(|d| render::var_at(d, &va, at)))
            .map(|v| v.name.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        println!(
            "  {s:04x}..{:04x}  {}",
            e - 1,
            if names.is_empty() { "(no variable defined here)".into() } else { names.join(", ") }
        );
        seen.extend(names);
    }
    if let Some(def) = def {
        println!();
        for name in seen {
            let v = &def.eeprom[&name];
            println!("  {name}: {}  ->  {}", render::inline_value(v, &va), render::inline_value(v, &vb));
        }
    }
    Ok(())
}

fn convert(input: &Path, output: &Path, id: Option<&str>) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let img = load_image(input, id, &catalog)?;
    img.save(output, &catalog).with_context(|| format!("saving {}", output.display()))?;
    println!(
        "Wrote {} ({}, {} bytes{})",
        output.display(),
        img.ecm_id,
        img.bytes.len(),
        if img.page_zero { ", with page 0" } else { ", no page 0" }
    );
    Ok(())
}

fn defs(id: &str, list: bool) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let ecm = catalog.lookup(id).ok_or_else(|| anyhow!("unknown ECM ID {id}"))?;
    println!("{} ({}), definition {}, category {}", ecm.id, ecm.family, ecm.definition, ecm.category);
    let Some(def) = catalog.definition(ecm) else {
        println!("Out of scope: no variable definitions loaded.");
        return Ok(());
    };
    let mut kinds = std::collections::BTreeMap::new();
    for v in def.eeprom.values() {
        *kinds.entry(format!("{:?}", v.kind)).or_insert(0) += 1;
    }
    println!("EEPROM variables: {} {kinds:?}", def.eeprom.len());
    println!("Runtime variables: {}", def.runtime.len());
    if !def.conflicts.is_empty() {
        println!("Left out (conflicting rows in ecmdroid.db): {}", def.conflicts.join(", "));
    }
    if def.issues.is_empty() {
        println!("Data issues: none");
    } else {
        println!("Data issues ({}):", def.issues.len());
        for i in &def.issues {
            println!("  - {i}");
        }
    }
    if list {
        println!();
        for v in def.eeprom_by_offset(ecm.xsize) {
            println!("{:>6} {:<24} {:<6} {}", v.offset, v.name, format!("{:?}", v.kind), v.label());
        }
        println!();
        for v in def.runtime.values() {
            println!("rt {:>4} {:<16} {}", v.offset, v.name, v.label());
        }
    }
    Ok(())
}
