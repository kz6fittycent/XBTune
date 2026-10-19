// SPDX-License-Identifier: GPL-3.0-or-later
//! The bike the advisor is told about (owner's design, 2026-10-19): the
//! model, chosen from the models the ECM ID fits (or typed in), and the
//! mods, chosen from a menu with their numbers. Kept per ECM ID in
//! `<files>/AI/bikes.toml` with free-text notes, and sent with every
//! question, so the advisor "just knows" and nobody retypes it.
//!
//! The model list is the in-scope bikes of KICKOFF §1 with their stock
//! displacement (XB9: 984 cc, XB12 and the tube-frame DDFI bikes: 1203 cc).
//! Which models an ECM ID fits comes from the family and the years in
//! `data/ecm_models.toml`, which is only a hint: ECMs get swapped, so the
//! user can always pick any model or type one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use xbtune_defs::{EcmDef, Family};

/// Which frame and engine family a model belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// 1999-2002 tube-frame bikes (DDFI).
    Tube,
    /// 2003-2009 XB bikes (DDFI-2, DDFI-3).
    Xb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub name: &'static str,
    pub line: Line,
    /// Stock displacement, cc.
    pub cc: u32,
}

const fn m(name: &'static str, line: Line, cc: u32) -> Model {
    Model { name, line, cc }
}

/// The bikes XBTune covers (KICKOFF §1, plus the tube-frame DDFI bikes the
/// owner added 2026-10-15).
pub const MODELS: &[Model] = &[
    m("XB9R Firebolt", Line::Xb, 984),
    m("XB9S Lightning", Line::Xb, 984),
    m("XB9SX Lightning CityX", Line::Xb, 984),
    m("XB12R Firebolt", Line::Xb, 1203),
    m("XB12S Lightning", Line::Xb, 1203),
    m("XB12Ss Lightning Long", Line::Xb, 1203),
    m("XB12Scg Lightning Low", Line::Xb, 1203),
    m("XB12STT Lightning Super TT", Line::Xb, 1203),
    m("XB12X Ulysses", Line::Xb, 1203),
    m("XB12XT Ulysses", Line::Xb, 1203),
    m("X1 Lightning", Line::Tube, 1203),
    m("S3 Thunderbolt", Line::Tube, 1203),
    m("S3T Thunderbolt", Line::Tube, 1203),
];

pub fn model(name: &str) -> Option<&'static Model> {
    MODELS.iter().find(|m| m.name.eq_ignore_ascii_case(name) || m.name.split(' ').next() == Some(name))
}

/// Model years in the ECM's catalogue hint ("XB (2005-2007)" -> 2005..=2007).
pub fn years_for(ecm: &EcmDef) -> Option<(u16, u16)> {
    let hint = ecm.bikes.as_deref()?;
    let inside = hint.split_once('(')?.1.split_once(')')?.0;
    let nums: Vec<u16> = inside
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() == 4)
        .filter_map(|s| s.parse().ok())
        .collect();
    match nums.as_slice() {
        [a] if inside.contains('+') => Some((*a, 2010)),
        [a] => Some((*a, *a)),
        [a, b, ..] => Some((*a, *b)),
        [] => None,
    }
}

/// The models an ECM fits (by family; XB9 only for a 2003-only ID).
pub fn models_for(ecm: &EcmDef) -> Vec<&'static Model> {
    let line = if ecm.family == Family::Ddfi1 { Line::Tube } else { Line::Xb };
    let only_2003 = years_for(ecm) == Some((2003, 2003));
    MODELS.iter().filter(|m| m.line == line && (!only_2003 || m.cc == 984)).collect()
}

