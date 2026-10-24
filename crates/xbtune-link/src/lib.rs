// SPDX-License-Identifier: GPL-3.0-or-later
//! Transports and the async ECM client.
//!
//! Every transport is turned into one byte stream type, [`BoxIo`]: anything
//! that is `AsyncRead + AsyncWrite`. The [`client::EcmClient`] runs the
//! protocol from `xbtune-proto` over it. The UI never touches a transport
//! directly; it talks to a client running on its own task.

pub mod bluetooth;
pub mod client;
pub mod serial;
pub mod win_ports;

use std::fmt;
use std::str::FromStr;

use tokio::io::{AsyncRead, AsyncWrite};

pub use client::EcmClient;

/// A bidirectional byte stream to the ECM.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// A boxed [`Io`].
pub type BoxIo = Box<dyn Io>;

/// Where the ECM is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// USB serial adapter, e.g. `/dev/ttyUSB0`.
    Serial { path: String, baud: u32 },
    /// Bluetooth Classic SPP adapter (BuellTooth), via an RFCOMM socket.
    /// Linux only; elsewhere a paired adapter is a serial port (`COM5`).
    Rfcomm { address: BtAddress, channel: u8 },
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serial { path, baud } => write!(f, "serial {path} @ {baud} baud"),
            Self::Rfcomm { address, channel } => write!(f, "Bluetooth {address} channel {channel}"),
        }
    }
}

/// Failure to open a transport.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("{target}: {source}")]
    Io { target: String, source: std::io::Error },
    #[error("{target}: {source}")]
    Serial { target: String, source: serialport::Error },
}

impl OpenError {
    /// True if the OS refused access (missing snap connection, missing
    /// `dialout` group, Bluetooth blocked).
    pub fn is_permission_denied(&self) -> bool {
        match self {
            Self::Io { source, .. } => source.kind() == std::io::ErrorKind::PermissionDenied,
            Self::Serial { source, .. } => {
                matches!(source.kind, serialport::ErrorKind::Io(std::io::ErrorKind::PermissionDenied))
                    || source.description.contains("Permission denied")
            }
        }
    }

    /// True if the OS says the device is busy (EBUSY). On RFCOMM this is
    /// BlueZ still holding the old link after the adapter lost power (the
    /// ignition was switched off): it clears by itself within ~20 s.
    pub fn is_busy(&self) -> bool {
        match self {
            #[cfg(unix)]
            Self::Io { source, .. } => source.raw_os_error() == Some(libc::EBUSY),
            #[cfg(not(unix))]
            Self::Io { .. } => false,
            Self::Serial { .. } => false,
        }
    }
}

/// Open a transport.
pub async fn open(target: &Target) -> Result<BoxIo, OpenError> {
    match target {
        Target::Serial { path, baud } => serial::open(path, *baud)
            .map(|s| Box::new(s) as BoxIo)
            .map_err(|source| OpenError::Serial { target: target.to_string(), source }),
        Target::Rfcomm { address, channel } => bluetooth::connect(*address, *channel)
            .await
            .map_err(|source| OpenError::Io { target: target.to_string(), source }),
    }
}

/// A Bluetooth device address, shown as `00:11:22:AA:BB:CC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BtAddress(pub [u8; 6]);

impl fmt::Display for BtAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e, g] = self.0;
        write!(f, "{a:02X}:{b:02X}:{c:02X}:{d:02X}:{e:02X}:{g:02X}")
    }
}

impl FromStr for BtAddress {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.split(':').collect();
        let mut out = [0u8; 6];
        if parts.len() != 6 {
            return Err("expected six hex bytes separated by ':'".into());
        }
        for (o, p) in out.iter_mut().zip(&parts) {
            if p.len() != 2 {
                return Err(format!("'{p}' is not a two-digit hex byte"));
            }
            *o = u8::from_str_radix(p, 16).map_err(|_| format!("'{p}' is not a two-digit hex byte"))?;
        }
        Ok(Self(out))
    }
}

/// Parse a Bluetooth address like `00:11:22:AA:BB:CC`.
pub fn parse_bt_address(s: &str) -> Result<BtAddress, String> {
    s.trim().parse().map_err(|e| format!("invalid Bluetooth address '{s}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bt_address_round_trip() {
        let a = parse_bt_address("00:11:22:aa:BB:cc").unwrap();
        assert_eq!(a.0, [0x00, 0x11, 0x22, 0xAA, 0xBB, 0xCC]);
        assert_eq!(a.to_string(), "00:11:22:AA:BB:CC");
        for bad in ["", "00:11:22:AA:BB", "00:11:22:AA:BB:CC:DD", "0:11:22:AA:BB:CC", "00:11:22:AA:BB:GG"] {
            assert!(parse_bt_address(bad).is_err(), "{bad}");
        }
    }
}
