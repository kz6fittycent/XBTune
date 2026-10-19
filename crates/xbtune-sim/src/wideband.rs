// SPDX-License-Identifier: GPL-3.0-or-later
//! A simulated Innovate LC-2: ISP2 packets every 81.92 ms
//! (`xbtune_proto::isp2`), with the lambda of the made-up ride
//! ([`crate::engine::lambda_at`]). Warming up for the first
//! [`WARM_UP`] seconds, free-air O2 level while the engine is stopped.

use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use xbtune_proto::isp2::{Lc1Words, encode};

use crate::SharedSim;

/// Time between packets (8 MHz / 655360).
pub const INTERVAL: Duration = Duration::from_micros(81_920);

/// Seconds of "warming up" after the stream starts.
pub const WARM_UP: f64 = 6.0;

/// The packet at `since_start` seconds after the controller came on, for
/// ride time `t` with the engine `running`.
pub fn packet(since_start: f64, t: f64, running: bool) -> Vec<u8> {
    let words = if since_start < WARM_UP {
        Lc1Words { function: 4, value: (since_start / WARM_UP * 1000.0) as u16, multiplier: 147 }
    } else {
        match crate::engine::lambda_at(t, running) {
            Some(l) => Lc1Words::lambda(l),
            None => Lc1Words { function: 1, value: 209, multiplier: 147 },
        }
    };
    encode(&[words], &[])
}

/// Stream packets for `sim`'s ride (its engine clock and running state)
/// into an in-memory stream; returns the reading end. Needs a tokio runtime.
pub fn in_process(sim: SharedSim) -> xbtune_link::BoxIo {
    let (client, mut server) = tokio::io::duplex(1024);
    tokio::spawn(async move {
        let started = Instant::now();
        let mut tick = tokio::time::interval(INTERVAL);
        loop {
            tick.tick().await;
            let (t, running) = {
                let s = sim.lock().expect("sim lock");
                (s.engine_time().unwrap_or(0.0), s.engine_running())
            };
            if server.write_all(&packet(started.elapsed().as_secs_f64(), t, running)).await.is_err() {
                break;
            }
        }
    });
    Box::new(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xbtune_proto::isp2::{Decoder, Status};

    #[test]
    fn warms_up_then_follows_the_ride() {
        let mut d = Decoder::new();
        let warm = d.push(&packet(1.0, 1.0, true));
        assert!(matches!(warm[0].sensors[0].status, Status::WarmingUp(_)));
        let pull = d.push(&packet(20.0, 11.0, true))[0].sensors[0];
        assert!(pull.lambda.unwrap() < 0.9);
        let off = d.push(&packet(20.0, 11.0, false))[0].sensors[0];
        assert_eq!(off.status, Status::O2Level(20.9));
    }
}
