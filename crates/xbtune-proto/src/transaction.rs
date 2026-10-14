// SPDX-License-Identifier: GPL-3.0-or-later
//! Transaction rules: which frame answers a request, when to retry, and
//! link-quality statistics.
//!
//! The acceptance rules are ported from EcmDroid `ECM.sendPDU()`
//! (`app/src/main/java/org/ecmdroid/ECM.java`, commit
//! 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e, Copyright (C) 2012 by Michel
//! Marti): the reply must come from the ECM and carry ACK. On top of that we
//! check the data length of GET replies (as `readEEPromPage()` does), ignore
//! echoed requests, and keep statistics for the link-health gate.

use std::collections::VecDeque;
use std::time::Duration;

use crate::pdu::{EcmAddress, ParseError, Pdu, Request};

/// What to do with a decoded frame while waiting for a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The reply we were waiting for.
    Accept,
    /// Not for us (an echo of our own request, or traffic for another
    /// address). Keep waiting.
    Ignore,
    /// A reply from the ECM that is wrong. The attempt failed.
    Reject(TransactionError),
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransactionError {
    #[error("no reply within {0:?}")]
    Timeout(Duration),
    #[error("ECM rejected the request (status {0:#04x})")]
    Nak(u8),
    #[error("ECM sent {got} data bytes, expected {expected}")]
    WrongLength { expected: usize, got: usize },
    #[error("corrupted frame: {0}")]
    Corrupt(ParseError),
    #[error("link error: {0}")]
    Io(String),
    #[error("gave up after {attempts} attempts; last error: {last}")]
    Exhausted { attempts: u32, last: Box<TransactionError> },
}

impl TransactionError {
    /// True if a retry might help. A NAK means the ECM understood and
    /// refused, so repeating the same request is pointless.
    pub fn is_transient(&self) -> bool {
        !matches!(self, Self::Nak(_))
    }
}

/// Classify `frame` as a reply to `request` sent to `ecm`.
pub fn judge(request: &Request, ecm: EcmAddress, frame: &Pdu) -> Verdict {
    if !frame.is_response_from(ecm) {
        return Verdict::Ignore;
    }
    match frame.first_byte() {
        Some(crate::pdu::ACK) => {}
        Some(code) => return Verdict::Reject(TransactionError::Nak(code)),
        None => return Verdict::Reject(TransactionError::WrongLength { expected: 1, got: 0 }),
    }
    if let Some(expected) = request.expected_data_len() {
        let got = frame.response_data().len();
        if got != expected {
            return Verdict::Reject(TransactionError::WrongLength { expected, got });
        }
    }
    Verdict::Accept
}

/// Per-request timeout and retry rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Time to wait for a complete reply. EcmDroid uses 1000 ms.
    pub timeout: Duration,
    /// Total attempts for idempotent requests (1 = no retries).
    pub max_attempts: u32,
    /// After a failed attempt, keep reading and discarding for this long so a
    /// late reply to the failed attempt can't be taken as the next reply.
    pub settle: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(1000),
            max_attempts: 3,
            settle: Duration::from_millis(150),
        }
    }
}

impl RetryPolicy {
    /// Attempts allowed for `request`. Non-idempotent requests (actuator
    /// commands) get exactly one.
    pub fn attempts_for(&self, request: &Request) -> u32 {
        if request.is_idempotent() { self.max_attempts.max(1) } else { 1 }
    }
}

/// Outcome of one attempt, for statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Timeout,
    Corrupt,
    Nak,
    Io,
}

/// Link quality counters and a sliding window for the health check.
#[derive(Debug, Clone)]
pub struct LinkStats {
    pub requests: u64,
    pub attempts: u64,
    pub ok: u64,
    pub retries: u64,
    pub timeouts: u64,
    pub corrupt: u64,
    pub naks: u64,
    pub io_errors: u64,
    pub bytes_discarded: u64,
    /// Exponentially weighted RTT of successful attempts.
    pub rtt_avg: Option<Duration>,
    pub rtt_min: Option<Duration>,
    pub rtt_max: Option<Duration>,
    window: VecDeque<bool>,
    window_len: usize,
}

impl Default for LinkStats {
    fn default() -> Self {
        Self::new(50)
    }
}

impl LinkStats {
    /// `window_len`: number of recent attempts the error rate is computed over.
    pub fn new(window_len: usize) -> Self {
        Self {
            requests: 0,
            attempts: 0,
            ok: 0,
            retries: 0,
            timeouts: 0,
            corrupt: 0,
            naks: 0,
            io_errors: 0,
            bytes_discarded: 0,
            rtt_avg: None,
            rtt_min: None,
            rtt_max: None,
            window: VecDeque::with_capacity(window_len),
            window_len: window_len.max(1),
        }
    }

