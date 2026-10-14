// SPDX-License-Identifier: GPL-3.0-or-later
//! The real client against the simulator, clean and with injected faults.

use std::time::Duration;

use xbtune_link::EcmClient;
use xbtune_link::client::fast_policy;
use xbtune_proto::{EcmAddress, Function, Request, TransactionError};
use xbtune_sim::{Fault, FaultRates, SharedSim, Simulator, in_process, shared};

fn client(sim: &SharedSim) -> EcmClient {
    EcmClient::new(in_process(sim.clone()), EcmAddress::Stock).with_policy(fast_policy())
}

#[tokio::test]
async fn version_state_and_runtime() {
    let sim = shared(Simulator::bueib());
    let mut c = client(&sim);
    assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    assert_eq!(c.current_state().await.unwrap(), 0);
    let rt = c.runtime_data().await.unwrap();
    assert_eq!(rt.as_bytes().len(), 107);
    assert_eq!(c.stats().ok, 3);
    assert_eq!(c.stats().retries, 0);
}

#[tokio::test]
async fn full_eeprom_read_matches_image() {
    let sim = shared(Simulator::bueib());
    let layout = sim.lock().unwrap().config().layout.clone();
    let mut c = client(&sim);
    let mut image = vec![0u8; layout.xsize];
    for page in &layout.pages {
        for chunk in layout.read_plan(page.nr).unwrap() {
            let Request::Get { page, offset, len } = chunk.get_request() else { unreachable!() };
            let data = c.read_bytes(page, offset, len).await.unwrap();
            image[chunk.image_range()].copy_from_slice(&data);
        }
    }
    assert_eq!(&image[..1206], xbtune_sim::BUEIB_EEPROM);
    assert_eq!(image, sim.lock().unwrap().image());
}

#[tokio::test]
async fn retries_through_scripted_faults() {
    let sim = shared(Simulator::bueib());
    sim.lock().unwrap().script([Fault::Drop, Fault::Corrupt]);
    let mut c = client(&sim);
    assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    let s = c.stats();
    assert_eq!((s.timeouts, s.corrupt, s.retries, s.ok), (1, 1, 2, 1));
}

#[tokio::test]
async fn garbage_fragments_and_echo_are_tolerated() {
    let mut sim = Simulator::bueib();
    sim.script([Fault::Garbage(7), Fault::Fragment(1), Fault::Fragment(3)]);
    let sim = shared(sim);
    let mut c = client(&sim);
    for _ in 0..3 {
        assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    }
    assert_eq!(c.stats().retries, 0);

    let mut cfg = xbtune_sim::SimConfig::bueib();
    cfg.echo = true;
    let sim = shared(Simulator::new(cfg, xbtune_sim::BUEIB_EEPROM));
    let mut c = client(&sim);
    assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    assert_eq!(c.stats().retries, 0);
}

#[tokio::test]
async fn late_reply_is_not_taken_for_the_next_one() {
    // The first version reply arrives after the timeout. The client must
    // retry and must not mistake that stale reply for a later GET's reply.
    let sim = shared(Simulator::bueib());
    sim.lock().unwrap().script([Fault::Delay(Duration::from_millis(250))]);
    let mut c = client(&sim);
    assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    let data = c.read_bytes(1, 0, 16).await.unwrap();
    assert_eq!(data, &xbtune_sim::BUEIB_EEPROM[..16]);
}

#[tokio::test]
async fn truncated_reply_times_out_then_recovers() {
    let sim = shared(Simulator::bueib());
    sim.lock().unwrap().script([Fault::Truncate(9)]);
    let mut c = client(&sim);
    assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    assert_eq!(c.stats().retries, 1);
}

#[tokio::test]
async fn gives_up_after_max_attempts() {
    let sim = shared(Simulator::bueib());
    sim.lock().unwrap().script([Fault::Drop, Fault::Drop, Fault::Drop]);
    let mut c = client(&sim);
    let err = c.version().await.unwrap_err();
    assert!(matches!(err, TransactionError::Exhausted { attempts: 3, .. }), "{err}");
}

#[tokio::test]
async fn nak_is_not_retried() {
    let sim = shared(Simulator::bueib());
    sim.lock().unwrap().script([Fault::Nak(0x15)]);
    let mut c = client(&sim);
    assert_eq!(c.version().await.unwrap_err(), TransactionError::Nak(0x15));
    assert_eq!(c.stats().attempts, 1);
}

#[tokio::test]
async fn out_of_range_read_is_refused() {
    let sim = shared(Simulator::bueib());
    let mut c = client(&sim);
    // Page 6 is 24 bytes long.
    assert!(matches!(c.read_bytes(6, 20, 8).await, Err(TransactionError::Nak(_))));
}

#[tokio::test]
async fn actuator_command_is_sent_exactly_once_even_on_timeout() {
    let sim = shared(Simulator::bueib());
    sim.lock().unwrap().script([Fault::Drop]);
    let mut c = client(&sim);
    let err = c.transact(&Request::Command(Function::FuelPump)).await.unwrap_err();
    assert!(matches!(err, TransactionError::Timeout(_)), "{err}");
    assert_eq!(sim.lock().unwrap().journal.functions, vec![Function::FuelPump.code()]);
}

#[tokio::test]
async fn sustained_random_faults() {
    let mut sim = Simulator::bueib();
    sim.set_rates(FaultRates {
        drop: 0.05,
        corrupt: 0.05,
        garbage: 0.1,
        fragment: 0.3,
        ..Default::default()
    });
    let sim = shared(sim);
    let mut c = client(&sim);
    let mut ok = 0;
    for _ in 0..200 {
        if c.version().await.as_deref() == Ok("BUEIB310 12-11-03") {
            ok += 1;
        }
    }
    // With 3 attempts and ~10% failure per attempt, essentially all succeed.
    assert!(ok >= 198, "only {ok}/200 succeeded");
    assert!(c.stats().retries > 0);
    assert!(c.stats().recent_error_rate() < 0.5);
}

#[tokio::test]
async fn race_ecm_address() {
    let catalog = xbtune_defs::Catalog::builtin().unwrap();
    let cfg = xbtune_sim::SimConfig::for_ecm(catalog.lookup("B2RIB").unwrap(), "310 01-01-05");
    assert_eq!(cfg.address, EcmAddress::Race);
    let sim = shared(Simulator::new(cfg, &[]));
    // A stock-address client gets no answer.
    let mut stock = EcmClient::new(in_process(sim.clone()), EcmAddress::Stock).with_policy(fast_policy());
    assert!(stock.version().await.is_err());
    let mut race = EcmClient::new(in_process(sim), EcmAddress::Race).with_policy(fast_policy());
    assert_eq!(race.version().await.unwrap(), "B2RIB310 01-01-05");
}
