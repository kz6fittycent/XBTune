// SPDX-License-Identifier: GPL-3.0-or-later
//! A wideband O2 controller beside the ECM (Innovate LC-2 and other ISP2
//! devices, `xbtune_proto::isp2`), on its own serial port.
//!
//! The controller streams a packet about every 82 ms whether anyone listens
//! or not. A task reads it and keeps the latest reading; the session puts
//! that reading on each runtime sample (if it is fresh), so live views, CSV
//! logs and auto-tune see the ECM data and the measured mixture together.
//!
//! In views the reading shows as two extra channels ([`channels`]): RtVars
//! whose offset lies far past any runtime frame. `Sample::value` reads them
//! from the sample's reading; code that decodes frames directly simply
//! finds nothing there.
//!
//! Built from Innovate's published spec and tested against the simulator
//! only: no real controller has been on the bench yet.

use std::fmt;
use std::time::{Duration, Instant};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use xbtune_defs::{Names, RtKind, RtVar};
pub use xbtune_proto::isp2::{Sensor, Status};

use crate::session::Opener;

/// A reading older than this is not put on a sample (3 packets missed).
pub const MAX_AGE: Duration = Duration::from_millis(300);

/// No packet for this long: tell the user the wideband went quiet.
pub const SILENCE: Duration = Duration::from_secs(2);

/// Channel names (also the CSV column names).
pub const AFR: &str = "wb_afr";
pub const LAMBDA: &str = "wb_lambda";
pub const STATUS: &str = "wb_status";

const OFFSET: usize = 1 << 30;

/// The wideband's channels, for views and pickers.
pub fn channels() -> Vec<RtVar> {
    let var = |i: usize, name: &str, label: &str, units: &str, decimals: u32, low: f64, high: f64| RtVar {
        name: name.into(),
        varname: name.into(),
        names: Names {
            label: Some(label.into()),
            description: Some("Measured by the wideband controller (not the ECM)".into()),
            units: (!units.is_empty()).then(|| units.into()),
            decimals: Some(decimals),
            ..Default::default()
        },
        kind: RtKind::Scalar,
        size: 1,
        offset: OFFSET + i,
        scale: 1.0,
        translate: 0.0,
        low: Some(low),
        high: Some(high),
        bits: None,
    };
    vec![var(0, AFR, "Wideband AFR", "AFR", 2, 6.0, 20.0), var(1, LAMBDA, "Wideband lambda", "", 3, 0.5, 1.5)]
}

/// True for the channels from [`channels`].
pub fn is_wideband(var: &RtVar) -> bool {
    var.offset >= OFFSET
}

/// A wideband channel's value from a reading.
pub fn value(var: &RtVar, s: Option<&Sensor>) -> Option<f64> {
    let s = s?;
    match var.offset - OFFSET {
        0 => s.afr(),
        1 => s.lambda,
        _ => None,
    }
}

/// Where the wideband is.
#[derive(Clone)]
pub struct WbLink {
    /// For the UI and the log header, e.g. "Innovate ISP2 on /dev/ttyUSB1".
    pub label: String,
    pub opener: Opener,
}

impl fmt::Debug for WbLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WbLink").field("label", &self.label).finish_non_exhaustive()
    }
}

impl WbLink {
    /// An ISP2 controller on a serial port (USB serial cable on its OUT port).
    pub fn serial(path: &str) -> Self {
        Self {
            label: Self::serial_label(path),
            opener: crate::session::target_opener(xbtune_link::Target::Serial {
                path: path.to_string(),
                baud: xbtune_proto::isp2::BAUD,
            }),
        }
    }
}

impl WbLink {
    pub fn serial_label(path: &str) -> String {
        format!("Innovate ISP2 on {path}")
    }
}

/// How the wideband link is doing.
#[derive(Debug, Clone, PartialEq)]
pub enum WbState {
    Connecting,
    /// Packets are arriving.
    Live,
    /// Open, but nothing (or nothing readable) for [`SILENCE`].
    Silent,
    /// Couldn't open or lost the port; retrying.
    Failed(String),
}

