// SPDX-License-Identifier: GPL-3.0-or-later
//! The fixed rules every AI-suggested change passes before the user sees
//! it as a proposal (docs/AI.md §3, stage A2). The model's quality decides
//! how useful its suggestions are; these rules decide what it may suggest.
//!
//! A suggestion names a map and a cell (by its load and rpm values, or row
//! and column) and a new value. It is refused, with the rule that refused
//! it, unless:
//! - the map is a fuel or spark map the tune can edit, and the cell exists;
//! - the value fits the map (no clamping) and actually changes the cell;
//! - fuel changes by at most `fuel_round` (10%) this round; spark by at most
//!   `spark_less` degrees less or `spark_more` degrees more advance;
//! - the cell stays within `drift` (15%) of the last ECM read (owner's cap,
//!   2026-10-19), so rounds of small changes can't add up;
//! - the cell has logged data (`min_hits`), unless the user allowed edits
//!   without data for this request (smoothing);
//! - fuel at load >= `lean_load` doesn't end up leaner than XBTune's own
//!   auto-tune would make it, and a cell the wideband shows lean only gets
//!   richer;
//! - spark advance doesn't increase at load >= `lean_load` or in a cell the
//!   wideband shows lean.
//!
//! Accepted changes carry flags (advance increased, disagrees with the
//! deterministic auto-tune) for the review table.

use std::path::Path;

use serde_json::Value;
use xbtune_core::autotune::{self, Proposal};
use xbtune_core::logfile::Log;
use xbtune_core::tune::{self, Tune};
use xbtune_defs::{Catalog, Shape};

/// One change as the model suggested it.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub map: String,
    pub load: Option<f64>,
    pub rpm: Option<f64>,
    pub row: Option<usize>,
    pub col: Option<usize>,
    pub value: f64,
    pub why: String,
}

impl Suggestion {
    fn describe(&self) -> String {
        let at = match (self.load, self.rpm, self.row, self.col) {
            (Some(l), Some(r), ..) => format!("load {l}, {r} rpm"),
            (_, _, Some(r), Some(c)) => format!("row {r}, column {c}"),
            _ => "an unnamed cell".into(),
        };
        format!("{} {at} -> {}", self.map, self.value)
    }
}

/// A change that passed, ready to review and apply.
#[derive(Debug, Clone, PartialEq)]
pub struct Accepted {
    pub map: String,
    pub row: usize,
    pub col: usize,
    /// Storage index for `Tune::set`.
    pub index: usize,
    pub load: Option<f64>,
    pub rpm: Option<f64>,
    pub old: f64,
    /// As the map will store it (rounded to its resolution).
    pub new: f64,
    pub why: String,
    /// Things to look at closely in the review.
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub what: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub fuel_round: f64,
    pub spark_less: f64,
    pub spark_more: f64,
    pub drift: f64,
    pub min_hits: f64,
    pub lean_load: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            fuel_round: 0.10,
            spark_less: 2.0,
            spark_more: 1.0,
            drift: 0.15,
            min_hits: autotune::Settings::default().min_hits,
            lean_load: autotune::LEAN_LOAD,
        }
    }
}

/// What the rules look at besides the suggestions.
pub struct Context<'a> {
    pub tune: &'a Tune,
    /// The last ECM read (drift is measured from it).
    pub baseline: &'a Tune,
    pub logs: &'a [Log],
    /// The user allowed changes to cells without logged data (smoothing).
    pub allow_no_data: bool,
}

pub fn is_advisable(name: &str) -> bool {
    name.starts_with("Tab_Fuel") || name.starts_with("Tab_Spark")
}

fn is_spark(name: &str) -> bool {
    name.starts_with("Tab_Spark")
}

