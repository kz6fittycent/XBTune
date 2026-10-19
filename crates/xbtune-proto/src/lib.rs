// SPDX-License-Identifier: GPL-3.0-or-later
//! Buell DDFI ECM serial protocol, without any I/O.
//!
//! Ported from EcmDroid (<https://github.com/ecmdroid/ecmdroid>,
//! Copyright (C) 2012 Michel Marti, GPL-3.0-or-later). Each module names the
//! Java file and commit it was ported from. See `docs/PROVENANCE.md`.
//!
//! - [`pdu`]: frame layout, checksum, request builders, response parsing.
//! - [`decoder`]: pulls frames out of a byte stream that may be split, padded
//!   with garbage, or contain our own echoed requests.
//! - [`eeprom`]: EEPROM page model and the read/write chunk plans.
//! - [`transaction`]: response validation, retry policy and link statistics.
//! - [`isp2`]: Innovate's wideband data stream (not from EcmDroid; from
//!   Innovate's published ISP2 specification).

pub mod decoder;
pub mod eeprom;
pub mod isp2;
pub mod pdu;
pub mod transaction;

pub use decoder::FrameDecoder;
pub use pdu::{EcmAddress, Function, Pdu, Request};
pub use transaction::{LinkStats, RetryPolicy, TransactionError};

/// EcmDroid commit the protocol code was ported from.
pub const ECMDROID_COMMIT: &str = "2f17d29f5b08f67fb357ee8f2ec21e7ae930343e";
