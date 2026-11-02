// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening web links in the user's browser. eframe is built without its
//! `links` feature (it pulls in another crate), so egui's "open this URL"
//! commands would go nowhere; [`Guard`] takes them at the end of each
//! frame and hands them to the OS: `xdg-open` on Linux (in the snap that
//! is snapd's own, which asks the desktop to open it), `open` on macOS,
//! the URL protocol handler on Windows. Only http(s) links are opened.

use std::process::Command;

/// Takes egui's OpenUrl commands when dropped (put one at the top of the
/// frame's `ui`, so every return path is covered).
pub struct Guard(pub egui::Context);

impl Drop for Guard {
    fn drop(&mut self) {
        let mut urls = Vec::new();
        self.0.output_mut(|o| {
            o.commands.retain(|c| match c {
                egui::OutputCommand::OpenUrl(u) => {
                    urls.push(u.url.clone());
                    false
                }
                _ => true,
            })
        });
        for url in urls {
            open(&url);
        }
    }
}

/// Open `url` in the browser, in the background; failures are logged.
pub fn open(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        tracing::warn!(url, "not opening a non-web link");
        return;
    }
    if std::env::var_os("XBTUNE_SCREENSHOT").is_some() {
        tracing::info!(url, "screenshot mode: not opening the link");
        return;
    }
    let mut cmd = command(url);
    match cmd.spawn() {
        Ok(mut child) => {
            // Reap it so it doesn't linger as a zombie.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            tracing::info!(url, "opened link");
        }
        Err(e) => tracing::warn!(url, error = %e, "couldn't open the link"),
    }
}

#[cfg(target_os = "windows")]
fn command(url: &str) -> Command {
    // The URL handler directly: no shell, so nothing in the URL is parsed.
    let mut c = Command::new("rundll32");
    c.args(["url.dll,FileProtocolHandler", url]);
    c
}

#[cfg(target_os = "macos")]
fn command(url: &str) -> Command {
    let mut c = Command::new("open");
    c.arg(url);
    c
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn command(url: &str) -> Command {
    let mut c = Command::new("xdg-open");
    c.arg(url);
    c
}
