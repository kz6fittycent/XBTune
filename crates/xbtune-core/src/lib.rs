// SPDX-License-Identifier: GPL-3.0-or-later
//! Core logic shared by the CLI and the app.
//!
//! - [`env`]: snap detection, storage locations, plug checks.
//! - [`image`]: EEPROM images and their file formats (`.xbt`, EcmDroid `.xpr`).
//! - [`read`]: reading the whole EEPROM, with resume and a verify pass.
//! - [`time`]: UTC timestamps without extra dependencies.

pub mod env;
pub mod image;
pub mod read;
pub mod time;
