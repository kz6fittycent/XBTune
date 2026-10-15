// SPDX-License-Identifier: GPL-3.0-or-later
//! Burn path against the simulator (KICKOFF §6.7): happy path, page 0,
//! NAKs, lost ACKs, ignored and corrupted writes, power cuts, weak links,
//! wrong ECM, and restoring the backup.

use std::time::Duration;

use xbtune_defs::Catalog;
use xbtune_proto::{EcmAddress, RetryPolicy};
use xbtune_sim::{Fault, FaultRates, SharedSim, Simulator, WriteFault, shared};

use super::*;
use crate::read::EepromRead;
use crate::tune::Tune;

struct Bench {
    sim: SharedSim,
    client: EcmClient,
    layout: Layout,
    def: std::sync::Arc<Definition>,
    ecm: EcmDef,
}

fn policy() -> RetryPolicy {
    RetryPolicy { timeout: Duration::from_millis(60), max_attempts: 3, settle: Duration::from_millis(5) }
}

fn bench() -> Bench {
    let c = Catalog::builtin().unwrap();
    let ecm = c.lookup("BUEIB").unwrap().clone();
    let def = c.definition(&ecm).unwrap().clone();
    let mut sim = Simulator::bueib();
    // Give the sim a real page 0 (the EcmDroid test image has none).
    let p0 = ecm.layout().unwrap().page(0).unwrap().start;
    sim.poke(p0, &vec![0x64; ecm.xsize - p0]);
    let sim = shared(sim);
    let client = EcmClient::new(xbtune_sim::in_process(sim.clone()), EcmAddress::Stock).with_policy(policy());
    Bench { sim, client, layout: ecm.layout().unwrap(), def, ecm }
}

async fn read_all(b: &mut Bench) -> Vec<u8> {
    let mut r = EepromRead::new(&b.layout, true);
    r.run(&mut b.client, |_| {}).await.unwrap();
    r.bytes().unwrap()
}

fn edited(b: &Bench, ecm: &[u8]) -> (Vec<u8>, Vec<String>) {
    let c = Catalog::builtin().unwrap();
    let mut img = crate::image::Image::from_raw(&b.ecm, ecm).unwrap();
    img.page_zero = true;
    let mut t = Tune::new(img, &c).unwrap();
    t.set(
        "edit",
        &[("KRPM_Hard_Hi".into(), 0, 7000.0), ("Tab_Fuel_Front".into(), 5, 99.0), ("KBaro".into(), 0, 98.0)],
    )
    .unwrap();
    (t.image.bytes.clone(), vec!["KRPM_Hard_Hi".into(), "Tab_Fuel_Front".into(), "KBaro".into()])
}

#[tokio::test]
async fn edits_are_planned_written_and_verified() {
    let mut b = bench();
    let ecm = read_all(&mut b).await;
    let (tune, vars) = edited(&b, &ecm);
    let ch = changes(&b.def, &ecm, &tune, Some(&ecm), true);
    let names: Vec<_> = ch.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(ch.iter().all(|c| c.edited && c.blocked.is_none()), "{ch:?}");
    let rev = ch.iter().find(|c| c.name == "KRPM_Hard_Hi").unwrap();
    assert_eq!((rev.ecm.as_str(), rev.tune.as_str()), ("6800 RPM", "7000 RPM"));
    let target = build_target(&b.def, &ecm, &tune, true, &vars).unwrap();
    assert_eq!(target, tune);
    let plan = plan(&b.layout, &b.def, &ecm, &target).unwrap();
    // Two 16-byte chunks in page 1/2/... plus the one KBaro SET in page 0.
    assert_eq!(plan.writes.len(), 3, "{plan:?}");
    let kb = plan.writes.iter().find(|w| w.page == 0).unwrap();
    let kbv = &b.def.eeprom["KBaro"];
    assert_eq!(kb.wire_offset as i64, 0xFF + kbv.offset as i64 + 1);
    assert_eq!(kb.data.len(), kbv.size);
    assert!(plan.writes.iter().all(|w| w.page == 0 || (w.wire_offset % 16 == 0 && w.data.len() <= 16)));
    let report = execute(&mut b.client, &b.layout, &plan, &target, |_| {}).await.unwrap();
    assert_eq!(report.writes, 3);
    assert_eq!(b.sim.lock().unwrap().image(), &target[..]);
    assert_eq!(b.sim.lock().unwrap().journal.writes.len(), 3);
}

