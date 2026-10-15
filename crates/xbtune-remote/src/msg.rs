// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages inside the encryption (docs/REMOTE.md 4.6). One JSON object
//! per frame, tagged by `m`. Unknown types decode as [`Msg::Unknown`] and
//! are ignored, so either side may be one version ahead. Binary data
//! (runtime frames, tune bytes, log chunks) travels as base64 ([`B64`]).
//!
//! `hello`, `chat` and `bye` are handled by the session; everything else
//! is passed to the app ([`crate::session::Event::Message`]).

use serde::{Deserialize, Serialize};

/// Protocol version carried in `hello`.
pub const PROTO: u32 = 1;
/// Longest chat message.
pub const MAX_CHAT: usize = 2000;
/// Longest display name.
pub const MAX_NAME: usize = 40;
/// Longest request note.
pub const MAX_NOTE: usize = 500;
/// Log chunk size before base64 (fits a frame with room to spare).
pub const LOG_CHUNK: usize = 192 * 1024;
/// Largest shared log.
pub const MAX_LOG: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "m", rename_all = "snake_case")]
pub enum Msg {
    // R1: session
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

    // R2: live data, host -> guest
    /// The ECM the host is connected to (`None`: not connected).
    Identity {
        version: Option<String>,
    },
    /// Runtime frames since the last batch.
    Frames {
        frames: Vec<FrameMsg>,
    },
    /// Host link state: "live", "connecting", "reconnecting", "stopped".
    LinkInfo {
        state: String,
        hz: f64,
        error_pct: f64,
        recording: bool,
    },

    // R3: shared tune
    /// Host -> guest: the shared tune (whole image). `share` changes when
    /// the host opens another tune; `seq` is the last applied edit.
    Tune {
        share: u32,
        seq: u64,
        name: String,
        image: ImageMsg,
        allow_save: bool,
    },
    /// Host -> guest: no tune is shared any more.
    TuneClosed,
    /// Host -> guest: an edit landed on the host's working copy.
    Applied {
        share: u32,
        seq: u64,
        by: String,
        /// The guest proposal this was, if any.
        proposal: Option<u64>,
        label: String,
        patches: Vec<PatchMsg>,
    },
    /// Guest -> host: a proposed edit.
    Propose {
        share: u32,
        id: u64,
        label: String,
        patches: Vec<PatchMsg>,
    },
    /// Host -> guest: a proposal was turned down (or didn't apply).
    Rejected {
        id: u64,
        reason: String,
    },
    /// Host -> guest: what the tuner may do directly.
    Permissions {
        edit: bool,
        burn: bool,
        tests: bool,
    },

    // R4: requests
    /// Guest -> host.
    Request {
        id: u64,
        kind: RequestKind,
        /// Run under a permission (after the host's countdown) rather than
        /// asking for the host's review.
        direct: bool,
        note: String,
    },
    /// Host -> guest: what happened to a request.
    RequestState {
        id: u64,
        state: ReqState,
        text: String,
    },
    /// Host -> guest: a direct burn's review (backup made, these changes).
    BurnReview {
        id: u64,
        lines: Vec<String>,
    },
    /// Guest -> host: the tuner confirmed a direct burn's review.
    Confirm {
        id: u64,
    },
    /// Guest -> host: never mind.
    Cancel {
        id: u64,
    },

