// SPDX-License-Identifier: GPL-3.0-or-later
//! Smoke tests of the built binary.

use std::process::Command;

fn cli(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_xbtune-cli")).args(args).output().unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    (out.status.success(), text)
}

fn tmp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("xbtune-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

#[test]
fn info_against_sim() {
    let (ok, out) = cli(&["info", "--sim", "--pings", "3"]);
    assert!(ok, "{out}");
    assert!(out.contains("Version:     BUEIB310 12-11-03"), "{out}");
    assert!(out.contains("ECM ID:      BUEIB (DDFI-2)"), "{out}");
    assert!(out.contains("Runtime:     107-byte frame (matches definition)"), "{out}");
    assert!(out.contains("Writes:      disabled"), "{out}");
}

#[test]
fn info_needs_a_target() {
    let (ok, _) = cli(&["info"]);
    assert!(!ok);
}

#[test]
fn ecms_lists_in_scope_ids() {
    let (ok, out) = cli(&["ecms"]);
    assert!(ok);
    assert!(out.contains("BUEIB  DDFI-2"));
    assert!(out.contains("BUEOD  DDFI-3"));
}

#[test]
fn dump_show_convert_diff() {
    let xbt = tmp("dump.xbt");
    let xpr = tmp("BUEIB_dump.xpr");
    let (ok, out) = cli(&["dump", "--sim", "-o", xbt.to_str().unwrap(), "--xpr", xpr.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(out.contains("1210 bytes (incl. page 0)"), "{out}");
    assert!(out.contains("Verified:    yes"), "{out}");
    // The EcmDroid copy is the full xsize buffer.
    assert_eq!(std::fs::metadata(&xpr).unwrap().len(), 1210);

    let (ok, out) = cli(&["show", xbt.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(out.contains("Serial:      204"), "{out}");
    let (ok, out) = cli(&["show", xbt.to_str().unwrap(), "Tab_Fuel_Front"]);
    assert!(ok);
    assert!(out.contains("    255 |    240    243    254"), "{out}");
    let (ok, out) = cli(&["show", xbt.to_str().unwrap(), "--list"]);
    assert!(ok);
    assert!(out.contains("KRPM_Hard_Hi"), "{out}");

    let (ok, out) = cli(&["diff", xbt.to_str().unwrap(), xpr.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(out.contains("Identical (1210 bytes compared)"), "{out}");

    let back = tmp("back.xbt");
    let (ok, out) = cli(&["convert", xpr.to_str().unwrap(), back.to_str().unwrap()]);
    assert!(ok, "{out}");
    // Change one byte of a known variable and check diff names it.
    let mut raw = std::fs::read(&xpr).unwrap();
    raw[0x1d2] = raw[0x1d2].wrapping_add(1); // KRPM_Hard_Hi
    let edited = tmp("BUEIB_edited.xpr");
    std::fs::write(&edited, raw).unwrap();
    let (ok, out) = cli(&["diff", xpr.to_str().unwrap(), edited.to_str().unwrap()]);
    assert!(ok, "{out}");
    assert!(out.contains("KRPM_Hard_Hi: 6800 RPM  ->  6850 RPM"), "{out}");
}

#[test]
fn dump_survives_dropouts() {
    // 30% of replies dropped and one attempt per request: the read keeps
    // failing, reconnecting and resuming.
    let xbt = tmp("lossy.xbt");
    let (ok, out) = cli(&[
        "dump",
        "--sim",
        "--sim-drop",
        "0.3",
        "--attempts",
        "1",
        "--timeout-ms",
        "50",
        "--reconnects",
        "1000",
        "--reconnect-delay-ms",
        "0",
        "-o",
        xbt.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    let (ok, clean) = cli(&["dump", "--sim", "-o", tmp("clean.xbt").to_str().unwrap()]);
    assert!(ok);
    let crc = |s: &str| s.lines().find(|l| l.starts_with("Image:")).unwrap().to_string();
    assert_eq!(crc(&out), crc(&clean));
}

#[test]
fn defs_reports_issues() {
    let (ok, out) = cli(&["defs", "BUEOD"]);
    assert!(ok);
    assert!(out.contains("KVS_RPM_Revs"), "{out}");
    let (ok, out) = cli(&["defs", "BUEIB"]);
    assert!(ok);
    assert!(out.contains("EEPROM variables: "), "{out}");
}
