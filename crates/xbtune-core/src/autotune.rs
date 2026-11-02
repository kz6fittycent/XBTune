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
//! - Closed loop is `Flags2` bit 7 (EcmDroid `Bin2MslConverter`: set =
//!   closed loop, EGO; clear = open loop, AFV). Logs without `Flags2`
//!   (.msl from other tools) fall back to "EGO changed in the last few
//!   samples", which misses closed loop where EGO sits at its limit.
//!   EGO starts from AFV when closed loop begins, so the first
//!   [`Filters::cl_settle`] seconds are left out.
//! - Fuel (fitted to injector pulse widths in EcmDroid's BUEIB and BUE2D
//!   test logs, see PROVENANCE "Auto-tune"): closed loop = table × EGO,
//!   open loop = table × OL × AFV; AFV is NOT applied on top of EGO and EGO
//!   not in open loop. So a cell's average EGO is its whole correction,
//!   whatever AFV is, and AFV only moves the open-loop fuel. Resetting AFV
//!   after burning corrections would lean out the open-loop areas: fold
//!   AFV into the maps first ([`fold_afv`]), then log and auto-tune.
//! - Warm-up enrichment (WUE) is applied in closed loop too, and cell
//!   averages taken during warm-up differ from the warm ones (neither
//!   way consistently, and EGO x WUE doesn't line them up), so samples
//!   with WUE above [`Filters::max_wue`] are left out. The ECM enters
//!   closed loop well below the tune's `KLCL_Min_TE` (33-59 °C in those
//!   logs), so that isn't a closed-loop threshold.
//! - DDFI-3 with its own front loop logs `EGO1 Corr.`, `Load1` and `AFV1`
//!   for the front cylinder ([`Cylinder`]); one sensor (DDFI-2) corrects
//!   both maps with the rear values.
//!
//! Filters (all adjustable): warm engine past warm-up enrichment, closed
//! loop, steady RPM and throttle, no acceleration/deceleration/WOT
//! enrichment, no gap before the sample. A cell needs a minimum weight of
//! samples, and each pass changes a cell by at most `max_change`.
//!
//! With a wideband ([`Mode::Wideband`], logs with `crate::wideband`
//! readings) the correction is `measured λ / target λ`, times EGO in
//! closed loop: the fuel the cell needs to reach the target AFR with EGO
//! back at 100% (open loop: with AFV as it is). Open-loop and WOT samples
//! count too (that's where a wideband helps); the reading is taken `lag`
//! seconds after the sample, for the time the exhaust takes to reach the
//! sensor. The default lag and targets ([`TargetAfr`]) are XBTune's
//! starting points, not Buell figures (no wideband log from a real bike
//! yet).

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

/// How many of `axis`'s breakpoints are real: trailing repeats of the
/// last value are padding (BUE2D's RPM axis ends `800, 0, 0, 0`; its
/// cells there are 0). A single trailing 0 is kept: on EcmDroid's BUEIB
/// image the 0-rpm column holds real fuel values.
pub fn used_len(axis: &[f64]) -> usize {
    let mut n = axis.len();
    while n >= 2 && axis[n - 1] == axis[n - 2] {
        n -= 1;
    }
    n
}

/// Fractional position of `x` in breakpoints `axis` (either direction);
/// outside the axis it sits on the nearer end. Padding past
/// [`used_len`] is never used.
pub fn frac_index(axis: &[f64], x: f64) -> Option<f64> {
    let axis = &axis[..used_len(axis)];
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
    /// Warm-up enrichment (WUE channel, %) at most this. WUE follows
    /// engine temperature (BUEIB, EcmDroid's ride and the owner's: ~102%
    /// at 120 °C, 101% at 140, 100.3% at 160); 102 keeps a normally warm
    /// ride and drops the warm-up itself.
    pub max_wue: f64,
    /// Closed loop without the ECM's flag in the log: EGO changed within
    /// this many samples before.
    pub ego_window: usize,
    /// Seconds left out after closed loop starts (EGO settles from AFV).
    pub cl_settle: f64,
    /// Steady state: RPM changing no faster than this (RPM/s).
    pub max_rpm_rate: f64,
    /// Steady state: throttle changing no faster than this (°/s).
    pub max_tpd_rate: f64,
    /// A sample more than this after the previous one is skipped (s).
    pub max_gap: f64,
    pub mode: Mode,
}

impl Default for Filters {
    fn default() -> Self {
        Self {
            min_clt: 80.0,
            max_wue: 102.0,
            ego_window: 3,
            cl_settle: 1.0,
            max_rpm_rate: 800.0,
            max_tpd_rate: 10.0,
            max_gap: 1.0,
            mode: Mode::Narrowband,
        }
    }
}

