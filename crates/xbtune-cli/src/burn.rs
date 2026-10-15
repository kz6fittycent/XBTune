// SPDX-License-Identifier: GPL-3.0-or-later
//! `burn`, `restore` and `afv-reset`: write to the ECM's EEPROM through the
//! session's guarded path (`xbtune_core::burn`, KICKOFF §6). Each one:
//! checks, takes a verified backup, shows exactly what will change, asks
//! for confirmation, writes only what differs, reads it back.

use std::io::{BufRead, Write as _};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use xbtune_core::burn::{self, BurnStep, VarChange};
use xbtune_core::image::Image;
use xbtune_core::read::Progress;
use xbtune_core::session::{self, BurnFailure, Command, Event, LinkState, Session, SessionConfig};
use xbtune_defs::Catalog;

use crate::{Connection, Endpoint};

#[derive(Args)]
pub struct BurnArgs {
    #[command(flatten)]
    conn: Connection,
    /// Tune to write (.xbt or .xpr).
    tune: PathBuf,
    /// Variables to write (comma separated). Default: list what differs
    /// and stop, so you can choose.
    #[arg(long, value_delimiter = ',')]
    vars: Vec<String>,
    /// Write every variable that differs and can be written.
    #[arg(long, conflicts_with = "vars")]
    all: bool,
    /// Note for the burn history.
    #[arg(long, default_value = "")]
    note: String,
    /// Don't ask for confirmation.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
pub struct RestoreArgs {
    #[command(flatten)]
    conn: Connection,
    /// Backup image to put back (e.g. from the backups folder).
    backup: PathBuf,
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
pub struct AfvArgs {
    #[command(flatten)]
    conn: Connection,
    #[arg(long)]
    yes: bool,
}

fn start(conn: &Connection) -> Result<(Session, bool)> {
    let ep = Endpoint::from_args(conn)?;
    let simulated = matches!(ep, Endpoint::Sim(_));
    if let Endpoint::Sim(sim) = &ep {
        sim.lock().expect("sim lock").set_engine_running(conn.sim_engine_running);
    }
    let mut cfg = SessionConfig::new(ep.label(), conn.address());
    cfg.policy = conn.policy_with(session::LIVE_TIMEOUT);
    cfg.max_reconnects = Some(3);
    cfg.write_permit = Some(xbtune_core::diag::write_permit(simulated).map_err(|e| anyhow!(e))?);
    eprintln!("Connecting: {}", ep.label());
    Ok((session::start(cfg, ep.opener(), Arc::new(Catalog::builtin()?), None), simulated))
}

async fn next(s: &mut Session) -> Result<Event> {
    loop {
        match s.events.recv().await {
            Some(Event::State(LinkState::Stopped { reason })) => {
                bail!("{}", reason.unwrap_or_else(|| "session stopped".into()))
            }
            Some(Event::State(LinkState::Reconnecting { reason, .. })) => {
                eprintln!("  link: {}; reconnecting...", reason.lines().next().unwrap_or_default())
            }
            Some(Event::Sample(_) | Event::Stats { .. }) => {}
            Some(e) => return Ok(e),
            None => bail!("session ended"),
        }
    }
}

async fn identify(s: &mut Session) -> Result<session::Identity> {
    loop {
        if let Event::Identified(id) = next(s).await? {
            eprintln!("ECM: {} ({}, {})", id.version, id.ecm.id, id.ecm.family);
            return Ok(id);
        }
    }
}

fn progress_read(p: &Progress) {
    eprint!("\r  backup read: {:>3}%   ", p.done * 100 / p.total.max(1));
    let _ = std::io::stderr().flush();
}

async fn prepare(s: &mut Session) -> Result<(Image, PathBuf)> {
    eprintln!("Checking the engine is stopped and the link is good, then reading a backup...");
    s.send(Command::PrepareBurn);
    loop {
        match next(s).await? {
            Event::ReadProgress(p) => progress_read(&p),
            Event::BurnPrepared { backup, path, link } => {
                eprintln!();
                eprintln!(
                    "Link: {} of {} test requests OK, average reply {} ms",
                    link.attempts - link.failures,
                    link.attempts,
                    link.rtt_avg.map_or(0, |d| d.as_millis())
                );
                println!("Backup:  {}", path.display());
                return Ok((*backup, path));
            }
            Event::BurnDone(Err(f)) => bail!("{}", f.message),
            _ => {}
        }
    }
}

fn show_changes(ch: &[VarChange], chosen: &[String]) {
    let w = ch.iter().map(|c| c.label.len() + c.name.len() + 3).max().unwrap_or(10).clamp(10, 60);
    println!("{:<3} {:<w$} {:<16} Tune", "", "Variable", "ECM now");
    for c in ch {
        let mark = if c.blocked.is_some() {
            "!!"
        } else if chosen.contains(&c.name) {
            "->"
        } else {
            ""
        };
        println!("{mark:<3} {:<w$} {:<16} {}", format!("{} ({})", c.label, c.name), c.ecm, c.tune);
        if let Some(b) = &c.blocked {
            println!("      can't write: {b}");
        }
    }
}

fn confirm(word: &str) -> Result<()> {
    eprint!("Type '{word}' to write to the ECM, anything else cancels: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    if line.trim() != word {
        bail!("cancelled; nothing was written");
    }
    Ok(())
}

async fn run_burn(s: &mut Session, target: Image, note: String) -> Result<()> {
    s.send(Command::Burn { target: Box::new(target), note });
    loop {
        match next(s).await? {
            Event::BurnProgress(BurnStep::Writing { done, total, page }) => {
                eprint!("\r  writing {done}/{total} (page {page})   ");
                let _ = std::io::stderr().flush();
            }
            Event::BurnProgress(BurnStep::Verifying { page }) => {
                eprint!("\r  reading back page {page}          ");
                let _ = std::io::stderr().flush();
            }
            Event::BurnDone(Ok(sum)) => {
                eprintln!();
                if sum.report.writes == 0 {
                    println!("Nothing to write: the ECM already holds these values.");
                } else {
                    println!(
                        "Written and verified: {} write(s), {} bytes, page(s) {:?}.",
                        sum.report.writes, sum.report.bytes, sum.report.pages
                    );
                    if let Some(h) = sum.history {
                        println!("History: {}", h.display());
                    }
                    println!(
                        "Turn the ignition off and on before riding, so the ECM starts from the new values."
                    );
                }
                return Ok(());
            }
            Event::BurnDone(Err(f)) => {
                eprintln!();
                return Err(failure(&f));
            }
            _ => {}
        }
    }
}

fn failure(f: &BurnFailure) -> anyhow::Error {
    let mut m = format!("BURN FAILED: {}", f.message);
    if f.wrote {
        match &f.mismatched {
            Some(p) if p.is_empty() => m.push_str("\nThe ECM holds the new data on every page checked."),
            Some(p) => {
                m.push_str("\nPages that don't hold the new data:");
                for pm in p {
                    let r: Vec<String> =
                        pm.ranges.iter().map(|(a, b)| format!("{a:#06x}..{b:#06x}")).collect();
                    m.push_str(&format!("\n  page {}: {}", pm.page, r.join(", ")));
                }
            }
            None => m.push_str("\nThe ECM's state couldn't be read back."),
        }
        m.push_str(
            "\nKeep the ignition ON. Run the same command again to retry, or put the backup back with\n    \
             xbtune.cli restore <backup file> ...\n(the backup path is printed above).",
        );
    } else {
        m.push_str("\nNothing was written.");
    }
    anyhow!(m)
}

pub async fn burn(a: BurnArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let tune =
        Image::load(&a.tune, None, &catalog).with_context(|| format!("opening {}", a.tune.display()))?;
    let (mut s, _) = start(&a.conn)?;
    let id = identify(&mut s).await?;
    burn::check_identity(&id.version, &tune.ecm_id, &id.ecm).map_err(|e| anyhow!(e))?;
    let (backup, _) = prepare(&mut s).await?;
    let ch =
        burn::changes(&id.definition, &backup.bytes, &tune.bytes, None, backup.page_zero && tune.page_zero);
    if ch.is_empty() {
        println!("The ECM already holds this tune. Nothing to write.");
        s.send(Command::Stop);
        return Ok(());
    }
    let chosen: Vec<String> = if a.all {
        ch.iter().filter(|c| c.blocked.is_none()).map(|c| c.name.clone()).collect()
    } else {
        a.vars.clone()
    };
    show_changes(&ch, &chosen);
    if chosen.is_empty() {
        s.send(Command::Stop);
        bail!(
            "nothing chosen. Variables the ECM changes by itself (AFV, stored codes, counters) may be listed; \
             choose what to write with --vars NAME,NAME or --all. Nothing was written."
        );
    }
    for v in &chosen {
        if !ch.iter().any(|c| &c.name == v) {
            s.send(Command::Stop);
            bail!("{v} doesn't differ from the ECM (or doesn't exist); nothing was written");
        }
    }
    let target_bytes =
        burn::build_target(&id.definition, &backup.bytes, &tune.bytes, backup.page_zero, &chosen)
            .map_err(|e| anyhow!(e))?;
    let mut target = backup.clone();
    target.bytes = target_bytes;
    println!("\nWill write: {}", chosen.join(", "));
    if !a.yes {
        confirm("burn")?;
    }
    let r = run_burn(&mut s, target, a.note.clone()).await;
    s.send(Command::Stop);
    r
}

pub async fn restore(a: RestoreArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let backup_file =
        Image::load(&a.backup, None, &catalog).with_context(|| format!("opening {}", a.backup.display()))?;
    let (mut s, _) = start(&a.conn)?;
    let id = identify(&mut s).await?;
    burn::check_identity(&id.version, &backup_file.ecm_id, &id.ecm).map_err(|e| anyhow!(e))?;
    let (now, _) = prepare(&mut s).await?;
    let ch = burn::changes(
        &id.definition,
        &now.bytes,
        &backup_file.bytes,
        None,
        now.page_zero && backup_file.page_zero,
    );
    if ch.is_empty() {
        println!("The ECM already matches the backup. Nothing to write.");
        s.send(Command::Stop);
        return Ok(());
    }
    println!("Restoring puts back every byte that differs, including values the ECM changes itself:");
    show_changes(&ch, &ch.iter().map(|c| c.name.clone()).collect::<Vec<_>>());
    if ch.iter().any(|c| c.blocked.is_some()) {
        s.send(Command::Stop);
        bail!("some differences can't be written (marked !!); nothing was written");
    }
    if !a.yes {
        confirm("restore")?;
    }
    let mut target = now.clone();
    target.bytes = backup_file.bytes.clone();
    let r = run_burn(&mut s, target, format!("restore {}", a.backup.display())).await;
    s.send(Command::Stop);
    r
}

pub async fn afv_reset(a: AfvArgs) -> Result<()> {
    let (mut s, _) = start(&a.conn)?;
    let id = identify(&mut s).await?;
    let (now, _) = prepare(&mut s).await?;
    let (bytes, names) =
        burn::afv_reset(&id.definition, &now.bytes, now.page_zero).map_err(|e| anyhow!(e))?;
    let ch = burn::changes(&id.definition, &now.bytes, &bytes, None, now.page_zero);
    if ch.is_empty() {
        println!("AFV is already {}%. Nothing to write.", burn::AFV_NEUTRAL);
        s.send(Command::Stop);
        return Ok(());
    }
    println!("AFV reset sets {} to {}%:", names.join(", "), burn::AFV_NEUTRAL);
    show_changes(&ch, &names);
    if !a.yes {
        confirm("reset")?;
    }
    let mut target = now.clone();
    target.bytes = bytes;
    let r = run_burn(&mut s, target, "AFV reset".into()).await;
    s.send(Command::Stop);
    r
}
