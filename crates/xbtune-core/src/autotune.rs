// SPDX-License-Identifier: GPL-3.0-or-later
//! EGO-based auto-tune for narrowband bikes (KICKOFF §5 "Tuning").
//!
//! In closed loop the ECM trims the fuel with the EGO correction until the
//! narrowband O2 sensor reads stoichiometric. Where a fuel cell needed, on
//! average, 95% to get there, it holds 5% too much fuel: the proposal is
//! `new = old × EGO / 100`. Only the working tune changes; burning stays a
//! separate, reviewed step.
//!
//! What the owner's logs showed (2026-10-14, BUEIB), which this relies on:
//! - The ECM's live table value (`veCurr1/2`) equals the fuel map
//!   interpolated bilinearly at (RPM, Load), wherever Load sits on a
//!   breakpoint. So a sample belongs to the 4 cells around it, weighted the
//!   same way.
//! - In open loop (warm-up, O2 sensor not active) the EGO correction holds
//!   exactly at the AFV value; in closed loop it changes on almost every
//!   frame. "EGO changed in the last few samples" therefore marks closed
//!   loop without the (unnamed) flag bits.
//! - EGO starts from AFV, so it is the whole closed-loop correction (AFV
//!   included). After burning corrected cells, AFV should be reset so it
//!   isn't counted twice.
//!
//! Filters (all adjustable): warm engine, closed loop, steady RPM and
//! throttle, no acceleration/deceleration/WOT enrichment, no gap before the
//! sample. A cell needs a minimum weight of samples, and each pass changes
//! a cell by at most `max_change`.

use xbtune_defs::{EeVar, Shape};

use crate::logfile::Log;
use crate::tune::Tune;

/// Which channel follows a map axis, by the axis name (XBTune's reading:
/// "Fuel Map RPM Axis" -> RPM, "Fuel Map Load Axis" -> Load).
pub fn channel_for_axis(axis_label: &str) -> Option<&'static str> {
    if axis_label.contains("RPM") {
        Some("RPM")
    } else if axis_label.contains("Load") {
        Some("Load")
    } else {
        None
    }
}

/// Fractional position of `x` in breakpoints `axis` (either direction);
/// outside the axis it sits on the nearer end.
pub fn frac_index(axis: &[f64], x: f64) -> Option<f64> {
    for i in 0..axis.len().saturating_sub(1) {
        let (a, b) = (axis[i], axis[i + 1]);
        if (a <= x && x <= b) || (b <= x && x <= a) {
            let f = if b != a { (x - a) / (b - a) } else { 0.0 };
            return Some(i as f64 + f);
        }
    }
    let (first, last) = (*axis.first()?, *axis.last()?);
    Some(if (x - first).abs() < (x - last).abs() { 0.0 } else { (axis.len() - 1) as f64 })
}