/// What the correction is based on.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Mode {
    /// The ECM's EGO correction in closed loop (stock O2 sensor).
    #[default]
    Narrowband,
    /// A wideband's measured lambda against a target AFR.
    Wideband {
        target: TargetAfr,
        /// Seconds from a sample to the wideband reading that matches it.
        lag: f64,
    },
}

impl Mode {
    /// Wideband with the default targets and lag.
    pub fn wideband() -> Self {
        Self::Wideband { target: TargetAfr::default(), lag: DEFAULT_LAG }
    }
}

/// Default exhaust-to-sensor delay (XBTune's starting point).
pub const DEFAULT_LAG: f64 = 0.2;

/// Measured AFR above this at load >= [`LEAN_LOAD`] is flagged.
pub const LEAN_AFR: f64 = 14.0;

/// Petrol's stoichiometric AFR: lambda 1.00.
pub const STOICH: f64 = 14.7;
pub const LEAN_LOAD: f64 = 80.0;

/// Target AFR by load (the fuel map's load axis), straight lines between
/// points and flat beyond the ends.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetAfr {
    /// (load, AFR), sorted by load.
    pub points: Vec<(f64, f64)>,
}

impl Default for TargetAfr {
    /// XBTune's starting point, to be adjusted: stoichiometric where the
    /// stock ECM runs closed loop (light load), richer as load rises.
    fn default() -> Self {
        Self { points: vec![(40.0, 14.7), (60.0, 14.0), (80.0, 13.5), (100.0, 13.2), (125.0, 13.0)] }
    }
}

impl TargetAfr {
    pub fn at(&self, load: f64) -> f64 {
        let p = &self.points;
        match (p.first(), p.last()) {
            (Some(&(l0, a0)), _) if load <= l0 => a0,
            (_, Some(&(l1, a1))) if load >= l1 => a1,
            _ => p
                .windows(2)
                .find(|w| load <= w[1].0)
                .map(|w| w[0].1 + (w[1].1 - w[0].1) * (load - w[0].0) / (w[1].0 - w[0].0))
                .unwrap_or(14.7),
        }
    }

    /// "40:14.7,60:14.0,..." (the CLI's --target form).
    pub fn text(&self) -> String {
        // Two decimals at most: lambda targets come back as long fractions.
        let short = |x: f64| format!("{x:.2}").trim_end_matches('0').trim_end_matches('.').to_string();
        self.points.iter().map(|(l, a)| format!("{}:{}", short(*l), short(*a))).collect::<Vec<_>>().join(",")
    }

    /// The same in lambda: "40:1.00,60:0.95,...".
    pub fn lambda_text(&self) -> String {
        self.points.iter().map(|(l, a)| format!("{l}:{:.2}", a / STOICH)).collect::<Vec<_>>().join(",")
    }

    /// load:target pairs, the targets all AFR (9-17) or all lambda
    /// (0.6-1.2, stored as AFR at petrol's 14.7).
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut points = Vec::new();
        for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (l, a) =
                part.split_once(':').ok_or_else(|| format!("'{part}': say load:AFR or load:lambda"))?;
            let (l, a): (f64, f64) = (
                l.trim().parse().map_err(|_| format!("'{l}' isn't a load"))?,
                a.trim().parse().map_err(|_| format!("'{a}' isn't an AFR or lambda"))?,
            );
            points.push((l, a));
        }
        let lambda = points.iter().all(|&(_, a)| (0.6..=1.2).contains(&a));
        for (_, a) in &mut points {
            if lambda {
                *a *= STOICH;
            } else if !(9.0..=17.0).contains(a) {
                return Err(format!("target {a} is neither an AFR (9-17) nor a lambda (0.60-1.20)"));
            }
        }
        if points.is_empty() {
            return Err("no targets given".into());
        }
        points.sort_by(|x, y| x.0.total_cmp(&y.0));
        Ok(Self { points })
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
    /// Warm-up enrichment still on.
    pub warm_up: usize,
    /// Open loop, or closed loop that has only just started.
    pub open_loop: usize,
    pub transient: usize,
    pub enrichment: usize,
    pub gap: usize,
    /// Wideband mode: no valid reading at the sample (+ lag).
    pub no_wideband: usize,
}

