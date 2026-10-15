// SPDX-License-Identifier: GPL-3.0-or-later
//! `codes`, `test` and `tps-reset` (`xbtune_core::diag`). Reading codes is
//! read-only; everything else changes the ECM, needs a permit, asks for
//! confirmation, and checks the engine is stopped.

use std::io::{BufRead, Write as _};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use xbtune_core::diag::{self, CodeKind, FunctionTiming, Stage, TroubleCode};
use xbtune_defs::Catalog;
use xbtune_link::EcmClient;
use xbtune_proto::Function;

use crate::{Connection, Endpoint};

#[derive(Args)]
pub struct CodesArgs {
    #[command(flatten)]
    conn: Connection,
    /// Clear the stored codes (asks first).
    #[arg(long)]
    clear: bool,
    /// Don't ask for confirmation.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
pub struct TestArgs {
    #[command(flatten)]
    conn: Connection,
    /// Test to run: front-coil, rear-coil, tachometer, fuel-pump,
    /// front-injector, rear-injector, fan, exhaust-valve, active-intake,
    /// shift-light.
    name: Option<String>,
    /// List the tests and what each does.
    #[arg(long)]
    list: bool,
    /// Don't ask for confirmation.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
pub struct TpsArgs {
    #[command(flatten)]
    conn: Connection,
    /// Don't ask for confirmation.
    #[arg(long)]
    yes: bool,
}

struct Connected {
    client: EcmClient,
    ecm: xbtune_defs::EcmDef,
    def: std::sync::Arc<xbtune_defs::Definition>,
    simulated: bool,
}

/// Connect and identify. With `--sim`, the simulated engine is stopped
/// unless --sim-engine-running is given.
async fn connect(conn: &Connection) -> Result<Connected> {
    let ep = Endpoint::from_args(conn)?;
    let simulated = matches!(ep, Endpoint::Sim(_));
    if let Endpoint::Sim(sim) = &ep {
        let mut s = sim.lock().expect("sim lock");
        s.set_engine_running(conn.sim_engine_running);
        if let Some(ms) = conn.sim_test_ms {
            s.set_test_duration(std::time::Duration::from_millis(ms));
        }
    }
    eprintln!("Connecting: {}", ep.label());
    let mut client = ep.open(conn).await?;
    let version = client
        .version()
        .await
        .map_err(|e| anyhow!("no answer to the version request ({e}). {}", conn.no_answer_hint()))?;
    let catalog = Catalog::builtin()?;
    let ecm = catalog.lookup(&version).cloned().ok_or_else(|| anyhow!("unknown ECM '{version}'"))?;
    let def = catalog.definition(&ecm).cloned().ok_or_else(|| anyhow!("{}: no definition", ecm.id))?;
    eprintln!("ECM: {version} ({}, {})", ecm.id, ecm.family);
    Ok(Connected { client, ecm, def, simulated })
}

fn print_codes(title: &str, codes: &[TroubleCode]) {
    println!("{title}:");
    if codes.is_empty() {
        println!("  none");
    }
    for c in codes {
        let code = if c.code.is_empty() || c.code == "0" { "--".to_string() } else { c.code.clone() };
        println!("  {code:>3}  {:<18} {}", c.name, c.description);
    }
}

async fn read_codes(c: &mut Connected) -> Result<()> {
    let frame = c.client.runtime_data().await.context("reading live data")?;
    let f = frame.as_bytes();
    print_codes("Current codes", &diag::codes_from_runtime(&c.def, f, CodeKind::Current));
    print_codes("Stored codes", &diag::codes_from_runtime(&c.def, f, CodeKind::Stored));
    Ok(())
}

/// Ask the user to type `word` to go ahead.
fn confirm(word: &str) -> Result<()> {
    eprint!("Type '{word}' to go ahead, anything else cancels: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim() != word {
        bail!("cancelled");
    }
    Ok(())
}

async fn run(c: &mut Connected, f: Function, yes: bool) -> Result<()> {
    let permit = diag::permit(c.simulated).map_err(|e| anyhow!(e))?;
    println!("{}: {}", f.label(), diag::safety_notice(f));
    if f != Function::ClearCodes {
        println!("The engine must be stopped (ignition ON, kill switch RUN).");
    }
    if !yes {
        confirm("yes")?;
    }
    let mut last = 0.0;
    let o =
        diag::run_function(
            &mut c.client,
            &c.ecm,
            &c.def,
            f,
            &permit,
            FunctionTiming::default(),
            |s| match s {
                Stage::Checking if f == Function::ClearCodes => eprintln!("  checking the ECM is idle..."),
                Stage::Checking => eprintln!("  checking the engine is stopped and the ECM is idle..."),
                Stage::Sent => eprintln!("  sent; waiting for the ECM to finish..."),
                Stage::Running { seconds } if seconds - last >= 1.0 => {
                    last = seconds;
                    eprintln!("  running {seconds:.0} s");
                }
                Stage::Running { .. } => {}
            },
        )
        .await
        .map_err(|e| anyhow!(e))?;
    println!(
        "{}: done{} after {:.1} s.",
        f.label(),
        if o.acknowledged { "" } else { " (reply was lost; the busy state showed it ran)" },
        o.busy_for.as_secs_f64()
    );
    Ok(())
}

pub async fn codes(a: CodesArgs) -> Result<()> {
    if a.clear {
        diag::permit(a.conn.sim).map_err(|e| anyhow!(e))?;
    }
    let mut c = connect(&a.conn).await?;
    read_codes(&mut c).await?;
    if a.clear {
        println!();
        run(&mut c, Function::ClearCodes, a.yes).await?;
        println!();
        read_codes(&mut c).await?;
    }
    Ok(())
}

pub async fn test(a: TestArgs) -> Result<()> {
    if a.list || a.name.is_none() {
        println!("Active tests (the ECM refuses ones for hardware that isn't fitted):");
        for f in diag::TESTS {
            println!("  {:<15} {}", slug(*f), diag::safety_notice(*f));
        }
        if a.name.is_none() && !a.list {
            bail!("say which test to run, e.g. `xbtune-cli test fan --bt ADDRESS`");
        }
        return Ok(());
    }
    let name = a.name.as_deref().unwrap_or_default();
    let f = diag::function_by_name(name)
        .filter(|f| diag::TESTS.contains(f))
        .ok_or_else(|| anyhow!("no test '{name}' (see `xbtune-cli test --list`)"))?;
    diag::permit(a.conn.sim).map_err(|e| anyhow!(e))?;
    let mut c = connect(&a.conn).await?;
    run(&mut c, f, a.yes).await
}

pub async fn tps_reset(a: TpsArgs) -> Result<()> {
    diag::permit(a.conn.sim).map_err(|e| anyhow!(e))?;
    let mut c = connect(&a.conn).await?;
    if !diag::tps_reset_supported(&c.ecm) {
        bail!("{} is a {} ECM: follow the service manual for its TPS reset.", c.ecm.id, c.ecm.family);
    }
    run(&mut c, Function::TpsReset, a.yes).await
}

/// `front-coil` style name for a function.
pub fn slug(f: Function) -> String {
    f.label().to_lowercase().replace(' ', "-")
}
