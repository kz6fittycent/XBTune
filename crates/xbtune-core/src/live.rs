// SPDX-License-Identifier: GPL-3.0-or-later
//! Live (runtime) data: channels, samples, the achieved rate, and CSV logs.
//!
//! Runtime frames are requested with EcmDroid's runtime-data command and
//! decoded through the definition's runtime variables (see `xbtune-defs`).
//! The log format is XBTune's own, documented in `docs/FILE_FORMATS.md`.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::{Duration, Instant, SystemTime};

use xbtune_defs::{Definition, RtKind, RtVar, format_number, unit_symbol};

/// Log format version written to the header.
pub const LOG_FORMAT: u32 = 1;

/// The channels of a definition in frame order (then by name), as a stable
/// list for tables, logs and pickers.
pub fn channels(def: &Definition) -> Vec<RtVar> {
    let mut v: Vec<RtVar> = def.runtime.values().cloned().collect();
    v.sort_by(|a, b| a.offset.cmp(&b.offset).then_with(|| a.name.cmp(&b.name)));
    v
}

/// Channels shown first by default, by EcmDroid channel name; ones a
/// definition lacks are skipped.
pub const DEFAULT_CHANNELS: &[&str] = &[
    "RPM",
    "TPD",
    "TPP",
    "spark1",
    "spark2",
    "pw1",
    "pw2",
    "CLT",
    "MAT",
    "Batt. Voltg.",
    "O2",
    "EGO Corr.",
    "AFV",
];

/// Display units for a channel: EcmDroid's short symbol if there is one,
/// else XBTune's abbreviation, else the database text.
pub fn units(var: &RtVar) -> String {
    let Some(u) = &var.names.units else { return String::new() };
    let short = unit_symbol(u).or(match u.as_str() {
        "Milliseconds" => Some("ms"),
        "Seconds" => Some("s"),
        "Fahrenheit" => Some("°F"),
        _ => None,
    });
    short.map(str::to_string).unwrap_or_else(|| u.clone())
}

/// A value formatted with the channel's decimals.
pub fn format_value(var: &RtVar, v: f64) -> String {
    match var.kind {
        RtKind::Bitfield => format!("{:08b}", v as u8),
        RtKind::Scalar => format_number(v, var.names.decimals),
    }
}

/// Names of the flags that are set, for a bitfield channel.
pub fn set_flags(var: &RtVar, v: f64) -> Vec<String> {
    let Some(bits) = &var.bits else { return Vec::new() };
    let byte = v as u8;
    bits.bits
        .iter()
        .filter(|b| !b.name.is_empty() && byte & (1 << b.nr) != 0)
        .map(|b| b.name.clone())
        .collect()
}

/// One runtime frame and when it arrived.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Seconds since the session started polling.
    pub t: f64,
    pub wall: SystemTime,
    /// The whole reply frame; channel offsets are into it.
    pub frame: Vec<u8>,
}

impl Sample {
    pub fn value(&self, var: &RtVar) -> Option<f64> {
        var.value(&self.frame)
    }
}

/// Samples per second over a sliding window.
#[derive(Debug, Clone)]
pub struct RateMeter {
    window: Duration,
    times: VecDeque<Instant>,
}

impl Default for RateMeter {
    fn default() -> Self {
        Self::new(Duration::from_secs(3))
    }
}

impl RateMeter {
    pub fn new(window: Duration) -> Self {
        Self { window, times: VecDeque::new() }
    }

    pub fn tick(&mut self, now: Instant) {
        self.times.push_back(now);
        while let Some(&first) = self.times.front() {
            if now.duration_since(first) > self.window {
                self.times.pop_front();
            } else {
                break;
            }
        }
    }

    /// Samples per second, 0 until there are two samples.
    pub fn hz(&self) -> f64 {
        match (self.times.front(), self.times.back()) {
            (Some(a), Some(b)) if self.times.len() > 1 => {
                let span = b.duration_since(*a).as_secs_f64();
                if span > 0.0 { (self.times.len() - 1) as f64 / span } else { 0.0 }
            }
            _ => 0.0,
        }
    }
}

/// What a log header records.
#[derive(Debug, Clone)]
pub struct LogMeta {
    pub ecm_id: String,
    pub version: String,
    pub target: String,
    pub started: SystemTime,
    pub note: Option<String>,
}

/// Writes samples as CSV: a `#` header block, a column row, then one row
/// per sample with every channel and the raw frame in hex.
pub struct CsvLogger<W: Write> {
    out: W,
    vars: Vec<RtVar>,
    rows: u64,
}