impl Rejected {
    pub fn total(&self) -> usize {
        self.missing
            + self.cold
            + self.warm_up
            + self.open_loop
            + self.transient
            + self.enrichment
            + self.gap
            + self.no_wideband
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CellStat {
    /// Sum of sample weights in this cell.
    pub hits: f64,
    /// Weighted average correction (%), meaningful when `hits > 0`: the
    /// EGO correction, or with a wideband EGO x measured / target lambda.
    pub ego: f64,
    /// Wideband: weighted average measured AFR and target AFR (else 0).
    pub afr: f64,
    pub target: f64,
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
    /// AFV from the logs (last value seen) for this map's cylinder, if
    /// logged.
    pub afv: Option<f64>,
    /// Every log had the ECM's closed-loop flag (else EGO movement was
    /// used for at least one).
    pub cl_flag: bool,
    /// The correction channel used (last log): "EGO Corr." or, for a
    /// DDFI-3 front map, "EGO1 Corr.".
    pub ego_channel: &'static str,
    pub wideband: bool,
    /// Wideband: cells at load >= `LEAN_LOAD` measured leaner than
    /// `LEAN_AFR` on average, as (row, col).
    pub lean: Vec<(usize, usize)>,
    /// Channels every sample needs that a log doesn't have at all (logs
    /// from other tools may lack them): then nothing from that log counts.
    pub missing_channels: Vec<String>,
}

fn grid_of(v: &EeVar) -> Result<(usize, usize), String> {
    match v.shape {
        Shape::Grid { rows, cols, .. } => Ok((rows, cols)),
        _ => Err(format!("{} isn't a map", v.name)),
    }
}

/// The log channels one fuel map is corrected from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cylinder {
    pub ego: &'static str,
    pub load: &'static str,
    pub afv: &'static str,
}

impl Cylinder {
    pub const REAR: Self = Self { ego: "EGO Corr.", load: "Load", afv: "AFV" };

    /// The channels for `map` in `log`. A DDFI-3 front map is read at the
    /// front load (`Load1`, ED-DB "Load for the front cylinder") where
    /// logged; its correction is the front one (`EGO1 Corr.`, `AFV1`) only
    /// when that moves by itself. With the front loop off, or one sensor,
    /// the rear correction applies to both cylinders.
    pub fn for_map(log: &Log, map: &str) -> Self {
        if !map.contains("Front") {
            return Self::REAR;
        }
        let load = if log.channel("Load1").is_some() { "Load1" } else { "Load" };
        if own_front_loop(log) {
            let afv = if log.channel("AFV1").is_some() { "AFV1" } else { "AFV" };
            Self { ego: "EGO1 Corr.", load, afv }
        } else {
            Self { load, ..Self::REAR }
        }
    }

