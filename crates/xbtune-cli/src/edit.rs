// SPDX-License-Identifier: GPL-3.0-or-later
//! `set`, `copy-table` and `fold-afv`: offline edits of image files (`xbtune_core::tune`).
//! They never touch the ECM, and write a new file unless --in-place.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use xbtune_core::image::Image;
use xbtune_core::tune::{Applied, Tune};
use xbtune_defs::{Catalog, Shape, format_number};

#[derive(Args)]
pub struct SetArgs {
    /// Image to edit (.xbt or .xpr).
    file: PathBuf,
    /// Edits: NAME=VALUE for values, NAME[ROW,COL]=VALUE for map cells
    /// (0-based), NAME[I]=VALUE for list/table elements (storage order).
    #[arg(required = true)]
    edits: Vec<String>,
    /// Output file. Required unless --in-place.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Overwrite the input file.
    #[arg(long)]
    in_place: bool,
}

#[derive(Args)]
pub struct CopyArgs {
    /// Image to copy from.
    from: PathBuf,
    /// Image to copy into.
    into: PathBuf,
    /// Variables to copy (e.g. Tab_Fuel_Front Tab_Fuel_Rear).
    #[arg(required = true)]
    vars: Vec<String>,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long)]
    in_place: bool,
}

#[derive(Args)]
pub struct FoldArgs {
    /// Image to edit: a fresh read of the ECM, made after switching the
    /// ignition off (the ECM stores AFV now and then, not at every change).
    file: PathBuf,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long)]
    in_place: bool,
}

fn out_path(input: &Path, output: &Option<PathBuf>, in_place: bool) -> Result<PathBuf> {
    match (output, in_place) {
        (Some(o), false) if o == input => bail!("-o is the input file; use --in-place to overwrite it"),
        (Some(o), false) => Ok(o.clone()),
        (None, true) => Ok(input.to_path_buf()),
        (Some(_), true) => bail!("give either -o or --in-place, not both"),
        (None, false) => bail!("say where to save: -o FILE (or --in-place)"),
    }
}

/// Parse `NAME=V`, `NAME[R,C]=V`, `NAME[I]=V` into (name, element index, value).
fn parse_edit(t: &Tune, s: &str) -> Result<(String, usize, f64)> {
    let (lhs, rhs) = s.split_once('=').ok_or_else(|| anyhow!("'{s}': expected NAME=VALUE"))?;
    let value: f64 = rhs.trim().parse().map_err(|_| anyhow!("'{s}': '{rhs}' isn't a number"))?;
    let (name, idx) = match lhs.split_once('[') {
        Some((n, rest)) => (n.trim(), Some(rest.trim_end_matches(']'))),
        None => (lhs.trim(), None),
    };
    let v = t.var(name)?;
    let index = match (idx, &v.shape) {
        (None, Shape::Scalar | Shape::Bits) => 0,
        (None, _) => bail!("{name} has several elements; say which, e.g. {name}[0,3]=..."),
        (Some(i), Shape::Grid { rows, cols, .. }) => {
            let (r, c) = i.split_once(',').ok_or_else(|| anyhow!("{name} is a map: use {name}[ROW,COL]"))?;
            let (r, c): (usize, usize) = (r.trim().parse()?, c.trim().parse()?);
            if r >= *rows || c >= *cols {
                bail!("{name} is {rows}x{cols}; [{r},{c}] is outside it");
            }
            r * cols + c
        }
        (Some(i), _) => i.trim().parse()?,
    };
    Ok((name.to_string(), index, value))
}

fn summary(a: Applied) -> String {
    let mut s = format!("{} element(s) changed", a.changed);
    if a.clamped > 0 {
        s.push_str(&format!(", {} clamped to the allowed range", a.clamped));
    }
    s
}

pub fn set(a: SetArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let out = out_path(&a.file, &a.output, a.in_place)?;
    let mut t = Tune::open(&a.file, &catalog).with_context(|| format!("opening {}", a.file.display()))?;
    let edits = a.edits.iter().map(|e| parse_edit(&t, e)).collect::<Result<Vec<_>>>()?;
    let before: Vec<f64> =
        edits.iter().map(|(n, i, _)| t.elements(n).map(|e| e[*i])).collect::<Result<_, _>>()?;
    let applied = t.set("cli set", &edits)?;
    for ((n, i, _), old) in edits.iter().zip(before) {
        let v = t.var(n)?;
        let new = t.elements(n)?[*i];
        let d = v.names.decimals;
        println!("{n}[{i}]: {} -> {}", format_number(old, d), format_number(new, d));
    }
    t.save(&out, &catalog)?;
    println!("{}; saved {}", summary(applied), out.display());
    Ok(())
}

pub fn copy_table(a: CopyArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let out = out_path(&a.into, &a.output, a.in_place)?;
    let from =
        Image::load(&a.from, None, &catalog).with_context(|| format!("opening {}", a.from.display()))?;
    let mut t = Tune::open(&a.into, &catalog).with_context(|| format!("opening {}", a.into.display()))?;
    let mut total = Applied::default();
    for v in &a.vars {
        let r = t.copy_from(&from, v)?;
        println!("{v}: {}", summary(r));
        total.changed += r.changed;
        total.clamped += r.clamped;
    }
    t.save(&out, &catalog)?;
    println!("{}; saved {}", summary(total), out.display());
    Ok(())
}

pub fn fold_afv(a: FoldArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let out = out_path(&a.file, &a.output, a.in_place)?;
    let mut t = Tune::open(&a.file, &catalog).with_context(|| format!("opening {}", a.file.display()))?;
    let plan = xbtune_core::autotune::afv_fold_plan(&t).map_err(anyhow::Error::msg)?;
    for f in &plan {
        println!("{}: x {:.3} ({} {:.1}%)", f.map, f.afv / 100.0, f.afv_var, f.afv);
    }
    let applied = xbtune_core::autotune::fold_afv(&mut t, &plan).map_err(anyhow::Error::msg)?;
    t.save(&out, &catalog)?;
    println!("{}; AFV set to 100%; saved {}", summary(applied), out.display());
    println!("{}", xbtune_core::disclaimer::SHORT);
    let mut vars: Vec<&str> = plan.iter().flat_map(|f| [f.afv_var.as_str(), f.map.as_str()]).collect();
    vars.sort_unstable();
    vars.dedup();
    println!(
        "Nothing was written to the ECM. Burn the maps and AFV together: burn {} --vars {}",
        out.display(),
        vars.join(",")
    );
    Ok(())
}
