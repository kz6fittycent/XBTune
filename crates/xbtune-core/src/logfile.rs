// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading XBTune live logs (`xbtune-log 1`, docs/FILE_FORMATS.md) back in.
//! `Log::open` also reads EcmDroid's binary logs and `.msl` logs
//! (`crate::logformats`).
//!
//! Only `time_s` and the raw `frame` of each row are used: the frame is
//! decoded again with the current definition, so a log reads the same as
//! it did live (and benefits from corrected definitions). The other columns
//! are for spreadsheets. `# event:` lines are kept, for the log viewer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use xbtune_defs::{Catalog, Definition, EcmDef, RtVar};

use crate::live::{Sample, channels};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}: not an XBTune log (no '# format: xbtune-log 1' line)")]
    NotALog(PathBuf),
    #[error("{0}: unknown ECM version '{1}', so the frames can't be decoded")]
    UnknownEcm(PathBuf, String),
    #[error("{0}: no column row with time_s and frame (or Time, in an .msl log)")]
    NoColumns(PathBuf),
    #[error("{0}: the log doesn't say which ECM it's from, so it can't be decoded")]
    NoEcmId(PathBuf),
}

/// Which kind of file a log came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogSource {
    /// XBTune's CSV (`xbtune-log 1`): raw frames.
    #[default]
    Xbtune,
    /// EcmDroid's binary log: raw frames.
    EcmDroidBin,
    /// MegaLogViewer text: values only; frames rebuilt from the columns.
    Msl,
}

impl LogSource {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Xbtune => "XBTune log",
            Self::EcmDroidBin => "EcmDroid binary log",
            Self::Msl => "MegaLogViewer .msl log (values only)",
        }
    }
}

/// A `# event:` line.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEvent {
    pub t: f64,
    pub text: String,
}

/// One log file, decoded.
#[derive(Debug, Clone)]
pub struct Log {
    pub path: PathBuf,
    pub ecm: EcmDef,
    pub version: String,
    pub definition: Arc<Definition>,
    pub channels: Vec<RtVar>,
    pub started: Option<String>,
    pub note: Option<String>,
    pub samples: Vec<Sample>,
    pub events: Vec<LogEvent>,
    /// Rows that couldn't be read (bad hex, wrong frame length, bad checksum).
    pub skipped: usize,
    pub source: LogSource,
}

impl Log {
    /// Open an XBTune CSV log, an EcmDroid binary log or an `.msl` log
    /// (told apart by their content).
    pub fn open(path: &Path, catalog: &Catalog) -> Result<Self, LogError> {
        let bytes = std::fs::read(path).map_err(|e| LogError::Io(path.to_path_buf(), e))?;
        if crate::logformats::looks_like_bin(&bytes) {
            return crate::logformats::parse_bin(path, &bytes, catalog);
        }
        let text = String::from_utf8_lossy(&bytes);
        if crate::logformats::looks_like_msl(&text) {
            return crate::logformats::parse_msl(path, &text, catalog);
        }
        Self::parse(path, &text, catalog)
    }