#[tokio::test]
async fn stale_ecm_owned_bytes_are_not_written_back() {
    let mut b = bench();
    let file = read_all(&mut b).await;
    // The ECM updates AFV after the file was saved.
    let lfuel = b.def.eeprom["LFuel"].clone();
    let at = lfuel.offset as usize;
    b.sim.lock().unwrap().poke(at, &[0xEE, 0x03]);
    let now = read_all(&mut b).await;
    // The user edits only the rev limit in the old file.
    let c = Catalog::builtin().unwrap();
    let mut img = crate::image::Image::from_raw(&b.ecm, &file).unwrap();
    img.page_zero = true;
    let mut t = Tune::new(img, &c).unwrap();
    t.set("rev", &[("KRPM_Hard_Hi".into(), 0, 7000.0)]).unwrap();
    let ch = changes(&b.def, &now, &t.image.bytes, Some(&file), true);
    let afv = ch.iter().find(|c| c.name == "LFuel").unwrap();
    assert!(!afv.edited, "AFV differs but wasn't edited");
    assert!(ch.iter().find(|c| c.name == "KRPM_Hard_Hi").unwrap().edited);
    let chosen: Vec<String> = ch.iter().filter(|c| c.edited).map(|c| c.name.clone()).collect();
    let target = build_target(&b.def, &now, &t.image.bytes, true, &chosen).unwrap();
    assert_eq!(target[at..at + 2], [0xEE, 0x03], "learned AFV kept");
    let p = plan(&b.layout, &b.def, &now, &target).unwrap();
    execute(&mut b.client, &b.layout, &p, &target, |_| {}).await.unwrap();
    assert_eq!(b.sim.lock().unwrap().image(), &target[..]);
}

#[tokio::test]
async fn page_zero_outside_writable_vars_is_refused() {
    let b = bench();
    let ecm = b.sim.lock().unwrap().image().to_vec();
    let mut target = ecm.clone();
    let p0 = b.layout.page(0).unwrap();
    // Find a page 0 byte not covered by LFuel1/KBaro.
    let view = ImageView { bytes: &ecm, page_zero: true };
    let kb = var_range(&b.def.eeprom["KBaro"], &view).unwrap();
    let stray = (p0.start..p0.start + p0.len).find(|i| !kb.contains(i)).unwrap();
    target[stray] ^= 0xFF;
    let e = plan(&b.layout, &b.def, &ecm, &target).unwrap_err();
    assert!(e.contains("page 0"), "{e}");
    // A named page 0 variable other than LFuel1/KBaro is reported as
    // blocked in the change list.
    let mut target = ecm.clone();
    let v = b
        .def
        .eeprom
        .values()
        .find(|v| v.offset < 0 && !PAGE_ZERO_WRITABLE.contains(&v.name.as_str()))
        .unwrap();
    let r = var_range(v, &view).unwrap();
    target[r.start] ^= 0xFF;
    assert!(plan(&b.layout, &b.def, &ecm, &target).unwrap_err().contains(&v.name));
    let ch = changes(&b.def, &ecm, &target, None, true);
    assert!(ch.iter().any(|c| c.blocked.is_some()), "{ch:?}");
    assert!(
        build_target(&b.def, &ecm, &target, true, &ch.iter().map(|c| c.name.clone()).collect::<Vec<_>>())
            .is_err()
    );
}