    pub fn record_request(&mut self) {
        self.requests += 1;
    }

    pub fn record_attempt(&mut self, outcome: Outcome, rtt: Option<Duration>, retry: bool) {
        self.attempts += 1;
        if retry {
            self.retries += 1;
        }
        match outcome {
            Outcome::Ok => self.ok += 1,
            Outcome::Timeout => self.timeouts += 1,
            Outcome::Corrupt => self.corrupt += 1,
            Outcome::Nak => self.naks += 1,
            Outcome::Io => self.io_errors += 1,
        }
        if let (Outcome::Ok, Some(rtt)) = (outcome, rtt) {
            self.rtt_avg = Some(match self.rtt_avg {
                None => rtt,
                Some(avg) => (avg * 7 + rtt) / 8,
            });
            self.rtt_min = Some(self.rtt_min.map_or(rtt, |m| m.min(rtt)));
            self.rtt_max = Some(self.rtt_max.map_or(rtt, |m| m.max(rtt)));
        }
        if self.window.len() == self.window_len {
            self.window.pop_front();
        }
        // A NAK is the ECM answering clearly, not a link fault.
        self.window.push_back(matches!(outcome, Outcome::Ok | Outcome::Nak));
    }

    /// Fraction of failed attempts in the recent window (0.0..=1.0).
    pub fn recent_error_rate(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let bad = self.window.iter().filter(|ok| !**ok).count();
        bad as f64 / self.window.len() as f64
    }

    /// Number of attempts in the recent window.
    pub fn recent_attempts(&self) -> usize {
        self.window.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdu::{Function, HOST_ID, STOCK_ECM_ID};

    fn reply(status: u8, data: &[u8]) -> Pdu {
        let mut p = vec![status];
        p.extend_from_slice(data);
        Pdu::new(STOCK_ECM_ID, HOST_ID, &p)
    }

    #[test]
    fn accepts_ack_with_right_length() {
        let req = Request::Get { page: 1, offset: 0, len: 4 };
        assert_eq!(judge(&req, EcmAddress::Stock, &reply(0x06, &[1, 2, 3, 4])), Verdict::Accept);
    }

    #[test]
    fn rejects_wrong_length() {
        let req = Request::Get { page: 1, offset: 0, len: 4 };
        assert_eq!(
            judge(&req, EcmAddress::Stock, &reply(0x06, &[1, 2, 3])),
            Verdict::Reject(TransactionError::WrongLength { expected: 4, got: 3 })
        );
    }

    #[test]
    fn rejects_nak() {
        let v = judge(&Request::Version, EcmAddress::Stock, &reply(0x15, &[]));
        assert_eq!(v, Verdict::Reject(TransactionError::Nak(0x15)));
        assert!(!TransactionError::Nak(0x15).is_transient());
    }

    #[test]
    fn ignores_echo_and_other_addresses() {
        let echo = Request::Version.to_pdu(EcmAddress::Stock);
        assert_eq!(judge(&Request::Version, EcmAddress::Stock, &echo), Verdict::Ignore);
        let race = Pdu::new(0x55, HOST_ID, &[0x06]);
        assert_eq!(judge(&Request::Version, EcmAddress::Stock, &race), Verdict::Ignore);
    }

    #[test]
    fn commands_are_never_retried() {
        let p = RetryPolicy { max_attempts: 5, ..Default::default() };
        assert_eq!(p.attempts_for(&Request::Command(Function::FuelPump)), 1);
        assert_eq!(p.attempts_for(&Request::Version), 5);
    }

    #[test]
    fn stats_window_and_rtt() {
        let mut s = LinkStats::new(4);
        s.record_attempt(Outcome::Ok, Some(Duration::from_millis(40)), false);
        s.record_attempt(Outcome::Timeout, None, false);
        s.record_attempt(Outcome::Ok, Some(Duration::from_millis(80)), true);
        assert_eq!(s.recent_error_rate(), 1.0 / 3.0);
        assert_eq!(s.rtt_min, Some(Duration::from_millis(40)));
        assert_eq!(s.rtt_max, Some(Duration::from_millis(80)));
        assert_eq!(s.rtt_avg, Some(Duration::from_millis(45)));
        for _ in 0..4 {
            s.record_attempt(Outcome::Ok, Some(Duration::from_millis(40)), false);
        }
        assert_eq!(s.recent_error_rate(), 0.0);
        assert_eq!(s.retries, 1);
        assert_eq!(s.timeouts, 1);
    }
}
