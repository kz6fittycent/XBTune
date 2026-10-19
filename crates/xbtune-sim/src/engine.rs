// SPDX-License-Identifier: GPL-3.0-or-later
//! A made-up engine for the simulator's runtime data, so live views and
//! logging have something that moves.
//!
//! Nothing here models a real Buell: it is a repeating 30 s ride (idle,
//! pull, cruise, decel, idle) with a warm-up, written into the runtime frame
//! through the definition's own offsets and scaling. Values are only
//! plausible, never reference data.

use std::sync::Arc;

use xbtune_defs::{Definition, RtKind};

/// One instant of the made-up ride, in engineering units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineState {
    pub rpm: f64,
    /// Throttle, percent.
    pub tps: f64,
    /// Engine (head) temperature, °C.
    pub engine_temp: f64,
    pub air_temp: f64,
    pub battery: f64,
    /// Front/rear spark advance, degrees.
    pub spark: (f64, f64),
    /// Front/rear injector pulse width, ms.
    pub pw: (f64, f64),
    pub o2_volts: f64,
    /// EGO correction, percent.
    pub ego: f64,
}

/// Length of one ride cycle, seconds.
pub const CYCLE: f64 = 30.0;

/// The made-up ride at `t` seconds after start.
pub fn state_at(t: f64) -> EngineState {
    let c = t.rem_euclid(CYCLE);
    let lerp = |a: f64, b: f64, x: f64| a + (b - a) * x.clamp(0.0, 1.0);
    let wobble = (t * 7.3).sin();
    let (rpm, tps) = match c {
        c if c < 5.0 => (1100.0 + 20.0 * wobble, 0.0),
        c if c < 12.0 => {
            let x = (c - 5.0) / 7.0;
            (lerp(1100.0, 6500.0, x * x), lerp(20.0, 85.0, x))
        }
        c if c < 20.0 => (3500.0 + 40.0 * wobble, 25.0 + 2.0 * wobble),
        c if c < 25.0 => (lerp(3500.0, 1200.0, (c - 20.0) / 5.0), 0.0),
        _ => (1100.0 + 20.0 * wobble, 0.0),
    };
    let engine_temp = 190.0 - 165.0 * (-t / 240.0).exp();
    let spark = (10.0 + (rpm / 180.0).min(28.0) - tps * 0.08).max(5.0);
    let pw = 2.2 + tps * 0.07 + rpm / 2500.0;
    let closed_loop = tps < 30.0 && rpm > 1500.0;
    let o2_volts = if closed_loop { 0.45 + 0.35 * (t * 6.0).sin() } else { 0.12 };
    EngineState {
        rpm,
        tps,
        engine_temp,
        air_temp: 28.0 + 3.0 * (t / 200.0).sin(),
        battery: 14.1 + 0.08 * (t * 3.1).sin(),
        spark: (spark, spark - 1.5),
        pw: (pw, pw * 1.03),
        o2_volts,
        ego: if closed_loop { 100.0 + 4.0 * (t * 0.7).sin() } else { 100.0 },
    }
}

/// What a wideband sensor would read at `t` in the made-up ride: rich on
/// the pull (open loop), near 1 in closed loop, a little lean at idle and
/// very lean on the overrun. With the engine stopped: `None` (free air).
/// Made up like the rest; it only gives the views something to show.
pub fn lambda_at(t: f64, running: bool) -> Option<f64> {
    if !running {
        return None;
    }
    let s = state_at(t);
    let c = t.rem_euclid(CYCLE);
    Some(match c {
        c if (20.0..25.0).contains(&c) => 1.6,
        _ if s.tps >= 30.0 => 0.86 + 0.01 * (t * 5.0).sin(),
        _ if s.tps < 30.0 && s.rpm > 1500.0 => 1.0 + 0.025 * (t * 6.0).sin(),
        _ => 1.03 + 0.01 * (t * 3.0).sin(),
    })
}

/// Writes [`state_at`] into runtime frames of one definition.
#[derive(Debug, Clone)]
pub struct Engine {
    def: Arc<Definition>,
}

impl Engine {
    pub fn new(def: Arc<Definition>) -> Self {
        Self { def }
    }