/// The (up to) 4 cells around a fractional (row, col), with bilinear
/// weights that add up to 1.
pub fn cell_weights(r: f64, c: f64, rows: usize, cols: usize) -> Vec<((usize, usize), f64)> {
    let (r0, c0) = (r.floor() as usize, c.floor() as usize);
    let (fr, fc) = (r - r0 as f64, c - c0 as f64);
    let mut out = Vec::with_capacity(4);
    for (dr, wr) in [(0, 1.0 - fr), (1, fr)] {
        for (dc, wc) in [(0, 1.0 - fc), (1, fc)] {
            let (rr, cc) = (r0 + dr, c0 + dc);
            let w = wr * wc;
            if w > 1e-9 && rr < rows && cc < cols {
                out.push(((rr, cc), w));
            }
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct Filters {
    /// Engine temperature at least this (CLT channel, °C).
    pub min_clt: f64,
    /// Closed loop: EGO changed within this many samples before.
    pub ego_window: usize,
    /// Steady state: RPM changing no faster than this (RPM/s).
    pub max_rpm_rate: f64,
    /// Steady state: throttle changing no faster than this (°/s).
    pub max_tpd_rate: f64,
    /// A sample more than this after the previous one is skipped (s).
    pub max_gap: f64,
}

impl Default for Filters {
    fn default() -> Self {
        Self { min_clt: 80.0, ego_window: 3, max_rpm_rate: 800.0, max_tpd_rate: 10.0, max_gap: 1.0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub filters: Filters,
    /// A cell needs at least this much sample weight (one sample right on
    /// a cell = 1; at ~6.5 Hz, 30 is about 5 s of riding in it).
    pub min_hits: f64,
    /// Most a cell may change in one pass, as a fraction (0.10 = ±10%).
    pub max_change: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { filters: Filters::default(), min_hits: 30.0, max_change: 0.10 }
    }
}

/// Why samples were left out, for the summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rejected {
    pub missing: usize,
    pub cold: usize,
    pub open_loop: usize,
    pub transient: usize,
    pub enrichment: usize,
    pub gap: usize,
}

impl Rejected {
    pub fn total(&self) -> usize {
        self.missing + self.cold + self.open_loop + self.transient + self.enrichment + self.gap
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CellStat {
    /// Sum of sample weights in this cell.
    pub hits: f64,
    /// Weighted average EGO correction (%), meaningful when `hits > 0`.
    pub ego: f64,
}

/// Samples from one or more logs placed on one fuel map.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub map: String,
    pub rows: usize,
    pub cols: usize,
    /// `cells[r][c]`, in the map's storage order.
    pub cells: Vec<Vec<CellStat>>,
    /// All samples, and those that passed the filters.
    pub samples: usize,
    pub used: usize,
    pub rejected: Rejected,
    /// AFV from the logs (last value seen), if logged.
    pub afv: Option<f64>,
}

fn grid_of(v: &EeVar) -> Result<(usize, usize), String> {
    match v.shape {
        Shape::Grid { rows, cols, .. } => Ok((rows, cols)),
        _ => Err(format!("{} isn't a map", v.name)),
    }
}

/// Breakpoints and live channel of one axis of `map`.
fn axis(tune: &Tune, name: Option<&String>) -> Result<(Vec<f64>, &'static str), String> {
    let name = name.ok_or("the map has no axis")?;
    let label = tune.var(name).map_err(|e| e.to_string())?.label().to_string();
    let ch = channel_for_axis(&label).ok_or_else(|| format!("no live channel follows '{label}'"))?;
    Ok((tune.elements(name).map_err(|e| e.to_string())?, ch))
}

/// Place the logs' samples on `map` (e.g. "Tab_Fuel_Front") and average
/// the EGO correction per cell.
pub fn analyze(logs: &[Log], tune: &Tune, map: &str, f: &Filters) -> Result<Analysis, String> {
    let v = tune.var(map).map_err(|e| e.to_string())?.clone();
    let (rows, cols) = grid_of(&v)?;
    let (xs, xch) = axis(tune, v.x_axis.as_ref())?;
    let (ys, ych) = axis(tune, v.y_axis.as_ref())?;
    let mut sum = vec![vec![(0.0f64, 0.0f64); cols]; rows];
    let mut a = Analysis {
        map: map.into(),
        rows,
        cols,
        cells: Vec::new(),
        samples: 0,
        used: 0,
        rejected: Rejected::default(),
        afv: None,
    };
    for log in logs {
        if log.ecm.id != tune.ecm.id {
            return Err(format!(
                "{} is from a {}, the tune is for a {}",
                log.name(),
                log.ecm.id,
                tune.ecm.id
            ));
        }
        let series = |n: &str| log.series(n);
        let (x, y, ego, clt, tpd) =
            (series(xch), series(ych), series("EGO Corr."), series("CLT"), series("TPD"));
        let (rpm, accel, decel, wot) =
            (series("RPM"), series("Accel Corr."), series("Decel Corr."), series("WOT Corr."));
        if let Some(Some(v)) = series("AFV").last() {
            a.afv = Some(*v);
        }
        for i in 0..log.samples.len() {
            a.samples += 1;
            let rej = &mut a.rejected;
            let (Some(xv), Some(yv), Some(e)) = (x[i], y[i], ego[i]) else {
                rej.missing += 1;
                continue;
            };
            if i == 0 {
                rej.gap += 1;
                continue;
            }
            let dt = log.samples[i].t - log.samples[i - 1].t;
            if dt <= 0.0 || dt > f.max_gap {
                rej.gap += 1;
                continue;
            }
            if clt[i].is_none_or(|c| c < f.min_clt) {
                rej.cold += 1;
                continue;
            }
            let lo = i.saturating_sub(f.ego_window);
            if !(lo..i).any(|j| ego[j].is_some_and(|p| p != e)) {
                rej.open_loop += 1;
                continue;
            }
            let rate = |s: &[Option<f64>]| match (s[i], s[i - 1]) {
                (Some(b), Some(a)) => ((b - a) / dt).abs(),
                _ => 0.0,
            };
            if rate(&rpm) > f.max_rpm_rate || rate(&tpd) > f.max_tpd_rate {
                rej.transient += 1;
                continue;
            }
            if accel[i].is_some_and(|v| v > 0.0)
                || decel[i].is_some_and(|v| v != 100.0)
                || wot[i].is_some_and(|v| v != 100.0)
            {
                rej.enrichment += 1;
                continue;
            }
            let (Some(c), Some(r)) = (frac_index(&xs, xv), frac_index(&ys, yv)) else {
                rej.missing += 1;
                continue;
            };
            for ((rr, cc), w) in cell_weights(r, c, rows, cols) {
                sum[rr][cc].0 += w;
                sum[rr][cc].1 += w * e;
            }
            a.used += 1;
        }
    }
    a.cells = sum
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|(h, s)| CellStat { hits: h, ego: if h > 0.0 { s / h } else { 0.0 } })
                .collect()
        })
        .collect();
    Ok(a)
}

/// One proposed change.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub map: String,
    pub row: usize,
    pub col: usize,
    /// Storage index for `Tune::set`.
    pub index: usize,
    pub old: f64,
    pub new: f64,
    pub ego: f64,
    pub hits: f64,
    /// The change was capped at `max_change`.
    pub limited: bool,
}

