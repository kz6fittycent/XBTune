// SPDX-License-Identifier: GPL-3.0-or-later
//! `logstats`, `autotune` and `log-convert`: offline work on recorded logs
//! (XBTune .csv, EcmDroid .bin, .msl). None talks to the ECM; `autotune`
//! writes a new tune file only when asked (-o).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;
use xbtune_core::autotune::{self, Filters, Settings};
use xbtune_core::logfile::Log;
use xbtune_core::logstats;
use xbtune_core::tune::Tune;
use xbtune_defs::Catalog;

#[derive(Args)]
pub struct StatsArgs {
    /// Log files: .csv from `log` or the app's Record, EcmDroid .bin, .msl.
    #[arg(required = true)]
    logs: Vec<PathBuf>,
    /// O2 target voltage that splits rich from lean.
    #[arg(long, default_value_t = 0.49)]
    midpoint: f64,
}

#[derive(Args)]
pub struct AutotuneArgs {
    /// Tune to correct (.xbt or .xpr): usually a fresh read of the ECM the
    /// logs were recorded with.
    #[arg(long)]
    tune: PathBuf,
    /// Log files to learn from.
    #[arg(required = true)]
    logs: Vec<PathBuf>,
    /// Save the corrected tune here. Without it (or --in-place) nothing is saved.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Overwrite the tune file.
    #[arg(long)]
    in_place: bool,
    /// Which fuel maps: front, rear or both.
    #[arg(long, default_value = "both", value_parser = ["front", "rear", "both"])]
    maps: String,
    /// Minimum sample weight per cell (~6.5 per second spent in a cell).
    #[arg(long, default_value_t = 30.0)]
    min_hits: f64,
    /// Largest change per cell, in percent.
    #[arg(long, default_value_t = 10.0)]
    max_change: f64,
    /// Ignore samples below this engine temperature (°C).
    #[arg(long, default_value_t = 80.0)]
    min_clt: f64,
    /// Correct toward a target AFR with the logs' wideband readings
    /// (logged with --wideband) instead of the ECM's EGO correction.
    #[arg(long)]
    wideband: bool,
    /// Wideband: target by load, as load:AFR or load:lambda pairs (straight lines
    /// between), e.g. 40:14.7,80:13.5 or 40:1.00,80:0.92.
    #[arg(long, value_name = "LOAD:AFR,...", requires = "wideband")]
    target: Option<String>,
    /// Wideband: seconds from a sample to the reading that matches it.
    #[arg(long, default_value_t = autotune::DEFAULT_LAG, requires = "wideband")]
    lag: f64,
}

#[derive(Args)]
pub struct ConvertArgs {
    /// Log to convert: XBTune .csv, EcmDroid .bin or .msl.
    input: PathBuf,
    /// Output: .msl (MegaLogViewer) or .csv (XBTune log).
    #[arg(short, long)]
    output: PathBuf,
}

fn open_logs(paths: &[PathBuf], catalog: &Catalog) -> Result<Vec<Log>> {
    paths.iter().map(|p| Log::open(p, catalog).map_err(anyhow::Error::from)).collect()
}

pub fn logstats(a: StatsArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    for log in open_logs(&a.logs, &catalog)? {
        println!("{} ({})", log.name(), log.source.describe());
        println!("  ECM:       {} ({})", log.version, log.ecm.family);
        if let Some(s) = &log.started {
            println!("  Started:   {s}");
        }
        if let Some(n) = &log.note {
            println!("  Note:      {n}");
        }
        println!(
            "  Samples:   {} over {:.0} s ({:.1} Hz); {} link event(s); {} unreadable row(s)",
            log.samples.len(),
            log.duration(),
            log.rate(),
            log.events.len(),
            log.skipped
        );
        let o2 = logstats::o2_stats(&log, a.midpoint);
        let v = |x: Option<f64>| x.map_or("-".into(), |v| format!("{v:.2} V"));
        println!(
            "  O2:        {:.0}% rich, {:.0}% lean, {:.0}% sensor error; average rich {}, lean {}",
            o2.rich_pct,
            o2.lean_pct,
            o2.error_pct,
            v(o2.avg_rich_v),
            v(o2.avg_lean_v)
        );
        let an = logstats::anomalies(&log);
        if an.is_empty() {
            println!("  Anomalies: none");
        } else {
            println!("  Anomalies: {}", an.len());
            for x in an.iter().take(20) {
                println!("    t={:.1} s  {}", x.t, x.text);
            }
            if an.len() > 20 {
                println!("    ... and {} more", an.len() - 20);
            }
        }
    }
    Ok(())
}

