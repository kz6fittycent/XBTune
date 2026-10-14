// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the whole EEPROM.
//!
//! Chunking follows EcmDroid `ECM.readEEPromPage()` via
//! `xbtune_proto::eeprom::Layout::read_plan` (16-byte GETs, page 0 one byte at
//! a time), pages in page-number order as `FetchTask` does.
//!
//! XBTune additions:
//! - **Resume.** The state lives in [`EepromRead`], not in the connection.
//!   When a request fails after the client's retries, [`EepromRead::run`]
//!   returns the error and keeps everything read so far; reconnect and call
//!   it again to continue.
//! - **Verify.** The frame checksum is an XOR, which misses some multi-bit
//!   errors. With verification on, a chunk is accepted only once two
//!   consecutive reads of it agree. An image used as a backup before writing
//!   must be read this way.

use xbtune_link::EcmClient;
use xbtune_proto::TransactionError;
use xbtune_proto::eeprom::{Chunk, Layout};

/// Reads of one chunk allowed before it is declared unstable.
pub const MAX_READS_PER_CHUNK: u32 = 6;

#[derive(Debug, Clone)]
struct ChunkState {
    chunk: Chunk,
    last: Option<Vec<u8>>,
    reads: u32,
    confirmed: bool,
}

/// Progress of a read, for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    /// Page of the chunk just read.
    pub page: u8,
    /// Chunks whose two reads disagreed so far.
    pub mismatches: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error(transparent)]
    Link(#[from] TransactionError),
    #[error(
        "page {page} offset {offset:#04x}: {reads} reads never agreed twice in a row; the link or the ECM is unreliable"
    )]
    Unstable { page: u8, offset: u8, reads: u32 },
}

/// A full EEPROM read in progress.
#[derive(Debug, Clone)]
pub struct EepromRead {
    layout: Layout,
    chunks: Vec<ChunkState>,
    verify: bool,
    mismatches: usize,
}

impl EepromRead {
    pub fn new(layout: &Layout, verify: bool) -> Self {
        let chunks = layout
            .pages
            .iter()
            .flat_map(|p| layout.read_plan(p.nr).expect("page from layout"))
            .map(|chunk| ChunkState { chunk, last: None, reads: 0, confirmed: false })
            .collect();
        Self { layout: layout.clone(), chunks, verify, mismatches: 0 }
    }

    /// Reads needed per chunk: 1, or 2 when verifying (more on mismatch).
    fn needed(&self) -> u32 {
        if self.verify { 2 } else { 1 }
    }

    pub fn progress(&self, page: u8) -> Progress {
        let n = self.needed() as usize;
        let done =
            self.chunks.iter().map(|c| if c.confirmed { n } else { (c.reads as usize).min(n - 1) }).sum();
        Progress { done, total: self.chunks.len() * n, page, mismatches: self.mismatches }
    }

    pub fn is_complete(&self) -> bool {
        self.chunks.iter().all(|c| c.confirmed)
    }

    /// Chunks whose reads disagreed at least once.
    pub fn mismatches(&self) -> usize {
        self.mismatches
    }

    /// Read until complete. On error, everything read so far is kept.
    ///
    /// Without verification, chunks are read once in order. With it, the
    /// whole EEPROM is read once, then once more, so the two reads of a chunk
    /// are far apart in time; chunks that disagree are re-read.
    pub async fn run(
        &mut self,
        client: &mut EcmClient,
        mut on_progress: impl FnMut(Progress),
    ) -> Result<(), ReadError> {
        loop {
            // Pick the next chunk: first pass (never read), then second pass.
            let next = self
                .chunks
                .iter()
                .position(|c| c.reads == 0)
                .or_else(|| self.chunks.iter().position(|c| !c.confirmed));
            let Some(i) = next else { return Ok(()) };
            let chunk = self.chunks[i].chunk;
            let data = client.read_bytes(chunk.page, chunk.wire_offset, chunk.len).await?;
            let verify = self.verify;
            let st = &mut self.chunks[i];
            st.reads += 1;
            if !verify {
                st.last = Some(data);
                st.confirmed = true;
            } else if st.last.as_ref() == Some(&data) {
                st.confirmed = true;
            } else {
                if st.last.is_some() {
                    self.mismatches += 1;
                    tracing::warn!(
                        page = chunk.page,
                        offset = chunk.wire_offset,
                        "EEPROM reads disagree; re-reading"
                    );
                }
                st.last = Some(data);
                if st.reads >= MAX_READS_PER_CHUNK {
                    return Err(ReadError::Unstable {
                        page: chunk.page,
                        offset: chunk.wire_offset,
                        reads: st.reads,
                    });
                }
            }
            on_progress(self.progress(chunk.page));
        }
    }