#[tokio::test]
async fn nak_stops_and_reports_pages_then_retry_and_restore_work() {
    let mut b = bench();
    let ecm = read_all(&mut b).await;
    let (tune, _) = edited(&b, &ecm);
    let p = plan(&b.layout, &b.def, &ecm, &tune).unwrap();
    // The second write is refused (status 0x15) on every attempt.
    b.sim.lock().unwrap().script([None, Some(Fault::Nak(0x15))]);
    let e = execute(&mut b.client, &b.layout, &p, &tune, |_| {}).await.unwrap_err();
    let BurnError::Write { mismatched: Some(m), .. } = &e else { panic!("{e:?}") };
    assert!(!m.is_empty(), "pages left wrong must be listed");
    // What the report says matches the sim.
    let img = b.sim.lock().unwrap().image().to_vec();
    for pm in m {
        for (a, z) in &pm.ranges {
            assert_ne!(img[*a..*z], tune[*a..*z]);
        }
    }
    // Retry from what the ECM holds now.
    let now = read_all(&mut b).await;
    let p2 = plan(&b.layout, &b.def, &now, &tune).unwrap();
    execute(&mut b.client, &b.layout, &p2, &tune, |_| {}).await.unwrap();
    assert_eq!(b.sim.lock().unwrap().image(), &tune[..]);
    // Restore the backup.
    let p3 = plan(&b.layout, &b.def, &tune, &ecm).unwrap();
    execute(&mut b.client, &b.layout, &p3, &ecm, |_| {}).await.unwrap();
    assert_eq!(b.sim.lock().unwrap().image(), &ecm[..]);
}

#[tokio::test]
async fn lost_ack_is_retried_and_verified() {
    let mut b = bench();
    let ecm = read_all(&mut b).await;
    let (tune, _) = edited(&b, &ecm);
    let p = plan(&b.layout, &b.def, &ecm, &tune).unwrap();
    b.sim.lock().unwrap().script([Some(Fault::Drop), None, Some(Fault::Corrupt)]);
    execute(&mut b.client, &b.layout, &p, &tune, |_| {}).await.unwrap();
    assert_eq!(b.sim.lock().unwrap().image(), &tune[..]);
}

#[tokio::test]
async fn ignored_and_corrupted_writes_are_caught_by_read_back() {
    for fault in [WriteFault::Ignore, WriteFault::FlipBit] {
        let mut b = bench();
        let ecm = read_all(&mut b).await;
        let (tune, _) = edited(&b, &ecm);
        let p = plan(&b.layout, &b.def, &ecm, &tune).unwrap();
        b.sim.lock().unwrap().set_write_fault(fault);
        let e = execute(&mut b.client, &b.layout, &p, &tune, |_| {}).await.unwrap_err();
        let BurnError::Verify(m) = &e else { panic!("{fault:?}: {e:?}") };
        assert_eq!(m.iter().map(|m| m.page).collect::<BTreeSet<_>>(), p.pages, "{fault:?}");
    }
}

#[tokio::test]
async fn corrupted_read_back_is_not_a_false_failure() {
    let mut b = bench();
    let ecm = read_all(&mut b).await;
    let (tune, _) = edited(&b, &ecm);
    let p = plan(&b.layout, &b.def, &ecm, &tune).unwrap();
    // A silently wrong read during verification is re-read, not reported.
    let n = p.writes.len();
    let mut script: Vec<Option<Fault>> = vec![None; n];
    script.push(Some(Fault::SilentFlip));
    b.sim.lock().unwrap().script(script);
    execute(&mut b.client, &b.layout, &p, &tune, |_| {}).await.unwrap();
}

#[tokio::test]
async fn power_cut_mid_burn_is_reported() {
    let mut b = bench();
    let ecm = read_all(&mut b).await;
    let (tune, _) = edited(&b, &ecm);
    let p = plan(&b.layout, &b.def, &ecm, &tune).unwrap();
    b.sim.lock().unwrap().cut_power_after_writes(Some(1));
    let e = execute(&mut b.client, &b.layout, &p, &tune, |_| {}).await.unwrap_err();
    // Nothing answers, so the state can't be checked: reported as unknown.
    assert!(matches!(e, BurnError::Write { mismatched: None, .. }), "{e:?}");
    assert_eq!(e.mismatched(), None);
}

