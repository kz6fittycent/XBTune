// SPDX-License-Identifier: GPL-3.0-or-later
//! Other log formats: EcmDroid's binary logs (`.bin`) and MegaLogViewer
//! text logs (`.msl`), read into a [`Log`]; and `.msl` export.
//!
//! The binary layout is ported from EcmDroid
//! `app/src/main/java/org/ecmdroid/EcmDroidService.java` (`startRecording`,
//! `logPacket`) and `app/src/main/java/org/ecmdroid/util/Bin2MslConverter.java`
//! (commit 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e).
//! Copyright (C) 2012 by Michel Marti; Bin2Msl Converter Copyright (C) 2013
//! by Gunter Baumann. Rust port Copyright (C) 2026 XBTune contributors.
//! Licensed GPL-3.0-or-later.
//!
//! Binary log: 5 ASCII bytes, the EEPROM's ECM ID (`UNKWN` when EcmDroid
//! hadn't read the EEPROM), then records of a big-endian `i32` time in
//! 1/100 s since recording started, followed by the whole runtime reply
//! frame (SOH first, XOR checksum of bytes 1..len-1 last). The frame
//! length is the ECM's runtime length. Records whose checksum fails are
//! skipped, as in `Bin2MslConverter`.
//!
//! `.msl`: a quoted title line, a tab-separated column row, then rows of
//! numbers (EcmDroid writes no units row). XBTune finds the ECM ID in the
//! title, takes the `Time` column, and rebuilds a frame from the columns
//! whose names are the definition's runtime channels (EcmDroid and XBTune
//! both use the database's channel names). Values are rounded in the file,
//! so a rebuilt frame matches the original only to the column's precision;
//! the log keeps only the channels the file has, so missing ones read as
//! missing, not 0.

use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

use xbtune_defs::{Catalog, RtKind, RtVar};

use crate::live::{Sample, channels};
use crate::logfile::{Log, LogError, LogSource};

/// EcmDroid's header when it hadn't identified the ECM.
const UNKNOWN_ID: &[u8] = b"UNKWN";

/// Whether `bytes` look like an EcmDroid binary log: an ECM ID, then binary.
pub fn looks_like_bin(bytes: &[u8]) -> bool {
    bytes.len() >= 5 && bytes[..5].iter().all(u8::is_ascii_alphanumeric) && bytes.get(5) != Some(&b'\n')
}

/// Whether `text` looks like an `.msl` log: a quoted title line.
pub fn looks_like_msl(text: &str) -> bool {
    text.trim_start_matches('\u{feff}').starts_with('"')
}

pub fn parse_bin(path: &Path, bytes: &[u8], catalog: &Catalog) -> Result<Log, LogError> {
    let p = || path.to_path_buf();
    if bytes.len() < 5 || &bytes[..5] == UNKNOWN_ID {
        return Err(LogError::NoEcmId(p()));
    }
    let id = String::from_utf8_lossy(&bytes[..5]).into_owned();
    let ecm = catalog.lookup(&id).cloned().ok_or_else(|| LogError::UnknownEcm(p(), id.clone()))?;
    let definition =
        catalog.definition(&ecm).cloned().ok_or_else(|| LogError::UnknownEcm(p(), id.clone()))?;
    let len = ecm.runtime_len;
    let mut samples = Vec::new();
    let mut skipped = 0;
    let mut records = bytes[5..].chunks_exact(4 + len);
    for r in records.by_ref() {
        let t = i32::from_be_bytes([r[0], r[1], r[2], r[3]]) as f64 / 100.0;
        let frame = &r[4..];
        let sum = frame[1..len - 1].iter().fold(0u8, |a, b| a ^ b);
        if sum != frame[len - 1] {
            skipped += 1;
            continue;
        }
        samples.push(Sample { t, wall: SystemTime::UNIX_EPOCH, frame: frame.to_vec() });
    }
    if !records.remainder().is_empty() {
        skipped += 1;
    }
    let channels = channels(&definition);
    Ok(Log {
        path: p(),
        ecm,
        version: id,
        definition,
        channels,
        started: None,
        note: None,
        samples,
        events: Vec::new(),
        skipped,
        source: LogSource::EcmDroidBin,
    })
}

