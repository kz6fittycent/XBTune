// SPDX-License-Identifier: GPL-3.0-or-later
//! The AI tuning advisor (docs/AI.md): any model, local or hosted, through
//! an OpenAI-compatible or an Anthropic request. The advisor only answers;
//! it never writes to the ECM or changes a tune by itself.
//!
//! - [`config`]: provider presets and `ai.toml` (mode 0600).
//! - [`client`]: building, sending and reading requests (blocking).
//! - [`context`]: the system prompt and the data block sent with a question.
//! - [`journal`]: the user's record of each conversation.
//! - [`bike`]: the bike profile (model from the ECM, mods, notes).
//! - [`memory`]: the profile and recent conversations, sent with requests.

pub mod bike;
pub mod client;
pub mod config;
pub mod context;
pub mod journal;
pub mod memory;

pub use client::{AiError, Prepared, Reply, Role, Turn};
pub use config::{Adapter, AiConfig, PRESETS, Preset};
