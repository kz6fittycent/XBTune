// SPDX-License-Identifier: GPL-3.0-or-later
//! XBTune desktop app: connection, dashboard, live data table, charts,
//! diagnostics, resets, tune editing and burning. Active tests, clearing
//! codes and TPS reset, and writes to the ECM, are each locked on real
//! hardware until the owner unlocks them in Settings.

// No console window beside the app on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod afv_fold_view;
mod ai_view;
mod ai_wizard;
mod app;
mod autotune_view;
mod burn_view;
mod charts;
mod connect;
mod dashboard;
mod diagnostics;
mod disclaimer_view;
mod gauge;
mod grid;
mod import;
mod logs_view;
mod remote_sync;
mod remote_view;
mod report;
mod report_view;
mod resets;
mod settings;
mod settings_view;
mod setup_view;
mod splash;
mod table;
mod theme;
mod tune_view;
mod update;
#[cfg(target_os = "linux")]
mod wayland_dnd;
mod wizard;

fn main() -> eframe::Result {
    // Logging (terminal + app log file) and crash capture.
    let crash = report::init();
    report::crash_test(std::env::var("XBTUNE_CRASH_TEST").ok().as_deref());

    // The ECM link and Bluetooth run here, off the UI thread.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let handle = rt.handle().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("XBTune")
            .with_app_id("xbtune")
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(window_icon()),
        ..Default::default()
    };
    let result = eframe::run_native(
        "XBTune",
        options,
        Box::new(move |cc| Ok(Box::new(app::XbApp::new(cc, handle, crash)))),
    );
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    report::clean_exit();
    result
}

/// The tach icon for the title bar and taskbar (Linux desktops use the
/// .desktop file's SVG). `tools/render_icon.py` makes the file.
fn window_icon() -> egui::IconData {
    const SIZE: u32 = 128;
    let rgba = include_bytes!("../assets/icon-128.rgba").to_vec();
    debug_assert_eq!(rgba.len(), (SIZE * SIZE * 4) as usize);
    egui::IconData { rgba, width: SIZE, height: SIZE }
}