    /// The live channel for an axis channel name ("Load" -> this load).
    fn axis(&self, ch: &'static str) -> &'static str {
        if ch == "Load" { self.load } else { ch }
    }
}

/// The log has a front correction that isn't constant and isn't a copy of
/// the rear one (on at least 5% of the samples that have both).
fn own_front_loop(log: &Log) -> bool {
    if log.channel("EGO1 Corr.").is_none() {
        return false;
    }
    let (front, rear) = (log.series("EGO1 Corr."), log.series("EGO Corr."));
    let both: Vec<(f64, Option<f64>)> =
        front.iter().zip(&rear).filter_map(|(f, r)| Some(((*f)?, *r))).collect();
    let Some(&(first, _)) = both.first() else { return false };
    let moves = both.iter().any(|&(f, _)| f != first);
    let differs = both.iter().filter(|&&(f, r)| r != Some(f)).count();
    moves && differs * 20 >= both.len()
}

/// Closed loop per sample from the ECM's flag (`Flags2` bit 7), if the log
/// has it.
fn closed_loop_flags(log: &Log) -> Option<Vec<Option<bool>>> {
    log.channel("Flags2")?;
    Some(log.series("Flags2").into_iter().map(|v| v.map(|b| (b as u32) & 0x80 != 0)).collect())
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
    // Per cell: weight, weighted correction, weighted AFR, weighted target.
    let mut sum = vec![vec![[0.0f64; 4]; cols]; rows];
    let wb = match &f.mode {
        Mode::Narrowband => None,
        Mode::Wideband { target, lag } => Some((target, *lag)),
    };
    let mut a = Analysis {
        map: map.into(),
        rows,
        cols,
        cells: Vec::new(),
        samples: 0,
        used: 0,
        rejected: Rejected::default(),
        afv: None,
        cl_flag: true,
        ego_channel: Cylinder::REAR.ego,
        wideband: wb.is_some(),
        lean: Vec::new(),
        missing_channels: Vec::new(),
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
        let cyl = Cylinder::for_map(log, map);
        let (xch, ych) = (cyl.axis(xch), cyl.axis(ych));
        a.ego_channel = cyl.ego;
        for name in [xch, ych, cyl.ego] {
            if log.channel(name).is_none() && !a.missing_channels.iter().any(|m| m == name) {
                a.missing_channels.push(name.to_string());
            }
        }
        let series = |n: &str| log.series(n);
        let (x, y, ego, clt, wue, tpd) =
            (series(xch), series(ych), series(cyl.ego), series("CLT"), series("WUE"), series("TPD"));
        let (rpm, accel, decel, wot) =
            (series("RPM"), series("Accel Corr."), series("Decel Corr."), series("WOT Corr."));
        if let Some(Some(v)) = series(cyl.afv).last() {
            a.afv = Some(*v);
        }
        let flags = closed_loop_flags(log);
        a.cl_flag &= flags.is_some();
        if wb.is_some() && log.wideband.is_none() {
            return Err(format!("{} has no wideband readings", log.name()));
        }
        // The load each sample sits at (for the target AFR).
        let load = series(cyl.load);
        let lag = wb.map_or(0.0, |(_, l)| l);
        let mut later = 0;
        // When the current stretch of closed loop began.
        let mut cl_since: Option<f64> = None;
        for i in 0..log.samples.len() {
            a.samples += 1;
            let t = log.samples[i].t;
            let closed = match &flags {
                Some(fl) => fl[i] == Some(true),
                None => {
                    let lo = i.saturating_sub(f.ego_window);
                    ego[i].is_some_and(|e| (lo..i).any(|j| ego[j].is_some_and(|p| p != e)))
                }
            };
            cl_since = if closed { cl_since.or(Some(t)) } else { None };
            let rej = &mut a.rejected;
            let (Some(xv), Some(yv), Some(e)) = (x[i], y[i], ego[i]) else {
                rej.missing += 1;
                continue;
            };
            if i == 0 {
                rej.gap += 1;
                continue;
            }
            let dt = t - log.samples[i - 1].t;
            if dt <= 0.0 || dt > f.max_gap {
                rej.gap += 1;
                continue;
            }
            if clt[i].is_none_or(|c| c < f.min_clt) {
                rej.cold += 1;
                continue;
            }
            if wue[i].is_some_and(|w| w > f.max_wue) {
                rej.warm_up += 1;
                continue;
            }
            let settled = cl_since.is_some_and(|s| t - s >= f.cl_settle);
            if wb.is_none() && !settled {
                rej.open_loop += 1;
                continue;
            }
            // Wideband: just after closed loop starts, EGO is still
            // moving from AFV; the fuel is right but the sample isn't
            // steady, so it's left out too.
            if wb.is_some() && closed && !settled {
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
            // WOT enrichment stays in the fuel after a change, so with a
            // wideband those samples count.
            if accel[i].is_some_and(|v| v > 0.0)
                || decel[i].is_some_and(|v| v != 100.0)
                || (wb.is_none() && wot[i].is_some_and(|v| v != 100.0))
            {
                rej.enrichment += 1;
                continue;
            }
            let (correction, afr, target) = match wb {
                None => (e, 0.0, 0.0),
                Some((targets, _)) => {
                    // The first reading at or after t + lag, if close enough.
                    let want = log.samples[i].t + lag;
                    later = later.max(i);
                    while later < log.samples.len() && log.samples[later].t < want {
                        later += 1;
                    }
                    let reading = log
                        .samples
                        .get(later)
                        .filter(|s| s.t - want <= crate::wideband::MAX_AGE.as_secs_f64())
                        .and_then(|s| s.wb)
                        .filter(|w| w.status == crate::wideband::Status::Normal);
                    let (Some(w), Some(l), Some(ld)) = (reading, reading.and_then(|w| w.lambda), load[i])
                    else {
                        rej.no_wideband += 1;
                        continue;
                    };
                    if !(0.6..=1.5).contains(&l) {
                        rej.no_wideband += 1;
                        continue;
                    }
                    let target = targets.at(ld);
                    // EGO is in the fuel in closed loop only.
                    let ego = if closed { e } else { 100.0 };
                    (ego * l / (target / w.stoich), l * w.stoich, target)
                }
            };
            let (Some(c), Some(r)) = (frac_index(&xs, xv), frac_index(&ys, yv)) else {
                rej.missing += 1;
                continue;
            };
            for ((rr, cc), w) in cell_weights(r, c, rows, cols) {
                let s = &mut sum[rr][cc];
                s[0] += w;
                s[1] += w * correction;
                s[2] += w * afr;
                s[3] += w * target;
            }
            a.used += 1;
        }
    }
    a.cells = sum
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|[h, e, afr, t]| {
                    let avg = |x: f64| if h > 0.0 { x / h } else { 0.0 };
                    CellStat { hits: h, ego: avg(e), afr: avg(afr), target: avg(t) }
                })
                .collect()
        })
        .collect();
    if a.wideband {
        for (r, row) in a.cells.iter().enumerate() {
            for (c, st) in row.iter().enumerate() {
                if st.hits >= 1.0 && ys.get(r).is_some_and(|&l| l >= LEAN_LOAD) && st.afr > LEAN_AFR {
                    a.lean.push((r, c));
                }
            }
        }
    }
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
    /// Wideband: average measured AFR and the target there.
    pub afr: Option<(f64, f64)>,
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
                afr: a.wideband.then_some((st.afr, st.target)),
            });
        }
    }
    Ok(out)
}

