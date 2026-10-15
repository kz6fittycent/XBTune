// SPDX-License-Identifier: GPL-3.0-or-later
//! App settings, saved as TOML in the XBTune config folder
//! (`$SNAP_USER_COMMON/config` in the snap).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::theme::ThemePref;

/// Where the ECM was last reached. `race`: a factory race ECM (address
/// 0x55, 19200 baud; EcmDroid's "Factory Race" protocol). Settings saved
/// before it existed read as stock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedTarget {
    Bluetooth {
        address: String,
        channel: u8,
        name: Option<String>,
        #[serde(default)]
        race: bool,
    },
    Serial {
        path: String,
        #[serde(default)]
        race: bool,
    },
    /// The built-in simulated ECM (a BUEIB, or a B2RIB when `race`).
    Demo {
        #[serde(default)]
        race: bool,
    },
}

impl SavedTarget {
    pub fn label(&self) -> String {
        let base = match self {
            Self::Bluetooth { address, name, .. } => match name {
                Some(n) => format!("{n} ({address})"),
                None => address.clone(),
            },
            Self::Serial { path, .. } => path.clone(),
            Self::Demo { .. } => "Demo (simulated ECM)".into(),
        };
        if self.race() { format!("{base}, factory race ECM") } else { base }
    }

    pub fn race(&self) -> bool {
        match self {
            Self::Bluetooth { race, .. } | Self::Serial { race, .. } | Self::Demo { race } => *race,
        }
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Self::Demo { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemePref,
    pub last: Option<SavedTarget>,
    /// Connect to `last` when the app starts.
    pub auto_connect: bool,
    /// Dashboard gauges, by channel name.
    pub gauges: Vec<String>,
    /// Charted channels, by channel name.
    pub chart_channels: Vec<String>,
    /// Seconds of history shown in charts.
    pub chart_seconds: f64,
    /// Tune file to reopen at start.
    pub last_tune: Option<std::path::PathBuf>,
    /// Show "Getting started" when the app starts.
    pub show_wizard: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePref::System,
            last: None,
            auto_connect: false,
            gauges: ["RPM", "TPD", "CLT", "Batt. Voltg.", "spark1", "pw1", "O2", "EGO Corr."]
                .map(String::from)
                .to_vec(),
            chart_channels: ["RPM", "TPD", "CLT", "O2"].map(String::from).to_vec(),
            chart_seconds: 60.0,
            last_tune: None,
            show_wizard: true,
        }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        xbtune_core::env::Dirs::detect().map(|d| d.config.join("settings.toml"))
    }

    /// Load, falling back to defaults on any problem.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(p) = path else { return Self::default() };
        match std::fs::read_to_string(p) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %p.display(), error = %e, "settings unreadable; using defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, path: Option<&Path>) {
        let Some(p) = path else { return };
        let result = toml::to_string_pretty(self).map_err(std::io::Error::other).and_then(|text| {
            if let Some(dir) = p.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = p.with_extension("toml.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, p)
        });
        if let Err(e) = result {
            tracing::warn!(path = %p.display(), error = %e, "could not save settings");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_partial_files() {
        let dir = std::env::temp_dir().join(format!("xbtune-settings-{}", std::process::id()));
        let p = dir.join("settings.toml");
        let s = Settings {
            last: Some(SavedTarget::Bluetooth {
                address: "00:11:22:AA:BB:CC".into(),
                channel: 1,
                name: None,
                race: true,
            }),
            theme: ThemePref::Dark,
            ..Default::default()
        };
        s.save(Some(&p));
        let back = Settings::load(Some(&p));
        assert_eq!(back.last, s.last);
        assert_eq!(back.theme, ThemePref::Dark);
        // Unknown and missing keys fall back to defaults.
        std::fs::write(&p, "theme = \"light\"\nfuture_key = 1\n").unwrap();
        let back = Settings::load(Some(&p));
        assert_eq!(back.theme, ThemePref::Light);
        assert_eq!(back.gauges, Settings::default().gauges);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn targets_saved_before_race_read_as_stock() {
        for (text, want) in [
            (
                "[last]\nkind = \"bluetooth\"\naddress = \"00:11:22:AA:BB:CC\"\nchannel = 1\n",
                SavedTarget::Bluetooth {
                    address: "00:11:22:AA:BB:CC".into(),
                    channel: 1,
                    name: None,
                    race: false,
                },
            ),
            (
                "[last]\nkind = \"serial\"\npath = \"/dev/ttyUSB0\"\n",
                SavedTarget::Serial { path: "/dev/ttyUSB0".into(), race: false },
            ),
            ("[last]\nkind = \"demo\"\n", SavedTarget::Demo { race: false }),
        ] {
            let s: Settings = toml::from_str(text).unwrap();
            assert_eq!(s.last, Some(want), "{text}");
        }
        let race = Settings { last: Some(SavedTarget::Demo { race: true }), ..Default::default() };
        let back: Settings = toml::from_str(&toml::to_string_pretty(&race).unwrap()).unwrap();
        assert_eq!(back.last, race.last);
        assert!(back.last.unwrap().label().contains("factory race"));
    }
}