impl<W: Write> CsvLogger<W> {
    pub fn new(mut out: W, meta: &LogMeta, vars: Vec<RtVar>) -> io::Result<Self> {
        writeln!(out, "# XBTune live log")?;
        writeln!(out, "# format: xbtune-log {LOG_FORMAT}")?;
        writeln!(out, "# app: xbtune {}", env!("CARGO_PKG_VERSION"))?;
        writeln!(out, "# ecm_id: {}", meta.ecm_id)?;
        writeln!(out, "# version: {}", meta.version)?;
        writeln!(out, "# target: {}", meta.target)?;
        writeln!(out, "# started: {}", crate::time::rfc3339(meta.started))?;
        if let Some(n) = &meta.note {
            writeln!(out, "# note: {}", n.replace(['\n', '\r'], " "))?;
        }
        for v in &vars {
            writeln!(out, "# channel: {} | {} | {}", v.name, v.label(), units(v))?;
        }
        let mut cols = vec!["time_s".to_string()];
        cols.extend(vars.iter().map(|v| csv_field(&v.name)));
        cols.push("frame".into());
        writeln!(out, "{}", cols.join(","))?;
        Ok(Self { out, vars, rows: 0 })
    }

    pub fn write(&mut self, s: &Sample) -> io::Result<()> {
        let mut row = format!("{:.3}", s.t);
        for v in &self.vars {
            row.push(',');
            if let Some(x) = v.value(&s.frame) {
                row.push_str(&match v.kind {
                    RtKind::Bitfield => format!("{}", x as u8),
                    RtKind::Scalar => format_number(x, v.names.decimals.or(Some(4))),
                });
            }
        }
        row.push(',');
        row.push_str(&frame_hex(&s.frame));
        writeln!(self.out, "{row}")?;
        self.rows += 1;
        Ok(())
    }

    /// A `# event: t=<s> <what>` line (link trouble, reconnects).
    pub fn event(&mut self, t: f64, what: &str) -> io::Result<()> {
        writeln!(self.out, "# event: t={t:.3} {}", what.replace(['\n', '\r'], " "))
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    pub fn into_inner(self) -> W {
        self.out
    }
}

/// The raw frame as uppercase hex without separators.
pub fn frame_hex(frame: &[u8]) -> String {
    frame.iter().map(|b| format!("{b:02X}")).collect()
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_defs::Catalog;

    fn bueib() -> std::sync::Arc<Definition> {
        let c = Catalog::builtin().unwrap();
        c.definition(c.lookup("BUEIB").unwrap()).unwrap().clone()
    }

    #[test]
    fn channels_are_ordered_and_defaults_exist() {
        let def = bueib();
        let ch = channels(&def);
        assert_eq!(ch.len(), 70);
        assert!(ch.windows(2).all(|w| w[0].offset <= w[1].offset));
        for name in ["RPM", "spark1", "pw1", "CLT", "EGO Corr.", "AFV"] {
            assert!(ch.iter().any(|v| v.name == name), "{name}");
        }
    }

    #[test]
    fn csv_has_header_and_rows() {
        let def = bueib();
        let frame = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin").to_vec();
        let vars = channels(&def);
        let meta = LogMeta {
            ecm_id: "BUEIB".into(),
            version: "BUEIB310 12-11-03".into(),
            target: "sim".into(),
            started: std::time::UNIX_EPOCH,
            note: Some("a, b\nc".into()),
        };
        let mut log = CsvLogger::new(Vec::new(), &meta, vars.clone()).unwrap();
        log.write(&Sample { t: 0.5, wall: std::time::UNIX_EPOCH, frame: frame.clone() }).unwrap();
        log.event(0.75, "1 failed attempt(s) (timeout 1)\nx").unwrap();
        let text = String::from_utf8(log.into_inner()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.contains(&"# note: a, b c"));
        assert!(lines.contains(&"# channel: RPM | Engine Speed | RPM"));
        let header = lines.iter().find(|l| l.starts_with("time_s,")).unwrap();
        assert_eq!(lines.last(), Some(&"# event: t=0.750 1 failed attempt(s) (timeout 1) x"));
        let row = lines.iter().rev().find(|l| !l.starts_with('#')).unwrap();
        let cols: Vec<&str> = header.split(',').collect();
        let vals: Vec<&str> = row.split(',').collect();
        assert_eq!(cols.len(), vals.len());
        assert_eq!(cols.len(), vars.len() + 2);
        assert_eq!(vals[0], "0.500");
        assert_eq!(*vals.last().unwrap(), frame_hex(&frame));
        assert!(vals.last().unwrap().starts_with("01"));
        let rpm = cols.iter().position(|c| *c == "RPM").unwrap();
        let want = def.runtime["RPM"].value(&frame).unwrap();
        assert_eq!(vals[rpm].parse::<f64>().unwrap(), want);
    }

    #[test]
    fn rate_meter() {
        let mut m = RateMeter::new(Duration::from_secs(3));
        let t0 = Instant::now();
        for i in 0..31 {
            m.tick(t0 + Duration::from_millis(100 * i));
        }
        assert!((m.hz() - 10.0).abs() < 0.01, "{}", m.hz());
    }
}
