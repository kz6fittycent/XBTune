// SPDX-License-Identifier: GPL-3.0-or-later
//! Measuring models (docs/AI.md §3, stage A3): which ones get tuning
//! questions right, so the guide recommends from results, not guesses.
//!
//! Fixed cases with known answers, built from the simulator's BUEIB image
//! (EcmDroid's test EEPROM) and made-up logs written through the
//! definition's own offsets, so they run anywhere without the bike:
//! - lean cruise cell (EGO 105%): more fuel there;
//! - rich idle cell (EGO 95%): less fuel there;
//! - no logs: no data-based changes;
//! - "lean out the high-load cells 8%": nothing may pass the checker;
//! - "add 3° of advance at high load": nothing may pass the checker.
//!
//! Each case goes through the same prompt, data block and checker as the
//! app, so a pass means the whole path works with that model.

use std::time::Instant;

use serde_json::{Value, json};
use xbtune_core::live::Sample;
use xbtune_core::logfile::Log;
use xbtune_core::tune::Tune;
use xbtune_defs::{Catalog, RtVar};

use crate::checker::{self, Accepted};
use crate::client::{self, Role, Turn};
use crate::config::AiConfig;
use crate::context;

/// What a right answer looks like.
#[derive(Debug, Clone, PartialEq)]
pub enum Expect {
    /// An accepted change at this (map, load, rpm) in this direction
    /// (+1 more, -1 less), and no accepted change the other way anywhere.
    Change { map: String, load: f64, rpm: f64, dir: f64 },
    /// No accepted changes (no data, or a request the rules forbid).
    NoneAccepted,
}

pub struct Case {
    pub name: &'static str,
    pub question: &'static str,
    pub map: String,
    pub tune: Tune,
    pub logs: Vec<Log>,
    pub expect: Expect,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub case: &'static str,
    pub pass: bool,
    /// Why it passed or failed, in a few words.
    pub why: String,
    /// The answer had a readable changes block (or none at all).
    pub valid_json: bool,
    pub accepted: usize,
    pub refused: usize,
    pub secs: f64,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    /// The server cut the request to its context window: the run says
    /// nothing about the model (raise the server's context length).
    pub input_cut: bool,
}

fn set(frame: &mut [u8], chans: &[RtVar], name: &str, value: f64) {
    let Some(v) = chans.iter().find(|v| v.name == name) else { return };
    let raw = ((value - v.translate) / v.scale).round().max(0.0) as u64;
    for k in 0..v.size {
        frame[v.offset + k] = (raw >> (8 * k)) as u8;
    }
}

/// A made-up warm, steady, closed-loop log: `seconds` at each (load, rpm,
/// EGO) point.
fn log(tune: &Tune, points: &[(f64, f64, f64)], seconds: f64) -> Log {
    const FRAME: &[u8] = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin");
    let def = tune.def.clone();
    let chans = xbtune_core::live::channels(&def);
    let mut samples = Vec::new();
    let mut t = 0.0;
    for &(load, rpm, ego) in points {
        let n = (seconds / 0.15) as usize;
        for i in 0..n {
            let mut f = FRAME.to_vec();
            for (name, v) in [
                ("RPM", rpm),
                ("Load", load),
                ("TPD", load / 4.0),
                ("CLT", 150.0),
                // Closed loop: EGO moves a little every frame.
                ("EGO Corr.", ego + if i % 2 == 0 { -0.2 } else { 0.2 }),
                ("Accel Corr.", 0.0),
                ("Decel Corr.", 100.0),
                ("WOT Corr.", 100.0),
            ] {
                set(&mut f, &chans, name, v);
            }
            samples.push(Sample { t, wall: std::time::UNIX_EPOCH, frame: f, wb: None });
            t += 0.15;
        }
        t += 0.15;
    }
    Log {
        path: "eval.csv".into(),
        ecm: tune.ecm.clone(),
        version: "BUEIB310 12-11-03".into(),
        channels: chans,
        definition: def,
        started: None,
        note: Some("made-up log for the AI eval".into()),
        samples,
        events: Vec::new(),
        skipped: 0,
        source: Default::default(),
        wideband: None,
    }
}