/// What's been changed from stock, as the menu offers it. Empty strings and
/// `None` mean stock / not set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mods {
    /// Displacement after a big bore kit, cc.
    pub big_bore_cc: Option<u32>,
    /// e.g. "Innovate LC-2".
    pub wideband: String,
    pub exhaust: String,
    pub intake: String,
    pub cams: String,
    pub ported_heads: bool,
    /// Final drive pulleys, teeth (stock if not set).
    pub front_pulley: Option<u32>,
    pub rear_pulley: Option<u32>,
    /// Usual fuel, octane (as sold where the owner lives).
    pub fuel_octane: Option<u32>,
    /// Race ECM, someone else's tune, ...
    pub ecm_tune: String,
    /// Anything else, one per line.
    pub other: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    /// A name from [`MODELS`], or what the user typed.
    pub model: String,
    pub year: Option<u16>,
    pub mods: Mods,
    /// Usual riding altitude, metres (else the home place's elevation).
    pub altitude_m: Option<u32>,
    /// Home town, from the Open-Meteo lookup (kept here; never sent to the
    /// advisor, only its elevation and weather numbers are).
    pub place: Option<crate::atmosphere::Place>,
    /// The last weather fetched for `place`.
    pub weather: Option<crate::atmosphere::Weather>,
    /// Free text, and the advisor's suggestions the user kept.
    pub notes: String,
}

impl Profile {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Add a line to the notes (an accepted suggestion), unless present.
    pub fn add_note(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || self.notes.lines().any(|l| l.trim().trim_start_matches("- ") == line) {
            return;
        }
        if !self.notes.trim().is_empty() {
            self.notes.push('\n');
        }
        self.notes.push_str("- ");
        self.notes.push_str(line);
    }

    /// The bike in one line, e.g. "2005 Buell XB9SX Lightning CityX".
    pub fn title(&self) -> String {
        let model = self.model.trim();
        match (self.year, model.is_empty()) {
            (_, true) => String::new(),
            (Some(y), false) => format!("{y} Buell {model}"),
            (None, false) => format!("Buell {model}"),
        }
    }

    /// The altitude to use: the usual riding altitude, else home's.
    pub fn altitude(&self) -> Option<f64> {
        self.altitude_m.map(f64::from).or(self.place.as_ref().map(|p| p.elevation_m))
    }

    /// The mods in words, for the advisor.
    pub fn mod_lines(&self) -> Vec<String> {
        let m = &self.mods;
        let mut out = Vec::new();
        let stock_cc = model(&self.model).map(|x| x.cc);
        if let Some(cc) = m.big_bore_cc {
            out.push(match stock_cc {
                Some(s) => format!("big bore kit: {cc} cc (stock {s} cc)"),
                None => format!("big bore kit: {cc} cc"),
            });
        }
        let text = |label: &str, v: &str, out: &mut Vec<String>| {
            if !v.trim().is_empty() {
                out.push(format!("{label}: {}", v.trim()));
            }
        };
        text("wideband O2", &m.wideband, &mut out);
        text("exhaust", &m.exhaust, &mut out);
        text("intake / air filter", &m.intake, &mut out);
        text("cams", &m.cams, &mut out);
        if m.ported_heads {
            out.push("ported heads".into());
        }
        match (m.front_pulley, m.rear_pulley) {
            (Some(f), Some(r)) => {
                out.push(format!("final drive: {f}/{r} teeth (ratio {:.2})", r as f64 / f as f64))
            }
            (Some(f), None) => out.push(format!("final drive: front pulley {f} teeth")),
            (None, Some(r)) => out.push(format!("final drive: rear pulley {r} teeth")),
            (None, None) => {}
        }
        if let Some(o) = m.fuel_octane {
            out.push(format!("fuel: {o} octane"));
        }
        text("ECM / tune", &m.ecm_tune, &mut out);
        out.extend(m.other.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        out
    }

    /// The profile for the data block.
    pub fn to_json(&self, ecm: &EcmDef) -> Value {
        let model = model(&self.model);
        json!({
            "bike": if self.title().is_empty() { Value::String("not set by the owner".into()) } else { Value::String(self.title()) },
            "ecm_fits": ecm.bikes,
            "stock_displacement_cc": model.map(|m| m.cc),
            "mods": if self.mod_lines().is_empty() { json!("none listed (stock)") } else { json!(self.mod_lines()) },
            "atmosphere": crate::atmosphere::to_json(self.altitude(), self.weather.as_ref()),
            "owner_notes": if self.notes.trim().is_empty() { Value::String("none".into()) } else { Value::String(self.notes.trim().to_string()) },
        })
    }
}

/// All bikes' profiles, by ECM ID.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Bikes {
    #[serde(default)]
    pub bikes: BTreeMap<String, Profile>,
}

