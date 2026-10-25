// SPDX-License-Identifier: GPL-3.0-or-later
//! Wideband logging mode: the tune changes riders make by hand in ECMSpy
//! before logging with a wideband, so the ECM's own corrections stay out
//! of the data:
//! - EGO correction limits both 100% (`KFBFuel_Min/Max`): closed loop can
//!   no longer move the fuel; on ECMs with a closed-loop switch
//!   (`KCL_Fuel_Config` bit 7, "Enable Closed Loop" on EcmDroid's Setup
//!   screen) that is turned off as well;
//! - AFV set to 100% and its limits both 100% (`LFuel`, `LFuel1`,
//!   `KLFuel_Min/Max`): it can't learn either.
//!
//! [`switch_on`] keeps what it replaced ([`Saved`]); [`restore`] puts the
//! limits and the switch back but leaves AFV at 100%: the maps changed, so
//! AFV starts again from neutral, as after an AFV reset.
//!
//! ASSUMED (XBTune's reading, not from Buell or ECMSpy): an EGO window of
//! 100..100% holds EGO at 100% on ECMs without the switch. The first log
//! with the mode on shows it: `EGO Corr.` should read 100 throughout.
//! Wideband auto-tune works with or without the mode (it divides out EGO).
//!
//! These are edits to the tune, one undo step each; writing them to the
//! ECM is the normal Burn.

use serde::{Deserialize, Serialize};
use xbtune_defs::{Shape, Value};

use crate::tune::{Tune, TuneError};

/// The bit of `KCL_Fuel_Config` that enables closed loop.
const CL_ENABLE_BIT: u8 = 7;
const CL_SWITCH: &str = "KCL_Fuel_Config";
const LOCKED: f64 = 100.0;
/// Numbers set to [`LOCKED`]. The AFV values themselves come last: they
/// stay at 100% on restore.
const NUMBERS: &[&str] = &["KFBFuel_Min", "KFBFuel_Max", "KLFuel_Min", "KLFuel_Max"];
const AFV: &[&str] = crate::burn::AFV_VARS;

/// One variable the mode changes (or changed back).
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub var: String,
    /// What it is, for the review list.
    pub title: &'static str,
    pub from: f64,
    pub to: f64,
    /// A flags byte (`from`/`to` are the byte).
    pub bits: bool,
}

impl Change {
    /// "EGO correction max: 200 % -> 100 %", "Closed loop: on -> off".
    pub fn line(&self) -> String {
        if self.bits {
            let on = |b: f64| if (b as u8) & (1 << CL_ENABLE_BIT) != 0 { "on" } else { "off" };
            format!("{}: {} → {}", self.title, on(self.from), on(self.to))
        } else {
            format!("{}: {:.1} % → {:.1} %", self.title, self.from, self.to)
        }
    }
}

/// What [`switch_on`] replaced, to put back later. Values are in the
/// variables' units (a flags byte as its number).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub ecm_id: String,
    /// When it was switched on (RFC 3339, UTC).
    pub at: String,
    pub values: Vec<(String, f64)>,
}

fn title(var: &str) -> &'static str {
    match var {
        "KFBFuel_Min" => "EGO correction min",
        "KFBFuel_Max" => "EGO correction max",
        "KLFuel_Min" => "AFV min",
        "KLFuel_Max" => "AFV max",
        "LFuel" => "AFV (rear)",
        "LFuel1" => "AFV (front)",
        CL_SWITCH => "Closed loop",
        _ => "",
    }
}

/// A scalar this tune has and can edit.
fn number(tune: &Tune, var: &str) -> Option<f64> {
    let v = tune.var(var).ok()?;
    if v.shape != Shape::Scalar || tune.editable(var).is_err() {
        return None;
    }
    match tune.value(var).ok()? {
        Value::Number(x) => Some(x),
        _ => None,
    }
}

fn switch_byte(tune: &Tune) -> Option<u8> {
    let v = tune.var(CL_SWITCH).ok()?;
    if v.shape != Shape::Bits || tune.editable(CL_SWITCH).is_err() {
        return None;
    }
    match tune.value(CL_SWITCH).ok()? {
        Value::Bits(b) => Some(b),
        _ => None,
    }
}

