// SPDX-License-Identifier: GPL-3.0-or-later
//! The ECM Setup screen: EcmDroid's named settings (switches and numbers),
//! grouped as on its Setup screen, resolved against a tune's definition.
//!
//! Ported from EcmDroid `fragments/SetupFragment.java` and
//! `res/xml/ecm_setup.xml` (commit 2f17d29f; Copyright (C) 2012 Michel
//! Marti, GPL-3.0-or-later). The screen itself is in `data/ecm_setup.toml`
//! (tools/extract_ecmdroid_setup.py). As in `setupPreference()`:
//! - a switch key is `Name[b]` or `Name[b1,b2]`; it is on when all its bits
//!   are set, off when none are, and unusable when they differ ("odd bit
//!   set") or the ECM lacks a bit; turning it on or off sets all of them;
//! - `A:B` alternatives: the first one this ECM has is used;
//! - a missing title falls back to the bit's or the variable's name.
//!
//! Changes go into the tune like any edit (one undo step each); writing
//! them to the ECM is the normal Burn.

use serde::Deserialize;
use xbtune_defs::{EeVar, Shape, Value};

use crate::tune::{Applied, Tune, TuneError};

/// EcmDroid's Setup screen (`data/ecm_setup.toml`).
pub const SETUP_TOML: &str = include_str!("../../../data/ecm_setup.toml");

#[derive(Debug, Clone, Deserialize)]
struct Screen {
    group: Vec<GroupDef>,
}

#[derive(Debug, Clone, Deserialize)]
struct GroupDef {
    path: Vec<String>,
    item: Vec<ItemDef>,
}

#[derive(Debug, Clone, Deserialize)]
struct ItemDef {
    kind: String,
    keys: Vec<String>,
    #[serde(default)]
    title: String,
}

/// A switch: some bits of a flags byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switch {
    pub var: String,
    pub bits: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// On or off.
    Switch { switch: Switch, on: bool },
    /// The bits disagree (some set, some not): shown, not changeable.
    Odd { switch: Switch },
    /// A number, in the variable's units.
    Number { var: String, value: f64, units: Option<String>, decimals: Option<u32> },
    /// This ECM has none of the keys.
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub title: String,
    /// The keys as EcmDroid wrote them (for tooltips).
    pub keys: Vec<String>,
    pub state: State,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// e.g. ["General Config", "Airbox Pressure Sensor"].
    pub path: Vec<String>,
    pub items: Vec<Item>,
}

fn parse_key(key: &str) -> Option<(String, Vec<u8>)> {
    let (name, rest) = key.split_once('[')?;
    let bits =
        rest.strip_suffix(']')?.split(',').map(|b| b.trim().parse().ok()).collect::<Option<Vec<u8>>>()?;
    Some((name.trim().to_string(), bits))
}

fn bit_name(v: &EeVar, nr: u8) -> Option<String> {
    v.bits.as_ref()?.bits.iter().find(|b| b.nr == nr).map(|b| b.name.clone()).filter(|n| !n.is_empty())
}

fn byte_of(tune: &Tune, name: &str) -> Option<u8> {
    match tune.value(name).ok()? {
        Value::Bits(b) => Some(b),
        Value::Number(x) => Some(x as u8),
        _ => None,
    }
}

fn resolve(tune: &Tune, def: &ItemDef) -> (State, String) {
    for key in &def.keys {
        if def.kind == "bits" {
            let Some((name, bits)) = parse_key(key) else { continue };
            let Ok(v) = tune.var(&name) else { continue };
            if v.shape != Shape::Bits || bits.iter().any(|&b| b > 7) {
                continue;
            }
            let Some(byte) = byte_of(tune, &name) else { continue };
            let set = bits.iter().filter(|&&b| byte & (1 << b) != 0).count();
            let title = bit_name(v, bits[0]).unwrap_or_else(|| format!("{} bit {}", v.label(), bits[0]));
            let switch = Switch { var: name, bits };
            if set != 0 && set != switch.bits.len() {
                return (State::Odd { switch }, title);
            }
            return (State::Switch { on: set > 0, switch }, title);
        }
        let Ok(v) = tune.var(key) else { continue };
        if !matches!(v.shape, Shape::Scalar) {
            continue;
        }
        if let Ok(Value::Number(x)) = tune.value(key) {
            let state = State::Number {
                var: key.clone(),
                value: x,
                units: v.names.units.clone(),
                decimals: v.names.decimals,
            };
            return (state, v.label().to_string());
        }
    }
    (State::Missing, def.keys.first().cloned().unwrap_or_default())
}