#[tokio::test]
async fn random_dropouts_never_report_success_falsely() {
    for seed in 1..=15u64 {
        let mut b = bench();
        let ecm = read_all(&mut b).await;
        let (tune, _) = edited(&b, &ecm);
        let p = plan(&b.layout, &b.def, &ecm, &tune).unwrap();
        b.sim.lock().unwrap().set_rates(FaultRates { drop: 0.2, corrupt: 0.1, ..Default::default() });
        let _ = seed;
        let r = execute(&mut b.client, &b.layout, &p, &tune, |_| {}).await;
        let img = b.sim.lock().unwrap().image().to_vec();
        match r {
            Ok(_) => assert_eq!(img, tune, "seed {seed}: success must mean verified"),
            Err(e) => {
                if let Some(m) = e.mismatched() {
                    for pm in m {
                        for (a, z) in &pm.ranges {
                            assert_ne!(
                                img[*a..*z],
                                tune[*a..*z],
                                "seed {seed}: reported range is actually right"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[tokio::test]
async fn link_check_and_identity() {
    let mut b = bench();
    let limits = LinkLimits { pings: 10, max_failures: 1, max_rtt: Duration::from_millis(300) };
    let good = check_link(&mut b.client, &limits).await;
    assert_eq!(good.failures, 0);
    assert!(good.verdict(&limits).is_ok());
    b.sim.lock().unwrap().set_rates(FaultRates { drop: 0.5, ..Default::default() });
    let bad = check_link(&mut b.client, &limits).await;
    assert!(bad.failures >= 2, "{bad:?}");
    assert!(bad.verdict(&limits).unwrap_err().contains("too weak"));
    let c = Catalog::builtin().unwrap();
    assert!(check_identity("BUEIB310 12-11-03", "BUEIB", c.lookup("BUEIB").unwrap()).is_ok());
    assert!(check_identity("BUEGB410", "BUEIB", c.lookup("BUEGB").unwrap()).is_err());
}

#[tokio::test]
async fn ecm_change_since_backup_is_detected() {
    let mut b = bench();
    let ecm = read_all(&mut b).await;
    let (tune, _) = edited(&b, &ecm);
    let p = plan(&b.layout, &b.def, &ecm, &tune).unwrap();
    assert!(still_matches(&mut b.client, &b.layout, &p, &ecm).await.unwrap().is_empty());
    let w = &p.writes[0];
    let v = b.sim.lock().unwrap().image()[w.image_offset];
    b.sim.lock().unwrap().poke(w.image_offset, &[v ^ 0x80]);
    assert_eq!(still_matches(&mut b.client, &b.layout, &p, &ecm).await.unwrap().len(), 1);
}

#[test]
fn afv_reset_targets_lfuel() {
    let c = Catalog::builtin().unwrap();
    let e = c.lookup("BUEIB").unwrap();
    let d = c.definition(e).unwrap();
    let mut ecm = xbtune_sim::BUEIB_EEPROM.to_vec();
    ecm.resize(e.xsize, 0);
    let (out, names) = afv_reset(d, &ecm, true).unwrap();
    assert_eq!(names, vec!["LFuel".to_string()]);
    let v = &d.eeprom["LFuel"];
    let raw = xbtune_defs::decode::le(&out, v.offset as usize, v.width);
    assert_eq!(v.scaled(raw), AFV_NEUTRAL);
    let changed: Vec<usize> = (0..ecm.len()).filter(|&i| ecm[i] != out[i]).collect();
    assert!(changed.iter().all(|&i| i >= v.offset as usize && i < v.offset as usize + v.size));
}
