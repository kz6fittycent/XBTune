// SPDX-License-Identifier: GPL-3.0-or-later
//! A simulated ECM.
//!
//! It answers the same requests as a real DDFI-2/DDFI-3 ECM, serving an
//! EEPROM image and a runtime-data frame, and can inject the faults a
//! Bluetooth link produces: dropped, corrupted, delayed, truncated and
//! fragmented replies, garbage bytes, NAKs, and echoed requests.
//!
//! The responder ([`Simulator::respond`]) is sans-I/O. [`in_process`] serves
//! it over an in-memory stream, [`pty`] over a pseudo-terminal that real
//! serial code can open.
//!
//! Behaviour that is *not* confirmed against a real ECM, and must not be
//! relied on outside tests, is marked "ASSUMED" (see docs/PROVENANCE.md).

pub mod engine;
pub mod pty;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use xbtune_defs::{Catalog, EcmDef};
use xbtune_proto::decoder::Decoded;
use xbtune_proto::eeprom::{Layout, PAGE_ZERO_END};
use xbtune_proto::pdu::{ACK, CMD_GET, CMD_RTDATA, CMD_SET, CMD_VERSION, CONTROL_PAGE, HOST_ID};
use xbtune_proto::{EcmAddress, FrameDecoder, Function, Pdu};

/// Status byte the simulator sends for requests it refuses. ASSUMED: the
/// real ECM's error codes are not documented in EcmDroid.
pub const SIM_NAK: u8 = 0x15;

/// Runtime frame captured from a BUEIB by EcmDroid (test resource).
pub const BUEIB_RUNTIME: &[u8] = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin");
/// BUEIB EEPROM image from EcmDroid's tests (1206 bytes, no page 0).
pub const BUEIB_EEPROM: &[u8] = include_bytes!("../../../tests/fixtures/BUEIB.eeprom");

/// A fault to apply to one reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// Send nothing.
    Drop,
    /// Flip bits in the checksum.
    Corrupt,
    /// Send the first `n` bytes only.
    Truncate(usize),
    /// Send random bytes before the reply.
    Garbage(usize),
    /// Send the reply in pieces of `n` bytes with a short pause between.
    Fragment(usize),
    /// Wait before replying.
    Delay(Duration),
    /// Refuse with this status byte.
    Nak(u8),
    /// Change the first data byte and fix up the checksum: a frame that
    /// passes every check but carries wrong data.
    SilentFlip,
    /// Like `SilentFlip`, but with a different wrong value every time, so
    /// repeated reads never agree.
    SilentFlipVarying,
    /// The request is lost on the way: the ECM never sees it (nothing is
    /// executed) and nothing comes back.
    LoseRequest,
}

/// Random faults, as probabilities per reply (0.0..=1.0).
#[derive(Debug, Clone, Default)]
pub struct FaultRates {
    pub drop: f64,
    pub corrupt: f64,
    pub garbage: f64,
    pub fragment: f64,
    pub delay: f64,
    pub delay_by: Duration,
}

/// Something the server should do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emission {
    Write(Vec<u8>),
    Sleep(Duration),
}

/// Simulator settings.
#[derive(Debug, Clone)]
pub struct SimConfig {
    pub version: String,
    pub address: EcmAddress,
    pub layout: Layout,
    pub runtime_len: usize,
    /// Echo every request back before replying, like a half-duplex adapter.
    pub echo: bool,
    pub rates: FaultRates,
    pub seed: u64,
    /// Model the K-line: each reply takes 10 bits per byte at this rate to
    /// arrive (a real 107-byte runtime frame is ~111 ms at 9600 baud).
    pub wire_baud: Option<u32>,
    /// How long the ECM reports busy after an active test or TPS reset.
    /// ASSUMED: real durations are not documented in EcmDroid.
    pub test_duration: Duration,
}

impl SimConfig {
    /// Settings for the ECM `id` from the built-in definitions.
    pub fn for_ecm(def: &EcmDef, version_suffix: &str) -> Self {
        Self {
            version: format!("{}{}", def.id, version_suffix),
            address: if def.is_race() { EcmAddress::Race } else { EcmAddress::Stock },
            layout: def.layout().expect("built-in layouts are valid"),
            runtime_len: def.runtime_len,
            echo: false,
            rates: FaultRates::default(),
            seed: 0x5EED_B0E1,
            wire_baud: None,
            test_duration: Duration::from_secs(3),
        }
    }

