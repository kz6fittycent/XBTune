// SPDX-License-Identifier: GPL-3.0-or-later
//! Bluetooth Classic SPP (BuellTooth and similar).
//!
//! Linux: the data link is a plain RFCOMM socket, so no `rfcomm bind`, no
//! `/dev/rfcomm*` and no root. Discovery and pairing state come from BlueZ
//! over D-Bus (`bluer`). Under snap confinement both need the `bluez` plug.
//!
//! Windows and macOS: the OS turns a paired SPP adapter into a serial port
//! (`COM5`, `/dev/cu.BuellTooth`), which goes through [`crate::serial`].
//! [`connect`] and [`scan`] say so instead of doing anything.

use std::time::Duration;

use crate::{BoxIo, BtAddress};

#[cfg(target_os = "linux")]
mod bluez;

/// True where XBTune talks to Bluetooth itself (BlueZ on Linux).
pub const AVAILABLE: bool = cfg!(target_os = "linux");

/// RFCOMM channel most SPP modules listen on.
pub const DEFAULT_CHANNEL: u8 = 1;

/// How long to wait for an RFCOMM connection. A page attempt to an absent
/// or powered-off device takes about 5 s, plus SDP and pairing.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// A Bluetooth device as BlueZ sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub address: BtAddress,
    pub name: Option<String>,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub rssi: Option<i16>,
    /// Advertises the Serial Port Profile (may be unknown until paired).
    pub has_spp: bool,
}

/// Bluetooth discovery failed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ScanError(pub String);

#[cfg(not(target_os = "linux"))]
const NOT_HERE: &str = "XBTune can't use Bluetooth directly on this system. Pair the adapter in the \
                        system settings, then pick its serial port (Bluetooth serial port)";

/// Open an RFCOMM stream to `address` on `channel`. The adapter should be
/// paired first (`bluetoothctl pair <addr>`), otherwise BlueZ asks for a PIN
/// through whatever agent is running, or the connect fails.
pub async fn connect(address: BtAddress, channel: u8) -> std::io::Result<BoxIo> {
    #[cfg(target_os = "linux")]
    return bluez::connect(address, channel).await.map(|s| Box::new(s) as BoxIo);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (address, channel);
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, NOT_HERE))
    }
}

/// List devices BlueZ knows, running discovery for `scan_for` first (zero
/// to skip discovery and list only known devices).
pub async fn scan(scan_for: Duration) -> Result<Vec<DeviceInfo>, ScanError> {
    #[cfg(target_os = "linux")]
    return bluez::scan(scan_for).await.map_err(|e| ScanError(e.to_string()));
    #[cfg(not(target_os = "linux"))]
    {
        let _ = scan_for;
        Err(ScanError(NOT_HERE.into()))
    }
}