/// The `changes` the model gave: the last ```json block (or bare JSON
/// object) holding a "changes" list. No block: no changes (a text-only
/// answer). A block that doesn't parse is an error the user sees.
pub fn parse_changes(answer: &str) -> Result<Vec<Suggestion>, String> {
    let mut candidates: Vec<&str> = Vec::new();
    let mut rest = answer;
    while let Some(i) = rest.find("```") {
        let after = &rest[i + 3..];
        let Some(end) = after.find("```") else { break };
        let block = after[..end].trim_start_matches(|c: char| c.is_ascii_alphabetic()).trim();
        candidates.push(block);
        rest = &after[end + 3..];
    }
    if candidates.is_empty()
        && let (Some(a), Some(b)) = (answer.find('{'), answer.rfind('}'))
        && a < b
    {
        candidates.push(&answer[a..=b]);
    }
    let Some(block) = candidates.into_iter().rev().find(|c| c.contains("\"changes\"")) else {
        return Ok(Vec::new());
    };
    let v: Value = serde_json::from_str(block).map_err(|e| format!("the changes aren't valid JSON ({e})"))?;
    let list = v.get("changes").and_then(Value::as_array).ok_or("\"changes\" isn't a list")?;
    let num = |c: &Value, k: &str| c.get(k).and_then(Value::as_f64);
    let idx = |c: &Value, k: &str| c.get(k).and_then(Value::as_u64).map(|x| x as usize);
    list.iter()
        .map(|c| {
            Ok(Suggestion {
                map: c
                    .get("map")
                    .and_then(Value::as_str)
                    .ok_or("a change has no \"map\"")?
                    .trim()
                    .to_string(),
                load: num(c, "load"),
                rpm: num(c, "rpm"),
                row: idx(c, "row"),
                col: idx(c, "col"),
                value: num(c, "value").ok_or("a change has no numeric \"value\"")?,
                why: c
                    .get("why")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .chars()
                    .take(400)
                    .collect(),
            })
        })
        .collect()
}

/// A map's axes: (load breakpoints by row, rpm breakpoints by column).
fn axes(tune: &Tune, map: &str) -> Option<(Vec<f64>, Vec<f64>)> {
    let v = tune.var(map).ok()?;
    Some((tune.elements(v.y_axis.as_ref()?).ok()?, tune.elements(v.x_axis.as_ref()?).ok()?))
}

fn find(axis: &[f64], x: f64) -> Option<usize> {
    axis.iter().position(|a| (a - x).abs() <= 0.5)
}

/// Logged sample weight per cell of `map` (warm engine, no gaps), for any
/// grid map whose axes follow the RPM and Load channels.
pub fn hits(logs: &[Log], tune: &Tune, map: &str) -> Option<Vec<Vec<f64>>> {
    let v = tune.var(map).ok()?;
    let Shape::Grid { rows, cols, .. } = v.shape else { return None };
    let (ys, xs) = axes(tune, map)?;
    let ch = |n: &Option<String>| -> Option<&'static str> {
        autotune::channel_for_axis(tune.var(n.as_ref()?).ok()?.label())
    };
    let (ych, xch) = (ch(&v.y_axis)?, ch(&v.x_axis)?);
    let min_clt = autotune::Filters::default().min_clt;
    let mut out = vec![vec![0.0; cols]; rows];
    for log in logs.iter().filter(|l| l.ecm.id == tune.ecm.id) {
        let (x, y, clt) = (log.series(xch), log.series(ych), log.series("CLT"));
        for i in 1..log.samples.len() {
            let dt = log.samples[i].t - log.samples[i - 1].t;
            let (Some(xv), Some(yv)) = (x[i], y[i]) else { continue };
            if dt <= 0.0 || dt > 1.0 || clt[i].is_none_or(|c| c < min_clt) {
                continue;
            }
            let (Some(c), Some(r)) = (autotune::frac_index(&xs, xv), autotune::frac_index(&ys, yv)) else {
                continue;
            };
            for ((rr, cc), w) in autotune::cell_weights(r, c, rows, cols) {
                out[rr][cc] += w;
            }
        }
    }
    Some(out)
}

/// Per map: logged hits per cell, the deterministic proposals, and the
/// cells the wideband shows lean.
type MapFacts = (Option<Vec<Vec<f64>>>, Vec<Proposal>, Vec<(usize, usize)>);

