// SPDX-License-Identifier: GPL-3.0-or-later
//! `xbtune-sim`: run a simulated ECM on a pty for manual testing.
//!
//! ```text
//! xbtune-sim                      # BUEIB on a new /dev/pts/N
//! xbtune-sim --drop 0.1 --corrupt 0.05 --fragment 0.3
//! xbtune-cli info --serial /dev/pts/N
//! ```

use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use xbtune_defs::Catalog;
use xbtune_sim::{FaultRates, SimConfig, Simulator, shared};

#[derive(Parser)]
#[command(version, about = "Simulated Buell ECM on a pseudo-terminal")]
struct Args {
    /// ECM ID to simulate (BUEIB, BUEGB, BUE2D, ...).
    #[arg(long, default_value = "BUEIB")]
    ecm: String,
    /// EEPROM image to serve (raw bytes). BUEIB defaults to EcmDroid's test image.
    #[arg(long)]
    image: Option<std::path::PathBuf>,
    /// Echo requests back, like a half-duplex adapter.
    #[arg(long)]
    echo: bool,
    /// Probability of dropping a reply.
    #[arg(long, default_value_t = 0.0)]
    drop: f64,
    /// Probability of corrupting a reply's checksum.
    #[arg(long, default_value_t = 0.0)]
    corrupt: f64,
    /// Probability of sending garbage bytes before a reply.
    #[arg(long, default_value_t = 0.0)]
    garbage: f64,
    /// Probability of sending a reply in small fragments.
    #[arg(long, default_value_t = 0.0)]
    fragment: f64,
    /// Probability of delaying a reply by --delay-ms.
    #[arg(long, default_value_t = 0.0)]
    delay: f64,
    #[arg(long, default_value_t = 1500)]
    delay_ms: u64,
    /// Also simulate an Innovate LC-2 wideband (ISP2) on a second pty.
    #[arg(long)]
    wideband: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let a = Args::parse();
    let catalog = Catalog::builtin()?;
    let Some(def) = catalog.lookup(&a.ecm) else { bail!("unknown ECM ID {}", a.ecm) };
    let mut cfg = SimConfig::for_ecm(def, "000 01-01-26");
    if def.id == "BUEIB" {
        cfg = SimConfig::bueib();
    }
    cfg.echo = a.echo;
    cfg.rates = FaultRates {
        drop: a.drop,
        corrupt: a.corrupt,
        garbage: a.garbage,
        fragment: a.fragment,
        delay: a.delay,
        delay_by: Duration::from_millis(a.delay_ms),
    };
    let image = match &a.image {
        Some(p) => std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
        None if def.id == "BUEIB" => xbtune_sim::BUEIB_EEPROM.to_vec(),
        None => vec![0; def.xsize],
    };
    let version = cfg.version.clone();
    let sim = shared(Simulator::new(cfg, &image));
    let pty = xbtune_sim::pty::open().context("opening a pty")?;
    println!("Simulated ECM {version} on {}", pty.path);
    println!("Try: xbtune-cli info --serial {}", pty.path);
    if a.wideband {
        let wb = xbtune_sim::pty::open().context("opening a pty")?;
        println!("Simulated Innovate LC-2 (ISP2) on {}", wb.path);
        println!("Try: xbtune-cli live --serial {} --wideband {}", pty.path, wb.path);
        wb.stream_wideband()?;
    }
    println!("Ctrl+C to stop.");
    pty.serve(sim)?.join().ok();
    Ok(())
}