    pub fn parse(path: &Path, text: &str, catalog: &Catalog) -> Result<Self, LogError> {
        let p = || path.to_path_buf();
        let mut format_ok = false;
        let (mut version, mut started, mut note) = (None, None, None);
        let mut events = Vec::new();
        let mut columns: Option<(usize, usize)> = None;
        let mut header_seen = false;
        let mut rows: Vec<(f64, &str)> = Vec::new();
        let mut skipped = 0;
        for line in text.lines() {
            if let Some(h) = line.strip_prefix('#') {
                let h = h.trim();
                if h == "format: xbtune-log 1" {
                    format_ok = true;
                } else if let Some(v) = h.strip_prefix("version:") {
                    version = Some(v.trim().to_string());
                } else if let Some(v) = h.strip_prefix("started:") {
                    started = Some(v.trim().to_string());
                } else if let Some(v) = h.strip_prefix("note:") {
                    note = Some(v.trim().to_string());
                } else if let Some(e) = h.strip_prefix("event:") {
                    let e = e.trim();
                    if let Some((t, rest)) = e.strip_prefix("t=").and_then(|r| r.split_once(' '))
                        && let Ok(t) = t.parse()
                    {
                        events.push(LogEvent { t, text: rest.to_string() });
                    }
                }
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            match columns {
                None if header_seen => {}
                None => {
                    header_seen = true;
                    let names = split_header(line);
                    let time = names.iter().position(|n| n == "time_s");
                    let frame = names.iter().position(|n| n == "frame");
                    columns = time.zip(frame);
                }
                Some((ti, fi)) => {
                    // Data rows hold numbers and hex only: no quoting.
                    let cells: Vec<&str> = line.split(',').collect();
                    match (cells.get(ti).and_then(|t| t.parse::<f64>().ok()), cells.get(fi)) {
                        (Some(t), Some(f)) if !f.is_empty() => rows.push((t, f)),
                        _ => skipped += 1,
                    }
                }
            }
        }
        if !format_ok {
            return Err(LogError::NotALog(p()));
        }
        let version = version.unwrap_or_default();
        let ecm =
            catalog.lookup(&version).cloned().ok_or_else(|| LogError::UnknownEcm(p(), version.clone()))?;
        let definition =
            catalog.definition(&ecm).cloned().ok_or_else(|| LogError::UnknownEcm(p(), version.clone()))?;
        if columns.is_none() {
            return Err(LogError::NoColumns(p()));
        }
        let mut samples = Vec::with_capacity(rows.len());
        for (t, hex) in rows {
            match from_hex(hex) {
                Some(frame) if frame.len() == ecm.runtime_len => {
                    samples.push(Sample { t, wall: SystemTime::UNIX_EPOCH, frame })
                }
                _ => skipped += 1,
            }
        }
        let channels = channels(&definition);
        Ok(Self {
            path: p(),
            ecm,
            version,
            definition,
            channels,
            started,
            note,
            samples,
            events,
            skipped,
            source: LogSource::Xbtune,
        })
    }

    pub fn channel(&self, name: &str) -> Option<&RtVar> {
        self.channels.iter().find(|v| v.name == name)
    }

    /// Seconds from the first to the last sample.
    pub fn duration(&self) -> f64 {
        match (self.samples.first(), self.samples.last()) {
            (Some(a), Some(b)) => b.t - a.t,
            _ => 0.0,
        }
    }

    /// Samples per second over the whole log.
    pub fn rate(&self) -> f64 {
        let d = self.duration();
        if d > 0.0 { (self.samples.len().saturating_sub(1)) as f64 / d } else { 0.0 }
    }

    /// One channel's values (`None` where a frame lacks it).
    pub fn series(&self, name: &str) -> Vec<Option<f64>> {
        match self.channel(name) {
            Some(v) => self.samples.iter().map(|s| s.value(v)).collect(),
            None => vec![None; self.samples.len()],
        }
    }

    /// The file name, for lists.
    pub fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// Split the column row, honouring `"` quoting (names with `,` or `"`).
fn split_header(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{CsvLogger, LogMeta};

    #[test]
    fn written_log_reads_back_the_same_samples() {
        let catalog = Catalog::builtin().unwrap();
        let ecm = catalog.lookup("BUEIB310 12-11-03").unwrap().clone();
        let def = catalog.definition(&ecm).unwrap().clone();
        let meta = LogMeta {
            ecm_id: ecm.id.clone(),
            version: "BUEIB310 12-11-03".into(),
            target: "sim".into(),
            started: SystemTime::UNIX_EPOCH,
            note: Some("a, \"quoted\" note".into()),
        };
        let mut w = CsvLogger::new(Vec::new(), &meta, channels(&def)).unwrap();
        let frame = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin").to_vec();
        let written: Vec<Sample> = (0..5)
            .map(|i| Sample { t: 0.15 * i as f64, wall: SystemTime::UNIX_EPOCH, frame: frame.clone() })
            .collect();
        for (i, s) in written.iter().enumerate() {
            w.write(s).unwrap();
            if i == 2 {
                w.event(s.t, "1 failed attempt(s) (timeout 1)").unwrap();
            }
        }
        let text = String::from_utf8(w.into_inner()).unwrap();
        let log = Log::parse(Path::new("t.csv"), &text, &catalog).unwrap();
        assert_eq!(log.samples.len(), 5);
        assert_eq!(log.samples[3].frame, frame);
        assert!((log.samples[4].t - 0.6).abs() < 1e-9);
        assert_eq!(log.events, vec![LogEvent { t: 0.3, text: "1 failed attempt(s) (timeout 1)".into() }]);
        assert_eq!(log.skipped, 0);
        assert_eq!(log.ecm.id, "BUEIB");
        let rpm = log.channel("RPM").unwrap();
        assert_eq!(log.samples[0].value(rpm), written[0].value(rpm));
    }

    #[test]
    fn owner_logs_decode() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/owner");
        let catalog = Catalog::builtin().unwrap();
        for name in ["idle.csv", "BUEIB_20261014-231006.csv"] {
            let path = dir.join(name);
            if !path.exists() {
                eprintln!("skipping: {} not present", path.display());
                continue;
            }
            let log = Log::open(&path, &catalog).unwrap();
            assert!(log.samples.len() > 200, "{name}: {}", log.samples.len());
            assert_eq!(log.skipped, 0, "{name}");
            assert!(log.rate() > 3.0 && log.rate() < 8.0, "{name}: {} Hz", log.rate());
            let rpm: Vec<f64> = log.series("RPM").into_iter().flatten().collect();
            assert!(rpm.iter().all(|r| (900.0..3000.0).contains(r)), "{name}");
        }
    }

    #[test]
    fn not_a_log_and_bad_rows() {
        let catalog = Catalog::builtin().unwrap();
        assert!(matches!(Log::parse(Path::new("x.csv"), "a,b\n1,2\n", &catalog), Err(LogError::NotALog(_))));
        let text = "# format: xbtune-log 1\n# version: NOPE\ntime_s,frame\n";
        assert!(matches!(Log::parse(Path::new("x.csv"), text, &catalog), Err(LogError::UnknownEcm(..))));
    }
}