/// Check every suggestion; accepted ones keep their order.
pub fn check(suggestions: &[Suggestion], cx: &Context, lim: &Limits) -> (Vec<Accepted>, Vec<Refused>) {
    let mut ok: Vec<Accepted> = Vec::new();
    let mut refused = Vec::new();
    // Per map: hits, deterministic proposals, wideband-lean cells.
    let mut cache: std::collections::HashMap<String, MapFacts> = Default::default();
    for s in suggestions {
        let refuse =
            |reason: String, refused: &mut Vec<Refused>| refused.push(Refused { what: s.describe(), reason });
        let t = cx.tune;
        let Ok(var) = t.var(&s.map) else {
            refuse(format!("there's no map called {}", s.map), &mut refused);
            continue;
        };
        if !is_advisable(&s.map) {
            refuse("only fuel and spark maps may be changed".into(), &mut refused);
            continue;
        }
        if let Err(e) = t.editable(&s.map) {
            refuse(e.to_string(), &mut refused);
            continue;
        }
        let Shape::Grid { rows, cols, .. } = var.shape else {
            refuse("not a map".into(), &mut refused);
            continue;
        };
        let Some((ys, xs)) = axes(t, &s.map) else {
            refuse("the map has no axes".into(), &mut refused);
            continue;
        };
        let cell = match (s.load, s.rpm, s.row, s.col) {
            (Some(l), Some(r), ..) => find(&ys, l).zip(find(&xs, r)),
            (_, _, Some(r), Some(c)) if r < rows && c < cols => Some((r, c)),
            _ => None,
        };
        let Some((row, col)) = cell else {
            refuse(
                "that cell isn't on the map's axes (use the load and rpm values given)".into(),
                &mut refused,
            );
            continue;
        };
        if ok.iter().any(|a| a.map == s.map && a.row == row && a.col == col) {
            refuse("the same cell was changed twice; only the first counts".into(), &mut refused);
            continue;
        }
        let index = row * cols + col;
        let Ok(now) = t.elements(&s.map) else { continue };
        let old = now[index];
        if !s.value.is_finite() {
            refuse("not a number".into(), &mut refused);
            continue;
        }
        let (raw, clamped) = tune::encode(var, index, s.value);
        if clamped {
            refuse("outside what the map can store".into(), &mut refused);
            continue;
        }
        let new = tune::element_value(var, index, raw);
        if (new - old).abs() < 1e-9 {
            refuse("no change once rounded to the map's steps".into(), &mut refused);
            continue;
        }
        let step = (tune::element_value(var, index, raw.saturating_add(1)) - new).abs().max(1e-9);
        let base = cx.baseline.elements(&s.map).ok().and_then(|b| b.get(index).copied()).unwrap_or(old);
        let load = ys.get(row).copied();
        let rpm = xs.get(col).copied();
        let high_load = load.is_some_and(|l| l >= lim.lean_load);
        let (hit_grid, proposals, lean) = cache.entry(s.map.clone()).or_insert_with(|| {
            let h = hits(cx.logs, t, &s.map);
            let (props, lean) = if s.map.starts_with("Tab_Fuel") || is_spark(&s.map) {
                let fuel_map =
                    if is_spark(&s.map) { s.map.replace("Tab_Spark", "Tab_Fuel") } else { s.map.clone() };
                deterministic(cx.logs, t, &fuel_map, is_spark(&s.map))
            } else {
                (Vec::new(), Vec::new())
            };
            (h, props, lean)
        });
        let cell_hits = hit_grid.as_ref().map_or(0.0, |g| g[row][col]);
        let is_lean = lean.contains(&(row, col));
        let mut flags = Vec::new();
        if is_spark(&s.map) {
            let d = new - old;
            if d > lim.spark_more + 1e-9 {
                refuse(
                    format!("{d:+.1}° more advance; at most {:.0}° per round", lim.spark_more),
                    &mut refused,
                );
                continue;
            }
            if d < -lim.spark_less - 1e-9 {
                refuse(
                    format!("{d:+.1}° less advance; at most {:.0}° per round", lim.spark_less),
                    &mut refused,
                );
                continue;
            }
            if d > 0.0 && high_load {
                refuse(
                    format!("no added advance at load {} or more (detonation risk)", lim.lean_load),
                    &mut refused,
                );
                continue;
            }
            if d > 0.0 && is_lean {
                refuse("no added advance where the wideband shows the mixture lean".into(), &mut refused);
                continue;
            }
            if (new - base).abs() > lim.drift * base.abs().max(1.0) + step {
                refuse(
                    format!("more than {:.0}% from the last ECM read ({base})", lim.drift * 100.0),
                    &mut refused,
                );
                continue;
            }
            if d > 0.0 {
                flags.push("more advance: check carefully (no knock sensing)".into());
            }
        } else {
            let pct = new / old - 1.0;
            if old > 0.0 && pct.abs() > lim.fuel_round + step / old {
                refuse(
                    format!("{:+.1}%; at most ±{:.0}% per round", pct * 100.0, lim.fuel_round * 100.0),
                    &mut refused,
                );
                continue;
            }
            if base > 0.0 && (new / base - 1.0).abs() > lim.drift + step / base {
                refuse(
                    format!(
                        "{:+.1}% from the last ECM read ({base}); at most ±{:.0}%",
                        (new / base - 1.0) * 100.0,
                        lim.drift * 100.0
                    ),
                    &mut refused,
                );
                continue;
            }
            if new < old && is_lean {
                refuse("the wideband shows this cell lean already: only more fuel".into(), &mut refused);
                continue;
            }
            let p = proposals.iter().find(|p| p.row == row && p.col == col);
            if new < old && high_load && p.is_none_or(|p| new < p.new - 1e-9) {
                refuse(
                    format!(
                        "leaner at load {} or more than XBTune's auto-tune would go ({})",
                        lim.lean_load,
                        p.map_or("no auto-tune change here".into(), |p| p.new.to_string())
                    ),
                    &mut refused,
                );
                continue;
            }
            if let Some(p) = p {
                if (new - old) * (p.new - old) < 0.0 {
                    flags.push(format!("opposite of auto-tune ({} -> {})", p.old, p.new));
                } else if (new - p.new).abs() > 3.0 {
                    flags.push(format!("differs from auto-tune ({})", p.new));
                }
            }
        }
        if cell_hits < lim.min_hits {
            if !cx.allow_no_data {
                refuse(
                    format!("not enough logged data in this cell ({cell_hits:.0} of {:.0})", lim.min_hits),
                    &mut refused,
                );
                continue;
            }
            flags.push(format!("little or no logged data ({cell_hits:.0})"));
        }
        ok.push(Accepted {
            map: s.map.clone(),
            row,
            col,
            index,
            load,
            rpm,
            old,
            new,
            why: s.why.clone(),
            flags,
        });
    }
    (ok, refused)
}

