// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal UTC time formatting (no date/time dependency needed).

use std::time::{SystemTime, UNIX_EPOCH};

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn parts(t: SystemTime) -> (i64, u32, u32, u64, u64, u64) {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (y, m, d) = civil((secs / 86_400) as i64);
    let s = secs % 86_400;
    (y, m, d, s / 3600, s % 3600 / 60, s % 60)
}

/// RFC 3339 UTC, e.g. `2026-10-14T19:40:05Z`.
pub fn rfc3339(t: SystemTime) -> String {
    let (y, m, d, hh, mm, ss) = parts(t);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Compact UTC stamp for file names, e.g. `20261014-194005`.
pub fn file_stamp(t: SystemTime) -> String {
    let (y, m, d, hh, mm, ss) = parts(t);
    format!("{y:04}{m:02}{d:02}-{hh:02}{mm:02}{ss:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn known_dates() {
        assert_eq!(rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let t = UNIX_EPOCH + Duration::from_secs(1_792_008_005);
        assert_eq!(rfc3339(t), "2026-10-14T20:00:05Z");
        assert_eq!(file_stamp(t), "20261014-200005");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(rfc3339(leap), "2000-02-29T00:00:00Z");
    }
}
