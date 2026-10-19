// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening whatever the user hands XBTune (owner's request, 2026-10-19):
//! files dropped on the window (egui's events on X11, `wayland_dnd` on
//! Wayland) and files named on the command line (`xbtune ride.csv`, or
//! "Open With" in the file manager). Tunes (.xbt, .xpr) open in Tune, or
//! as the comparison when the open tune has unsaved edits; logs (XBTune
//! .csv, EcmDroid .bin, .msl) open in Logs.

use std::path::{Path, PathBuf};

use crate::app::{View, XbApp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tune,
    Log,
    Unknown,
}

/// What a file is, by its extension (both kinds include binary formats,
/// so content alone can't tell an .xpr from a .bin).
pub fn kind(path: &Path) -> Kind {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("xbt" | "xpr") => Kind::Tune,
        Some("csv" | "bin" | "msl") => Kind::Log,
        _ => Kind::Unknown,
    }
}

/// Open each file as what it is; say what happened.
pub fn open_paths(app: &mut XbApp, paths: &[PathBuf]) {
    let mut view = None;
    let mut skipped = Vec::new();
    for p in paths {
        if p.is_dir() {
            skipped.push(format!("{} (a folder)", p.display()));
            continue;
        }
        match kind(p) {
            Kind::Tune => {
                let compare = app.tune.as_ref().is_some_and(|t| t.is_modified());
                crate::tune_view::open(app, p, compare);
                if compare {
                    app.say(
                        format!("Your tune has unsaved edits, so {} opened for comparison.", p.display()),
                        false,
                    );
                }
                view.get_or_insert(View::Tune);
            }
            Kind::Log => {
                let before = app.logs.logs.len();
                crate::logs_view::add(app, p);
                if app.logs.logs.len() > before {
                    view = Some(View::Logs);
                }
            }
            Kind::Unknown => skipped.push(p.display().to_string()),
        }
    }
    tracing::info!(files = paths.len(), skipped = skipped.len(), "files opened by drop or command line");
    if !skipped.is_empty() {
        app.say(
            format!(
                "XBTune opens tunes (.xbt, .xpr) and logs (.csv, .bin, .msl); not: {}",
                skipped.join(", ")
            ),
            true,
        );
    }
    if let Some(v) = view {
        app.view = v;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(kind(Path::new("BUEIB_x.XPR")), Kind::Tune);
        assert_eq!(kind(Path::new("a.xbt")), Kind::Tune);
        assert_eq!(kind(Path::new("ride.csv")), Kind::Log);
        assert_eq!(kind(Path::new("ed.bin")), Kind::Log);
        assert_eq!(kind(Path::new("notes.txt")), Kind::Unknown);
    }
}
