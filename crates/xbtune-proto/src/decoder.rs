// SPDX-License-Identifier: GPL-3.0-or-later
//! Streaming frame decoder.
//!
//! EcmDroid's `ECM.receivePDU()` (`app/src/main/java/org/ecmdroid/ECM.java`,
//! commit 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e, Copyright (C) 2012 by
//! Michel Marti) reads a 6-byte header, then `len + 1` more bytes, and on any
//! error drains the input. That loses sync whenever a Bluetooth link delivers
//! a frame in pieces or with stray bytes in front of it. This decoder keeps
//! the same frame rules but resynchronises on the next SOH instead, so a bad
//! byte costs one frame, not the session.

use crate::pdu::{EOH, HEADER_LEN, MIN_FRAME_LEN, ParseError, Pdu, SOH, SOT};

/// Something the decoder found in the byte stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// A complete, valid frame.
    Frame(Pdu),
    /// A frame-shaped sequence that failed validation (bad checksum, etc.).
    /// Its first byte has been skipped; decoding continues after it.
    Invalid(ParseError),
}

/// Accumulates bytes and yields frames.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// Bytes thrown away while hunting for a frame start.
    discarded: u64,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append received bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Drop everything buffered. Used before sending a new request so a late
    /// reply to an earlier one can't be mistaken for the new reply.
    pub fn clear(&mut self) {
        self.discarded += self.buf.len() as u64;
        self.buf.clear();
    }

    /// Bytes buffered but not yet decoded.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Total bytes skipped as noise.
    pub fn discarded(&self) -> u64 {
        self.discarded
    }

    /// Try to take the next frame out of the buffer. Returns `None` when more
    /// bytes are needed.
    pub fn next_frame(&mut self) -> Option<Decoded> {
        loop {
            // Skip to the next SOH.
            match self.buf.iter().position(|&b| b == SOH) {
                Some(0) => {}
                Some(i) => self.skip(i),
                None => {
                    let n = self.buf.len();
                    self.skip(n);
                    return None;
                }
            }
            if self.buf.len() < HEADER_LEN {
                return None;
            }
            // A real header has EOH and SOT at fixed places. If not, this SOH
            // was a data byte: skip it and look for the next one.
            if self.buf[4] != EOH || self.buf[5] != SOT {
                self.skip(1);
                continue;
            }
            let total = self.buf[3] as usize + 7;
            if total < MIN_FRAME_LEN {
                self.skip(1);
                continue;
            }
            if self.buf.len() < total {
                return None;
            }
            return match Pdu::parse(&self.buf[..total]) {
                Ok(pdu) => {
                    self.buf.drain(..total);
                    Some(Decoded::Frame(pdu))
                }
                Err(e) => {
                    self.skip(1);
                    Some(Decoded::Invalid(e))
                }
            };
        }
    }

    fn skip(&mut self, n: usize) {
        self.buf.drain(..n);
        self.discarded += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdu::{EcmAddress, Request};

    fn frames(d: &mut FrameDecoder) -> Vec<Decoded> {
        std::iter::from_fn(|| d.next_frame()).collect()
    }

    fn version_reply() -> Vec<u8> {
        let mut p = vec![0x06];
        p.extend_from_slice(b"BUEIB310 12-11-03");
        Pdu::new(0x42, 0x00, &p).into_bytes()
    }

    #[test]
    fn whole_frame() {
        let mut d = FrameDecoder::new();
        d.push(&version_reply());
        let got = frames(&mut d);
        assert_eq!(got.len(), 1);
        assert!(matches!(&got[0], Decoded::Frame(p) if p.response_data() == b"BUEIB310 12-11-03"));
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn byte_by_byte() {
        let mut d = FrameDecoder::new();
        let bytes = version_reply();
        for (i, b) in bytes.iter().enumerate() {
            d.push(&[*b]);
            let got = d.next_frame();
            if i + 1 < bytes.len() {
                assert_eq!(got, None, "early frame at byte {i}");
            } else {
                assert!(matches!(got, Some(Decoded::Frame(_))));
            }
        }
    }

    #[test]
    fn garbage_before_and_between() {
        let mut d = FrameDecoder::new();
        d.push(&[0x00, 0xFF, 0x01, 0x13, 0x37]);
        d.push(&version_reply());
        d.push(&[0x01, 0x01, 0x02]);
        d.push(&version_reply());
        let got: Vec<_> = frames(&mut d).into_iter().filter(|f| matches!(f, Decoded::Frame(_))).collect();
        assert_eq!(got.len(), 2);
        assert!(d.discarded() >= 8);
    }

    #[test]
    fn corrupted_frame_then_good_frame() {
        let mut d = FrameDecoder::new();
        let mut bad = version_reply();
        let n = bad.len();
        bad[n - 1] ^= 0xFF;
        d.push(&bad);
        d.push(&version_reply());
        let got = frames(&mut d);
        assert!(matches!(got.first(), Some(Decoded::Invalid(ParseError::Checksum { .. }))));
        assert!(matches!(got.last(), Some(Decoded::Frame(_))));
    }

    #[test]
    fn truncated_frame_followed_by_good_frame() {
        // The first reply loses its tail (a Bluetooth dropout). The decoder
        // must not swallow the next reply as the missing bytes forever: once
        // the bogus "frame" fails validation it resyncs on the next SOH.
        let mut d = FrameDecoder::new();
        let first = version_reply();
        d.push(&first[..10]);
        d.push(&version_reply());
        let got = frames(&mut d);
        assert!(got.iter().any(|f| matches!(f, Decoded::Frame(_))), "{got:?}");
    }

    #[test]
    fn echoed_request_is_a_frame() {
        let mut d = FrameDecoder::new();
        let req = Request::Version.to_pdu(EcmAddress::Stock);
        d.push(req.as_bytes());
        d.push(&version_reply());
        let got = frames(&mut d);
        assert_eq!(got.len(), 2);
        assert!(matches!(&got[0], Decoded::Frame(p) if p.is_request_to(EcmAddress::Stock)));
        assert!(matches!(&got[1], Decoded::Frame(p) if p.is_response_from(EcmAddress::Stock)));
    }

    #[test]
    fn clear_drops_pending() {
        let mut d = FrameDecoder::new();
        d.push(&version_reply()[..7]);
        d.clear();
        assert_eq!(d.pending(), 0);
        d.push(&version_reply());
        assert!(matches!(d.next_frame(), Some(Decoded::Frame(_))));
    }
}
