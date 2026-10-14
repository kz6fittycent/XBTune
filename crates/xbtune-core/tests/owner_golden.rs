// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden test against the owner's real ECM (KICKOFF §7): XBTune's dump and
//! EcmDroid's save of the same ECM must be identical, and decoded values must
//! match what EcmDroid shows. Skipped until the fixtures exist; see
//! `tests/fixtures/owner/README.md`.

use std::path::PathBuf;

use xbtune_core::image::{Image, diff_ranges};
use xbtune_defs::{Catalog, Value, format_number};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner").join(name)
}

fn present(names: &[&str]) -> bool {
    let missing: Vec<_> = names.iter().filter(|n| !fixture(n).exists()).collect();
    if !missing.is_empty() {
        eprintln!("SKIPPED: owner fixtures missing: {missing:?} (see tests/fixtures/owner/README.md)");
    }
    missing.is_empty()
}

#[test]
fn xbtune_dump_matches_ecmdroid_save() {
    if !present(&["xbtune.xbt", "ecmdroid.xpr"]) {
        return;
    }
    let c = Catalog::builtin().unwrap();
    let ours = Image::load(&fixture("xbtune.xbt"), None, &c).unwrap();
    let theirs = Image::load(&fixture("ecmdroid.xpr"), Some(&ours.ecm_id), &c).unwrap();
    assert_eq!(ours.ecm_id, "BUEIB");
    let ecm = c.lookup(&ours.ecm_id).unwrap();
    let n = if ours.page_zero && theirs.page_zero { ecm.xsize } else { ecm.size };
    let d = diff_ranges(&ours.bytes[..n], &theirs.bytes[..n]);
    assert!(d.is_empty(), "images differ at {d:x?}");
}

/// `ecmdroid_values.toml`: `[values]` table of variable name -> the text
/// EcmDroid displays, e.g. `KRPM_Hard_Hi = "6800"`.
#[test]
fn decoded_values_match_ecmdroid() {
    if !present(&["xbtune.xbt", "ecmdroid_values.toml"]) {
        return;
    }
    #[derive(serde::Deserialize)]
    struct Expected {
        values: std::collections::BTreeMap<String, String>,
    }
    let c = Catalog::builtin().unwrap();
    let img = Image::load(&fixture("xbtune.xbt"), None, &c).unwrap();
    let def = c.definition(c.lookup(&img.ecm_id).unwrap()).unwrap();
    let exp: Expected =
        toml::from_str(&std::fs::read_to_string(fixture("ecmdroid_values.toml")).unwrap()).unwrap();
    let mut bad = Vec::new();
    for (name, want) in &exp.values {
        let var = def.eeprom.get(name).unwrap_or_else(|| panic!("no variable {name}"));
        let got = match var.decode(&img.view()).unwrap() {
            Value::Number(x) => format_number(x, var.names.decimals),
            Value::Bits(b) => format!("{b:08b}"),
            other => format!("{other:?}"),
        };
        if &got != want {
            bad.push(format!("{name}: XBTune {got}, EcmDroid {want}"));
        }
    }
    assert!(bad.is_empty(), "{bad:#?}");
}