pub fn parse_msl(path: &Path, text: &str, catalog: &Catalog) -> Result<Log, LogError> {
    let p = || path.to_path_buf();
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    let title = lines.next().unwrap_or_default().trim().trim_matches('"').to_string();
    // The ECM ID is a word of the title ("EcmDroid/Bin2Msl BUE2D").
    let ecm = title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 5)
        .find_map(|w| catalog.lookup(w).filter(|e| e.id == w[..5]))
        .cloned()
        .ok_or_else(|| LogError::NoEcmId(p()))?;
    let definition =
        catalog.definition(&ecm).cloned().ok_or_else(|| LogError::UnknownEcm(p(), ecm.id.clone()))?;
    let names: Vec<String> =
        lines.next().unwrap_or_default().split('\t').map(|s| s.trim().to_string()).collect();
    let time = names.iter().position(|n| n == "Time").ok_or_else(|| LogError::NoColumns(p()))?;
    let all = channels(&definition);
    // Column -> channel; a name that appears twice uses its last column
    // (EcmDroid puts its own computed "Engine" before the ECM's).
    let mut cols: Vec<(usize, RtVar)> = Vec::new();
    for v in &all {
        if let Some(i) = names.iter().rposition(|n| *n == v.name) {
            cols.push((i, v.clone()));
        }
    }
    // Some channels share bytes (BAS ADC and BAS Voltg.). Printed values
    // are rounded, so the one with the largest scale is the most exact in
    // raw terms: write it last.
    cols.sort_by(|a, b| a.1.scale.abs().total_cmp(&b.1.scale.abs()));
    let mut samples = Vec::new();
    let mut skipped = 0;
    for (n, line) in lines.enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        let Some(t) = cells.get(time).and_then(|c| c.trim().parse::<f64>().ok()) else {
            // The first row may be units (MegaLogViewer allows one).
            skipped += usize::from(n > 0);
            continue;
        };
        let mut frame = vec![0u8; ecm.runtime_len];
        for (i, v) in &cols {
            if let Some(x) = cells.get(*i).and_then(|c| c.trim().parse::<f64>().ok()) {
                put(&mut frame, v, x);
            }
        }
        samples.push(Sample { t, wall: SystemTime::UNIX_EPOCH, frame });
    }
    let channels: Vec<RtVar> =
        all.into_iter().filter(|v| cols.iter().any(|(_, c)| c.name == v.name)).collect();
    Ok(Log {
        path: p(),
        version: ecm.id.clone(),
        ecm,
        definition,
        channels,
        started: None,
        note: Some(title),
        samples,
        events: Vec::new(),
        skipped,
        source: LogSource::Msl,
    })
}

/// Store `value` (engineering units) into the frame at `var`'s place.
fn put(frame: &mut [u8], var: &RtVar, value: f64) {
    if var.offset + var.size > frame.len() || var.size == 0 || var.size > 4 {
        return;
    }
    let raw = match var.kind {
        RtKind::Bitfield => value,
        RtKind::Scalar if var.scale != 0.0 => (value - var.translate) / var.scale,
        RtKind::Scalar => return,
    };
    let max = if var.size == 4 { u32::MAX as f64 } else { ((1u64 << (8 * var.size)) - 1) as f64 };
    let raw = raw.round().clamp(0.0, max) as u32;
    for i in 0..var.size {
        frame[var.offset + i] = (raw >> (8 * i)) as u8;
    }
}

/// Write `log` as an `.msl` (MegaLogViewer) log: title, column row, rows.
/// Like EcmDroid's, no units row.
pub fn write_msl<W: Write>(log: &Log, mut out: W) -> io::Result<()> {
    write!(out, "\"XBTune {} {}\"\r\n", log.ecm.id, log.version)?;
    let mut head = vec!["Time".to_string()];
    head.extend(log.channels.iter().map(|v| v.name.replace(['\t', '\r', '\n'], " ")));
    write!(out, "{}\r\n", head.join("\t"))?;
    let t0 = log.samples.first().map_or(0.0, |s| s.t);
    for s in &log.samples {
        let mut row = format!("{:.3}", s.t - t0);
        for v in &log.channels {
            row.push('\t');
            if let Some(x) = s.value(v) {
                row.push_str(&match v.kind {
                    RtKind::Bitfield => format!("{}", x as u32),
                    RtKind::Scalar => trim_number(x),
                });
            }
        }
        write!(out, "{row}\r\n")?;
    }
    out.flush()
}