/// Proposed values for the cells with enough samples whose value would
/// change (after the map's rounding).
pub fn propose(a: &Analysis, tune: &Tune, s: &Settings) -> Result<Vec<Proposal>, String> {
    let v = tune.var(&a.map).map_err(|e| e.to_string())?.clone();
    let el = tune.elements(&a.map).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for (r, row) in a.cells.iter().enumerate() {
        for (c, st) in row.iter().enumerate() {
            if st.hits < s.min_hits || st.hits <= 0.0 {
                continue;
            }
            let index = r * a.cols + c;
            let old = el[index];
            let want = st.ego / 100.0;
            let factor = want.clamp(1.0 - s.max_change, 1.0 + s.max_change);
            let (raw, _) = crate::tune::encode(&v, index, old * factor);
            let new = crate::tune::element_value(&v, index, raw);
            if (new - old).abs() < 1e-9 {
                continue;
            }
            out.push(Proposal {
                map: a.map.clone(),
                row: r,
                col: c,
                index,
                old,
                new,
                ego: st.ego,
                hits: st.hits,
                limited: (factor - want).abs() > 1e-9,
            });
        }
    }
    Ok(out)
}

/// Start of the undo label of an applied auto-tune.
pub const AUTOTUNE_STEP: &str = "auto-tune: ";

/// Whether the tune holds auto-tune corrections that haven't been undone.
/// They already include AFV (EGO starts from it), so folding AFV in as well
/// would count it twice: reset AFV instead.
pub fn has_autotune_step(tune: &Tune) -> bool {
    tune.undo_labels().any(|l| l.starts_with(AUTOTUNE_STEP))
}

/// Apply the chosen proposals to the tune as one undoable step.
pub fn apply(tune: &mut Tune, chosen: &[Proposal]) -> Result<usize, String> {
    let changes: Vec<(String, usize, f64)> = chosen.iter().map(|p| (p.map.clone(), p.index, p.new)).collect();
    let applied =
        tune.set(format!("{AUTOTUNE_STEP}{} cell(s)", chosen.len()), &changes).map_err(|e| e.to_string())?;
    Ok(applied.changed)
}

