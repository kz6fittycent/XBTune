// SPDX-License-Identifier: GPL-3.0-or-later
//! BlueZ side of [`super`]: RFCOMM sockets and discovery over D-Bus.

use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::time::Duration;

use bluer::rfcomm::{SocketAddr, Stream};
use bluer::{AdapterEvent, Address};
use futures::StreamExt;

use super::{CONNECT_TIMEOUT, DeviceInfo};
use crate::BtAddress;

/// Serial Port Profile UUID.
pub const SPP_UUID: bluer::Uuid = bluer::Uuid::from_u128(0x00001101_0000_1000_8000_00805F9B34FB);

/// Open an RFCOMM stream to `address` on `channel`.
///
/// Two workarounds for `bluer::rfcomm::Stream::connect` (bluer 0.17):
/// - It calls `bind()` first. The snap `bluez` interface's seccomp filter
///   refuses `bind()` with EPERM, and the kernel connects an unbound RFCOMM
///   socket from any adapter anyway, so we skip it.
/// - It registers the socket with tokio before `connect()`, so a stale
///   "writable" event from the fresh socket ends its wait while the
///   connection is still being set up. The first write then fails with
///   ENOTCONN. We wait with `poll()` until the kernel says connected or failed.
pub async fn connect(address: BtAddress, channel: u8) -> std::io::Result<Stream> {
    let address = Address::new(address.0);
    let stream = bluer::rfcomm::Socket::new()?.connect(SocketAddr::new(address, channel)).await?;
    let fd = stream.as_raw_fd();
    tokio::task::spawn_blocking(move || wait_connected(fd, CONNECT_TIMEOUT))
        .await
        .map_err(std::io::Error::other)??;
    Ok(stream)
}

/// Block until the connecting socket `fd` is connected, failed, or `timeout`
/// passes. While an RFCOMM socket is connecting the kernel reports no poll
/// events; once connected it reports POLLOUT; on failure POLLERR/POLLHUP.
fn wait_connected(fd: std::os::fd::RawFd, timeout: Duration) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(Error::new(ErrorKind::TimedOut, "Bluetooth connection timed out"));
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        // SAFETY: `pfd` is a valid pollfd for one descriptor; `fd` stays open
        // because the caller holds the stream for the duration of this call.
        let rc = unsafe { libc::poll(&mut pfd, 1, left.as_millis().min(i32::MAX as u128) as i32) };
        if rc < 0 {
            let e = Error::last_os_error();
            if e.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if rc == 0 {
            continue; // re-check the deadline
        }
        socket_error(fd)?;
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(Error::new(ErrorKind::ConnectionRefused, "Bluetooth device closed the connection"));
        }
        if pfd.revents & libc::POLLOUT != 0 {
            return Ok(());
        }
    }
}

fn socket_error(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    let mut err: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `fd` is a live socket owned by the caller; `err` and `len` are
    // valid for writes of the sizes given.
    let rc =
        unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_ERROR, (&raw mut err).cast(), &mut len) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if err != 0 {
        return Err(std::io::Error::from_raw_os_error(err));
    }
    Ok(())
}

pub async fn scan(scan_for: Duration) -> bluer::Result<Vec<DeviceInfo>> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    if !adapter.is_powered().await? {
        return Err(bluer::Error {
            kind: bluer::ErrorKind::NotReady,
            message: format!("Bluetooth adapter {} is powered off", adapter.name()),
        });
    }
    let mut seen: BTreeMap<Address, ()> = BTreeMap::new();
    for a in adapter.device_addresses().await? {
        seen.insert(a, ());
    }
    if !scan_for.is_zero() {
        let events = adapter.discover_devices().await?;
        let mut events = std::pin::pin!(events);
        let deadline = tokio::time::Instant::now() + scan_for;
        while let Ok(Some(ev)) = tokio::time::timeout_at(deadline, events.next()).await {
            if let AdapterEvent::DeviceAdded(a) = ev {
                seen.insert(a, ());
            }
        }
    }
    let mut out = Vec::new();
    for address in seen.into_keys() {
        let Ok(dev) = adapter.device(address) else { continue };
        let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
        out.push(DeviceInfo {
            address: BtAddress(address.0),
            name: dev.name().await.ok().flatten(),
            paired: dev.is_paired().await.unwrap_or(false),
            trusted: dev.is_trusted().await.unwrap_or(false),
            connected: dev.is_connected().await.unwrap_or(false),
            rssi: dev.rssi().await.ok().flatten(),
            has_spp: uuids.contains(&SPP_UUID),
        });
    }
    // Likely ECM adapters first, then by name.
    out.sort_by_key(|d| (!d.has_spp, !d.paired, d.name.clone().unwrap_or_default()));
    Ok(out)
}
