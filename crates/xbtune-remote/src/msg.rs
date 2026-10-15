// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages inside the encryption (docs/REMOTE.md 4.6). One JSON object
//! per frame, tagged by `m`. Unknown types decode as [`Msg::Unknown`] and
//! are ignored, so either side may be one version ahead.

use serde::{Deserialize, Serialize};

/// Protocol version carried in `hello`.
pub const PROTO: u32 = 1;
/// Longest chat message.
pub const MAX_CHAT: usize = 2000;
/// Longest display name.
pub const MAX_NAME: usize = 40;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "m", rename_all = "snake_case")]
pub enum Msg {
    Hello {
        app: String,
        proto: u32,
        role: String,
        name: String,
    },
    Chat {
        text: String,
    },
    Bye,
    #[serde(other)]
    Unknown,
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("messages always serialize")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("bad message: {e}"))
    }
}

/// Trim a name to something displayable.
pub fn clean_name(name: &str) -> String {
    let n: String = name.chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    let n = n.trim();
    if n.is_empty() { "(no name)".into() } else { n.into() }
}

/// Trim chat text the other side sent.
pub fn clean_chat(text: &str) -> String {
    text.chars().filter(|c| *c == '\n' || !c.is_control()).take(MAX_CHAT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_unknown() {
        let m = Msg::Hello { app: "0.1.0".into(), proto: PROTO, role: "host".into(), name: "Jim".into() };
        assert_eq!(Msg::decode(&m.encode()).unwrap(), m);
        assert_eq!(Msg::decode(br#"{"m":"bye"}"#).unwrap(), Msg::Bye);
        assert_eq!(Msg::decode(br#"{"m":"frames","x":[1,2]}"#).unwrap(), Msg::Unknown);
        assert!(Msg::decode(b"not json").is_err());
        assert!(Msg::decode(br#"{"m":"chat"}"#).is_err());
    }

    #[test]
    fn hostile_text_is_trimmed() {
        assert_eq!(clean_name("  \u{7}Dave\u{1b}[31m  "), "Dave[31m");
        assert_eq!(clean_name("\n\t"), "(no name)");
        assert_eq!(clean_chat(&"x".repeat(5000)).len(), MAX_CHAT);
        assert_eq!(clean_chat("a\nb\u{0}"), "a\nb");
    }
}