/// Start of the undo label of an applied auto-tune.
pub const AUTOTUNE_STEP: &str = "auto-tune: ";

/// Whether the tune holds auto-tune corrections that haven't been undone.
/// Those cells already hold their whole closed-loop correction, so folding
/// AFV in afterwards would move them by AFV again: fold first, then
/// auto-tune from logs made after burning the folded tune.
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

/// (rows, cols) of `map` and how many of each have a real breakpoint
/// ([`used_len`]).
fn used_grid(tune: &Tune, map: &str) -> Result<(usize, usize, usize, usize), String> {
    let v = tune.var(map).map_err(|e| e.to_string())?;
    let (rows, cols) = grid_of(v)?;
    let used = |axis: Option<&String>, n: usize| {
        axis.and_then(|a| tune.elements(a).ok()).map_or(n, |el| used_len(&el).min(n))
    };
    Ok((rows, cols, used(v.y_axis.as_ref(), rows), used(v.x_axis.as_ref(), cols)))
}

/// What to tell the user about AFV after an analysis.
#[derive(Debug, Clone, PartialEq)]
pub enum AfvNote {
    /// The logs were made with a different AFV than the tune holds (it
    /// was folded or reset since): the open-loop fuel the logs saw isn't
    /// what this tune gives, and the corrections assume the logged state.
    Changed { logged: f64, tune: f64, var: String },
    /// AFV isn't 100%: the corrections are right as they are (EGO is the
    /// whole closed-loop correction); AFV still moves the open-loop fuel.
    /// Don't reset it after burning; fold it into the maps first.
    NotNeutral { afv: f64, var: String },
}

