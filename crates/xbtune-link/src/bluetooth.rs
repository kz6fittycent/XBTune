// SPDX-License-Identifier: GPL-3.0-or-later
//! Bluetooth Classic SPP (BuellTooth and similar) through BlueZ.
//!
//! The data link is a plain RFCOMM socket, so no `rfcomm bind`, no
//! `/dev/rfcomm*` and no root. Discovery and pairing state come from BlueZ
//! over D-Bus (`bluer`). Under snap confinement both need the `bluez` plug.

use std::collections::BTreeMap;
use std::time::Duration;

use bluer::rfcomm::{SocketAddr, Stream};
use bluer::{Address, AdapterEvent};
use futures::StreamExt;

/// Serial Port Profile UUID.
pub const SPP_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x00001101_0000_1000_8000_00805F9B34FB);

/// RFCOMM channel most SPP modules listen on.
pub const DEFAULT_CHANNEL: u8 = 1;

/// Open an RFCOMM stream to `address` on `channel`. The adapter should be
/// paired first (`bluetoothctl pair <addr>`), otherwise BlueZ asks for a PIN
/// through whatever agent is running, or the connect fails.
pub async fn connect(address: Address, channel: u8) -> std::io::Result<Stream> {
    Stream::connect(SocketAddr::new(address, channel)).await
}

/// A Bluetooth device as BlueZ sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    pub address: Address,
    pub name: Option<String>,
    pub paired: bool,
    pub trusted: bool,
    pub connected: bool,
    pub rssi: Option<i16>,
    /// Advertises the Serial Port Profile (may be unknown until paired).
    pub has_spp: bool,
}

/// List devices BlueZ knows, running discovery for `scan_for` first (zero
/// to skip discovery and list only known devices).
pub async fn scan(scan_for: Duration) -> bluer::Result<Vec<DeviceInfo>> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    if !adapter.is_powered().await? {
        return Err(bluer::Error {
            kind: bluer::ErrorKind::NotReady,
            message: format!("Bluetooth adapter {} is powered off", adapter.name()),
        });
    }
    let mut seen: BTreeMap<Address, ()> = BTreeMap::new();
    for a in adapter.device_addresses().await? {
        seen.insert(a, ());
    }
    if !scan_for.is_zero() {
        let events = adapter.discover_devices().await?;
        let mut events = std::pin::pin!(events);
        let deadline = tokio::time::Instant::now() + scan_for;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.next()).await {
            if let AdapterEvent::DeviceAdded(a) = ev {
                seen.insert(a, ());
            }
        }
    }
    let mut out = Vec::new();
    for address in seen.into_keys() {
        let Ok(dev) = adapter.device(address) else { continue };
        let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
        out.push(DeviceInfo {
            address,
            name: dev.name().await.ok().flatten(),
            paired: dev.is_paired().await.unwrap_or(false),
            trusted: dev.is_trusted().await.unwrap_or(false),
            connected: dev.is_connected().await.unwrap_or(false),
            rssi: dev.rssi().await.ok().flatten(),
            has_spp: uuids.contains(&SPP_UUID),
        });
    }
    // Likely ECM adapters first, then by name.
    out.sort_by_key(|d| (!d.has_spp, !d.paired, d.name.clone().unwrap_or_default()));
    Ok(out)
}