    /// The image bytes (`xsize`), once complete.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        if !self.is_complete() {
            return None;
        }
        let mut out = vec![0u8; self.layout.xsize];
        for c in &self.chunks {
            let data = c.last.as_ref().expect("complete");
            out[c.chunk.image_range()].copy_from_slice(data);
        }
        Some(out)
    }

    /// True if the layout has a page 0 (so the image's page 0 bytes are real).
    pub fn has_page_zero(&self) -> bool {
        self.layout.page(0).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_link::client::fast_policy;
    use xbtune_proto::EcmAddress;
    use xbtune_sim::{Fault, Simulator, in_process, shared};

    fn client(sim: &xbtune_sim::SharedSim) -> EcmClient {
        EcmClient::new(in_process(sim.clone()), EcmAddress::Stock).with_policy(fast_policy())
    }

    fn sim_with_page_zero() -> xbtune_sim::SharedSim {
        let mut img = xbtune_sim::BUEIB_EEPROM.to_vec();
        img.extend_from_slice(&[0xA1, 0xB2, 0xC3, 0xD4]);
        shared(Simulator::new(xbtune_sim::SimConfig::bueib(), &img))
    }

    #[tokio::test]
    async fn verified_read_matches_sim() {
        let sim = sim_with_page_zero();
        let layout = sim.lock().unwrap().config().layout.clone();
        let mut r = EepromRead::new(&layout, true);
        let mut last = None;
        r.run(&mut client(&sim), |p| last = Some(p)).await.unwrap();
        let p = last.unwrap();
        assert_eq!(p.done, p.total);
        assert_eq!(r.bytes().unwrap(), sim.lock().unwrap().image());
        assert_eq!(&r.bytes().unwrap()[1206..], &[0xA1, 0xB2, 0xC3, 0xD4]);
        assert!(r.has_page_zero());
        assert_eq!(r.mismatches(), 0);
    }

    #[tokio::test]
    async fn resumes_after_link_failure() {
        let sim = sim_with_page_zero();
        let layout = sim.lock().unwrap().config().layout.clone();
        let mut r = EepromRead::new(&layout, false);
        // Let 10 requests through, then drop enough replies to exhaust retries.
        sim.lock()
            .unwrap()
            .script(std::iter::repeat_n(None, 10).chain(std::iter::repeat_n(Some(Fault::Drop), 3)));
        let err = r.run(&mut client(&sim), |_| {}).await.unwrap_err();
        assert!(matches!(err, ReadError::Link(_)));
        let before = r.progress(0).done;
        assert_eq!(before, 10);
        // "Reconnect" (new client) and continue.
        r.run(&mut client(&sim), |_| {}).await.unwrap();
        assert_eq!(r.bytes().unwrap(), sim.lock().unwrap().image());
    }

    #[tokio::test]
    async fn verify_catches_a_flipped_byte() {
        // A reply whose data is wrong but whose checksum still passes (the
        // XOR misses it) must be caught by the second read.
        let sim = sim_with_page_zero();
        let layout = sim.lock().unwrap().config().layout.clone();
        sim.lock().unwrap().script([Some(Fault::SilentFlip)]);
        let mut r = EepromRead::new(&layout, true);
        r.run(&mut client(&sim), |_| {}).await.unwrap();
        assert_eq!(r.mismatches(), 1);
        assert_eq!(r.bytes().unwrap(), sim.lock().unwrap().image());
    }

    #[tokio::test]
    async fn unverified_read_is_fooled_by_silent_flip() {
        let sim = sim_with_page_zero();
        let layout = sim.lock().unwrap().config().layout.clone();
        sim.lock().unwrap().script([Some(Fault::SilentFlip)]);
        let mut r = EepromRead::new(&layout, false);
        r.run(&mut client(&sim), |_| {}).await.unwrap();
        assert_ne!(r.bytes().unwrap(), sim.lock().unwrap().image());
    }

    #[tokio::test]
    async fn unstable_chunk_fails() {
        let sim = sim_with_page_zero();
        let layout = sim.lock().unwrap().config().layout.clone();
        // First chunk's first read is fine; then every read of it differs.
        let n = layout.pages.iter().map(|p| layout.read_plan(p.nr).unwrap().len()).sum::<usize>();
        let mut script: Vec<Option<Fault>> = vec![None; n];
        script.extend(std::iter::repeat_n(Some(Fault::SilentFlipVarying), 10));
        sim.lock().unwrap().script(script);
        let mut r = EepromRead::new(&layout, true);
        assert!(matches!(r.run(&mut client(&sim), |_| {}).await, Err(ReadError::Unstable { .. })));
    }
}
