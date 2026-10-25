// SPDX-License-Identifier: GPL-3.0-or-later
//! Core logic shared by the CLI and the app.
//!
//! - [`burn`]: writing changes to the ECM, verified (KICKOFF §6).
//! - [`diag`]: trouble codes, active tests, TPS reset (guarded by a permit).
//! - [`disclaimer`]: the engine-damage warning (app and CLI).
//! - [`env`]: snap detection, storage locations, plug checks.
//! - [`image`]: EEPROM images and their file formats (`.xbt`, EcmDroid `.xpr`).
//! - [`read`]: reading the whole EEPROM, with resume and a verify pass.
//! - [`live`]: runtime channels, samples, rate and CSV logs.
//! - [`logmode`]: wideband logging mode (closed loop and AFV locked).
//! - [`session`]: the live-data task (poll, reconnect, log).
//! - [`setup`]: the ECM Setup screen (EcmDroid's named settings).
//! - [`tune`]: offline editing with undo/redo.
//! - [`time`]: UTC timestamps without extra dependencies.
//! - [`wideband`]: an Innovate (ISP2) wideband controller beside the ECM.

pub mod autotune;
pub mod burn;
pub mod diag;
pub mod disclaimer;
pub mod env;
pub mod image;
pub mod live;
pub mod logfile;
pub mod logformats;
pub mod logmode;
pub mod logstats;
pub mod read;
pub mod session;
pub mod setup;
pub mod time;
pub mod tune;
pub mod wideband;
