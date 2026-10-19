// SPDX-License-Identifier: GPL-3.0-or-later
//! The AI tuning advisor (docs/AI.md): any model, local or hosted, through
//! an OpenAI-compatible or an Anthropic request. The advisor only answers;
//! it never writes to the ECM or changes a tune by itself.
//!
//! - [`config`]: provider presets and `ai.toml` (mode 0600).
//! - [`client`]: building, sending and reading requests (blocking).
//! - [`eval`]: fixed cases that measure how well a model does.
//! - [`discover`]: model servers running on this computer (setup wizard).
//! - [`context`]: the system prompt and the data block sent with a question.
//! - [`journal`]: the user's record of each conversation.
//! - [`atmosphere`]: air pressure and density at altitude (Open-Meteo on a click).
//! - [`bike`]: the bike profile (model from the ECM, mods, notes).
//! - [`triage`]: "Check with the AI first" for bug reports.
//! - [`memory`]: the profile and recent conversations, sent with requests.

pub mod atmosphere;
pub mod bike;
pub mod checker;
pub mod client;
pub mod config;
pub mod context;
pub mod discover;
pub mod eval;
pub mod journal;
pub mod memory;
pub mod triage;

pub use client::{AiError, Prepared, Reply, Role, Turn};
pub use config::{Adapter, AiConfig, PRESETS, Preset};