/// Up to 3 decimals, without trailing zeros.
fn trim_number(x: f64) -> String {
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

/// Shared definitions for the tests.
#[cfg(test)]
fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ecmdroid").join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(name: &str) -> Log {
        Log::open(&fixture(name), &Catalog::builtin().unwrap()).unwrap()
    }

    #[test]
    fn ecmdroid_binary_logs_decode() {
        let log = open("BUEIB_log_2000.bin");
        assert_eq!((log.ecm.id.as_str(), log.source), ("BUEIB", LogSource::EcmDroidBin));
        assert_eq!(log.samples.len() + log.skipped, 2000);
        assert!(log.skipped < 20, "{}", log.skipped);
        assert!((0.17..0.19).contains(&log.samples[0].t), "{}", log.samples[0].t);
        assert!(log.samples.windows(2).all(|w| w[1].t >= w[0].t));
        let batt: Vec<f64> = log.series("Batt. Voltg.").into_iter().flatten().collect();
        assert!(batt.iter().all(|v| (10.0..16.0).contains(v)), "{:?}", &batt[..5]);
        let log = open("BUE2D_log.bin");
        assert_eq!(log.ecm.runtime_len, 135);
        assert_eq!(log.samples.len() + log.skipped, 2153);
    }

    #[test]
    fn ecmdroid_msl_matches_its_columns() {
        let log = open("BUE2D_log.msl");
        assert_eq!((log.ecm.id.as_str(), log.source), ("BUE2D", LogSource::Msl));
        assert_eq!(log.samples.len(), 176);
        assert_eq!(log.skipped, 0);
        // Spot values of the first row, read back through rebuilt frames.
        let first = |n: &str| log.series(n)[0].unwrap();
        assert_eq!(first("Batt. Voltg."), 11.75);
        // EcmDroid writes whole-number columns truncated (CLT 7 for 7.2 °C).
        assert!((first("CLT") - 7.0).abs() < 1.0, "{}", first("CLT"));
        assert!((first("O2") - 0.49).abs() < 0.005);
        assert_eq!(first("AFV"), 100.0);
        // Columns the file doesn't have are missing, not 0.
        assert!(log.channels.len() > 50 && log.channels.len() < channels(&log.definition).len() + 1);
    }

    #[test]
    fn ecmdroid_msl_and_bin_agree() {
        // EcmDroid's test .msl is its converter's output for the first 176
        // records of BUE2D_log.bin (made by an older converter: its times
        // are 1/10 of the current one's). Every column it shares with the
        // definition must agree with the frame, within the column's
        // rounding (whole-number columns are truncated).
        let (msl, bin) = (open("BUE2D_log.msl"), open("BUE2D_log.bin"));
        let mut bad = Vec::new();
        for v in &msl.channels {
            let tol = 1.0 + v.scale.abs();
            for (a, b) in msl.samples.iter().zip(&bin.samples) {
                let (x, y) = (a.value(v).unwrap(), b.value(v).unwrap());
                if (x - y).abs() > tol {
                    bad.push(format!("{}: msl {x} bin {y}", v.name));
                    break;
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
        assert!((msl.samples[10].t * 10.0 - bin.samples[10].t).abs() < 0.01);
    }

    #[test]
    fn msl_export_reads_back() {
        let log = open("BUEIB_log_2000.bin");
        let mut out = Vec::new();
        write_msl(&log, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\"XBTune BUEIB BUEIB\"\r\nTime\t"));
        let back = parse_msl(Path::new("x.msl"), &text, &Catalog::builtin().unwrap()).unwrap();
        assert_eq!(back.samples.len(), log.samples.len());
        let t0 = log.samples[0].t;
        for name in ["RPM", "CLT", "Batt. Voltg.", "O2", "EGO Corr.", "TPD", "Flags1"] {
            let v = log.channel(name).unwrap();
            for (a, b) in log.samples.iter().zip(&back.samples).step_by(97) {
                let (x, y) = (a.value(v).unwrap(), b.value(v).unwrap());
                assert!((x - y).abs() <= v.scale.abs() / 2.0 + 1e-9, "{name}: {x} vs {y}");
                assert!((a.t - t0 - b.t).abs() < 0.001);
            }
        }
    }

    #[test]
    fn unknown_ids_and_junk_are_refused() {
        let c = Catalog::builtin().unwrap();
        let p = Path::new("x.bin");
        assert!(matches!(parse_bin(p, b"UNKWN\0\0\0\0", &c), Err(LogError::NoEcmId(_))));
        assert!(matches!(parse_bin(p, b"ZZZZZ\0\0\0\0", &c), Err(LogError::UnknownEcm(..))));
        assert!(matches!(
            parse_msl(p, "\"no id here\"\r\nTime\tRPM\r\n0\t1\r\n", &c),
            Err(LogError::NoEcmId(_))
        ));
        // A truncated last record counts as skipped.
        let mut b = std::fs::read(fixture("BUEIB_log_2000.bin")).unwrap();
        b.truncate(5 + 3 * 111 + 50);
        let log = parse_bin(p, &b, &c).unwrap();
        assert_eq!((log.samples.len() + log.skipped - 1, log.skipped >= 1), (3, true));
    }
}
