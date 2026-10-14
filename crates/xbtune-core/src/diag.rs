// SPDX-License-Identifier: GPL-3.0-or-later
//! Diagnostics: trouble codes, active tests, clearing codes, TPS reset.
//!
//! Ported from EcmDroid (commit 2f17d29f5b08f67fb357ee8f2ec21e7ae930343e,
//! Copyright (C) 2012 by Michel Marti):
//! - trouble codes: `ECM.getErrors()` - current codes are the set bits of
//!   the runtime `CDiag<n>` bytes, stored codes of `HDiag<n>_LD` (or of the
//!   EEPROM's `HDiag<n>` when there is no live data), `n` counting up from 0
//!   until a byte is missing; the code is the bit's DTC, the text its remark;
//! - functions: `ECM.runTest()` sends the function as a SET to the control
//!   page and requires an ACK; `ActiveTestsFragment` then polls the busy
//!   state every 500 ms until it reads 0; the test list, the TPS reset
//!   instructions, and "DDFI-3: follow the service manual" also come from
//!   there.
//!
//! XBTune additions, for safety on a Bluetooth link:
//! - nothing that changes the ECM runs on real hardware without a
//!   [`Permit`], which the owner grants after signing off on the simulator
//!   tests (KICKOFF §6.7, §9);
//! - actuator tests and TPS reset refuse to run unless a fresh runtime frame
//!   shows the engine stopped (RPM 0), and refuse while the ECM is busy;
//! - a function is never re-sent automatically. If its reply is lost, the
//!   busy state decides whether it ran.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use xbtune_defs::{Definition, EcmDef, Family, ImageView};
use xbtune_link::EcmClient;
use xbtune_proto::{Function, TransactionError};

/// Current (active now) or stored (history) codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeKind {
    Current,
    Stored,
}

/// One trouble code bit that is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TroubleCode {
    pub kind: CodeKind,
    /// Blink code, e.g. "21". Empty if the definition gives none.
    pub code: String,
    /// Short name, e.g. "AMC open".
    pub name: String,
    /// Description, e.g. "AMC stuck open".
    pub description: String,
    /// Where it came from, e.g. `CDiag4` bit 0.
    pub var: String,
    pub bit: u8,
}

/// (variable name, byte value if readable, its bits)
type CodeByte = (String, Option<u8>, Vec<xbtune_defs::BitDef>);

fn collect(kind: CodeKind, bytes: Vec<CodeByte>) -> Vec<TroubleCode> {
    let mut out = Vec::new();
    for (var, byte, bits) in bytes {
        let Some(byte) = byte else { continue };
        for b in bits.iter().filter(|b| byte & (1 << b.nr) != 0) {
            out.push(TroubleCode {
                kind,
                code: b.dtc.clone().unwrap_or_default(),
                name: b.name.clone(),
                description: b.description.clone().unwrap_or_else(|| b.name.clone()),
                var: var.clone(),
                bit: b.nr,
            });
        }
    }
    out
}

/// Codes of `kind` in a runtime frame.
pub fn codes_from_runtime(def: &Definition, frame: &[u8], kind: CodeKind) -> Vec<TroubleCode> {
    let mut bytes = Vec::new();
    for i in 0.. {
        let varname = match kind {
            CodeKind::Current => format!("CDiag{i}"),
            CodeKind::Stored => format!("HDiag{i}_LD"),
        };
        let Some(v) = def.runtime.values().find(|v| v.varname == varname) else { break };
        let bits = v.bits.as_ref().map(|b| b.bits.clone()).unwrap_or_default();
        bytes.push((varname, v.raw(frame).map(|r| r as u8), bits));
    }
    collect(kind, bytes)
}

/// Stored codes in an EEPROM image (`HDiag<n>`), for when there is no
/// live data. Bytes in a missing page 0 are skipped.
pub fn stored_codes_from_image(def: &Definition, img: &ImageView) -> Vec<TroubleCode> {
    let mut bytes = Vec::new();
    for i in 0.. {
        let varname = format!("HDiag{i}");
        let Some(v) = def.eeprom.get(&varname) else { break };
        let byte = v.raw(img).ok().and_then(|r| r.first().copied()).map(|r| r as u8);
        let bits = v.bits.as_ref().map(|b| b.bits.clone()).unwrap_or_default();
        bytes.push((varname, byte, bits));
    }
    collect(CodeKind::Stored, bytes)
}

