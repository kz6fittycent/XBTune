// SPDX-License-Identifier: GPL-3.0-or-later
//! The real serial transport against the simulator on a pty.

use xbtune_link::EcmClient;
use xbtune_link::client::fast_policy;
use xbtune_proto::EcmAddress;
use xbtune_sim::{Fault, Simulator, shared};

#[tokio::test(flavor = "multi_thread")]
async fn serial_transport_over_pty() {
    let sim = shared(Simulator::bueib());
    sim.lock().unwrap().script([Fault::Fragment(2), Fault::Drop]);
    let pty = xbtune_sim::pty::open().unwrap();
    let path = pty.path.clone();
    let _server = pty.serve(sim.clone()).unwrap();

    let io = xbtune_link::serial::open(&path, 9600).unwrap();
    let mut c = EcmClient::new(Box::new(io), EcmAddress::Stock).with_policy(fast_policy());
    assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    assert_eq!(c.version().await.unwrap(), "BUEIB310 12-11-03");
    assert_eq!(c.read_bytes(0, 0xFC, 1).await.unwrap().len(), 1);
    assert_eq!(c.stats().retries, 1);
}