    /// A BUEIB (2005-2007 XB, DDFI-2) like the one on the first test bike.
    pub fn bueib() -> Self {
        let catalog = Catalog::builtin().expect("built-in definitions load");
        Self::for_ecm(catalog.lookup("BUEIB").expect("BUEIB defined"), "310 12-11-03")
    }
}

/// What the simulated ECM has done, for test assertions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Journal {
    pub requests: u64,
    pub writes: Vec<(u8, u8, Vec<u8>)>,
    pub functions: Vec<u8>,
}

/// The simulated ECM.
#[derive(Debug)]
pub struct Simulator {
    cfg: SimConfig,
    image: Vec<u8>,
    runtime: Vec<u8>,
    state: u8,
    script: VecDeque<Option<Fault>>,
    rng: u64,
    flips: u8,
    /// Made-up live data: the engine, when it started, and a fixed time
    /// for tests.
    engine: Option<(engine::Engine, Instant, Option<f64>)>,
    engine_running: bool,
    /// Busy (state 1) until then, after a test command.
    busy_until: Option<Instant>,
    pub journal: Journal,
}

impl Simulator {
    /// A simulator serving `image` (either `size` or `xsize` bytes; a
    /// missing page 0 is filled with zeros).
    pub fn new(cfg: SimConfig, image: &[u8]) -> Self {
        let mut full = image.to_vec();
        full.resize(cfg.layout.xsize, 0);
        let runtime = if cfg.version.starts_with("BUEIB") && cfg.runtime_len == BUEIB_RUNTIME.len() {
            BUEIB_RUNTIME.to_vec()
        } else {
            // Zeros with an ACK, the right length for this ECM.
            let mut payload = vec![0u8; cfg.runtime_len.saturating_sub(8)];
            if let Some(b) = payload.first_mut() {
                *b = ACK;
            }
            Pdu::new(cfg.address.id(), HOST_ID, &payload).into_bytes()
        };
        let rng = cfg.seed.max(1);
        Self {
            cfg,
            image: full,
            runtime,
            state: 0,
            script: VecDeque::new(),
            rng,
            flips: 0,
            engine: None,
            engine_running: true,
            busy_until: None,
            journal: Journal::default(),
        }
    }

    /// The default BUEIB simulator with EcmDroid's test image.
    pub fn bueib() -> Self {
        Self::new(SimConfig::bueib(), BUEIB_EEPROM)
    }

    /// [`Simulator::bueib`] with the made-up engine running.
    pub fn bueib_running() -> Self {
        let c = Catalog::builtin().expect("built-in definitions load");
        let def = c.definition(c.lookup("BUEIB").expect("BUEIB defined")).expect("BUEIB definition").clone();
        Self::bueib().with_engine(def)
    }

    /// Serve moving runtime data from the made-up engine in [`engine`],
    /// using `def`'s offsets (the captured frame stays the template).
    pub fn with_engine(mut self, def: Arc<xbtune_defs::Definition>) -> Self {
        self.engine = Some((engine::Engine::new(def), Instant::now(), None));
        self
    }

    /// Start or stop the made-up engine (stopped: ignition on, RPM 0).
    pub fn set_engine_running(&mut self, running: bool) {
        self.engine_running = running;
    }

    pub fn engine_running(&self) -> bool {
        self.engine_running
    }

    /// How long tests keep the ECM busy.
    pub fn set_test_duration(&mut self, d: Duration) {
        self.cfg.test_duration = d;
    }

    /// Freeze the engine at `t` seconds (tests), or let it run (`None`).
    pub fn set_engine_time(&mut self, t: Option<f64>) {
        if let Some(e) = &mut self.engine {
            e.2 = t;
        }
    }

    pub fn config(&self) -> &SimConfig {
        &self.cfg
    }

    pub fn image(&self) -> &[u8] {
        &self.image
    }

    /// Model wire time at this baud rate (`None`: replies are instant).
    pub fn set_wire_baud(&mut self, baud: Option<u32>) {
        self.cfg.wire_baud = baud;
    }

    pub fn set_rates(&mut self, rates: FaultRates) {
        self.cfg.rates = rates;
    }

    /// Queue faults for the next replies, one per reply, before random ones.
    /// `None` lets a reply through untouched.
    pub fn script<F: Into<Option<Fault>>>(&mut self, faults: impl IntoIterator<Item = F>) {
        self.script.extend(faults.into_iter().map(Into::into));
    }