/// Active tests, in EcmDroid's order. The ECM refuses (NAK) or ignores
/// tests for hardware that isn't fitted.
pub const TESTS: &[Function] = &[
    Function::FrontCoil,
    Function::RearCoil,
    Function::Tachometer,
    Function::FuelPump,
    Function::FrontInjector,
    Function::RearInjector,
    Function::Fan,
    Function::ExhaustValve,
    Function::ActiveIntake,
    Function::ShiftLight,
];

/// Find a function by a loose name ("front-coil", "FrontCoil", "fan").
pub fn function_by_name(name: &str) -> Option<Function> {
    let norm = |s: &str| s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    let want = norm(name);
    TESTS
        .iter()
        .chain([Function::ClearCodes, Function::TpsReset].iter())
        .copied()
        .find(|f| norm(f.label()) == want)
}

/// What the rider must check before a function runs.
pub fn safety_notice(f: Function) -> &'static str {
    match f {
        Function::FrontCoil | Function::RearCoil => {
            "Fires the ignition coil. Keep clear of plug leads and fuel vapour; plugs must be in their caps."
        }
        Function::FrontInjector | Function::RearInjector => {
            "Opens the injector and sprays fuel into the cylinder. Engine stopped, no ignition source nearby; \
             don't run it repeatedly or the cylinder can flood."
        }
        Function::FuelPump => "Runs the fuel pump. Check for leaks; keep away from open fuel lines.",
        Function::Fan => "Runs the cooling fan. Keep hands and loose items clear of the fan.",
        Function::ExhaustValve => {
            "Moves the exhaust valve actuator (if fitted). Keep clear of the muffler valve."
        }
        Function::ActiveIntake => "Moves the active intake actuator (if fitted). Keep clear of the airbox.",
        Function::Tachometer => "Sweeps the tachometer.",
        Function::ShiftLight => "Lights the shift light (if fitted).",
        Function::TpsReset => {
            "Back out idle adjuster. Open and snap shut the throttle grip 2-3 times. Make sure the throttle \
             plate is completely closed (\"sticks\" in the throttle body). Then run the reset. Afterwards open \
             the throttle 5-6 degrees and set idle to 1050-1100 rpm."
        }
        Function::ClearCodes => {
            "Permanently clears the stored trouble codes. Current codes return while the fault exists."
        }
    }
}

/// Whether XBTune offers a TPS reset for this ECM. EcmDroid sends it only
/// to DDFI-2; for DDFI-3 it says to follow the service manual.
pub fn tps_reset_supported(ecm: &EcmDef) -> bool {
    ecm.family == Family::Ddfi2
}

/// Proof that commands which change the ECM may be sent to this target.
#[derive(Debug, Clone)]
pub struct Permit {
    _private: (),
}

/// File that unlocks ECM commands on real hardware.
pub fn permit_file() -> Option<PathBuf> {
    crate::env::Dirs::detect().map(|d| d.config.join("hardware.toml"))
}

/// Why a [`Permit`] was refused.
pub fn locked_message() -> String {
    let path =
        permit_file().map(|p| p.display().to_string()).unwrap_or_else(|| "<config>/hardware.toml".into());
    format!(
        "ECM commands (active tests, clearing codes, TPS reset) are locked on real hardware until the \
         owner signs off on the simulator tests (KICKOFF section 6.7). To unlock, create {path} containing:\n    \
         ecm_commands = true"
    )
}

/// A permit for the simulator (always), or for real hardware when the
/// owner has unlocked it.
pub fn permit(simulated: bool) -> Result<Permit, String> {
    if simulated || permit_file().is_some_and(|p| unlocked(&p)) {
        Ok(Permit { _private: () })
    } else {
        Err(locked_message())
    }
}

fn unlocked(path: &std::path::Path) -> bool {
    #[derive(serde::Deserialize)]
    struct Hw {
        #[serde(default)]
        ecm_commands: bool,
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| toml::from_str::<Hw>(&t).ok())
        .is_some_and(|h| h.ecm_commands)
}