impl Bikes {
    pub fn path(files: &Path) -> PathBuf {
        files.join("AI").join("bikes.toml")
    }

    pub fn load(files: &Path) -> Self {
        std::fs::read_to_string(Self::path(files))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, files: &Path) -> std::io::Result<()> {
        let path = Self::path(files);
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(
            &tmp,
            format!("# XBTune AI advisor: your bikes (by ECM ID), sent with every question.\n{text}"),
        )?;
        std::fs::rename(tmp, path)
    }

    /// The profile for an ECM, preset from the ECM when it's new.
    pub fn get(&self, ecm: &EcmDef) -> Profile {
        self.bikes.get(&ecm.id).cloned().unwrap_or_else(|| {
            let fits = models_for(ecm);
            let years = years_for(ecm);
            Profile {
                model: if fits.len() == 1 { fits[0].name.to_string() } else { String::new() },
                year: years.filter(|(a, b)| a == b).map(|(a, _)| a),
                ..Default::default()
            }
        })
    }

    pub fn set(&mut self, ecm_id: &str, p: Profile) {
        if p.is_empty() {
            self.bikes.remove(ecm_id);
        } else {
            self.bikes.insert(ecm_id.to_string(), p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_defs::Catalog;

    fn ecm(id: &str) -> EcmDef {
        Catalog::builtin().unwrap().lookup(id).unwrap().clone()
    }

    #[test]
    fn models_follow_the_ecm() {
        let b = ecm("BUEIB");
        assert_eq!(years_for(&b), Some((2005, 2007)));
        let fits = models_for(&b);
        assert!(fits.iter().any(|m| m.name.starts_with("XB9SX")));
        assert!(fits.iter().all(|m| m.line == Line::Xb));
        assert!(models_for(&ecm("BUECB")).iter().all(|m| m.cc == 984));
        assert!(models_for(&ecm("BUEGC")).iter().all(|m| m.line == Line::Tube));
    }

    #[test]
    fn profile_in_words_and_back() {
        let files = std::env::temp_dir().join(format!("xbtune-ai-bikes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&files);
        let mut all = Bikes::load(&files);
        let e = ecm("BUEIB");
        let mut p = all.get(&e);
        assert!(p.model.is_empty());
        p.model = "XB9SX Lightning CityX".into();
        p.year = Some(2005);
        p.mods.big_bore_cc = Some(1150);
        p.mods.wideband = "Innovate LC-2".into();
        p.mods.front_pulley = Some(27);
        p.mods.rear_pulley = Some(64);
        p.mods.other = vec!["K&N filter".into()];
        p.add_note("rear cylinder runs hotter");
        all.set(&e.id, p.clone());
        all.save(&files).unwrap();
        let back = Bikes::load(&files).get(&e);
        assert_eq!(back, p);
        assert_eq!(p.title(), "2005 Buell XB9SX Lightning CityX");
        let lines = p.mod_lines();
        assert_eq!(lines[0], "big bore kit: 1150 cc (stock 984 cc)");
        assert!(lines.contains(&"final drive: 27/64 teeth (ratio 2.37)".to_string()));
        p.place = Some(crate::atmosphere::Place {
            name: "Denver".into(),
            region: "Colorado, United States".into(),
            latitude: 39.739,
            longitude: -104.985,
            elevation_m: 1609.0,
        });
        // The advisor gets the numbers, never the place.
        let text = p.to_json(&e).to_string();
        assert!(!text.contains("Denver") && !text.contains("39.7"), "{text}");
        assert!(text.contains("\"altitude_m\":1609"));
        let j = p.to_json(&e);
        assert_eq!(j["stock_displacement_cc"], 984);
        assert_eq!(j["owner_notes"], "- rear cylinder runs hotter");
        let _ = std::fs::remove_dir_all(&files);
    }
}
