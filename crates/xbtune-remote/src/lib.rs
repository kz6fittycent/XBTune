// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote tuning (phase 8, docs/REMOTE.md): the code, SPAKE2 pairing and
//! frame encryption, the relay client, a local relay, and the session.

pub mod code;
pub mod crypto;
pub mod local_relay;
pub mod msg;
pub mod relay;
pub mod session;

/// The owner's relay (`relay/remote/`, deployed on the owner's Cloudflare
/// account). `XBTUNE_REMOTE_URL` overrides it.
pub const DEFAULT_RELAY: &str = "wss://xbtune-remote.jimmy-tigert.workers.dev/v1";

/// The relay to use: `XBTUNE_REMOTE_URL` or [`DEFAULT_RELAY`].
pub fn relay_url() -> String {
    std::env::var("XBTUNE_REMOTE_URL").unwrap_or_else(|_| DEFAULT_RELAY.into())
}
