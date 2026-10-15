// SPDX-License-Identifier: GPL-3.0-or-later
//! Core logic shared by the CLI and the app.
//!
//! - [`burn`]: writing changes to the ECM, verified (KICKOFF §6).
//! - [`diag`]: trouble codes, active tests, TPS reset (guarded by a permit).
//! - [`env`]: snap detection, storage locations, plug checks.
//! - [`image`]: EEPROM images and their file formats (`.xbt`, EcmDroid `.xpr`).
//! - [`read`]: reading the whole EEPROM, with resume and a verify pass.
//! - [`live`]: runtime channels, samples, rate and CSV logs.
//! - [`session`]: the live-data task (poll, reconnect, log).
//! - [`tune`]: offline editing with undo/redo.
//! - [`time`]: UTC timestamps without extra dependencies.

pub mod burn;
pub mod diag;
pub mod env;
pub mod image;
pub mod live;
pub mod logfile;
pub mod read;
pub mod session;
pub mod time;
pub mod tune;
