// SPDX-License-Identifier: GPL-3.0-or-later
//! Summaries of a live log: narrowband O2 statistics and anomaly flags
//! (docs/PARITY.md, items marked NEW for phase 7).

use crate::logfile::Log;

/// Narrowband O2 statistics. Rich/lean are split at the ECM's O2 target
/// voltage (`KO2_Midpoint`, 0.49 V on the owner's BUEIB); readings above
/// 1.10 V (or below 0) count as sensor errors. Readings near 0 V are lean,
/// not errors: the owner's warm cruise log (2026-10-15) dips to 0.00-0.04 V
/// between normal switching while EGO keeps working.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct O2Stats {
    pub samples: usize,
    pub rich_pct: f64,
    pub lean_pct: f64,
    pub error_pct: f64,
    pub avg_rich_v: Option<f64>,
    pub avg_lean_v: Option<f64>,
}

pub fn o2_stats(log: &Log, midpoint: f64) -> O2Stats {
    let (mut rich, mut lean, mut err) = (Vec::new(), Vec::new(), 0usize);
    for v in log.series("O2").into_iter().flatten() {
        if !(0.0..=1.10).contains(&v) {
            err += 1;
        } else if v >= midpoint {
            rich.push(v);
        } else {
            lean.push(v);
        }
    }
    let n = rich.len() + lean.len() + err;
    let pct = |k: usize| if n > 0 { 100.0 * k as f64 / n as f64 } else { 0.0 };
    let avg = |v: &[f64]| (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64);
    O2Stats {
        samples: n,
        rich_pct: pct(rich.len()),
        lean_pct: pct(lean.len()),
        error_pct: pct(err),
        avg_rich_v: avg(&rich),
        avg_lean_v: avg(&lean),
    }
}

/// Something in a log worth a look.
#[derive(Debug, Clone, PartialEq)]
pub struct Anomaly {
    pub t: f64,
    pub text: String,
}

/// Gaps over 2 s, engine temperature jumping more than 3.5 °C between two
/// readings, battery below 11 V or above 16 V (each run reported once).
pub fn anomalies(log: &Log) -> Vec<Anomaly> {
    let mut out = Vec::new();
    for w in log.samples.windows(2) {
        let gap = w[1].t - w[0].t;
        if gap > 2.0 {
            out.push(Anomaly { t: w[0].t, text: format!("no data for {gap:.1} s") });
        }
    }
    let clt = log.series("CLT");
    for i in 1..clt.len() {
        if let (Some(a), Some(b)) = (clt[i - 1], clt[i])
            && (b - a).abs() > 3.5
        {
            out.push(Anomaly {
                t: log.samples[i].t,
                text: format!("engine temperature jumped {a:.0} -> {b:.0} °C"),
            });
        }
    }
    let batt = log.series("Batt. Voltg.");
    let mut was_bad = false;
    for (i, v) in batt.iter().enumerate() {
        let bad = v.is_some_and(|v| !(11.0..=16.0).contains(&v));
        if bad && !was_bad {
            out.push(Anomaly {
                t: log.samples[i].t,
                text: format!("battery at {:.1} V", v.unwrap_or_default()),
            });
        }
        was_bad = bad;
    }
    out.sort_by(|a, b| a.t.total_cmp(&b.t));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use xbtune_defs::Catalog;

    #[test]
    fn owner_logs_stats() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner");
        let path = dir.join("BUEIB_20261014-231006.csv");
        if !path.exists() {
            return;
        }
        let log = Log::open(&path, &Catalog::builtin().unwrap()).unwrap();
        let s = o2_stats(&log, 0.49);
        assert_eq!(s.samples, log.samples.len());
        assert!((s.rich_pct + s.lean_pct + s.error_pct - 100.0).abs() < 1e-9);
        // Switching around the target, as a closed-loop idle does.
        assert!((40.0..80.0).contains(&s.rich_pct), "{s:?}");
        assert!(s.avg_rich_v.unwrap() > 0.49 && s.avg_lean_v.unwrap() < 0.49);
        // The owner's logs show reply gaps of ~1.3-2.5 s (lost replies).
        for a in anomalies(&log) {
            assert!(a.t >= log.samples[0].t, "{a:?}");
        }
    }

    /// Warm ride, 2026-10-15: lean swings reach 0.00 V; those are lean
    /// readings, not sensor errors (28% were counted as errors before).
    #[test]
    fn owner_ride_low_o2_is_lean() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner");
        let path = dir.join("BUEIB_20261015-200748.csv");
        if !path.exists() {
            return;
        }
        let log = Log::open(&path, &Catalog::builtin().unwrap()).unwrap();
        let s = o2_stats(&log, 0.49);
        assert_eq!(s.error_pct, 0.0, "{s:?}");
        assert!(s.lean_pct > 40.0 && s.rich_pct > 40.0, "{s:?}");
    }
}