#[cfg(test)]
pub(crate) fn test_permit() -> Permit {
    Permit { _private: () }
}

/// How a function ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub function: Function,
    /// False when the reply was lost but the busy state showed it running.
    pub acknowledged: bool,
    /// How long the ECM stayed busy.
    pub busy_for: Duration,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FunctionError {
    #[error("the engine is running ({0:.0} RPM); stop it first")]
    EngineRunning(f64),
    #[error("can't confirm the engine is stopped: {0}")]
    NoEngineCheck(String),
    #[error("the ECM is busy (state {0:#04x}); wait for it to finish")]
    Busy(u8),
    #[error("{0} isn't offered for this ECM: {1}")]
    NotSupported(&'static str, &'static str),
    #[error("the ECM refused {0} (status {1:#04x}); the hardware may not be fitted")]
    Refused(&'static str, u8),
    #[error(
        "no reply to {0}, and the ECM is idle, so it probably didn't run. It was not re-sent; run it again if you want to."
    )]
    NotConfirmed(&'static str),
    #[error(
        "no reply to {0}, and the busy state can't be read either ({1}); check the bike before doing anything else"
    )]
    Unknown(&'static str, String),
    #[error("{0} was still running after {1:?}; stopped waiting (the ECM may still be busy)")]
    StillBusy(&'static str, Duration),
    #[error("link error: {0}")]
    Link(String),
}

/// Timing for [`run_function`].
#[derive(Debug, Clone, Copy)]
pub struct FunctionTiming {
    /// Busy-state poll interval (EcmDroid: 500 ms).
    pub poll: Duration,
    /// Give up waiting for idle after this long.
    pub max_busy: Duration,
}

impl Default for FunctionTiming {
    fn default() -> Self {
        Self { poll: Duration::from_millis(500), max_busy: Duration::from_secs(60) }
    }
}

/// Progress callback stages.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    Checking,
    Sent,
    Running { seconds: f64 },
}