    // R5: logs, host -> guest
    LogOffer {
        id: u64,
        name: String,
        size: u64,
        sha256: String,
    },
    LogChunk {
        id: u64,
        index: u32,
        data: B64,
    },
    LogDone {
        id: u64,
    },

    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameMsg {
    /// Seconds on the host's session clock.
    pub t: f64,
    pub raw: B64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchMsg {
    pub offset: usize,
    pub bytes: B64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageMsg {
    pub ecm_id: String,
    pub ecm_version: Option<String>,
    pub page_zero: bool,
    pub bytes: B64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum RequestKind {
    /// Burn the shared tune (the host's working copy).
    Burn,
    AfvReset,
    /// An ECM function by name (`xbtune_core::diag::function_by_name`):
    /// tests, TPS reset, clear codes.
    Function {
        name: String,
    },
    Record {
        start: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReqState {
    /// The host sees the request.
    Shown,
    Declined,
    /// Direct: the host's countdown is running.
    Countdown,
    /// The host stopped it during the countdown.
    Stopped,
    /// Backup / review in progress on the host.
    Preparing,
    Running,
    Done,
    Failed,
}

impl Msg {
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("messages always serialize")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("bad message: {e}"))
    }
}

/// Bytes carried as a base64 string (RFC 4648, with padding).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct B64(pub Vec<u8>);

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.as_bytes();
    if !s.len().is_multiple_of(4) {
        return Err("base64 length".into());
    }
    let val = |b: u8| -> Result<u32, String> {
        match b {
            b'A'..=b'Z' => Ok(u32::from(b - b'A')),
            b'a'..=b'z' => Ok(u32::from(b - b'a') + 26),
            b'0'..=b'9' => Ok(u32::from(b - b'0') + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err("base64 character".into()),
        }
    };
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    for (k, q) in s.chunks(4).enumerate() {
        let last = k == s.len() / 4 - 1;
        let pad = q.iter().rev().take_while(|&&b| b == b'=').count();
        if pad > 2 || (pad > 0 && !last) {
            return Err("base64 padding".into());
        }
        let mut n = 0u32;
        for &b in &q[..4 - pad] {
            n = (n << 6) | val(b)?;
        }
        n <<= 6 * pad as u32;
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..3 - pad]);
    }
    Ok(out)
}

impl Serialize for B64 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&b64_encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for B64 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        b64_decode(&s).map(B64).map_err(serde::de::Error::custom)
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

/// Trim a one-line text (labels, notes, reasons) the other side sent.
pub fn clean_line(text: &str, max: usize) -> String {
    text.chars().map(|c| if c.is_control() { ' ' } else { c }).take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_unknown() {
        let m = Msg::Hello { app: "0.1.0".into(), proto: PROTO, role: "host".into(), name: "Jim".into() };
        assert_eq!(Msg::decode(&m.encode()).unwrap(), m);
        assert_eq!(Msg::decode(br#"{"m":"bye"}"#).unwrap(), Msg::Bye);
        assert_eq!(Msg::decode(br#"{"m":"from_the_future","x":[1,2]}"#).unwrap(), Msg::Unknown);
        assert!(Msg::decode(b"not json").is_err());
        assert!(Msg::decode(br#"{"m":"chat"}"#).is_err());
        let m = Msg::Request {
            id: 3,
            kind: RequestKind::Function { name: "fan".into() },
            direct: true,
            note: "check the fan".into(),
        };
        assert_eq!(Msg::decode(&m.encode()).unwrap(), m);
        let m = Msg::Frames { frames: vec![FrameMsg { t: 1.5, raw: B64(vec![1, 2, 255]) }] };
        assert!(String::from_utf8(m.encode()).unwrap().contains(r#""raw":"AQL/""#));
        assert_eq!(Msg::decode(&m.encode()).unwrap(), m);
    }

    #[test]
    fn base64_matches_rfc4648() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(b64_encode(raw.as_bytes()), enc);
            assert_eq!(b64_decode(enc).unwrap(), raw.as_bytes());
        }
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(b64_decode(&b64_encode(&all)).unwrap(), all);
        for bad in ["Zg=", "Zg==Zg==", "Z===", "Zm9v!A==", "===="] {
            assert!(b64_decode(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn hostile_text_is_trimmed() {
        assert_eq!(clean_name("  \u{7}Dave\u{1b}[31m  "), "Dave[31m");
        assert_eq!(clean_name("\n\t"), "(no name)");
        assert_eq!(clean_chat(&"x".repeat(5000)).len(), MAX_CHAT);
        assert_eq!(clean_chat("a\nb\u{0}"), "a\nb");
        assert_eq!(clean_line("a\nb", 10), "a b");
    }
}