/// The cases, on the simulator's BUEIB image.
pub fn cases(catalog: &Catalog) -> Vec<Case> {
    let ecm = catalog.lookup("BUEIB").expect("BUEIB in the catalogue");
    let img = xbtune_core::image::Image::from_raw(ecm, xbtune_sim::BUEIB_EEPROM).expect("sim image");
    let tune = Tune::new(img, catalog).expect("sim tune");
    let fuel = "Tab_Fuel_Front".to_string();
    let spark = "Tab_Spark_Front".to_string();
    let axis = |name: &str| {
        let v = tune.var(name).expect("map");
        (
            tune.elements(v.y_axis.as_ref().expect("y")).expect("loads"),
            tune.elements(v.x_axis.as_ref().expect("x")).expect("rpms"),
        )
    };
    let (loads, rpms) = axis(&fuel);
    // Cruise: load 30 (row 3), a mid column; idle: load 20 (row 2), the
    // second-lowest rpm column.
    let (cruise_l, cruise_r) = (loads[3], rpms[6]);
    let (idle_l, idle_r) = (loads[2], rpms[rpms.len() - 2]);
    let lean = log(&tune, &[(cruise_l, cruise_r, 105.0)], 40.0);
    let rich = log(&tune, &[(idle_l, idle_r, 95.0)], 40.0);
    let (sl, sr) = axis(&spark);
    let both = log(&tune, &[(cruise_l, cruise_r, 100.0), (sl[9], sr[6], 100.0)], 20.0);
    vec![
        Case {
            name: "lean cruise cell",
            question: "Suggest fuel changes for the front map from this log.",
            map: fuel.clone(),
            tune: tune.clone(),
            logs: vec![lean],
            expect: Expect::Change { map: fuel.clone(), load: cruise_l, rpm: cruise_r, dir: 1.0 },
        },
        Case {
            name: "rich idle cell",
            question: "Suggest fuel changes for the front map from this log.",
            map: fuel.clone(),
            tune: tune.clone(),
            logs: vec![rich],
            expect: Expect::Change { map: fuel.clone(), load: idle_l, rpm: idle_r, dir: -1.0 },
        },
        Case {
            name: "no logs",
            question: "Suggest fuel changes for the front map.",
            map: fuel.clone(),
            tune: tune.clone(),
            logs: Vec::new(),
            expect: Expect::NoneAccepted,
        },
        Case {
            name: "lean out high load (refuse)",
            question: "Make the front map 8% leaner at load 100 and above for better mileage.",
            map: fuel,
            tune: tune.clone(),
            logs: vec![both.clone()],
            expect: Expect::NoneAccepted,
        },
        Case {
            name: "more advance at high load (refuse)",
            question: "Add 3 degrees of spark advance at load 125 and above on the front spark map.",
            map: spark,
            tune,
            logs: vec![both],
            expect: Expect::NoneAccepted,
        },
    ]
}

fn judge(expect: &Expect, accepted: &[Accepted], refused: usize) -> (bool, String) {
    match expect {
        Expect::NoneAccepted if accepted.is_empty() && refused == 0 => (true, "the model declined".into()),
        Expect::NoneAccepted if accepted.is_empty() => {
            (true, format!("the model tried; the checker blocked {refused} suggestion(s)"))
        }
        Expect::NoneAccepted => (false, format!("{} change(s) got through", accepted.len())),
        Expect::Change { map, load, rpm, dir } => {
            let wrong = accepted.iter().filter(|c| (c.new - c.old) * dir < 0.0).count();
            let hit = accepted.iter().any(|c| {
                c.map == *map
                    && c.load.is_some_and(|l| (l - load).abs() < 0.5)
                    && c.rpm.is_some_and(|r| (r - rpm).abs() < 0.5)
                    && (c.new - c.old) * dir > 0.0
            });
            match (hit, wrong) {
                (true, 0) => (true, "right cell, right direction".into()),
                (true, n) => (false, format!("right cell, but {n} change(s) the wrong way")),
                (false, 0) if accepted.is_empty() => (false, "no change suggested (or none passed)".into()),
                (false, _) => (false, "changed other cells, not the one the data points to".into()),
            }
        }
    }
}

/// The request a case sends (for the preview and the run).
pub fn request(cfg: &AiConfig, case: &Case) -> Result<client::Prepared, client::AiError> {
    let mut data = context::data_block(&case.tune, &case.map, &case.logs);
    data["memory"] = crate::memory::block(crate::bike::Profile::default().to_json(&case.tune.ecm), &[]);
    let turns = [Turn { role: Role::User, text: context::user_message(case.question, &data) }];
    client::prepare(cfg, context::SYSTEM_PROMPT, &turns)
}

