// SPDX-License-Identifier: GPL-3.0-or-later
//! Serve the simulator on a pseudo-terminal, so the real serial transport
//! can open it like a USB adapter (`/dev/pts/N`).

use std::ffi::CStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::thread;

use xbtune_proto::FrameDecoder;
use xbtune_proto::decoder::Decoded;

use crate::{Emission, SharedSim};

/// The master side of a pty and the path of its slave device.
pub struct Pty {
    master: OwnedFd,
    pub path: String,
}

/// Open a new pty in raw mode.
pub fn open() -> io::Result<Pty> {
    // SAFETY: plain libc calls; every return value is checked and the fd is
    // owned by `OwnedFd` from here on.
    unsafe {
        let fd = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let master = OwnedFd::from_raw_fd(fd);
        if libc::grantpt(fd) != 0 || libc::unlockpt(fd) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut name = [0 as libc::c_char; 128];
        if libc::ptsname_r(fd, name.as_mut_ptr(), name.len()) != 0 {
            return Err(io::Error::last_os_error());
        }
        let path = CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) == 0 {
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(fd, libc::TCSANOW, &t);
        }
        Ok(Pty { master, path })
    }
}

impl Pty {
    /// Serve `sim` on a background thread. The thread ends when the master
    /// is closed or on an unexpected error. While no client has the slave
    /// open, reads fail with EIO; that is waited out.
    pub fn serve(self, sim: SharedSim) -> io::Result<thread::JoinHandle<()>> {
        let mut file = File::from(self.master);
        thread::Builder::new().name("xbtune-sim-pty".into()).spawn(move || {
            let mut decoder = FrameDecoder::new();
            let mut buf = [0u8; 512];
            loop {
                let n = match file.read(&mut buf) {
                    Ok(0) => {
                        thread::sleep(std::time::Duration::from_millis(20));
                        continue;
                    }
                    Ok(n) => n,
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                        // No client attached.
                        decoder.clear();
                        thread::sleep(std::time::Duration::from_millis(50));
                        continue;
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return,
                };
                decoder.push(&buf[..n]);
                while let Some(d) = decoder.next_frame() {
                    let Decoded::Frame(frame) = d else { continue };
                    let emissions = sim.lock().expect("sim lock").respond(&frame);
                    for e in emissions {
                        match e {
                            Emission::Write(b) => {
                                if file.write_all(&b).is_err() {
                                    return;
                                }
                            }
                            Emission::Sleep(d) => thread::sleep(d),
                        }
                    }
                }
            }
        })
    }
}