impl WbState {
    pub fn text(&self) -> String {
        match self {
            Self::Connecting => "connecting".into(),
            Self::Live => "receiving".into(),
            Self::Silent => "no data: is the controller powered, and its serial OUT on this port?".into(),
            Self::Failed(e) => format!("not open: {e}"),
        }
    }
}

/// The newest reading and when it arrived.
pub type Latest = Option<(Instant, Sensor)>;

/// Read the wideband until the returned task is aborted. Readings go to
/// `latest`; state changes to `state`.
pub fn spawn(
    link: WbLink,
    latest: watch::Sender<Latest>,
    state: impl Fn(WbState) + Send + Sync + 'static,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut last = None;
        let mut report = |s: WbState| {
            if last.as_ref() != Some(&s) {
                tracing::info!(state = ?s, link = %link.label, "wideband");
                state(s.clone());
                last = Some(s);
            }
        };
        loop {
            report(WbState::Connecting);
            let mut io = match (link.opener)().await {
                Ok(io) => io,
                Err(e) => {
                    report(WbState::Failed(e));
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };
            let mut dec = xbtune_proto::isp2::Decoder::new();
            let mut buf = [0u8; 256];
            let mut heard = Instant::now();
            let failure = loop {
                match tokio::time::timeout(Duration::from_millis(500), io.read(&mut buf)).await {
                    Ok(Ok(0)) => break "the port closed".to_string(),
                    Ok(Ok(n)) => {
                        for p in dec.push(&buf[..n]) {
                            if let Some(s) = p.sensors.first() {
                                heard = Instant::now();
                                latest.send_replace(Some((heard, *s)));
                                report(WbState::Live);
                            }
                        }
                    }
                    Ok(Err(e)) => break e.to_string(),
                    Err(_) => {}
                }
                if heard.elapsed() >= SILENCE {
                    report(WbState::Silent);
                }
            };
            report(WbState::Failed(failure));
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    })
}

/// The latest reading if it is fresh enough to go on a sample taken now.
pub fn fresh(latest: &watch::Receiver<Latest>, now: Instant) -> Option<Sensor> {
    let (at, s) = (*latest.borrow())?;
    (now.saturating_duration_since(at) <= MAX_AGE).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use xbtune_proto::isp2::{Lc1Words, encode};

    #[test]
    fn channels_read_from_the_reading_not_the_frame() {
        let ch = channels();
        let s = Sensor { status: Status::Normal, lambda: Some(0.9), stoich: 14.7 };
        assert!(ch.iter().all(is_wideband));
        assert!((value(&ch[0], Some(&s)).unwrap() - 13.23).abs() < 1e-9);
        assert_eq!(value(&ch[1], Some(&s)), Some(0.9));
        assert_eq!(value(&ch[0], None), None);
        // Decoding a frame directly finds nothing (and doesn't overflow).
        assert_eq!(ch[0].value(&[0u8; 200]), None);
    }

    #[tokio::test]
    async fn task_reads_a_stream_and_reports_silence() {
        let (mut tx_end, rx_end) = tokio::io::duplex(1024);
        let io = std::sync::Mutex::new(Some(rx_end));
        let opener: Opener = Arc::new(move || {
            let io = io.lock().unwrap().take();
            Box::pin(async move {
                io.map(|i| Box::new(i) as xbtune_link::BoxIo).ok_or_else(|| "gone".to_string())
            })
        });
        let (latest_tx, latest) = watch::channel(None);
        let (state_tx, mut states) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn(WbLink { label: "test".into(), opener }, latest_tx, move |s| {
            let _ = state_tx.send(s);
        });
        use tokio::io::AsyncWriteExt;
        tx_end.write_all(&encode(&[Lc1Words::lambda(0.88)], &[])).await.unwrap();
        assert_eq!(states.recv().await, Some(WbState::Connecting));
        assert_eq!(states.recv().await, Some(WbState::Live));
        let s = fresh(&latest, Instant::now()).unwrap();
        assert!((s.lambda.unwrap() - 0.88).abs() < 1e-9);
        assert_eq!(fresh(&latest, Instant::now() + Duration::from_secs(1)), None);
        assert_eq!(states.recv().await, Some(WbState::Silent));
        task.abort();
    }
}