    /// Set the busy/idle state byte.
    pub fn set_state(&mut self, state: u8) {
        self.state = state;
    }

    /// Handle one decoded frame. Returns what to send back.
    pub fn respond(&mut self, frame: &Pdu) -> Vec<Emission> {
        if !frame.is_request_to(self.cfg.address) {
            return Vec::new();
        }
        self.journal.requests += 1;
        let mut out = Vec::new();
        if self.cfg.echo {
            out.push(Emission::Write(frame.as_bytes().to_vec()));
        }
        let fault = self.next_fault();
        if fault == Some(Fault::LoseRequest) {
            self.journal.requests -= 1;
            return out;
        }
        if let Some(Fault::Nak(code)) = fault {
            out.push(Emission::Write(self.reply(code, &[])));
            return out;
        }
        let reply = self.answer(frame.payload());
        self.apply(fault, reply, &mut out);
        if let Some(baud) = self.cfg.wire_baud.filter(|b| *b > 0) {
            out = out
                .into_iter()
                .flat_map(|e| match e {
                    Emission::Write(b) => {
                        let wire = Duration::from_micros(b.len() as u64 * 10 * 1_000_000 / baud as u64);
                        vec![Emission::Sleep(wire), Emission::Write(b)]
                    }
                    other => vec![other],
                })
                .collect();
        }
        out
    }

    fn answer(&mut self, req: &[u8]) -> Vec<u8> {
        match req {
            [CMD_VERSION] => {
                let v = self.cfg.version.clone();
                self.reply(ACK, v.as_bytes())
            }
            [CMD_RTDATA] => match &self.engine {
                Some((e, start, fixed)) => {
                    let t = fixed.unwrap_or_else(|| start.elapsed().as_secs_f64());
                    e.frame_with(&self.runtime, t, self.engine_running)
                }
                None => self.runtime.clone(),
            },
            [CMD_GET, 0, CONTROL_PAGE, 1] => {
                let busy = self.busy_until.is_some_and(|t| Instant::now() < t);
                // ASSUMED: busy reads as 1 during a test.
                let s = if busy { 1 } else { self.state };
                self.reply(ACK, &[s])
            }
            [CMD_GET, offset, page, len] => match self.map(*page, *offset, *len as usize) {
                Some(r) => {
                    let data = self.image[r].to_vec();
                    self.reply(ACK, &data)
                }
                None => self.reply(SIM_NAK, &[]),
            },
            [CMD_SET, 0, CONTROL_PAGE, code] => {
                self.journal.functions.push(*code);
                if *code == Function::ClearCodes.code() {
                    // ASSUMED: clearing zeroes the stored-code bytes; current
                    // codes stay while the fault exists.
                    self.clear_stored_codes();
                } else {
                    self.busy_until = Some(Instant::now() + self.cfg.test_duration);
                }
                self.reply(ACK, &[])
            }
            [CMD_SET, offset, page, data @ ..] if !data.is_empty() => {
                match self.map(*page, *offset, data.len()) {
                    Some(r) => {
                        self.image[r].copy_from_slice(data);
                        self.journal.writes.push((*page, *offset, data.to_vec()));
                        self.reply(ACK, &[])
                    }
                    None => self.reply(SIM_NAK, &[]),
                }
            }
            _ => self.reply(SIM_NAK, &[]),
        }
    }

    /// Zero the stored-code bytes (`HDiag<n>_LD`) in the runtime frame.
    fn clear_stored_codes(&mut self) {
        let Ok(c) = Catalog::builtin() else { return };
        let Some(def) = c.lookup(&self.cfg.version).and_then(|e| c.definition(e)) else { return };
        for v in def.runtime.values().filter(|v| v.varname.starts_with("HDiag") && v.varname.ends_with("_LD"))
        {
            if let Some(b) = self.runtime.get_mut(v.offset) {
                *b = 0;
            }
        }
        let n = self.runtime.len();
        if n > 1 {
            self.runtime[n - 1] = xbtune_proto::pdu::checksum(&self.runtime);
        }
    }

    /// Map a wire (page, offset, len) to a range of the flat image.
    fn map(&self, page: u8, offset: u8, len: usize) -> Option<std::ops::Range<usize>> {
        let p = self.cfg.layout.page(page)?;
        if len == 0 || len > 16 {
            return None;
        }
        let in_page = if page == 0 {
            // Page 0 occupies wire offsets ending at 0xFF.
            (offset as usize).checked_sub(PAGE_ZERO_END + 1 - p.len)?
        } else {
            offset as usize
        };
        (in_page + len <= p.len).then(|| p.start + in_page..p.start + in_page + len)
    }

