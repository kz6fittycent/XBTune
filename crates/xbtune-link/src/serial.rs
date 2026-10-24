// SPDX-License-Identifier: GPL-3.0-or-later
//! USB serial adapters (FTDI, CH340, CP210x) via the `serialport` crate.
//!
//! `serialport` is blocking, so a reader thread and a writer thread move
//! bytes between the port and async channels. The async side never blocks.
//! Line settings: 8N1, no flow control (EcmDroid `ECM.java` class comment).

use std::io::{self, Read, Write};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

/// An open serial port as an async byte stream.
pub struct SerialIo {
    rx: mpsc::Receiver<io::Result<Vec<u8>>>,
    pending: Vec<u8>,
    tx: Option<std_mpsc::Sender<Vec<u8>>>,
    write_error: Arc<Mutex<Option<io::Error>>>,
    stop: Arc<AtomicBool>,
}

/// Open `path` at `baud`, 8N1, no flow control.
pub fn open(path: &str, baud: u32) -> serialport::Result<SerialIo> {
    let port = serialport::new(path, baud)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(20))
        .open()?;
    // Throw away anything the adapter buffered before we opened it.
    let _ = port.clear(serialport::ClearBuffer::All);
    let mut reader = port.try_clone()?;
    let mut writer = port;

    let stop = Arc::new(AtomicBool::new(false));
    let (in_tx, in_rx) = mpsc::channel::<io::Result<Vec<u8>>>(256);
    let reader_stop = stop.clone();
    thread::Builder::new().name("xbtune-serial-rx".into()).spawn(move || {
        let mut buf = [0u8; 512];
        while !reader_stop.load(Ordering::Relaxed) {
            match reader.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => {
                    if in_tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::TimedOut => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    let _ = in_tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    })?;

    let (out_tx, out_rx) = std_mpsc::channel::<Vec<u8>>();
    let write_error = Arc::new(Mutex::new(None));
    let writer_error = write_error.clone();
    thread::Builder::new().name("xbtune-serial-tx".into()).spawn(move || {
        for data in out_rx {
            if let Err(e) = writer.write_all(&data).and_then(|()| writer.flush()) {
                *writer_error.lock().expect("lock") = Some(e);
                break;
            }
        }
    })?;

    Ok(SerialIo { rx: in_rx, pending: Vec::new(), tx: Some(out_tx), write_error, stop })
}

impl Drop for SerialIo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl AsyncRead for SerialIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.pending.is_empty() {
            match self.rx.poll_recv(cx) {
                Poll::Ready(Some(Ok(data))) => self.pending = data,
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Err(e)),
                Poll::Ready(None) => return Poll::Ready(Ok(())), // EOF
                Poll::Pending => return Poll::Pending,
            }
        }
        let n = self.pending.len().min(buf.remaining());
        buf.put_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for SerialIo {
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        if let Some(e) = self.write_error.lock().expect("lock").take() {
            return Poll::Ready(Err(e));
        }
        match &self.tx {
            Some(tx) if tx.send(data.to_vec()).is_ok() => Poll::Ready(Ok(data.len())),
            _ => Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "serial writer stopped"))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.tx = None;
        self.stop.store(true, Ordering::Relaxed);
        Poll::Ready(Ok(()))
    }
}

/// A serial port that looks like an ECM adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub path: String,
    pub description: String,
}

/// List serial ports that could be an ECM adapter.
///
/// Linux: USB serial ports only (a Bluetooth adapter is reached over RFCOMM
/// instead). Falls back to globbing `/dev` when udev enumeration is
/// unavailable (it can be inside a confined snap).
///
/// Windows and macOS: every port, because a paired Bluetooth adapter is a
/// serial port there. Windows makes two per adapter and only the
/// "Outgoing" one connects: [`crate::win_ports`] tells them apart.
pub fn list_ports() -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| Some(PortInfo { description: describe(p.port_type)?, path: p.port_name }))
        .collect();
    #[cfg(target_os = "linux")]
    if let Ok(dir) = std::fs::read_dir("/dev") {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("ttyUSB") || name.starts_with("ttyACM") {
                let path = format!("/dev/{name}");
                if !out.iter().any(|p| p.path == path) {
                    out.push(PortInfo { path, description: String::new() });
                }
            }
        }
    }
    // macOS lists each port twice; the call-out (`cu.`) side is the one to open.
    #[cfg(target_os = "macos")]
    out.retain(|p| !p.path.starts_with("/dev/tty."));
    out.sort_by_key(|p| port_order(&p.path));
    #[cfg(windows)]
    windows_names(&mut out);
    out
}

/// Say which COM port is which, outgoing Bluetooth ports first.
#[cfg(windows)]
fn windows_names(out: &mut [PortInfo]) {
    use crate::win_ports::{self, PortKind};
    let kinds = win_ports::ports();
    for p in out.iter_mut() {
        match kinds.get(&p.path) {
            Some(PortKind::BluetoothOut { address, name }) => {
                let name = name.as_deref().unwrap_or("adapter");
                p.description = format!("Bluetooth {name} ({address}), outgoing: use this one");
            }
            Some(PortKind::BluetoothIn) => p.description = "Bluetooth, incoming: not this one".into(),
            Some(PortKind::Other { friendly: Some(f) }) if p.description.is_empty() => {
                p.description = f.clone()
            }
            _ => {}
        }
    }
    out.sort_by_key(|p| !matches!(kinds.get(&p.path), Some(PortKind::BluetoothOut { .. })));
}

/// What to show next to a port, or `None` to leave it out.
fn describe(t: serialport::SerialPortType) -> Option<String> {
    match t {
        serialport::SerialPortType::UsbPort(u) => Some(
            format!(
                "{:04x}:{:04x} {} {}",
                u.vid,
                u.pid,
                u.manufacturer.unwrap_or_default(),
                u.product.unwrap_or_default()
            )
            .trim()
            .to_string(),
        ),
        serialport::SerialPortType::BluetoothPort if !cfg!(target_os = "linux") => Some("Bluetooth".into()),
        _ if !cfg!(target_os = "linux") => Some(String::new()),
        _ => None,
    }
}

/// Sort key: `COM2` before `COM10`.
fn port_order(path: &str) -> (String, u32) {
    let digits = path.len() - path.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (stem, num) = path.split_at(path.len() - digits);
    (stem.to_string(), num.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    #[test]
    fn port_order_is_numeric() {
        let mut v = ["COM10", "COM2", "/dev/ttyUSB1", "COM1", "/dev/ttyUSB0"];
        v.sort_by_key(|p| super::port_order(p));
        assert_eq!(v, ["/dev/ttyUSB0", "/dev/ttyUSB1", "COM1", "COM2", "COM10"]);
    }
}