    /// `template` (a whole runtime reply frame) with the ride at `t`
    /// written in, and the checksum fixed.
    pub fn frame(&self, template: &[u8], t: f64) -> Vec<u8> {
        self.frame_with(template, t, true)
    }

    /// Like [`Engine::frame`]; with `running` false the engine is stopped
    /// (ignition on): no RPM, throttle closed, no fuel or spark.
    pub fn frame_with(&self, template: &[u8], t: f64, running: bool) -> Vec<u8> {
        let mut s = state_at(t);
        if !running {
            s.rpm = 0.0;
            s.tps = 0.0;
            s.pw = (0.0, 0.0);
            s.spark = (0.0, 0.0);
            s.o2_volts = 0.0;
            s.ego = 100.0;
            s.battery = 12.6;
        }
        let mut f = template.to_vec();
        // Database variable names, which are the same across the XB
        // definitions. Variables an ECM doesn't have are skipped.
        let values: &[(&str, f64)] = &[
            ("MS_10", (t.fract() * 100.0).floor() * 0.01),
            ("Seconds", t.floor()),
            ("RPM", s.rpm),
            ("Adv1", s.spark.0),
            ("Adv2", s.spark.1),
            ("TabFuel1", 30.0 + s.tps * 1.5),
            ("TabFuel2", 31.0 + s.tps * 1.5),
            ("Fuel1", s.pw.0),
            ("Fuel2", s.pw.1),
            ("TPD", s.tps * 0.84),
            ("TPP", s.tps),
            ("Bat", s.battery),
            ("TE", s.engine_temp),
            ("TA", s.air_temp),
            ("O2", s.o2_volts),
            ("CrTE", 100.0 + ((120.0 - s.engine_temp) / 2.0).max(0.0)),
            ("CrTA", 100.0),
            ("CrAcc", 0.0),
            ("CrDec", 100.0),
            ("CrWOT", 100.0),
            ("CrIdle", 100.0),
            ("CrOL", 100.0),
            ("LFuel_LD", 100.0),
            ("FBFuel", s.ego),
            ("ATPS", 100.0 + s.tps * 7.0),
            ("Fan_Duty", if s.engine_temp > 220.0 { 100.0 } else { 0.0 }),
            ("VSS_Count", s.rpm / 60.0),
        ];
        for (varname, v) in values {
            let Some(var) = self.def.runtime.values().find(|r| r.varname == *varname) else { continue };
            if var.kind != RtKind::Scalar || var.scale == 0.0 || var.offset + var.size > f.len() {
                continue;
            }
            let max = (1u64 << (8 * var.size.min(4))) - 1;
            let raw = ((v - var.translate) / var.scale).round().clamp(0.0, max as f64) as u64;
            for i in 0..var.size {
                f[var.offset + i] = (raw >> (8 * i)) as u8;
            }
        }
        let n = f.len();
        f[n - 1] = xbtune_proto::pdu::checksum(&f);
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_defs::Catalog;
    use xbtune_proto::Pdu;

    #[test]
    fn ride_moves_and_frames_stay_valid() {
        let c = Catalog::builtin().unwrap();
        for id in ["BUEIB", "BUEOD"] {
            let ecm = c.lookup(id).unwrap();
            let def = c.definition(ecm).unwrap().clone();
            let rpm = def.runtime.values().find(|r| r.varname == "RPM").unwrap().clone();
            let e = Engine::new(def);
            let mut p = vec![0u8; ecm.runtime_len - 8];
            p[0] = xbtune_proto::pdu::ACK;
            let template = Pdu::new(0x00, 0x10, &p).into_bytes();
            assert_eq!(template.len(), ecm.runtime_len);
            let idle = e.frame(&template, 1.0);
            let pull = e.frame(&template, 11.9);
            assert!(Pdu::parse(&idle).is_ok() && Pdu::parse(&pull).is_ok(), "{id}");
            let (a, b) = (rpm.value(&idle).unwrap(), rpm.value(&pull).unwrap());
            assert!((1000.0..1200.0).contains(&a) && b > 6000.0, "{id}: {a} {b}");
        }
    }
}