/// Every variable the mode uses that this tune has, with its value now.
fn current(tune: &Tune) -> Vec<(String, f64, bool)> {
    let mut out: Vec<(String, f64, bool)> = NUMBERS
        .iter()
        .chain(AFV)
        .filter_map(|v| number(tune, v).map(|x| (v.to_string(), x, false)))
        .collect();
    if let Some(b) = switch_byte(tune) {
        out.push((CL_SWITCH.into(), f64::from(b), true));
    }
    out
}

/// What switching the mode on would change (empty: already on).
/// `Err`: this ECM lacks the EGO limits, so the mode can't work.
pub fn changes(tune: &Tune) -> Result<Vec<Change>, String> {
    let now = current(tune);
    if !["KFBFuel_Min", "KFBFuel_Max"].iter().all(|n| now.iter().any(|(v, ..)| v == n)) {
        return Err(format!("{} has no EGO correction limits to lock", tune.ecm.id));
    }
    Ok(now
        .into_iter()
        .filter_map(|(var, from, bits)| {
            let to = if bits { f64::from((from as u8) & !(1 << CL_ENABLE_BIT)) } else { LOCKED };
            (from != to).then(|| Change { title: title(&var), var, from, to, bits })
        })
        .collect())
}

/// Whether the tune is in logging mode now.
pub fn is_on(tune: &Tune) -> bool {
    changes(tune).is_ok_and(|c| c.is_empty())
}

/// Switch the mode on in the tune (one undo step) and return what it
/// replaced. Values already as the mode wants them are kept as they are,
/// so switching on twice saves the original values, not the locked ones.
pub fn switch_on(tune: &mut Tune, earlier: Option<&Saved>) -> Result<Saved, String> {
    let todo = changes(tune)?;
    if todo.is_empty() {
        return Err("the tune is in wideband logging mode already".into());
    }
    let mut values: Vec<(String, f64)> = current(tune).into_iter().map(|(v, x, _)| (v, x)).collect();
    // A second switch-on (e.g. after an AFV change) keeps the first
    // originals for whatever was still locked.
    if let Some(e) = earlier.filter(|e| e.ecm_id == tune.ecm.id) {
        for (var, x) in &mut values {
            if !todo.iter().any(|c| &c.var == var)
                && let Some((_, old)) = e.values.iter().find(|(v, _)| v == var)
            {
                *x = *old;
            }
        }
    }
    let edits: Vec<(String, usize, f64)> = todo.iter().map(|c| (c.var.clone(), 0, c.to)).collect();
    tune.set("Wideband logging mode", &edits).map_err(|e: TuneError| e.to_string())?;
    Ok(Saved { ecm_id: tune.ecm.id.clone(), at: crate::time::rfc3339(std::time::SystemTime::now()), values })
}

/// What [`restore`] would change: the limits and the closed-loop switch
/// back to `saved`, AFV left at 100%.
pub fn restore_changes(tune: &Tune, saved: &Saved) -> Result<Vec<Change>, String> {
    if saved.ecm_id != tune.ecm.id {
        return Err(format!("the saved values are for a {}, this tune is a {}", saved.ecm_id, tune.ecm.id));
    }
    let now = current(tune);
    Ok(saved
        .values
        .iter()
        .filter(|(var, _)| !AFV.contains(&var.as_str()))
        .filter_map(|(var, to)| {
            let (_, from, bits) = now.iter().find(|(v, ..)| v == var)?;
            (from != to).then(|| Change {
                var: var.clone(),
                title: title(var),
                from: *from,
                to: *to,
                bits: *bits,
            })
        })
        .collect())
}

