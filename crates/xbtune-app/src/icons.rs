// SPDX-License-Identifier: GPL-3.0-or-later
//! Icons from Lucide (ISC; see `assets/fonts/Lucide-LICENSE.txt`), drawn
//! as text from a subset of its icon font that sits behind Inter in every
//! font family. Each line names the Lucide icon after `// lucide:`;
//! `tools/subset_lucide.py` reads them and builds `assets/fonts/lucide.ttf`
//! with exactly these glyphs, and a test checks every one is in it.

pub const CONNECT: &str = "\u{e37f}"; // lucide: plug
pub const DASHBOARD: &str = "\u{e1bf}"; // lucide: gauge
pub const DATA: &str = "\u{e2f9}"; // lucide: table-2
pub const CHARTS: &str = "\u{e60d}"; // lucide: chart-spline
pub const DIAGNOSTICS: &str = "\u{e2f1}"; // lucide: stethoscope
pub const RESETS: &str = "\u{e148}"; // lucide: rotate-ccw
pub const TUNE: &str = "\u{e29a}"; // lucide: sliders-horizontal
pub const SETUP: &str = "\u{e245}"; // lucide: settings-2
pub const LOGS: &str = "\u{e313}"; // lucide: file-chart-line
pub const REMOTE: &str = "\u{e404}"; // lucide: radio-tower
pub const SETTINGS: &str = "\u{e154}"; // lucide: settings
pub const HELP: &str = "\u{e101}"; // lucide: life-buoy
pub const BUG: &str = "\u{e20c}"; // lucide: bug
pub const SUN: &str = "\u{e178}"; // lucide: sun
pub const MOON: &str = "\u{e11e}"; // lucide: moon
pub const SUN_MOON: &str = "\u{e2b2}"; // lucide: sun-moon
pub const UNDO: &str = "\u{e2a1}"; // lucide: undo-2
pub const REDO: &str = "\u{e2a0}"; // lucide: redo-2
pub const SAVE: &str = "\u{e14d}"; // lucide: save
pub const OPEN: &str = "\u{e247}"; // lucide: folder-open
pub const DOWNLOAD: &str = "\u{e0b2}"; // lucide: download
pub const BURN: &str = "\u{e0d2}"; // lucide: flame
pub const QUESTION: &str = "\u{e082}"; // lucide: circle-help
pub const CLOSE: &str = "\u{e1b2}"; // lucide: x
pub const PLUS: &str = "\u{e13d}"; // lucide: plus
pub const MINUS: &str = "\u{e11c}"; // lucide: minus
pub const PERCENT: &str = "\u{e132}"; // lucide: percent
pub const EQUAL: &str = "\u{e1bd}"; // lucide: equal
pub const COPY: &str = "\u{e09e}"; // lucide: copy
pub const PASTE: &str = "\u{e3e8}"; // lucide: clipboard-paste
pub const SMOOTH: &str = "\u{e283}"; // lucide: waves
pub const ACROSS: &str = "\u{e1c6}"; // lucide: move-horizontal
pub const DOWN: &str = "\u{e1c7}"; // lucide: move-vertical
pub const CUBE: &str = "\u{e061}"; // lucide: box
pub const COMPARE: &str = "\u{e359}"; // lucide: git-compare
pub const BLUETOOTH: &str = "\u{e05c}"; // lucide: bluetooth
pub const USB: &str = "\u{e356}"; // lucide: usb
pub const PLAY: &str = "\u{e13c}"; // lucide: play
pub const PAUSE: &str = "\u{e12e}"; // lucide: pause
pub const RECORD: &str = "\u{e345}"; // lucide: circle-dot
pub const STOP: &str = "\u{e083}"; // lucide: circle-stop
pub const REFRESH: &str = "\u{e145}"; // lucide: refresh-cw
pub const SEARCH: &str = "\u{e151}"; // lucide: search
pub const LOCK: &str = "\u{e10b}"; // lucide: lock
pub const UNLOCK: &str = "\u{e10c}"; // lucide: lock-open
pub const DISCONNECT: &str = "\u{e45d}"; // lucide: unplug
pub const DEMO: &str = "\u{e485}"; // lucide: monitor-play
pub const WIDEBAND: &str = "\u{e038}"; // lucide: activity
pub const READ: &str = "\u{e4e5}"; // lucide: hard-drive-download

/// `icon` and `text` as one label.
pub fn with(icon: &str, text: &str) -> String {
    format!("{icon}  {text}")
}

#[cfg(test)]
mod tests {
    /// Every icon here is in the bundled subset (see `tools/subset_lucide.py`).
    #[test]
    fn every_icon_is_in_the_font() {
        let font = crate::theme::LUCIDE;
        let src = include_str!("icons.rs");
        let mapped = crate::theme::tests::mapped_chars(font);
        for line in src.lines().filter(|l| l.starts_with("pub const ")) {
            let esc = line.split("\"\\u{").nth(1).and_then(|r| r.split('}').next()).expect("escape");
            let c = char::from_u32(u32::from_str_radix(esc, 16).unwrap()).unwrap();
            assert!(mapped.contains(&c), "not in lucide.ttf: {line}");
        }
    }
}
