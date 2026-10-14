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

#[test]
fn live_prints_frames() {
    let (ok, out) = cli(&["live", "--sim", "--count", "2", "--channels", "RPM,CLT,pw1"]);
    assert!(ok, "{out}");
    assert_eq!(out.matches("t=").count(), 2, "{out}");
    assert!(out.contains("Engine Speed") && out.contains("Engine Temperature"), "{out}");
    assert!(out.contains(" ms"), "{out}");
    let (ok, out) = cli(&["live", "--sim", "--channels", "nope"]);
    assert!(!ok);
    assert!(out.contains("no channel 'nope'"), "{out}");
}

#[test]
fn log_writes_csv_through_dropouts() {
    let csv = tmp("live.csv");
    let (ok, out) = cli(&[
        "log",
        "--sim",
        "--sim-drop",
        "0.2",
        "--sim-latency-ms",
        "5",
        "--sim-baud",
        "0",
        "--attempts",
        "1",
        "--timeout-ms",
        "50",
        "--duration",
        "2",
        "--note",
        "test ride",
        "-o",
        csv.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("Rows:"), "{out}");
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.starts_with("# XBTune live log\n"), "{text}");
    // Lost replies are noted in the log.
    assert!(text.lines().any(|l| l.starts_with("# event: t=") && l.contains("timeout")), "{text}");
    assert!(text.contains("# ecm_id: BUEIB\n") && text.contains("# note: test ride\n"));
    let rows: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    assert!(rows[0].starts_with("time_s,") && rows[0].ends_with(",frame"), "{}", rows[0]);
    assert!(rows.len() > 20, "{} rows", rows.len());
    let n = rows[0].split(',').count();
    assert!(rows.iter().all(|r| r.split(',').count() == n));
    // Time only goes forward.
    let t: Vec<f64> = rows[1..].iter().map(|r| r.split(',').next().unwrap().parse().unwrap()).collect();
    assert!(t.windows(2).all(|w| w[0] < w[1]));
}

#[test]
fn codes_read_and_clear_on_sim() {
    let (ok, out) = cli(&["codes", "--sim"]);
    assert!(ok, "{out}");
    assert!(out.contains("Current codes:\n   21  AMC open"), "{out}");
    let (ok, out) = cli(&["codes", "--sim", "--clear", "--yes"]);
    assert!(ok, "{out}");
    assert!(out.ends_with("Stored codes:\n  none\n"), "{out}");
}

#[test]
fn active_tests_are_guarded() {
    let (ok, out) = cli(&["test", "fan", "--sim", "--yes", "--sim-test-ms", "300"]);
    assert!(ok, "{out}");
    assert!(out.contains("Fan: done after"), "{out}");
    // Engine running: refused before anything is sent.
    let (ok, out) = cli(&["test", "fan", "--sim", "--yes", "--sim-engine-running"]);
    assert!(!ok);
    assert!(out.contains("the engine is running"), "{out}");
    // Real hardware stays locked (the test environment has no unlock file).
    let (ok, out) = cli(&["test", "fan", "--serial", "/dev/null", "--yes"]);
    assert!(!ok);
    assert!(out.contains("locked on real hardware"), "{out}");
    let (ok, out) = cli(&["test", "warp-drive", "--sim"]);
    assert!(!ok);
    assert!(out.contains("no test 'warp-drive'"), "{out}");
    // Without --yes, anything but "yes" cancels.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xbtune-cli"))
        .args(["test", "fan", "--sim"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("cancelled"));
}