/// Put the normal settings back (one undo step). Returns the changes made.
pub fn restore(tune: &mut Tune, saved: &Saved) -> Result<Vec<Change>, String> {
    let todo = restore_changes(tune, saved)?;
    if todo.is_empty() {
        return Err("nothing to restore: the tune already has the saved settings".into());
    }
    let edits: Vec<(String, usize, f64)> = todo.iter().map(|c| (c.var.clone(), 0, c.to)).collect();
    tune.set("Normal mode (after wideband logging)", &edits).map_err(|e| e.to_string())?;
    Ok(todo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_defs::Catalog;

    fn tune(id: &str) -> Tune {
        let c = Catalog::builtin().unwrap();
        let ecm = c.lookup(id).unwrap();
        let raw = if id == "BUEIB" { xbtune_sim::BUEIB_EEPROM.to_vec() } else { vec![0; ecm.xsize] };
        Tune::new(crate::image::Image::from_raw(ecm, &raw).unwrap(), &c).unwrap()
    }

    fn num(t: &Tune, v: &str) -> f64 {
        number(t, v).unwrap()
    }

    #[test]
    fn on_then_restore_on_the_bueib() {
        let mut t = tune("BUEIB");
        let before: Vec<_> = NUMBERS.iter().map(|v| num(&t, v)).collect();
        assert!(!is_on(&t));
        let plan = changes(&t).unwrap();
        // The BUEIB has no closed-loop switch: EGO and AFV limits, AFV.
        assert!(plan.iter().all(|c| !c.bits), "{plan:?}");
        assert!(plan.iter().any(|c| c.var == "KFBFuel_Max" && c.to == 100.0));
        let saved = switch_on(&mut t, None).unwrap();
        assert!(is_on(&t));
        for v in NUMBERS.iter().chain(AFV).filter(|v| number(&t, v).is_some()) {
            assert_eq!(num(&t, v), 100.0, "{v}");
        }
        assert_eq!(t.undo_label(), Some("Wideband logging mode"));
        assert!(switch_on(&mut t, Some(&saved)).is_err(), "already on");
        // Restore: limits back, AFV stays neutral.
        let done = restore(&mut t, &saved).unwrap();
        assert!(done.iter().all(|c| !AFV.contains(&c.var.as_str())));
        let after: Vec<_> = NUMBERS.iter().map(|v| num(&t, v)).collect();
        assert_eq!(after, before);
        for v in AFV.iter().filter(|v| number(&t, v).is_some()) {
            assert_eq!(num(&t, v), 100.0, "{v}");
        }
        assert!(restore(&mut t, &saved).is_err(), "nothing left");
        // Undo twice: back to the opened tune.
        t.undo();
        t.undo();
        assert!(!t.is_modified());
    }

    #[test]
    fn a_second_switch_on_keeps_the_first_originals() {
        let mut t = tune("BUEIB");
        let max = num(&t, "KFBFuel_Max");
        let first = switch_on(&mut t, None).unwrap();
        // The rider changes only the EGO max back by hand, then switches on.
        t.set("by hand", &[("KFBFuel_Max".into(), 0, 150.0)]).unwrap();
        let second = switch_on(&mut t, Some(&first)).unwrap();
        let get = |s: &Saved, v: &str| s.values.iter().find(|(n, _)| n == v).unwrap().1;
        assert_eq!(get(&second, "KFBFuel_Max"), 150.0);
        assert_eq!(get(&second, "KFBFuel_Min"), get(&first, "KFBFuel_Min"));
        assert_ne!(get(&first, "KFBFuel_Max"), 100.0);
        assert_eq!(get(&first, "KFBFuel_Max"), max);
    }

    #[test]
    fn the_closed_loop_switch_goes_off_where_there_is_one() {
        let c = Catalog::builtin().unwrap();
        let with_switch: Vec<_> = c
            .iter()
            .filter(|e| e.family.in_scope())
            .filter_map(|e| {
                let d = c.definition(e)?;
                (d.eeprom.get(CL_SWITCH).is_some_and(|v| v.shape == Shape::Bits)).then(|| e.id.clone())
            })
            .collect();
        assert!(!with_switch.is_empty(), "some ECM has KCL_Fuel_Config");
        let mut tested = 0;
        for id in with_switch {
            let mut t = tune(&id);
            t.set_bits(CL_SWITCH, 0x81).unwrap();
            let Ok(plan) = changes(&t) else { continue };
            let sw = plan.iter().find(|c| c.bits).unwrap_or_else(|| panic!("{id}: {plan:?}"));
            assert_eq!((sw.from, sw.to), (129.0, 1.0), "{id}: only bit 7 cleared");
            assert_eq!(sw.line(), "Closed loop: on → off");
            let saved = switch_on(&mut t, None).unwrap();
            assert_eq!(switch_byte(&t), Some(0x01));
            restore(&mut t, &saved).unwrap();
            assert_eq!(switch_byte(&t), Some(0x81), "{id}");
            tested += 1;
        }
        assert!(tested > 0);
    }

    #[test]
    fn other_ecms_saved_values_are_refused() {
        let t = tune("BUEIB");
        let s = Saved { ecm_id: "BUEOD".into(), at: String::new(), values: vec![] };
        assert!(restore_changes(&t, &s).is_err());
    }
}
