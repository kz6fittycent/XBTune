// SPDX-License-Identifier: GPL-3.0-or-later
//! Files dropped on the window under Wayland. winit 0.30 (eframe's
//! windowing) handles drops only on X11, so on Wayland XBTune listens
//! itself: a thread with its own event queue on winit's Wayland connection
//! (libwayland allows several queues per connection) binds the seat's data
//! device, accepts `text/uri-list` drags, and reads the dropped file URIs.
//! Uses smithay-client-toolkit's data-device handlers (owner approved the
//! deps 2026-10-19). On X11, egui's own dropped-file events are used.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::{delegate_data_device, delegate_registry, delegate_seat, registry_handlers};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_data_source::WlDataSource;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, QueueHandle};

const URI_LIST: &str = "text/uri-list";

/// The listener thread; [`Listener::stop`] it before the window (and its
/// `wl_display`) goes away.
pub struct Listener {
    pub rx: Receiver<Vec<PathBuf>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Listener {
    /// Stop and wait (at most one poll interval).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How long the thread waits for Wayland events before checking `stop`.
const POLL_MS: i32 = 100;

/// Start listening on the Wayland display `display` (winit's `wl_display`).
/// Dropped paths arrive on the receiver; `wake` is called after each drop.
///
/// # Safety
/// `display` must be a valid `wl_display` pointer, and stay valid until
/// [`Listener::stop`] has returned.
pub unsafe fn start(display: *mut std::ffi::c_void, wake: impl Fn() + Send + 'static) -> Option<Listener> {
    // SAFETY: the caller guarantees `display` is winit's live wl_display.
    // (`from_foreign_display` exists with wayland-backend's `client_system`
    // feature, which eframe's Wayland support turns on.)
    let backend = unsafe { wayland_client::backend::Backend::from_foreign_display(display.cast()) };
    let conn = Connection::from_backend(backend);
    let (globals, mut queue) = registry_queue_init::<Dnd>(&conn).ok()?;
    let qh = queue.handle();
    let manager = DataDeviceManagerState::bind(&globals, &qh).ok()?;
    let (tx, rx) = channel();
    let mut state = Dnd {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        manager,
        devices: Vec::new(),
        tx,
        wake: Box::new(wake),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let thread = std::thread::Builder::new()
        .name("xbtune-dnd".into())
        .spawn(move || {
            // Not blocking_dispatch: it would still be waiting on the display
            // when winit closes it at exit.
            while !stopping.load(Ordering::Relaxed) {
                if queue.dispatch_pending(&mut state).is_err() || queue.flush().is_err() {
                    break;
                }
                let Some(guard) = queue.prepare_read() else { continue };
                let mut fds = [libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }];
                // SAFETY: one valid pollfd for the connection's open fd.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, POLL_MS) };
                if ready > 0 {
                    if guard.read().is_err() {
                        break;
                    }
                } else {
                    drop(guard);
                }
            }
            tracing::debug!("Wayland drop listener ended");
        })
        .ok()?;
    tracing::info!("listening for dropped files (Wayland)");
    Some(Listener { rx, stop, thread: Some(thread) })
}

struct Dnd {
    registry: RegistryState,
    seats: SeatState,
    manager: DataDeviceManagerState,
    devices: Vec<(WlSeat, DataDevice)>,
    tx: Sender<Vec<PathBuf>>,
    wake: Box<dyn Fn() + Send>,
}

impl Dnd {
    fn offer(&self, device: &WlDataDevice) -> Option<DragOffer> {
        self.devices.iter().find(|(_, d)| d.inner() == device).and_then(|(_, d)| d.data().drag_offer())
    }
}

/// `file://` URIs (one per line, `#` comments) as paths.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // file://host/path: only the local host's files can be opened.
        .filter_map(|rest| rest.find('/').map(|i| &rest[i..]))
        .map(|p| PathBuf::from(percent_decode(p)))
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl DataDeviceHandler for Dnd {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        device: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
        let Some(offer) = self.offer(device) else { return };
        if offer.with_mime_types(|m| m.iter().any(|t| t == URI_LIST)) {
            offer.accept_mime_type(offer.serial, Some(URI_LIST.into()));
            offer.set_actions(DndAction::Copy, DndAction::Copy);
        } else {
            offer.accept_mime_type(offer.serial, None);
        }
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, conn: &Connection, _: &QueueHandle<Self>, device: &WlDataDevice) {
        let Some(offer) = self.offer(device) else { return };
        let Ok(pipe) = offer.receive(URI_LIST.into()) else { return };
        // The sender writes only once the request reaches the compositor.
        let _ = conn.flush();
        let mut file = std::fs::File::from(std::os::fd::OwnedFd::from(pipe));
        let mut text = String::new();
        let _ = file.read_to_string(&mut text);
        offer.finish();
        offer.destroy();
        let paths = parse_uri_list(&text);
        if !paths.is_empty() && self.tx.send(paths).is_ok() {
            (self.wake)();
        }
    }
}

impl DataOfferHandler for Dnd {
    fn source_actions(&mut self, _: &Connection, _: &QueueHandle<Self>, offer: &mut DragOffer, _: DndAction) {
        offer.set_actions(DndAction::Copy, DndAction::Copy);
    }

    fn selected_action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &mut DragOffer, _: DndAction) {}
}

// XBTune never offers data; the trait is required by the delegate.
impl DataSourceHandler for Dnd {
    fn accept_mime(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: Option<String>) {}
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: String,
        _: WritePipe,
    ) {
    }
    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}
    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl SeatHandler for Dnd {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: WlSeat) {
        if !self.devices.iter().any(|(s, _)| *s == seat) {
            let d = self.manager.get_data_device(qh, &seat);
            self.devices.push((seat, d));
        }
    }

    fn new_capability(&mut self, conn: &Connection, qh: &QueueHandle<Self>, seat: WlSeat, _: Capability) {
        self.new_seat(conn, qh, seat);
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat, _: Capability) {}

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, seat: WlSeat) {
        self.devices.retain(|(s, _)| *s != seat);
    }
}

impl ProvidesRegistryState for Dnd {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![SeatState];
}

delegate_data_device!(Dnd);
delegate_seat!(Dnd);
delegate_registry!(Dnd);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_lists() {
        let text = "# from Files\r\nfile:///home/me/My%20Logs/ride%231.csv\r\nfile://localhost/tmp/a.xpr\r\nhttps://x.org/y\r\n";
        assert_eq!(
            parse_uri_list(text),
            vec![PathBuf::from("/home/me/My Logs/ride#1.csv"), PathBuf::from("/tmp/a.xpr")]
        );
    }
}
