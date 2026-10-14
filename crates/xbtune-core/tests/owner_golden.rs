// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden test against the owner's real ECM (KICKOFF §7): XBTune's dump and
//! EcmDroid's save of the same ECM must be identical, and decoded values must
//! match what EcmDroid shows. Skipped until the fixtures exist; see
//! `tests/fixtures/owner/README.md`.

use std::path::PathBuf;

use xbtune_core::image::{Image, crc32, diff_ranges};
use xbtune_defs::{Catalog, Value, format_number};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner").join(name)
}

/// XBTune's dump: the `.xbt`, or the `--xpr` copy written by the same dump.
const OURS: &[&str] = &["xbtune.xbt", "BUEIB_xbtune.xpr"];
/// EcmDroid's save: `ecmdroid.xpr`, or EcmDroid's own `BUEIB_<date>-<time>.xpr` name.
fn theirs() -> Option<PathBuf> {
    let dir = fixture("");
    if dir.join("ecmdroid.xpr").exists() {
        return Some(dir.join("ecmdroid.xpr"));
    }
    let mut found: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.starts_with("BUEIB_2") && n.ends_with(".xpr")
        })
        .collect();
    found.sort();
    found.pop()
}

fn ours() -> Option<PathBuf> {
    OURS.iter().map(|n| fixture(n)).find(|p| p.exists())
}

/// CRC-32 of the full 1210-byte image read off the owner's ECM on 2026-10-14.
const BIKE_CRC: u32 = 0x3b54974e;

fn present(names: &[&str]) -> bool {
    let missing: Vec<_> = names.iter().filter(|n| !fixture(n).exists()).collect();
    if !missing.is_empty() {
        eprintln!("SKIPPED: owner fixtures missing: {missing:?} (see tests/fixtures/owner/README.md)");
    }
    missing.is_empty()
}

#[test]
fn xbtune_dump_matches_ecmdroid_save() {
    let (Some(ours), Some(theirs)) = (ours(), theirs()) else {
        eprintln!("SKIPPED: owner dump or EcmDroid save missing (see tests/fixtures/owner/README.md)");
        return;
    };
    let c = Catalog::builtin().unwrap();
    let ours = Image::load(&ours, Some("BUEIB"), &c).unwrap();
    let theirs = Image::load(&theirs, Some("BUEIB"), &c).unwrap();
    assert_eq!(ours.ecm_id, "BUEIB");
    assert!(ours.page_zero && theirs.page_zero);
    assert_eq!(crc32(&ours.bytes), BIKE_CRC);
    let ecm = c.lookup(&ours.ecm_id).unwrap();
    let n = if ours.page_zero && theirs.page_zero { ecm.xsize } else { ecm.size };
    let d = diff_ranges(&ours.bytes[..n], &theirs.bytes[..n]);
    assert!(d.is_empty(), "images differ at {d:x?}");
}

/// `ecmdroid_values.toml`: `[values]` table of variable name -> the text
/// EcmDroid displays, e.g. `KRPM_Hard_Hi = "6800"`.
#[test]
fn decoded_values_match_ecmdroid() {
    let Some(ours) = ours() else { return };
    if !present(&["ecmdroid_values.toml"]) {
        return;
    }
    #[derive(serde::Deserialize)]
    struct Expected {
        values: std::collections::BTreeMap<String, String>,
    }
    let c = Catalog::builtin().unwrap();
    let img = Image::load(&ours, Some("BUEIB"), &c).unwrap();
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

/// The front fuel and spark maps as `xbtune.cli show` printed them on the
/// owner's laptop (snap git 23ea31c, 2026-10-14). Guards the map layout
/// against regressions; EcmDroid itself only shows these as raw bytes.
#[test]
fn owner_maps_decode_as_on_the_bike() {
    let Some(ours) = ours() else { return };
    let c = Catalog::builtin().unwrap();
    let img = Image::load(&ours, Some("BUEIB"), &c).unwrap();
    let def = c.definition(c.lookup("BUEIB").unwrap()).unwrap();
    let grid = |name: &str| match def.eeprom.get(name).unwrap().decode(&img.view()).unwrap() {
        Value::Grid(g) => g,
        other => panic!("{name}: {other:?}"),
    };
    let fuel = grid("Tab_Fuel_Front");
    assert_eq!(fuel.len(), 12);
    assert_eq!(fuel[0], [20.0, 20.0, 22.0, 22.0, 23.0, 23.0, 23.0, 24.0, 30.0, 34.0, 40.0, 54.0, 56.0]);
    assert_eq!(
        fuel[11],
        [185.0, 210.0, 203.0, 185.0, 185.0, 182.0, 160.0, 165.0, 185.0, 200.0, 175.0, 145.0, 145.0]
    );
    let spark = grid("Tab_Spark_Front");
    assert_eq!(spark.len(), 10);
    assert_eq!(spark[3], [0.0, 45.0, 45.0, 45.0, 41.5, 37.5, 24.0, 12.0, 7.5, 5.0]);
    assert_eq!(spark[9], [0.0, 42.5, 37.25, 37.5, 35.0, 27.0, 21.5, 7.0, 5.0, 5.0]);
}

/// Every live log from the bike (`*.csv`): each stored frame is a valid
/// 107-byte BUEIB runtime frame, and decoding it with today's definitions
/// gives exactly the values that were logged.
#[test]
fn owner_logs_decode_as_logged() {
    use xbtune_defs::RtKind;
    use xbtune_proto::Pdu;
    let Ok(dir) = std::fs::read_dir(fixture("")) else { return };
    let mut logs: Vec<PathBuf> = dir
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "csv"))
        .collect();
    logs.sort();
    if logs.is_empty() {
        eprintln!("SKIPPED: no owner logs");
        return;
    }
    let c = Catalog::builtin().unwrap();
    let ecm = c.lookup("BUEIB").unwrap();
    let def = c.definition(ecm).unwrap();
    for path in logs {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# ecm_id: BUEIB"), "{}", path.display());
        let mut lines = text.lines().filter(|l| !l.starts_with('#'));
        let header: Vec<&str> = lines.next().unwrap().split(',').collect();
        assert_eq!(header.first(), Some(&"time_s"));
        assert_eq!(header.last(), Some(&"frame"));
        let mut rows = 0;
        for line in lines {
            let cells: Vec<&str> = line.split(',').collect();
            assert_eq!(cells.len(), header.len(), "{}: {line}", path.display());
            let hex = cells.last().unwrap();
            let frame: Vec<u8> =
                (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect();
            assert_eq!(frame.len(), ecm.runtime_len);
            Pdu::parse(&frame)
                .unwrap_or_else(|e| panic!("{}: bad frame at t={}: {e}", path.display(), cells[0]));
            for (name, logged) in header[1..header.len() - 1].iter().zip(&cells[1..]) {
                let var = def.runtime.get(*name).unwrap_or_else(|| panic!("no channel {name}"));
                let x = var.value(&frame).unwrap();
                let want = match var.kind {
                    RtKind::Bitfield => format!("{}", x as u8),
                    RtKind::Scalar => format_number(x, var.names.decimals.or(Some(4))),
                };
                assert_eq!(&want, logged, "{}: {name} at t={}", path.display(), cells[0]);
            }
            rows += 1;
        }
        assert!(rows > 100, "{}: {rows} rows", path.display());
    }
}