pub fn autotune(a: AutotuneArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let out = match (&a.output, a.in_place) {
        (Some(o), false) if *o == a.tune => bail!("-o is the tune file; use --in-place to overwrite it"),
        (Some(o), false) => Some(o.clone()),
        (None, true) => Some(a.tune.clone()),
        (Some(_), true) => bail!("give either -o or --in-place, not both"),
        (None, false) => None,
    };
    let mut tune = Tune::open(&a.tune, &catalog).with_context(|| format!("opening {}", a.tune.display()))?;
    let logs = open_logs(&a.logs, &catalog)?;
    let maps: Vec<String> = autotune::fuel_maps(&tune)
        .into_iter()
        .filter(|m| match a.maps.as_str() {
            "front" => m.contains("Front"),
            "rear" => m.contains("Rear"),
            _ => true,
        })
        .collect();
    if maps.is_empty() {
        bail!("{}: no fuel maps to tune (--maps front, rear or both)", tune.ecm.id);
    }
    let mode = if a.wideband {
        let target = match &a.target {
            Some(t) => autotune::TargetAfr::parse(t).map_err(anyhow::Error::msg)?,
            None => autotune::TargetAfr::default(),
        };
        println!("Wideband: target AFR by load {}, lag {} s", target.text(), a.lag);
        autotune::Mode::Wideband { target, lag: a.lag }
    } else {
        autotune::Mode::Narrowband
    };
    let settings = Settings {
        filters: Filters { min_clt: a.min_clt, mode, ..Default::default() },
        min_hits: a.min_hits,
        max_change: a.max_change / 100.0,
    };
    let mut all = Vec::new();
    let mut afv = None;
    for map in &maps {
        let an = autotune::analyze(&logs, &tune, map, &settings.filters).map_err(anyhow::Error::msg)?;
        afv = afv.or(an.afv);
        let r = &an.rejected;
        if an.wideband {
            println!(
                "{map}: {} of {} samples used (left out: {} no wideband reading, {} cold, {} changing, {} enrichment, {} after a gap, {} missing)",
                an.used, an.samples, r.no_wideband, r.cold, r.transient, r.enrichment, r.gap, r.missing
            );
        } else {
            println!(
                "{map}: {} of {} samples used (left out: {} open loop, {} cold, {} changing, {} enrichment, {} after a gap, {} missing)",
                an.used, an.samples, r.open_loop, r.cold, r.transient, r.enrichment, r.gap, r.missing
            );
        }
        if !an.missing_channels.is_empty() {
            println!(
                "  a log has no {} channel: auto-tune needs RPM, Load and EGO Corr. in every sample",
                an.missing_channels.join(", ")
            );
        }
        let ys = tune.var(map).ok().and_then(|v| v.y_axis.clone()).and_then(|n| tune.elements(&n).ok());
        let xs = tune.var(map).ok().and_then(|v| v.x_axis.clone()).and_then(|n| tune.elements(&n).ok());
        let p = autotune::propose(&an, &tune, &settings).map_err(anyhow::Error::msg)?;
        if p.is_empty() {
            println!("  no cell has enough samples ({} needed) and a change to make", a.min_hits);
        }
        let at = |v: &Option<Vec<f64>>, i: usize| {
            v.as_ref().and_then(|v| v.get(i)).map_or("?".into(), |v| format!("{v}"))
        };
        for x in &p {
            let why = match x.afr {
                Some((afr, target)) => format!("AFR {afr:.1} for {target:.1}, x{:.3}", x.ego / 100.0),
                None => format!("EGO {:.1}%", x.ego),
            };
            println!(
                "  load {:>4} rpm {:>5}: {:>6.1} -> {:>6.1}  ({why}, {:.0} hits){}",
                at(&ys, x.row),
                at(&xs, x.col),
                x.old,
                x.new,
                x.hits,
                if x.limited { "  capped" } else { "" }
            );
        }
        for &(r, c) in &an.lean {
            println!(
                "  WARNING: lean at high load: load {} rpm {} averaged AFR {:.1}. Check this before riding hard.",
                at(&ys, r),
                at(&xs, c),
                an.cells[r][c].afr
            );
        }
        all.extend(p);
    }
    if let Some(v) = afv {
        println!(
            "AFV in the logs: {v:.1}%. The EGO corrections above already include it: after burning this \
             tune, reset AFV (xbtune.cli afv-reset) so it isn't counted twice."
        );
    }
    match out {
        None => println!("Nothing saved (add -o FILE to save the corrected tune)."),
        Some(_) if all.is_empty() => println!("Nothing to change; nothing saved."),
        Some(path) => {
            let n = autotune::apply(&mut tune, &all).map_err(anyhow::Error::msg)?;
            tune.save(&path, &catalog)?;
            println!("{n} cell(s) changed; saved {}. Nothing was written to the ECM.", path.display());
            println!("{}", xbtune_core::disclaimer::SHORT);
        }
    }
    Ok(())
}

pub fn log_convert(a: ConvertArgs) -> Result<()> {
    use xbtune_core::live::{CsvLogger, LogMeta};
    use xbtune_core::logfile::LogSource;
    let catalog = Catalog::builtin()?;
    if a.output == a.input {
        bail!("-o is the input file");
    }
    let ext = a.output.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("msl" | "csv")) {
        bail!("say .msl or .csv for the output file");
    }
    let log = Log::open(&a.input, &catalog).with_context(|| format!("opening {}", a.input.display()))?;
    let file =
        std::fs::File::create(&a.output).with_context(|| format!("creating {}", a.output.display()))?;
    let out = std::io::BufWriter::new(file);
    match ext.as_deref() {
        Some("msl") => xbtune_core::logformats::write_msl(&log, out)?,
        Some("csv") => {
            let meta = LogMeta {
                ecm_id: log.ecm.id.clone(),
                version: log.version.clone(),
                target: format!("converted from {} ({})", log.name(), log.source.describe()),
                started: std::time::SystemTime::now(),
                note: log.note.clone(),
                wideband: log.wideband.clone(),
            };
            let mut w = CsvLogger::new(out, &meta, log.channels.clone())?;
            for s in &log.samples {
                w.write(s)?;
            }
            for e in &log.events {
                w.event(e.t, &e.text)?;
            }
            w.flush()?;
        }
        _ => bail!("say .msl or .csv for the output file"),
    }
    println!(
        "{}: {} sample(s) from a {}{}; saved {}",
        log.name(),
        log.samples.len(),
        log.source.describe(),
        if log.skipped > 0 {
            format!(", {} unreadable record(s) left out", log.skipped)
        } else {
            String::new()
        },
        a.output.display()
    );
    if log.source == LogSource::Msl && ext.as_deref() == Some("csv") {
        println!("The .msl held values, not frames: the CSV's frames are rebuilt from them (rounded).");
    }
    Ok(())
}