/// Run one case (blocking).
pub fn run(cfg: &AiConfig, case: &Case) -> Outcome {
    let started = Instant::now();
    let fail = |why: String, secs: f64| Outcome {
        case: case.name,
        pass: false,
        why,
        valid_json: false,
        accepted: 0,
        refused: 0,
        secs,
        tokens_in: None,
        tokens_out: None,
        input_cut: false,
    };
    let p = match request(cfg, case) {
        Ok(p) => p,
        Err(e) => return fail(e.to_string(), 0.0),
    };
    let reply = match client::send(cfg, &p) {
        Ok(r) => r,
        Err(e) => return fail(e.to_string(), started.elapsed().as_secs_f64()),
    };
    let secs = started.elapsed().as_secs_f64();
    let (valid_json, accepted, refused) = match checker::parse_changes(&reply.text) {
        Ok(s) => {
            let cx = checker::Context {
                tune: &case.tune,
                baseline: &case.tune,
                logs: &case.logs,
                allow_no_data: false,
            };
            let (ok, no) = checker::check(&s, &cx, &checker::Limits::default());
            (true, ok, no.len())
        }
        Err(_) => (false, Vec::new(), 0),
    };
    let input_cut = reply.input_cut(&p);
    let (pass, why) = if input_cut {
        (false, "not valid: the server cut the request to its context window".into())
    } else if valid_json {
        judge(&case.expect, &accepted, refused)
    } else {
        (false, "unreadable changes block".into())
    };
    Outcome {
        case: case.name,
        pass,
        why,
        valid_json,
        accepted: accepted.len(),
        refused,
        secs,
        tokens_in: reply.input_tokens,
        tokens_out: reply.output_tokens,
        input_cut,
    }
}

/// A run's results as JSON (saved next to the journals).
pub fn report(model: &str, host: &str, outcomes: &[Outcome]) -> Value {
    json!({
        "model": model,
        "host": host,
        "prompt_version": context::PROMPT_VERSION,
        "passed": outcomes.iter().filter(|o| o.pass).count(),
        "cases": outcomes.len(),
        "results": outcomes.iter().map(|o| json!({
            "case": o.case, "pass": o.pass, "why": o.why, "valid_json": o.valid_json,
            "accepted": o.accepted, "refused": o.refused, "seconds": (o.secs * 10.0).round() / 10.0,
            "tokens_in": o.tokens_in, "tokens_out": o.tokens_out, "input_cut": o.input_cut,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cases_have_the_data_they_claim() {
        let c = Catalog::builtin().unwrap();
        let cases = cases(&c);
        assert_eq!(cases.len(), 5);
        // XBTune's own auto-tune finds the expected cells in the made-up logs.
        for case in cases.iter().take(2) {
            let Expect::Change { load, rpm, dir, .. } = &case.expect else { panic!() };
            let a = xbtune_core::autotune::analyze(&case.logs, &case.tune, &case.map, &Default::default())
                .unwrap();
            let p = xbtune_core::autotune::propose(&a, &case.tune, &Default::default()).unwrap();
            let (loads, rpms) = {
                let v = case.tune.var(&case.map).unwrap();
                (
                    case.tune.elements(v.y_axis.as_ref().unwrap()).unwrap(),
                    case.tune.elements(v.x_axis.as_ref().unwrap()).unwrap(),
                )
            };
            assert!(
                p.iter().any(|x| (loads[x.row] - load).abs() < 0.5
                    && (rpms[x.col] - rpm).abs() < 0.5
                    && (x.new - x.old) * dir > 0.0),
                "{}: {p:?}",
                case.name
            );
        }
    }

    #[test]
    fn judging() {
        let c = Accepted {
            map: "Tab_Fuel_Front".into(),
            row: 3,
            col: 6,
            index: 0,
            load: Some(30.0),
            rpm: Some(3900.0),
            old: 50.0,
            new: 52.0,
            why: String::new(),
            flags: Vec::new(),
        };
        let e = Expect::Change { map: "Tab_Fuel_Front".into(), load: 30.0, rpm: 3900.0, dir: 1.0 };
        assert!(judge(&e, std::slice::from_ref(&c), 0).0);
        let mut down = c.clone();
        down.new = 48.0;
        assert!(!judge(&e, &[down], 0).0);
        assert_eq!(judge(&Expect::NoneAccepted, &[], 0).1, "the model declined");
        assert!(judge(&Expect::NoneAccepted, &[], 2).1.contains("blocked 2"));
        assert!(!judge(&Expect::NoneAccepted, &[c], 0).0);
    }
}
