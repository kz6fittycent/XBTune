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

/// List USB serial ports. Falls back to globbing `/dev` when udev
/// enumeration is unavailable (it can be inside a confined snap).
pub fn list_ports() -> Vec<PortInfo> {
    let mut out: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => Some(PortInfo {
                path: p.port_name,
                description: format!(
                    "{:04x}:{:04x} {} {}",
                    u.vid,
                    u.pid,
                    u.manufacturer.unwrap_or_default(),
                    u.product.unwrap_or_default()
                )
                .trim()
                .to_string(),
            }),
            _ => None,
        })
        .collect();
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
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}