/// The fuel maps auto-tune works on, front and rear, where defined.
pub fn fuel_maps(tune: &Tune) -> Vec<String> {
    ["Tab_Fuel_Front", "Tab_Fuel_Rear"]
        .into_iter()
        .filter(|n| tune.var(n).is_ok_and(|v| matches!(v.shape, Shape::Grid { .. })))
        .map(String::from)
        .collect()
}

/// One fuel map and the AFV that applies to it.
#[derive(Debug, Clone, PartialEq)]
pub struct AfvFold {
    pub map: String,
    /// `LFuel` ("AFV Rear") or `LFuel1` ("AFV Front").
    pub afv_var: String,
    /// Percent, as stored in the tune.
    pub afv: f64,
}

/// Which AFV applies to which fuel map (XBTune's reading, see PROVENANCE):
/// the rear map uses `LFuel`; the front map uses `LFuel1` where the ECM has
/// it and `KCL_Fuel_Config` bit 0 ("Enable front closed loop") is set (or
/// the config byte isn't defined), otherwise `LFuel` too.
pub fn afv_fold_plan(tune: &Tune) -> Result<Vec<AfvFold>, String> {
    let number = |name: &str| match tune.value(name) {
        Ok(xbtune_defs::Value::Number(x)) => Ok(x),
        Ok(_) => Err(format!("{name} isn't a single value")),
        Err(e) => Err(e.to_string()),
    };
    let rear = crate::burn::AFV_VARS[0];
    if tune.var(rear).is_err() {
        return Err(format!("{} has no AFV ({rear})", tune.ecm.id));
    }
    let front_own = tune.var("LFuel1").is_ok()
        && match tune.value("KCL_Fuel_Config") {
            Ok(xbtune_defs::Value::Bits(b)) => b & 1 != 0,
            _ => true,
        };
    let mut out = Vec::new();
    for map in fuel_maps(tune) {
        let afv_var = if map.contains("Front") && front_own { "LFuel1" } else { rear };
        tune.editable(afv_var).map_err(|e| e.to_string())?;
        tune.editable(&map).map_err(|e| e.to_string())?;
        out.push(AfvFold { afv: number(afv_var)?, afv_var: afv_var.to_string(), map });
    }
    if out.is_empty() {
        return Err(format!("{} has no fuel maps", tune.ecm.id));
    }
    Ok(out)
}

