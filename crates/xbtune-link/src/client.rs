// SPDX-License-Identifier: GPL-3.0-or-later
//! The ECM client: sends requests, waits for replies, retries, keeps stats.
//!
//! Request/reply flow follows EcmDroid `ECM.sendPDU()` / `receivePDU()`
//! (`app/src/main/java/org/ecmdroid/ECM.java`, commit
//! 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e, Copyright (C) 2012 by Michel
//! Marti). Differences, for unreliable Bluetooth links:
//! - frames are found by a resyncing decoder, not fixed-size reads;
//! - echoed requests and stray frames are ignored, not treated as errors;
//! - idempotent requests are retried; actuator commands never are;
//! - after a failed attempt the input is drained for a short settle time so
//!   a late reply can't answer the next request.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;
use tracing::{debug, trace, warn};
use xbtune_proto::decoder::Decoded;
use xbtune_proto::pdu::hex;
use xbtune_proto::transaction::{Outcome, Verdict, judge};
use xbtune_proto::{EcmAddress, FrameDecoder, LinkStats, Pdu, Request, RetryPolicy, TransactionError};

use crate::BoxIo;

/// Talks to one ECM over one byte stream.
pub struct EcmClient {
    io: BoxIo,
    ecm: EcmAddress,
    policy: RetryPolicy,
    decoder: FrameDecoder,
    stats: LinkStats,
    buf: Vec<u8>,
}

impl EcmClient {
    pub fn new(io: BoxIo, ecm: EcmAddress) -> Self {
        Self {
            io,
            ecm,
            policy: RetryPolicy::default(),
            decoder: FrameDecoder::new(),
            stats: LinkStats::default(),
            buf: vec![0; 512],
        }
    }

    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy(&self) -> RetryPolicy {
        self.policy
    }

    pub fn ecm(&self) -> EcmAddress {
        self.ecm
    }

    pub fn stats(&self) -> &LinkStats {
        &self.stats
    }

    /// Send `request` and return the accepted reply, retrying idempotent
    /// requests per the policy.
    pub async fn transact(&mut self, request: &Request) -> Result<Pdu, TransactionError> {
        self.stats.record_request();
        let attempts = self.policy.attempts_for(request);
        let mut last = None;
        let mut made = 0;
        for attempt in 1..=attempts {
            made = attempt;
            let started = Instant::now();
            let result = self.attempt(request).await;
            let retry = attempt > 1;
            match result {
                Ok(pdu) => {
                    self.stats.record_attempt(Outcome::Ok, Some(started.elapsed()), retry);
                    return Ok(pdu);
                }
                Err(e) => {
                    let outcome = match &e {
                        TransactionError::Timeout(_) => Outcome::Timeout,
                        TransactionError::Nak(_) => Outcome::Nak,
                        TransactionError::Io(_) => Outcome::Io,
                        _ => Outcome::Corrupt,
                    };
                    self.stats.record_attempt(outcome, None, retry);
                    debug!(attempt, attempts, ?request, error = %e, "attempt failed");
                    let transient = e.is_transient();
                    let io = matches!(e, TransactionError::Io(_));
                    last = Some(e);
                    if !transient || io {
                        break;
                    }
                    self.settle().await;
                }
            }
        }
        let last = last.expect("at least one attempt");
        warn!(?request, error = %last, "ECM request failed");
        if made > 1 {
            Err(TransactionError::Exhausted { attempts: made, last: Box::new(last) })
        } else {
            Err(last)
        }
    }

    async fn attempt(&mut self, request: &Request) -> Result<Pdu, TransactionError> {
        self.decoder.clear();
        let frame = request.to_pdu(self.ecm);
        trace!(tx = %hex(frame.as_bytes()));
        self.io.write_all(frame.as_bytes()).await.map_err(io_err)?;
        self.io.flush().await.map_err(io_err)?;

        let deadline = Instant::now() + self.policy.timeout;
        loop {
            while let Some(d) = self.decoder.next_frame() {
                match d {
                    Decoded::Frame(pdu) => {
                        trace!(rx = %hex(pdu.as_bytes()));
                        match judge(request, self.ecm, &pdu) {
                            Verdict::Accept => return Ok(pdu),
                            Verdict::Ignore => debug!(frame = ?pdu, "ignoring frame"),
                            Verdict::Reject(e) => return Err(e),
                        }
                    }
                    Decoded::Invalid(e) => return Err(TransactionError::Corrupt(e)),
                }
            }
            let n = match tokio::time::timeout_at(deadline, self.io.read(&mut self.buf)).await {
                Err(_) => return Err(TransactionError::Timeout(self.policy.timeout)),
                Ok(Err(e)) => return Err(io_err(e)),
                Ok(Ok(0)) => return Err(TransactionError::Io("connection closed".into())),
                Ok(Ok(n)) => n,
            };
            trace!(rx_raw = %hex(&self.buf[..n]));
            self.decoder.push(&self.buf[..n]);
        }
    }

    /// Read and throw away input for the settle time.
    async fn settle(&mut self) {
        let deadline = Instant::now() + self.policy.settle;
        while let Ok(Ok(n)) = tokio::time::timeout_at(deadline, self.io.read(&mut self.buf)).await {
            if n == 0 {
                break;
            }
            self.stats.bytes_discarded += n as u64;
        }
        self.decoder.clear();
    }

    /// Read the version string, e.g. `"BUEIB310 12-11-03"`.
    pub async fn version(&mut self) -> Result<String, TransactionError> {
        let pdu = self.transact(&Request::Version).await?;
        Ok(String::from_utf8_lossy(pdu.response_data()).trim_end_matches('\0').trim().to_string())
    }

    /// Busy/idle state byte: 0 means idle.
    pub async fn current_state(&mut self) -> Result<u8, TransactionError> {
        let pdu = self.transact(&Request::CURRENT_STATE).await?;
        Ok(pdu.response_data()[0])
    }

    /// Read one runtime-data frame (the whole frame, as runtime variable
    /// offsets in the definitions are relative to the frame start).
    pub async fn runtime_data(&mut self) -> Result<Pdu, TransactionError> {
        self.transact(&Request::RuntimeData).await
    }

    /// Read `len` (1..=16) bytes at `offset` of EEPROM page `page`.
    pub async fn read_bytes(&mut self, page: u8, offset: u8, len: u8) -> Result<Vec<u8>, TransactionError> {
        let pdu = self.transact(&Request::Get { page, offset, len }).await?;
        Ok(pdu.response_data().to_vec())
    }

    /// Recover the underlying stream (e.g. to reconnect).
    pub fn into_inner(self) -> BoxIo {
        self.io
    }
}

fn io_err(e: std::io::Error) -> TransactionError {
    TransactionError::Io(e.to_string())
}

/// Convenience for tests and tools: a policy with a short timeout.
pub fn fast_policy() -> RetryPolicy {
    RetryPolicy { timeout: Duration::from_millis(200), max_attempts: 3, settle: Duration::from_millis(20) }
}