/// The Setup screen for this tune's ECM, in EcmDroid's order.
pub fn screen(tune: &Tune) -> Vec<Group> {
    let s: Screen = toml::from_str(SETUP_TOML).expect("data/ecm_setup.toml parses");
    s.group
        .into_iter()
        .map(|g| Group {
            path: g.path,
            items: g
                .item
                .iter()
                .map(|d| {
                    let (state, fallback) = resolve(tune, d);
                    Item {
                        title: if d.title.is_empty() { fallback } else { d.title.clone() },
                        keys: d.keys.clone(),
                        state,
                    }
                })
                .collect(),
        })
        .collect()
}

/// Turn a switch on or off (all its bits), as one undo step.
pub fn set_switch(tune: &mut Tune, s: &Switch, on: bool) -> Result<Applied, TuneError> {
    let byte =
        byte_of(tune, &s.var).ok_or_else(|| TuneError::Other(format!("{} isn't a flags byte", s.var)))?;
    let mask = s.bits.iter().fold(0u8, |m, &b| m | (1 << b));
    tune.set_bits(&s.var, if on { byte | mask } else { byte & !mask })
}

/// Set a number, as one undo step.
pub fn set_number(tune: &mut Tune, var: &str, value: f64) -> Result<Applied, TuneError> {
    tune.set(format!("setup: {var}"), &[(var.to_string(), 0, value)])
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

    #[test]
    fn the_screen_parses_and_resolves() {
        let t = tune("BUEIB");
        let g = screen(&t);
        assert_eq!(g.len(), 18);
        assert_eq!(g.iter().map(|g| g.items.len()).sum::<usize>(), 113);
        let airbox = g.iter().find(|g| g.path.last().is_some_and(|p| p == "Airbox Pressure Sensor")).unwrap();
        let baro = &airbox.items[0];
        assert_eq!(baro.title, "Baro Correction Feature");
        assert!(matches!(baro.state, State::Switch { .. }), "{baro:?}");
        let limits = g.iter().find(|g| g.path == ["Limits", "RPM Limits", "Fixed"]).unwrap();
        assert!(
            limits
                .items
                .iter()
                .any(|i| matches!(i.state, State::Number { ref var, .. } if var == "KRPM_Hard_Hi"))
        );
        // Most settings exist on the owner's ECM.
        let missing = g.iter().flat_map(|g| &g.items).filter(|i| i.state == State::Missing).count();
        assert!(missing < 40, "{missing}");
    }

    #[test]
    fn switches_and_numbers_edit_the_tune() {
        let mut t = tune("BUEIB");
        let g = screen(&t);
        let item =
            g.iter().flat_map(|g| &g.items).find(|i| i.title == "Baro Correction Feature").unwrap().clone();
        let State::Switch { switch, on } = item.state else { panic!() };
        set_switch(&mut t, &switch, !on).unwrap();
        let again = screen(&t);
        let now = again.iter().flat_map(|g| &g.items).find(|i| i.title == "Baro Correction Feature").unwrap();
        assert!(matches!(now.state, State::Switch { on: o, .. } if o == !on));
        t.undo();
        set_number(&mut t, "KRPM_Hard_Hi", 7450.0).unwrap();
        assert!(matches!(t.value("KRPM_Hard_Hi"), Ok(Value::Number(x)) if (x - 7450.0).abs() < 1e-9));
    }

    #[test]
    fn keys() {
        assert_eq!(parse_key("EDiag1[6,7]"), Some(("EDiag1".into(), vec![6, 7])));
        assert_eq!(parse_key("KBaro"), None);
    }
}