/// XBTune's own auto-tune proposals for a fuel map, and the cells the
/// wideband shows lean (for a spark map: the lean cells of its fuel map,
/// matched by load and rpm values).
fn deterministic(
    logs: &[Log],
    tune: &Tune,
    fuel_map: &str,
    for_spark: bool,
) -> (Vec<Proposal>, Vec<(usize, usize)>) {
    let usable: Vec<Log> = logs.iter().filter(|l| l.ecm.id == tune.ecm.id).cloned().collect();
    if usable.is_empty() || tune.var(fuel_map).is_err() {
        return (Vec::new(), Vec::new());
    }
    let wideband = usable.iter().all(|l| l.wideband.is_some());
    let mode = if wideband { autotune::Mode::wideband() } else { autotune::Mode::Narrowband };
    let settings = autotune::Settings {
        filters: autotune::Filters { mode, ..Default::default() },
        ..Default::default()
    };
    let Ok(a) = autotune::analyze(&usable, tune, fuel_map, &settings.filters) else {
        return (Vec::new(), Vec::new());
    };
    if for_spark {
        // Lean cells by their load/rpm values, found again on the spark
        // map's own axes (the two maps needn't share breakpoints).
        let (Some((fy, fx)), Some((sy, sx))) =
            (axes(tune, fuel_map), axes(tune, &fuel_map.replace("Tab_Fuel", "Tab_Spark")))
        else {
            return (Vec::new(), Vec::new());
        };
        let lean =
            a.lean.iter().filter_map(|&(r, c)| find(&sy, *fy.get(r)?).zip(find(&sx, *fx.get(c)?))).collect();
        return (Vec::new(), lean);
    }
    (autotune::propose(&a, tune, &settings).unwrap_or_default(), a.lean.clone())
}

/// Start of the undo label of applied AI changes.
pub const AI_STEP: &str = "AI: ";

