// SPDX-License-Identifier: GPL-3.0-or-later
//! Core logic shared by the CLI and the app.
//!
//! - [`diag`]: trouble codes, active tests, TPS reset (guarded by a permit).
//! - [`env`]: snap detection, storage locations, plug checks.
//! - [`image`]: EEPROM images and their file formats (`.xbt`, EcmDroid `.xpr`).
//! - [`read`]: reading the whole EEPROM, with resume and a verify pass.
//! - [`live`]: runtime channels, samples, rate and CSV logs.
//! - [`session`]: the live-data task (poll, reconnect, log).
//! - [`time`]: UTC timestamps without extra dependencies.

pub mod diag;
pub mod env;
pub mod image;
pub mod live;
pub mod read;
pub mod session;
pub mod time;
