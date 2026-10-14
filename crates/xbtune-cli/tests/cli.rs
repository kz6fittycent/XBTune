// SPDX-License-Identifier: GPL-3.0-or-later
//! Smoke tests of the built binary.

use std::process::Command;

fn cli(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_xbtune-cli")).args(args).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned())
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