/// Multiply each fuel map by its AFV / 100 and set those AFVs to 100%, as
/// one undoable step. The ECM then fuels the same with nothing learned.
pub fn fold_afv(tune: &mut Tune, plan: &[AfvFold]) -> Result<crate::tune::Applied, String> {
    if plan.iter().all(|f| (f.afv - crate::burn::AFV_NEUTRAL).abs() < 1e-9) {
        return Err("AFV is already 100%: nothing to fold into the maps".into());
    }
    let mut changes = Vec::new();
    for f in plan {
        let factor = f.afv / crate::burn::AFV_NEUTRAL;
        let cells = tune.elements(&f.map).map_err(|e| e.to_string())?;
        changes.extend(cells.iter().enumerate().map(|(i, v)| (f.map.clone(), i, v * factor)));
    }
    let mut vars: Vec<&str> = plan.iter().map(|f| f.afv_var.as_str()).collect();
    vars.sort_unstable();
    vars.dedup();
    changes.extend(vars.iter().map(|v| (v.to_string(), 0, crate::burn::AFV_NEUTRAL)));
    let mut label: Vec<String> = plan.iter().map(|f| format!("{} {:.1}%", f.afv_var, f.afv)).collect();
    label.dedup();
    tune.set(format!("fold AFV into fuel maps ({})", label.join(", ")), &changes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::Sample;
    use std::path::Path;
    use xbtune_defs::Catalog;

    #[test]
    fn afv_folds_into_both_maps_on_ddfi2() {
        let c = Catalog::builtin().unwrap();
        let img =
            crate::image::Image::from_raw(c.lookup("BUEIB").unwrap(), xbtune_sim::BUEIB_EEPROM).unwrap();
        let mut t = Tune::new(img, &c).unwrap();
        t.set("afv", &[("LFuel".into(), 0, 100.0)]).unwrap();
        let plan = afv_fold_plan(&t).unwrap();
        assert!(fold_afv(&mut t, &plan).unwrap_err().contains("already 100%"));
        t.set("afv", &[("LFuel".into(), 0, 95.0)]).unwrap();
        let plan = afv_fold_plan(&t).unwrap();
        let maps: Vec<_> = plan.iter().map(|f| (f.map.as_str(), f.afv_var.as_str(), f.afv)).collect();
        assert_eq!(maps, [("Tab_Fuel_Front", "LFuel", 95.0), ("Tab_Fuel_Rear", "LFuel", 95.0)]);
        let before: Vec<_> = plan.iter().map(|f| t.elements(&f.map).unwrap()).collect();
        let applied = fold_afv(&mut t, &plan).unwrap();
        assert!(applied.changed > 0 && applied.clamped == 0, "{applied:?}");
        assert_eq!(t.elements("LFuel").unwrap(), [100.0]);
        for (f, old) in plan.iter().zip(&before) {
            let v = t.var(&f.map).unwrap().clone();
            let new = t.elements(&f.map).unwrap();
            for (i, (o, n)) in old.iter().zip(&new).enumerate() {
                // Stored at the map's resolution.
                let step = crate::tune::element_value(&v, i, 1) - crate::tune::element_value(&v, i, 0);
                assert!((o * 0.95 - n).abs() <= step / 2.0 + 1e-9, "{} [{i}]: {o} -> {n}", f.map);
            }
        }
        assert_eq!(t.undo_label(), Some("fold AFV into fuel maps (LFuel 95.0%)"));
        assert!(!has_autotune_step(&t));
        let p = Proposal {
            map: "Tab_Fuel_Front".into(),
            index: 0,
            row: 0,
            col: 0,
            old: 0.0,
            new: 0.0,
            ego: 100.0,
            hits: 0.0,
            limited: false,
        };
        apply(&mut t, &[p]).unwrap();
        assert!(has_autotune_step(&t));
        t.undo();
        t.undo();
        assert_eq!(t.elements("LFuel").unwrap(), [95.0]);
        assert_eq!(t.elements("Tab_Fuel_Front").unwrap(), before[0]);
    }

    #[test]
    fn front_afv_used_only_with_independent_front_loop() {
        let c = Catalog::builtin().unwrap();
        let ecm = c.lookup("BUE2D").unwrap();
        let mut img =
            crate::image::Image::from_raw(ecm, include_bytes!("../../../tests/fixtures/BUE2D.eeprom"))
                .unwrap();
        img.page_zero = true; // LFuel1 lives in page 0
        let mut t = Tune::new(img, &c).unwrap();
        t.set("afv", &[("LFuel".into(), 0, 97.0), ("LFuel1".into(), 0, 104.0)]).unwrap();
        let Ok(xbtune_defs::Value::Bits(cfg)) = t.value("KCL_Fuel_Config") else { panic!() };
        let vars = |t: &Tune| afv_fold_plan(t).unwrap().into_iter().map(|f| f.afv_var).collect::<Vec<_>>();
        t.set_bits("KCL_Fuel_Config", cfg | 1).unwrap();
        assert_eq!(vars(&t), ["LFuel1", "LFuel"]);
        t.set_bits("KCL_Fuel_Config", cfg & !1).unwrap();
        assert_eq!(vars(&t), ["LFuel", "LFuel"]);
        // Folding with the shared AFV leaves LFuel1 alone.
        let plan = afv_fold_plan(&t).unwrap();
        fold_afv(&mut t, &plan).unwrap();
        assert_eq!((t.elements("LFuel").unwrap()[0], t.elements("LFuel1").unwrap()[0]), (100.0, 104.0));
    }

    #[test]
    fn weights_add_up_and_follow_the_point() {
        let w = cell_weights(1.25, 2.5, 12, 13);
        let total: f64 = w.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-12);
        assert_eq!(w.len(), 4);
        let at = |rc| w.iter().find(|(p, _)| *p == rc).map(|(_, w)| *w).unwrap();
        assert!((at((1, 2)) - 0.75 * 0.5).abs() < 1e-12);
        assert!((at((2, 3)) - 0.25 * 0.5).abs() < 1e-12);
        // On a breakpoint: one cell.
        assert_eq!(cell_weights(3.0, 4.0, 12, 13), vec![((3, 4), 1.0)]);
        // Last row/column: no cell past the edge.
        assert_eq!(cell_weights(11.0, 12.0, 12, 13), vec![((11, 12), 1.0)]);
        // Descending axis (RPM 8000..800) and clamping outside it.
        let rpm = [8000.0, 7000.0, 1000.0, 800.0];
        assert_eq!(frac_index(&rpm, 900.0), Some(2.5));
        assert_eq!(frac_index(&rpm, 500.0), Some(3.0));
        assert_eq!(frac_index(&rpm, 9000.0), Some(0.0));
    }

    fn owner() -> Option<(Catalog, Tune, Vec<Log>)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner");
        let img = dir.join("BUEIB_xbtune.xpr");
        if !img.exists() {
            eprintln!("skipping: owner fixtures not present");
            return None;
        }
        let catalog = Catalog::builtin().unwrap();
        let tune = Tune::open(&img, &catalog).unwrap();
        let logs = ["idle.csv", "BUEIB_20261014-231006.csv"]
            .iter()
            .map(|n| Log::open(&dir.join(n), &catalog).unwrap())
            .collect();
        Some((catalog, tune, logs))
    }

    /// The ECM's own table value (veCurr1) is the map interpolated at the
    /// sample's (RPM, Load): proof that samples land in the right cells.
    /// (Measured 2026-10-15: 70% within 0.15, median error 0.03. The rest
    /// is the ECM's finer internal load and RPM moving between readings.)
    #[test]
    fn owner_logs_land_where_the_ecm_reads_the_map() {
        let Some((_, tune, logs)) = owner() else { return };
        let v = tune.var("Tab_Fuel_Front").unwrap().clone();
        let el = tune.elements("Tab_Fuel_Front").unwrap();
        let xs = tune.elements(v.x_axis.as_ref().unwrap()).unwrap();
        let ys = tune.elements(v.y_axis.as_ref().unwrap()).unwrap();
        let (rows, cols) = grid_of(&v).unwrap();
        let mut errors = Vec::new();
        for log in &logs {
            let (rpm, load, ve) = (log.series("RPM"), log.series("Load"), log.series("veCurr1"));
            for i in 0..log.samples.len() {
                let (Some(x), Some(y), Some(ve)) = (rpm[i], load[i], ve[i]) else { continue };
                let (c, r) = (frac_index(&xs, x).unwrap(), frac_index(&ys, y).unwrap());
                let ours: f64 =
                    cell_weights(r, c, rows, cols).iter().map(|((r, c), w)| w * el[r * cols + c]).sum();
                errors.push((ours - ve).abs());
            }
        }
        let exact = errors.iter().filter(|e| **e <= 0.15).count() as f64 / errors.len() as f64;
        errors.sort_by(f64::total_cmp);
        let median = errors[errors.len() / 2];
        assert!(exact > 0.6 && median < 0.2, "exact {exact:.2}, median {median:.2}");
    }

    #[test]
    fn owner_idle_logs_give_idle_cells_and_sane_proposals() {
        let Some((_, tune, logs)) = owner() else { return };
        let a = analyze(&logs, &tune, "Tab_Fuel_Front", &Filters::default()).unwrap();
        assert_eq!(a.samples, logs.iter().map(|l| l.samples.len()).sum::<usize>());
        assert_eq!(a.used + a.rejected.total(), a.samples);
        // The 64 s warm-up hold in the ride log is open loop or cold.
        assert!(a.rejected.open_loop + a.rejected.cold > 250, "{:?}", a.rejected);
        assert!(a.used > 300, "{a:?}");
        // Idle: Load ~20, RPM 1000-1350: the busiest cells are there.
        let ys = tune.elements("Tab_Fuel_Load_Ax").unwrap_or_default();
        let (mut best, mut at) = (0.0, (0, 0));
        for (r, row) in a.cells.iter().enumerate() {
            for (c, s) in row.iter().enumerate() {
                if s.hits > best {
                    best = s.hits;
                    at = (r, c);
                }
            }
        }
        if !ys.is_empty() {
            assert!((15.0..=30.0).contains(&ys[at.0]), "busiest row {} (load {})", at.0, ys[at.0]);
        }
        let s = Settings::default();
        let p = propose(&a, &tune, &s).unwrap();
        assert!(!p.is_empty());
        for x in &p {
            assert!(x.hits >= s.min_hits);
            assert!((x.new / x.old - 1.0).abs() <= s.max_change + 0.02, "{x:?}");
            assert!((80.0..=110.0).contains(&x.ego), "{x:?}");
        }
    }

    /// Made-up log: steady in one cell with EGO at 90%: that cell is
    /// proposed 10% leaner; a rich cell is capped at max_change.
    #[test]
    fn steady_lean_correction_and_cap() {
        let catalog = Catalog::builtin().unwrap();
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner");
        let img = dir.join("BUEIB_xbtune.xpr");
        if !img.exists() {
            return;
        }
        let tune = Tune::open(&img, &catalog).unwrap();
        let ecm = tune.ecm.clone();
        let def = tune.def.clone();
        let frame0 = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin").to_vec();
        let chans = crate::live::channels(&def);
        // Write a value into a frame (test only): raw = (v - translate) / scale, LE.
        let set = |frame: &mut Vec<u8>, name: &str, value: f64| {
            let v = chans.iter().find(|v| v.name == name).unwrap();
            let raw = ((value - v.translate) / v.scale).round() as u32;
            for k in 0..v.size {
                frame[v.offset + k] = (raw >> (8 * k)) as u8;
            }
            assert!((v.value(frame).unwrap() - value).abs() <= v.scale.abs(), "{name}");
        };
        let mut samples = Vec::new();
        for i in 0..200 {
            let mut f = frame0.clone();
            set(&mut f, "RPM", 2900.0);
            set(&mut f, "Load", 30.0);
            set(&mut f, "TPD", 10.0);
            set(&mut f, "CLT", 150.0);
            set(&mut f, "EGO Corr.", if i % 2 == 0 { 89.8 } else { 90.2 });
            set(&mut f, "Accel Corr.", 0.0);
            set(&mut f, "Decel Corr.", 100.0);
            set(&mut f, "WOT Corr.", 100.0);
            samples.push(Sample { t: i as f64 * 0.15, wall: std::time::UNIX_EPOCH, frame: f });
        }
        let log = Log {
            path: "made-up.csv".into(),
            ecm,
            version: "BUEIB310 12-11-03".into(),
            channels: chans.clone(),
            definition: def,
            started: None,
            note: None,
            samples,
            events: Vec::new(),
            skipped: 0,
        };
        let a = analyze(std::slice::from_ref(&log), &tune, "Tab_Fuel_Front", &Filters::default()).unwrap();
        assert_eq!(a.used, 199, "{:?}", a.rejected);
        let s = Settings { max_change: 0.05, ..Default::default() };
        let p = propose(&a, &tune, &s).unwrap();
        assert_eq!(p.len(), 1, "{p:?}");
        let x = &p[0];
        assert!((x.ego - 90.0).abs() < 0.01);
        assert!(x.limited);
        assert!((x.new - x.old * 0.95).abs() <= 0.6, "{x:?}");
        let s = Settings { max_change: 0.2, ..Default::default() };
        let x = &propose(&a, &tune, &s).unwrap()[0];
        assert!(!x.limited && (x.new - x.old * 0.90).abs() <= 0.6, "{x:?}");
        // Applying is one undoable step.
        let mut t = tune.clone();
        assert_eq!(apply(&mut t, std::slice::from_ref(x)).unwrap(), 1);
        assert_eq!(t.elements("Tab_Fuel_Front").unwrap()[x.index], x.new);
        t.undo();
        assert_eq!(t.elements("Tab_Fuel_Front").unwrap()[x.index], x.old);
    }
}