/// Run an active test, TPS reset or code clear on the ECM, with the safety
/// checks described in the module docs.
pub async fn run_function(
    client: &mut EcmClient,
    ecm: &EcmDef,
    def: &Definition,
    f: Function,
    _permit: &Permit,
    timing: FunctionTiming,
    mut progress: impl FnMut(Stage),
) -> Result<Outcome, FunctionError> {
    let name = f.label();
    progress(Stage::Checking);
    if f == Function::TpsReset && !tps_reset_supported(ecm) {
        return Err(FunctionError::NotSupported(name, "follow the service manual for a DDFI-3 TPS reset"));
    }
    if f != Function::ClearCodes {
        let frame = client.runtime_data().await.map_err(|e| FunctionError::NoEngineCheck(e.to_string()))?;
        let rpm = def
            .runtime
            .values()
            .find(|v| v.varname == "RPM")
            .and_then(|v| v.value(frame.as_bytes()))
            .ok_or_else(|| FunctionError::NoEngineCheck("no RPM channel in the definition".into()))?;
        if rpm > 0.0 {
            return Err(FunctionError::EngineRunning(rpm));
        }
    }
    let state = client.current_state().await.map_err(|e| FunctionError::Link(e.to_string()))?;
    if state != 0 {
        return Err(FunctionError::Busy(state));
    }

    // Sent once. Command requests are never retried by the client.
    let acknowledged = match client.transact(&xbtune_proto::Request::Command(f)).await {
        Ok(_) => true,
        Err(TransactionError::Nak(code)) => return Err(FunctionError::Refused(name, code)),
        Err(lost) => {
            tracing::warn!(function = name, error = %lost, "no reply to function; checking busy state");
            match client.current_state().await {
                Ok(0) if f == Function::ClearCodes => {
                    return Err(FunctionError::Unknown(
                        name,
                        "clearing doesn't show as busy; read the codes again".into(),
                    ));
                }
                Ok(0) => return Err(FunctionError::NotConfirmed(name)),
                Ok(_) => false,
                Err(e) => return Err(FunctionError::Unknown(name, e.to_string())),
            }
        }
    };
    progress(Stage::Sent);

    let started = Instant::now();
    if f != Function::ClearCodes {
        let mut failures = 0;
        loop {
            tokio::time::sleep(timing.poll).await;
            match client.current_state().await {
                Ok(0) => break,
                Ok(_) => failures = 0,
                Err(e) => {
                    failures += 1;
                    if failures >= 5 {
                        return Err(FunctionError::Unknown(name, e.to_string()));
                    }
                }
            }
            let busy = started.elapsed();
            progress(Stage::Running { seconds: busy.as_secs_f64() });
            if busy >= timing.max_busy {
                return Err(FunctionError::StillBusy(name, timing.max_busy));
            }
        }
    }
    Ok(Outcome { function: f, acknowledged, busy_for: started.elapsed() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_defs::Catalog;
    use xbtune_proto::EcmAddress;
    use xbtune_sim::{Fault, Simulator, shared};

    const RT_BUEIB: &[u8] = include_bytes!("../../../tests/fixtures/RT_BUEIB.bin");
    const RT_BUEIB242: &[u8] = include_bytes!("../../../tests/fixtures/RT_BUEIB242.bin");

    fn bueib() -> (EcmDef, std::sync::Arc<Definition>) {
        let c = Catalog::builtin().unwrap();
        let e = c.lookup("BUEIB").unwrap().clone();
        let d = c.definition(&e).unwrap().clone();
        (e, d)
    }

    #[test]
    fn codes_match_ecmdroid() {
        let (_, d) = bueib();
        // TestECM.testErrorParsing: one current code "21" in RT_BUEIB242.
        let cur = codes_from_runtime(&d, RT_BUEIB242, CodeKind::Current);
        assert_eq!(cur.iter().map(|c| c.code.as_str()).collect::<Vec<_>>(), ["21"]);
        let c = &cur[0];
        assert_eq!(
            (c.name.as_str(), c.description.as_str(), c.var.as_str(), c.bit),
            ("AMC open", "AMC stuck open", "CDiag4", 0)
        );
        // RT_BUEIB: 21 both current and stored.
        assert_eq!(codes_from_runtime(&d, RT_BUEIB, CodeKind::Stored)[0].code, "21");
        // A frame with no bits set has no codes.
        let mut clean = RT_BUEIB.to_vec();
        for o in [67, 68, 69, 70, 75, 76, 77, 78, 101, 103] {
            clean[o] = 0;
        }
        assert!(codes_from_runtime(&d, &clean, CodeKind::Current).is_empty());
        assert!(codes_from_runtime(&d, &clean, CodeKind::Stored).is_empty());
    }

    #[test]
    fn stored_codes_from_owner_style_image() {
        let (e, d) = bueib();
        let mut img = xbtune_sim::BUEIB_EEPROM.to_vec();
        img.resize(e.xsize, 0);
        // No page 0: HDiag bytes there are skipped, not misread.
        let none = stored_codes_from_image(&d, &ImageView { bytes: &img, page_zero: false });
        let hd0 = &d.eeprom["HDiag0"];
        if hd0.offset >= 0 {
            img[hd0.offset as usize] = 0b0010_0000; // TPS low -> 11
            let got = stored_codes_from_image(&d, &ImageView { bytes: &img, page_zero: false });
            assert!(got.iter().any(|c| c.code == "11" && c.name == "TPS low"), "{got:?}");
        } else {
            assert!(none.is_empty());
        }
    }

    #[test]
    fn names_and_notices() {
        assert_eq!(function_by_name("front-coil"), Some(Function::FrontCoil));
        assert_eq!(function_by_name("FuelPump"), Some(Function::FuelPump));
        assert_eq!(function_by_name("tps reset"), Some(Function::TpsReset));
        assert_eq!(function_by_name("nope"), None);
        for f in TESTS {
            assert!(!safety_notice(*f).is_empty());
        }
        let c = Catalog::builtin().unwrap();
        assert!(tps_reset_supported(c.lookup("BUEIB").unwrap()));
        assert!(!tps_reset_supported(c.lookup("BUEOD").unwrap()));
    }

    #[test]
    fn hardware_is_locked_without_the_file() {
        assert!(permit(true).is_ok());
        let unlocked_file = std::env::temp_dir().join(format!("xbtune-hw-{}.toml", std::process::id()));
        std::fs::write(&unlocked_file, "ecm_commands = true\n").unwrap();
        assert!(unlocked(&unlocked_file));
        std::fs::write(&unlocked_file, "ecm_commands = false\n").unwrap();
        assert!(!unlocked(&unlocked_file));
        std::fs::write(&unlocked_file, "garbage =").unwrap();
        assert!(!unlocked(&unlocked_file));
        let _ = std::fs::remove_file(&unlocked_file);
        assert!(!unlocked(std::path::Path::new("/nonexistent/hardware.toml")));
        assert!(locked_message().contains("ecm_commands = true"));
    }

    fn fast() -> FunctionTiming {
        FunctionTiming { poll: Duration::from_millis(20), max_busy: Duration::from_secs(2) }
    }

    /// A stopped BUEIB sim with 150 ms tests.
    fn sim() -> xbtune_sim::SharedSim {
        let mut s = Simulator::bueib_running();
        s.set_engine_running(false);
        s.set_test_duration(Duration::from_millis(150));
        shared(s)
    }

    fn client(sim: &xbtune_sim::SharedSim) -> EcmClient {
        EcmClient::new(xbtune_sim::in_process(sim.clone()), EcmAddress::Stock)
            .with_policy(xbtune_link::client::fast_policy())
    }

    async fn run(sim: &xbtune_sim::SharedSim, f: Function) -> Result<Outcome, FunctionError> {
        let (e, d) = bueib();
        let mut c = client(sim);
        run_function(&mut c, &e, &d, f, &test_permit(), fast(), |_| {}).await
    }

    #[tokio::test]
    async fn test_runs_and_waits_for_idle() {
        let s = sim();
        let mut stages = Vec::new();
        let (e, d) = bueib();
        let mut c = client(&s);
        let o = run_function(&mut c, &e, &d, Function::Fan, &test_permit(), fast(), |st| stages.push(st))
            .await
            .unwrap();
        assert!(o.acknowledged);
        assert!(o.busy_for >= Duration::from_millis(100), "{o:?}");
        assert_eq!(s.lock().unwrap().journal.functions, vec![Function::Fan.code()]);
        assert_eq!(stages.first(), Some(&Stage::Checking));
        assert!(stages.iter().any(|s| matches!(s, Stage::Running { .. })));
    }

    #[tokio::test]
    async fn refuses_with_engine_running() {
        let s = sim();
        s.lock().unwrap().set_engine_running(true);
        let r = run(&s, Function::FrontInjector).await;
        assert!(matches!(r, Err(FunctionError::EngineRunning(rpm)) if rpm > 500.0), "{r:?}");
        assert!(s.lock().unwrap().journal.functions.is_empty());
    }

    #[tokio::test]
    async fn refuses_while_busy() {
        let s = sim();
        s.lock().unwrap().set_state(2);
        assert_eq!(run(&s, Function::Fan).await, Err(FunctionError::Busy(2)));
        assert!(s.lock().unwrap().journal.functions.is_empty());
    }

    #[tokio::test]
    async fn nak_is_reported_not_retried() {
        let s = sim();
        // runtime, state, then the command is NAKed.
        s.lock().unwrap().script([None, None, Some(Fault::Nak(0x15))]);
        assert_eq!(run(&s, Function::ShiftLight).await, Err(FunctionError::Refused("Shift Light", 0x15)));
        assert!(s.lock().unwrap().journal.functions.is_empty());
    }

    #[tokio::test]
    async fn lost_reply_but_running_counts_once() {
        let s = sim();
        // The ECM runs the test but its ACK is lost; never re-sent. The test
        // outlasts the reply timeout, as real ones (seconds) do.
        s.lock().unwrap().set_test_duration(Duration::from_millis(600));
        s.lock().unwrap().script([None, None, Some(Fault::Drop)]);
        let o = run(&s, Function::FuelPump).await.unwrap();
        assert!(!o.acknowledged);
        assert_eq!(s.lock().unwrap().journal.functions, vec![Function::FuelPump.code()]);
    }

    #[tokio::test]
    async fn lost_request_is_not_resent() {
        let s = sim();
        s.lock().unwrap().script([None, None, Some(Fault::LoseRequest)]);
        assert_eq!(run(&s, Function::RearCoil).await, Err(FunctionError::NotConfirmed("Rear Coil")));
        assert!(s.lock().unwrap().journal.functions.is_empty());
    }

    #[tokio::test]
    async fn corrupt_reply_is_not_resent() {
        let s = sim();
        s.lock().unwrap().script([None, None, Some(Fault::Corrupt)]);
        let o = run(&s, Function::Tachometer).await.unwrap();
        assert!(!o.acknowledged);
        assert_eq!(s.lock().unwrap().journal.functions, vec![Function::Tachometer.code()]);
    }

    #[tokio::test]
    async fn stuck_busy_times_out() {
        let s = sim();
        s.lock().unwrap().set_test_duration(Duration::from_secs(30));
        let r = run(&s, Function::Fan).await;
        assert!(matches!(r, Err(FunctionError::StillBusy("Fan", _))), "{r:?}");
    }

    #[tokio::test]
    async fn clear_codes_clears_stored_only() {
        let s = sim();
        let (e, d) = bueib();
        let mut c = client(&s);
        let before = c.runtime_data().await.unwrap();
        assert_eq!(codes_from_runtime(&d, before.as_bytes(), CodeKind::Stored).len(), 1);
        run_function(&mut c, &e, &d, Function::ClearCodes, &test_permit(), fast(), |_| {}).await.unwrap();
        let after = c.runtime_data().await.unwrap();
        assert!(codes_from_runtime(&d, after.as_bytes(), CodeKind::Stored).is_empty());
        assert_eq!(codes_from_runtime(&d, after.as_bytes(), CodeKind::Current).len(), 1);
    }

    #[tokio::test]
    async fn tps_reset_only_on_ddfi2() {
        let c = Catalog::builtin().unwrap();
        let e = c.lookup("BUEOD").unwrap().clone();
        let d = c.definition(&e).unwrap().clone();
        let s = sim();
        let mut cl = client(&s);
        let r = run_function(&mut cl, &e, &d, Function::TpsReset, &test_permit(), fast(), |_| {}).await;
        assert!(matches!(r, Err(FunctionError::NotSupported(..))), "{r:?}");
        assert!(s.lock().unwrap().journal.functions.is_empty());
        assert!(run(&s, Function::TpsReset).await.unwrap().acknowledged);
    }

    #[tokio::test]
    async fn dropouts_never_duplicate_a_command() {
        // Random drops/corruption on every reply, a different pattern per
        // seed: whatever happens, the ECM sees the function at most once.
        let (e, d) = bueib();
        let mut ran = 0;
        for seed in 1..=40u64 {
            let mut cfg = xbtune_sim::SimConfig::bueib();
            cfg.seed = seed * 0x9E37_79B9;
            cfg.test_duration = Duration::from_millis(120);
            cfg.rates = xbtune_sim::FaultRates { drop: 0.25, corrupt: 0.15, ..Default::default() };
            let mut sm = Simulator::new(cfg, xbtune_sim::BUEIB_EEPROM).with_engine(d.clone());
            sm.set_engine_running(false);
            let s = shared(sm);
            let policy = xbtune_proto::RetryPolicy {
                timeout: Duration::from_millis(50),
                max_attempts: 3,
                settle: Duration::from_millis(5),
            };
            let mut c =
                EcmClient::new(xbtune_sim::in_process(s.clone()), EcmAddress::Stock).with_policy(policy);
            let r =
                run_function(&mut c, &e, &d, Function::FrontInjector, &test_permit(), fast(), |_| {}).await;
            let n = s.lock().unwrap().journal.functions.len();
            assert!(n <= 1, "seed {seed}: sent {n} times");
            // Success means it really ran.
            if r.is_ok() {
                assert_eq!(n, 1, "seed {seed}");
                ran += 1;
            }
        }
        assert!(ran > 5, "only {ran} runs got through");
    }
}
