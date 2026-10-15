// SPDX-License-Identifier: GPL-3.0-or-later
//! XBTune desktop app: connection, dashboard, live data table, charts and
//! diagnostics. It never writes the EEPROM; active tests, clearing codes and
//! TPS reset are locked on real hardware until the owner unlocks them.

mod app;
mod charts;
mod connect;
mod dashboard;
mod diagnostics;
mod gauge;
mod grid;
mod settings;
mod table;
mod theme;
mod tune_view;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // zbus warns when confinement blocks the accessibility bus; harmless.
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,zbus=error")),
        )
        .with_writer(std::io::stderr)
        .init();

    // The ECM link and Bluetooth run here, off the UI thread.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let handle = rt.handle().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("XBTune")
            .with_app_id("xbtune")
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    let result =
        eframe::run_native("XBTune", options, Box::new(move |cc| Ok(Box::new(app::XbApp::new(cc, handle)))));
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    result
}
