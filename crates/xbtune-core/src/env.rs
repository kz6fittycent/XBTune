// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime environment: snap confinement and storage locations.
//!
//! Inside the snap, everything is stored under `$SNAP_USER_COMMON`, which
//! survives refreshes and reverts. `$SNAP_USER_DATA` is per revision and is
//! never used: a revert would roll back EEPROM backups and tune history.

use std::path::PathBuf;
use std::process::Command;

/// True when running inside a snap.
pub fn in_snap() -> bool {
    std::env::var_os("SNAP").is_some() && std::env::var_os("SNAP_NAME").is_some()
}

/// Name of the snap, when running inside one.
pub fn snap_name() -> Option<String> {
    std::env::var("SNAP_INSTANCE_NAME").or_else(|_| std::env::var("SNAP_NAME")).ok()
}

/// Where settings, backups, images and logs go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl Dirs {
    /// `$SNAP_USER_COMMON/{config,data}` in the snap, XDG directories
    /// otherwise.
    pub fn detect() -> Option<Self> {
        if in_snap()
            && let Some(common) = std::env::var_os("SNAP_USER_COMMON")
        {
            let common = PathBuf::from(common);
            return Some(Self { config: common.join("config"), data: common.join("data") });
        }
        let d = directories::ProjectDirs::from("", "", "xbtune")?;
        Some(Self { config: d.config_dir().to_path_buf(), data: d.data_dir().to_path_buf() })
    }

    /// EEPROM backups taken before writes.
    pub fn backups(&self) -> PathBuf {
        self.data.join("backups")
    }

    /// Data logs.
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }
}

/// Snap plugs XBTune needs that are not auto-connected.
pub const MANUAL_PLUGS: &[(&str, &str)] = &[
    ("bluez", "Bluetooth adapters (BuellTooth)"),
    ("raw-usb", "USB serial adapters (/dev/ttyUSB*, /dev/ttyACM*)"),
    ("removable-media", "EEPROM images and logs on USB sticks"),
];

/// Connection state of one plug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlugState {
    pub plug: &'static str,
    pub purpose: &'static str,
    /// `None` if it could not be determined.
    pub connected: Option<bool>,
}

impl PlugState {
    /// The command that connects this plug.
    pub fn connect_command(&self) -> String {
        format!("sudo snap connect {}:{}", snap_name().unwrap_or_else(|| "xbtune".into()), self.plug)
    }
}

/// Ask snapd which of [`MANUAL_PLUGS`] are connected. Returns an empty list
/// outside a snap.
pub fn plug_states() -> Vec<PlugState> {
    if !in_snap() {
        return Vec::new();
    }
    MANUAL_PLUGS
        .iter()
        .map(|&(plug, purpose)| {
            // `snapctl is-connected` exits 0 if connected, 1 if not.
            let connected =
                Command::new("snapctl").args(["is-connected", plug]).status().ok().and_then(|s| {
                    match s.code() {
                        Some(0) => Some(true),
                        Some(1) => Some(false),
                        _ => None,
                    }
                });
            PlugState { plug, purpose, connected }
        })
        .collect()
}

/// Advice for a "permission denied" opening a transport.
pub fn permission_hint(plug: &str) -> String {
    if in_snap() {
        format!(
            "The snap is not allowed to use this device. Run:\n    sudo snap connect {}:{plug}",
            snap_name().unwrap_or_else(|| "xbtune".into())
        )
    } else if plug == "raw-usb" {
        "Your user cannot open the serial port. Add yourself to the 'dialout' group:\n    \
         sudo usermod -aG dialout $USER   (then log out and back in)"
            .to_string()
    } else {
        "Bluetooth access was refused. Check that Bluetooth is on and not blocked (rfkill list).".to_string()
    }
}
