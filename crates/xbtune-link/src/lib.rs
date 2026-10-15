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
    Rfcomm { address: bluer::Address, channel: u8 },
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
            Self::Io { source, .. } => source.raw_os_error() == Some(libc::EBUSY),
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
            .map(|s| Box::new(s) as BoxIo)
            .map_err(|source| OpenError::Io { target: target.to_string(), source }),
    }
}

/// Parse a Bluetooth address like `00:11:22:AA:BB:CC`.
pub fn parse_bt_address(s: &str) -> Result<bluer::Address, String> {
    bluer::Address::from_str(s).map_err(|e| format!("invalid Bluetooth address '{s}': {e}"))
}