/// The AFV note for `a` on `tune`, if any.
pub fn afv_note(a: &Analysis, tune: &Tune) -> Option<AfvNote> {
    let fold = afv_fold_plan(tune).ok()?.into_iter().find(|f| f.map == a.map)?;
    let neutral = crate::burn::AFV_NEUTRAL;
    match a.afv {
        Some(logged) if (logged - fold.afv).abs() > 0.5 => {
            Some(AfvNote::Changed { logged, tune: fold.afv, var: fold.afv_var })
        }
        _ if (fold.afv - neutral).abs() > 0.05 => {
            Some(AfvNote::NotNeutral { afv: fold.afv, var: fold.afv_var })
        }
        _ => None,
    }
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
        let (rows, cols, used_rows, used_cols) = used_grid(tune, &f.map)?;
        for r in 0..rows {
            for c in 0..cols {
                // Padding past the real breakpoints stays as it is.
                if r < used_rows && c < used_cols {
                    let i = r * cols + c;
                    changes.push((f.map.clone(), i, cells[i] * factor));
                }
            }
        }
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
            afr: None,
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
        let logs = ["idle.csv", "BUEIB_20261014-231006.csv", "BUEIB_20261015-200748.csv"]
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
            set(&mut f, "Flags2", 128.0);
            set(&mut f, "WUE", 100.0);
            set(&mut f, "Accel Corr.", 0.0);
            set(&mut f, "Decel Corr.", 100.0);
            set(&mut f, "WOT Corr.", 100.0);
            samples.push(Sample { t: i as f64 * 0.15, wall: std::time::UNIX_EPOCH, frame: f, wb: None });
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
            source: Default::default(),
            wideband: None,
            unread_columns: Vec::new(),
        };
        let a = analyze(std::slice::from_ref(&log), &tune, "Tab_Fuel_Front", &Filters::default()).unwrap();
        // Sample 0 has no gap before it; closed loop counts from 1 s on.
        assert_eq!(a.used, 193, "{:?}", a.rejected);
        assert!(a.cl_flag && a.ego_channel == "EGO Corr.");
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

    #[test]
    fn target_afr_interpolates_and_parses() {
        let t = TargetAfr::default();
        assert_eq!(t.at(10.0), 14.7);
        assert!((t.at(50.0) - 14.35).abs() < 1e-9);
        assert_eq!(t.at(255.0), 13.0);
        assert_eq!(TargetAfr::parse(&t.text()).unwrap(), t);
        assert!(TargetAfr::parse("60:20").is_err());
        // Lambda targets, as some tuners think in lambda.
        let l = TargetAfr::parse("40:1.0, 80:0.9").unwrap();
        assert_eq!(l.points, vec![(40.0, 14.7), (80.0, 0.9 * 14.7)]);
        assert!((TargetAfr::parse(&t.lambda_text()).unwrap().at(60.0) - t.at(60.0)).abs() < 0.08);
        // Mixed units are refused rather than guessed.
        assert!(TargetAfr::parse("40:1.0,80:13.2").is_err());
        assert!(TargetAfr::parse("x").is_err());
    }

    #[test]
    fn wideband_corrects_toward_the_target_after_the_lag() {
        let catalog = Catalog::builtin().unwrap();
        let img = crate::image::Image::from_raw(catalog.lookup("BUEIB").unwrap(), xbtune_sim::BUEIB_EEPROM)
            .unwrap();
        let tune = Tune::new(img, &catalog).unwrap();
        let def = tune.def.clone();
        let frame0 = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin").to_vec();
        let chans = crate::live::channels(&def);
        let set = |frame: &mut Vec<u8>, name: &str, value: f64| {
            let v = chans.iter().find(|v| v.name == name).unwrap();
            let raw = ((value - v.translate) / v.scale).round() as u32;
            for k in 0..v.size {
                frame[v.offset + k] = (raw >> (8 * k)) as u8;
            }
        };
        // 2900 rpm: 40 s at load 30 (target 14.7), then 40 s at load 100
        // (target 13.2). Open loop: EGO holds at 100. The mixture is
        // lambda 1.05, then 0.85; the wideband shows it 0.3 s late.
        let mut samples = Vec::new();
        for i in 0..534 {
            let t = i as f64 * 0.15;
            let (load, lambda) = if t < 40.0 { (30.0, 1.05) } else { (100.0, 0.85) };
            let mut f = frame0.clone();
            for (n, v) in [
                ("RPM", 2900.0),
                ("Load", load),
                ("TPD", 10.0),
                ("CLT", 150.0),
                ("EGO Corr.", 100.0),
                ("WUE", 100.0),
                ("Accel Corr.", 0.0),
                ("Decel Corr.", 100.0),
                ("WOT Corr.", if load > 50.0 { 110.0 } else { 100.0 }),
            ] {
                set(&mut f, n, v);
            }
            let wb = crate::wideband::Sensor {
                status: crate::wideband::Status::Normal,
                lambda: Some(lambda),
                stoich: 14.7,
            };
            samples.push(Sample { t, wall: std::time::UNIX_EPOCH, frame: f, wb: Some(wb) });
        }
        // Each reading shows the mixture of 2 samples (0.3 s) earlier.
        let shift = 2;
        for i in (shift..samples.len()).rev() {
            samples[i].wb = samples[i - shift].wb;
        }
        let mut channels = chans.clone();
        channels.extend(crate::wideband::channels());
        let log = Log {
            path: "made-up.csv".into(),
            ecm: tune.ecm.clone(),
            version: "BUEIB310 12-11-03".into(),
            channels,
            definition: def,
            started: None,
            note: None,
            samples,
            events: Vec::new(),
            skipped: 0,
            source: Default::default(),
            wideband: Some("test".into()),
            unread_columns: Vec::new(),
        };
        let f =
            Filters { mode: Mode::Wideband { target: TargetAfr::default(), lag: 0.3 }, ..Default::default() };
        let a = analyze(std::slice::from_ref(&log), &tune, "Tab_Fuel_Front", &f).unwrap();
        assert!(a.wideband && a.used > 500, "{:?}", a.rejected);
        let s = Settings { max_change: 0.2, ..Default::default() };
        let p = propose(&a, &tune, &s).unwrap();
        let lean =
            p.iter().find(|x| x.afr.is_some_and(|(_, t)| (t - 14.7).abs() < 1e-6)).expect("load 30 cell");
        // Lean at light load: 5% more fuel.
        assert!((lean.ego - 105.0).abs() < 0.01, "{lean:?}");
        let (afr, _) = lean.afr.unwrap();
        assert!((afr - 1.05 * 14.7).abs() < 0.01);
        let rich =
            p.iter().find(|x| x.afr.is_some_and(|(_, t)| (t - 13.2).abs() < 1e-6)).expect("load 100 cell");
        // 0.85 lambda = 12.5 AFR against 13.2: less fuel; the lag hid the
        // old mixture, so the average is exact.
        assert!((rich.ego - 100.0 * 0.85 / (13.2 / 14.7)).abs() < 0.01, "{rich:?}");
        assert!(a.lean.is_empty());
        // Narrowband logs can't be used in wideband mode.
        let mut nb = log.clone();
        nb.wideband = None;
        assert!(analyze(&[nb], &tune, "Tab_Fuel_Front", &f).is_err());
    }

    fn ed_log(name: &str, c: &Catalog) -> Log {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ecmdroid").join(name);
        Log::open(&p, c).unwrap()
    }

    fn ed_tune(id: &str, c: &Catalog) -> Tune {
        let bytes: &[u8] = match id {
            "BUEIB" => include_bytes!("../../../tests/fixtures/BUEIB.eeprom"),
            _ => include_bytes!("../../../tests/fixtures/BUE2D.eeprom"),
        };
        let mut img = crate::image::Image::from_raw(c.lookup(id).unwrap(), bytes).unwrap();
        img.page_zero = true;
        Tune::new(img, c).unwrap()
    }

    /// The fuel equation auto-tune relies on, from injector pulse widths in
    /// EcmDroid's BUE2D log (front AFV 95%): (pw - battery correction) /
    /// (table x IAT x WUE x fuel pressure x correction) is one constant
    /// when the correction is EGO alone in closed loop and OL x AFV in open
    /// loop. EGO x AFV in closed loop is about 5% off.
    #[test]
    fn closed_loop_fuel_is_ego_alone_and_afv_open_loop_only() {
        let c = Catalog::builtin().unwrap();
        let log = ed_log("BUE2D_log.bin", &c);
        let s = |n: &str| log.series(n);
        let (pw, bat, ve, iat, wue, fp) =
            (s("pw1"), s("Batt. Corr."), s("veCurr1"), s("IAT Corr."), s("WUE"), s("F. Pr. Corr."));
        let (ego, afv, ol, flags) =
            (s("EGO1 Corr."), s("AFV1"), s("OL Corr."), closed_loop_flags(&log).unwrap());
        let (rpm, acc, dec, wot) = (s("RPM"), s("Accel Corr."), s("Decel Corr."), s("WOT Corr."));
        let median = |mut v: Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let (mut cl, mut cl_afv, mut open) = (Vec::new(), Vec::new(), Vec::new());
        for i in 0..log.samples.len() {
            let all = [pw[i], bat[i], ve[i], iat[i], wue[i], fp[i], ego[i], afv[i], ol[i], rpm[i]];
            let Some(v) = all.iter().copied().collect::<Option<Vec<f64>>>() else { continue };
            let [pw, bat, ve, iat, wue, fp, ego, afv, ol, rpm] = v[..] else { unreachable!() };
            if rpm < 800.0 || acc[i] != Some(0.0) || dec[i] != Some(100.0) || wot[i] != Some(100.0) {
                continue;
            }
            let k = (pw - bat) / (ve * iat / 100.0 * wue / 100.0 * fp / 100.0);
            if flags[i] == Some(true) {
                cl.push(k / (ego / 100.0));
                cl_afv.push(k / (ego / 100.0 * afv / 100.0));
            } else {
                open.push(k / (ol / 100.0 * afv / 100.0));
            }
        }
        assert!(cl.len() > 1000 && open.len() > 500, "{} {}", cl.len(), open.len());
        let (cl, cl_afv, open) = (median(cl), median(cl_afv), median(open));
        assert!((cl / open - 1.0).abs() < 0.02, "closed {cl} open {open}");
        assert!((cl_afv / open - 1.0).abs() > 0.04, "closed with AFV {cl_afv} open {open}");
    }

    #[test]
    fn ddfi3_front_map_follows_the_front_cylinder() {
        let c = Catalog::builtin().unwrap();
        let d3 = ed_log("BUE2D_log.bin", &c);
        let front = Cylinder { ego: "EGO1 Corr.", load: "Load1", afv: "AFV1" };
        assert_eq!(Cylinder::for_map(&d3, "Tab_Fuel_Front"), front);
        assert_eq!(Cylinder::for_map(&d3, "Tab_Fuel_Rear"), Cylinder::REAR);
        // DDFI-2: one sensor, the rear values for both maps.
        let d2 = ed_log("BUEIB_log_2000.bin", &c);
        assert_eq!(Cylinder::for_map(&d2, "Tab_Fuel_Front"), Cylinder::REAR);
        // A front correction that only copies the rear one isn't its own.
        let mut copy = d3.clone();
        let (f, r) =
            (copy.channel("EGO1 Corr.").unwrap().clone(), copy.channel("EGO Corr.").unwrap().clone());
        for smp in &mut copy.samples {
            let rear = smp.frame[r.offset..r.offset + r.size].to_vec();
            smp.frame[f.offset..f.offset + f.size].copy_from_slice(&rear);
        }
        assert_eq!(Cylinder::for_map(&copy, "Tab_Fuel_Front"), Cylinder { load: "Load1", ..Cylinder::REAR });
        let t = ed_tune("BUE2D", &c);
        let a = analyze(std::slice::from_ref(&d3), &t, "Tab_Fuel_Front", &Filters::default()).unwrap();
        assert_eq!((a.ego_channel, a.afv), ("EGO1 Corr.", Some(95.0)));
    }

    /// EcmDroid's BUEIB ride: closed loop by the ECM's flag. Without
    /// Flags2 in the log, EGO movement stands in for it.
    #[test]
    fn closed_loop_comes_from_the_ecm_flag() {
        let c = Catalog::builtin().unwrap();
        let t = ed_tune("BUEIB", &c);
        let log = ed_log("BUEIB_log_2000.bin", &c);
        let f = Filters::default();
        let a = analyze(std::slice::from_ref(&log), &t, "Tab_Fuel_Front", &f).unwrap();
        assert!(a.cl_flag && a.used > 0, "{:?}", a.rejected);
        let mut no_flag = log.clone();
        no_flag.channels.retain(|v| v.name != "Flags2");
        let b = analyze(&[no_flag], &t, "Tab_Fuel_Front", &f).unwrap();
        assert!(!b.cl_flag);
        assert_ne!(a.used, b.used);
        // Every sample used with the flag is closed loop and past warm-up.
        let flags = closed_loop_flags(&log).unwrap();
        let wue = log.series("WUE");
        assert!(flags.contains(&Some(false)) && flags.contains(&Some(true)));
        assert!(wue.iter().flatten().any(|w| *w > f.max_wue) && a.rejected.warm_up > 0, "{:?}", a.rejected);
    }

    #[test]
    fn padding_breakpoints_are_left_alone() {
        assert_eq!(used_len(&[8000.0, 800.0, 0.0, 0.0, 0.0]), 3);
        assert_eq!(used_len(&[8000.0, 1000.0, 800.0, 0.0]), 4);
        assert_eq!(used_len(&[0.0]), 1);
        let rpm = [8000.0, 1000.0, 800.0, 0.0, 0.0, 0.0];
        assert_eq!(frac_index(&rpm, 400.0), Some(2.5));
        assert_eq!(frac_index(&rpm, 0.0), Some(3.0));
        let c = Catalog::builtin().unwrap();
        let mut t = ed_tune("BUE2D", &c);
        let (rows, cols, ur, uc) = used_grid(&t, "Tab_Fuel_Front").unwrap();
        assert_eq!((rows, cols, ur, uc), (16, 20, 16, 18));
        // A pad cell with a value in it keeps it through a fold.
        t.set(
            "pad",
            &[("Tab_Fuel_Front".into(), 19, 50.0), ("LFuel".into(), 0, 95.0), ("LFuel1".into(), 0, 95.0)],
        )
        .unwrap();
        let before = t.elements("Tab_Fuel_Front").unwrap();
        let plan = afv_fold_plan(&t).unwrap();
        fold_afv(&mut t, &plan).unwrap();
        let after = t.elements("Tab_Fuel_Front").unwrap();
        assert_eq!(after[19], 50.0);
        assert!(after[0] < before[0], "{} -> {}", before[0], after[0]);
    }

    #[test]
    fn afv_note_says_fold_first_or_log_again() {
        let c = Catalog::builtin().unwrap();
        let mut t = ed_tune("BUEIB", &c);
        let log = ed_log("BUEIB_log_2000.bin", &c);
        let f = Filters::default();
        let a = analyze(std::slice::from_ref(&log), &t, "Tab_Fuel_Front", &f).unwrap();
        assert_eq!(a.afv, Some(100.0));
        t.set("afv", &[("LFuel".into(), 0, 100.0)]).unwrap();
        assert_eq!(afv_note(&a, &t), None);
        t.set("afv", &[("LFuel".into(), 0, 103.0)]).unwrap();
        assert!(matches!(afv_note(&a, &t), Some(AfvNote::Changed { .. })));
        let mut a2 = a.clone();
        a2.afv = Some(103.0);
        assert_eq!(afv_note(&a2, &t), Some(AfvNote::NotNeutral { afv: 103.0, var: "LFuel".into() }));
    }
}