/// Apply accepted changes as one undo step.
pub fn apply(tune: &mut Tune, changes: &[Accepted], model: &str) -> Result<usize, String> {
    let list: Vec<(String, usize, f64)> = changes.iter().map(|c| (c.map.clone(), c.index, c.new)).collect();
    let a = tune
        .set(format!("{AI_STEP}{} cell(s) ({model})", changes.len()), &list)
        .map_err(|e| e.to_string())?;
    Ok(a.changed)
}

/// Undo steps in the tune's history made by the AI.
pub fn ai_steps(tune: &Tune) -> Vec<String> {
    tune.undo_labels().filter(|l| l.starts_with(AI_STEP)).map(str::to_string).collect()
}

/// The last ECM read of this bike, to measure drift from: the newest image
/// in the files folder that was read from an ECM with this ID (and version,
/// when both are known). Falls back to the tune as it was opened. Returns
/// the tune and where it came from.
pub fn baseline(files: &Path, tune: &Tune, catalog: &Catalog) -> (Tune, String) {
    let mut best: Option<(String, Tune, String)> = None;
    for dir in [xbtune_core::env::TUNES, xbtune_core::env::BACKUPS] {
        let Ok(rd) = std::fs::read_dir(files.join(dir)) else { continue };
        for p in rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "xbt")) {
            let Ok(img) = xbtune_core::image::Image::load(&p, None, catalog) else { continue };
            let same_version = match (&img.ecm_version, &tune.image.ecm_version) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            };
            if img.ecm_id != tune.ecm.id || !same_version || !img.meta.source.starts_with("read from ECM") {
                continue;
            }
            let created = img.meta.created.clone();
            if best.as_ref().is_some_and(|(c, ..)| *c >= created) {
                continue;
            }
            if let Ok(t) = Tune::new(img, catalog) {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                best = Some((created, t, format!("the ECM read {name}")));
            }
        }
    }
    if let Some((_, t, why)) = best {
        return (t, why);
    }
    let mut img = tune.image.clone();
    img.bytes = tune.opened_bytes().to_vec();
    match Tune::new(img, catalog) {
        Ok(t) => (t, "the tune as it was opened (no ECM read found)".into()),
        Err(_) => (tune.clone(), "the tune as it is".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tune() -> (Catalog, Tune) {
        let c = Catalog::builtin().unwrap();
        let img = xbtune_core::image::Image::from_raw(c.lookup("BUEIB").unwrap(), xbtune_sim::BUEIB_EEPROM)
            .unwrap();
        let t = Tune::new(img, &c).unwrap();
        (c, t)
    }

    fn sug(map: &str, load: f64, rpm: f64, value: f64) -> Suggestion {
        Suggestion {
            map: map.into(),
            load: Some(load),
            rpm: Some(rpm),
            row: None,
            col: None,
            value,
            why: "test".into(),
        }
    }

    fn cell(t: &Tune, map: &str, row: usize, col: usize) -> (f64, f64, f64) {
        let (ys, xs) = axes(t, map).unwrap();
        let cols = xs.len();
        (ys[row], xs[col], t.elements(map).unwrap()[row * cols + col])
    }

    #[test]
    fn parses_the_json_block() {
        let a = "Lower load 30.\n\n```json\n{\"changes\": [{\"map\": \"Tab_Fuel_Front\", \"load\": 30, \"rpm\": 2900, \"value\": 54, \"why\": \"EGO 94%\"}]}\n```\nRemember: x";
        let s = parse_changes(a).unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].load, s[0].rpm, s[0].value), (Some(30.0), Some(2900.0), 54.0));
        assert_eq!(parse_changes("Just words.").unwrap(), vec![]);
        assert!(parse_changes("```json\n{\"changes\": [{\"map\": 1}]}\n```").is_err());
        assert!(parse_changes("```json\n{\"changes\": [oops\n```").is_err());
    }

    #[test]
    fn the_rules() {
        let (_, t) = tune();
        let base = t.clone();
        let cx = Context { tune: &t, baseline: &base, logs: &[], allow_no_data: true };
        let lim = Limits::default();
        let f = "Tab_Fuel_Front";
        // A light-load cell (row 3 = load 30 on the BUEIB axis) and a high-load one.
        let (l30, r, v) = cell(&t, f, 3, 6);
        let (lhi, rhi, vhi) = cell(&t, f, 9, 6);
        assert!(lhi >= lim.lean_load);
        let cases = [
            (sug(f, l30, r, v * 1.05), true, ""),
            (sug(f, l30, r, v * 1.2), false, "per round"),
            (sug(f, 33.3, r, v), false, "axes"),
            (sug("KRPM_Hard_Hi", 0.0, 0.0, 7000.0), false, "fuel and spark"),
            (sug("Tab_Nope", 0.0, 0.0, 1.0), false, "no map"),
            (sug(f, l30, r, v), false, "no change"),
            (sug(f, lhi, rhi, vhi * 0.95), false, "leaner at load"),
            (sug(f, lhi, rhi, vhi * 1.05), true, ""),
            (sug(f, l30, r, 1e9), false, "store"),
        ];
        for (s, want_ok, why) in cases {
            let (ok, no) = check(std::slice::from_ref(&s), &cx, &lim);
            assert_eq!(ok.len() == 1, want_ok, "{s:?}: {no:?}");
            if !want_ok {
                assert!(no[0].reason.contains(why), "{s:?}: {}", no[0].reason);
            }
        }
        // Without "allow no data", cells need logged samples.
        let strict = Context { allow_no_data: false, ..cx };
        let (ok, no) = check(&[sug(f, l30, r, v * 1.05)], &strict, &lim);
        assert!(ok.is_empty() && no[0].reason.contains("logged data"));
        // Twice the same cell: the second is refused.
        let (ok, no) = check(&[sug(f, l30, r, v * 1.05), sug(f, l30, r, v * 1.02)], &cx, &lim);
        assert_eq!((ok.len(), no.len()), (1, 1));
    }

    #[test]
    fn spark_rules_and_drift() {
        let (_, t) = tune();
        let lim = Limits::default();
        let s = "Tab_Spark_Front";
        let (l, r, v) = cell(&t, s, 2, 6);
        let (lhi, rhi, vhi) = cell(&t, s, 9, 6);
        let base = t.clone();
        let cx = Context { tune: &t, baseline: &base, logs: &[], allow_no_data: true };
        let one = |sg| check(&[sg], &cx, &lim);
        assert!(one(sug(s, l, r, v - 2.0)).0.len() == 1);
        assert!(one(sug(s, l, r, v - 3.0)).1[0].reason.contains("less advance"));
        let (ok, _) = one(sug(s, l, r, v + 1.0));
        assert!(ok[0].flags.iter().any(|f| f.contains("more advance")));
        assert!(one(sug(s, l, r, v + 2.0)).1[0].reason.contains("more advance"));
        assert!(one(sug(s, lhi, rhi, vhi + 1.0)).1[0].reason.contains("no added advance"));
        // Drift: a tune already 9% richer than the ECM read may not go to +18%.
        let f = "Tab_Fuel_Front";
        let (lf, rf, vf) = cell(&t, f, 3, 6);
        let mut moved = t.clone();
        let idx = 3 * axes(&t, f).unwrap().1.len() + 6;
        moved.set("test", &[(f.to_string(), idx, vf * 1.09)]).unwrap();
        let now = moved.elements(f).unwrap()[idx];
        let cx = Context { tune: &moved, baseline: &t, logs: &[], allow_no_data: true };
        let (_, no) = check(&[sug(f, lf, rf, now * 1.08)], &cx, &lim);
        assert!(no[0].reason.contains("last ECM read"), "{no:?}");
    }

    #[test]
    fn apply_is_one_undo_step() {
        let (_, mut t) = tune();
        let base = t.clone();
        let f = "Tab_Fuel_Front";
        let (l, r, v) = cell(&t, f, 3, 6);
        let cx = Context { tune: &t, baseline: &base, logs: &[], allow_no_data: true };
        let (ok, _) = check(&[sug(f, l, r, v * 1.05)], &cx, &Limits::default());
        assert_eq!(apply(&mut t, &ok, "gemma3-4b").unwrap(), 1);
        assert_eq!(ai_steps(&t), vec!["AI: 1 cell(s) (gemma3-4b)"]);
        t.undo();
        assert_eq!(t.elements(f).unwrap(), base.elements(f).unwrap());
    }
}