    fn reply(&self, status: u8, data: &[u8]) -> Vec<u8> {
        let mut payload = Vec::with_capacity(data.len() + 1);
        payload.push(status);
        payload.extend_from_slice(data);
        Pdu::new(self.cfg.address.id(), HOST_ID, &payload).into_bytes()
    }

    fn next_fault(&mut self) -> Option<Fault> {
        if let Some(f) = self.script.pop_front() {
            return f;
        }
        let r = self.cfg.rates.clone();
        if self.roll(r.drop) {
            Some(Fault::Drop)
        } else if self.roll(r.corrupt) {
            Some(Fault::Corrupt)
        } else if self.roll(r.garbage) {
            Some(Fault::Garbage(1 + (self.rand() % 8) as usize))
        } else if self.roll(r.fragment) {
            Some(Fault::Fragment(1 + (self.rand() % 6) as usize))
        } else if self.roll(r.delay) {
            Some(Fault::Delay(r.delay_by))
        } else {
            None
        }
    }

    fn apply(&mut self, fault: Option<Fault>, mut reply: Vec<u8>, out: &mut Vec<Emission>) {
        match fault {
            None | Some(Fault::Nak(_)) => out.push(Emission::Write(reply)),
            Some(Fault::Drop | Fault::LoseRequest) => {}
            Some(Fault::Corrupt) => {
                let n = reply.len();
                reply[n - 1] ^= 0x5A;
                out.push(Emission::Write(reply));
            }
            Some(Fault::Truncate(n)) => {
                reply.truncate(n.min(reply.len()));
                out.push(Emission::Write(reply));
            }
            Some(Fault::Garbage(n)) => {
                let mut g: Vec<u8> = (0..n).map(|_| self.rand() as u8).collect();
                g.extend_from_slice(&reply);
                out.push(Emission::Write(g));
            }
            Some(Fault::Fragment(n)) => {
                for piece in reply.chunks(n.max(1)) {
                    out.push(Emission::Write(piece.to_vec()));
                    out.push(Emission::Sleep(Duration::from_millis(3)));
                }
            }
            Some(Fault::Delay(d)) => {
                out.push(Emission::Sleep(d));
                out.push(Emission::Write(reply));
            }
            Some(f @ (Fault::SilentFlip | Fault::SilentFlipVarying)) => {
                // Data starts after the 6-byte header and the status byte.
                let n = reply.len();
                if n > 9 {
                    let mask = if f == Fault::SilentFlip {
                        0x01
                    } else {
                        self.flips = self.flips.wrapping_add(1).max(1);
                        self.flips
                    };
                    reply[7] ^= mask;
                    reply[n - 1] = xbtune_proto::pdu::checksum(&reply);
                }
                out.push(Emission::Write(reply));
            }
        }
    }

    fn roll(&mut self, p: f64) -> bool {
        p > 0.0 && (self.rand() as f64 / u64::MAX as f64) < p
    }

    fn rand(&mut self) -> u64 {
        // xorshift64
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

/// A simulator shared between a server task and a test.
pub type SharedSim = Arc<Mutex<Simulator>>;

/// Serve `sim` over an async byte stream until it closes.
pub async fn serve<T: AsyncRead + AsyncWrite + Unpin>(sim: SharedSim, mut io: T) -> std::io::Result<()> {
    let mut decoder = FrameDecoder::new();
    let mut buf = [0u8; 512];
    loop {
        let n = io.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        decoder.push(&buf[..n]);
        while let Some(d) = decoder.next_frame() {
            let Decoded::Frame(frame) = d else { continue };
            let emissions = sim.lock().expect("sim lock").respond(&frame);
            for e in emissions {
                match e {
                    Emission::Write(b) => {
                        io.write_all(&b).await?;
                        io.flush().await?;
                    }
                    Emission::Sleep(d) => tokio::time::sleep(d).await,
                }
            }
        }
    }
}

/// Start `sim` on a background task and return the client end of an
/// in-memory stream connected to it. Needs a tokio runtime.
pub fn in_process(sim: SharedSim) -> xbtune_link::BoxIo {
    let (client, server) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let _ = serve(sim, server).await;
    });
    Box::new(client)
}

/// Wrap a simulator for sharing.
pub fn shared(sim: Simulator) -> SharedSim {
    Arc::new(Mutex::new(sim))
}
