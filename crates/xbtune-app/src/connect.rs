// SPDX-License-Identifier: GPL-3.0-or-later
//! The Connect view: pick a Bluetooth adapter, a USB serial port or the demo.

use std::sync::mpsc;
use std::time::Duration;

use egui::{RichText, Ui};
use xbtune_core::session::LinkState;
use xbtune_link::bluetooth::{self, DeviceInfo};
use xbtune_link::serial::{self, PortInfo};

use crate::app::XbApp;
use crate::settings::SavedTarget;
use crate::theme::{self, Palette};

pub struct ConnectUi {
    pub devices: Vec<DeviceInfo>,
    scanning: Option<mpsc::Receiver<Result<Vec<DeviceInfo>, String>>>,
    scan_error: Option<String>,
    pub ports: Vec<PortInfo>,
    manual_address: String,
    manual_channel: u8,
    /// Connect as a factory race ECM (address 0x55, 19200 baud).
    pub race: bool,
}

impl ConnectUi {
    pub fn new(last: Option<SavedTarget>) -> Self {
        let race = last.as_ref().is_some_and(SavedTarget::race);
        let (manual_address, manual_channel) = match last {
            Some(SavedTarget::Bluetooth { address, channel, .. }) => (address, channel),
            _ => (String::new(), bluetooth::DEFAULT_CHANNEL),
        };
        Self {
            devices: Vec::new(),
            scanning: None,
            scan_error: None,
            ports: serial::list_ports(),
            manual_address,
            manual_channel,
            race,
        }
    }

    /// Collect a finished scan. Called every frame.
    pub fn poll(&mut self) {
        let Some(rx) = &self.scanning else { return };
        match rx.try_recv() {
            Ok(Ok(mut d)) => {
                // Likely adapters first: SPP, then paired, then by name.
                d.sort_by_key(|x| (!x.has_spp, !x.paired, x.name.clone().unwrap_or_default()));
                self.devices = d;
                self.scan_error = None;
                self.scanning = None;
            }
            Ok(Err(e)) => {
                self.scan_error = Some(e);
                self.scanning = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.scanning = None,
        }
    }

    fn scan(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, seconds: u64) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        rt.spawn(async move {
            let r = bluetooth::scan(Duration::from_secs(seconds)).await.map_err(|e| {
                let hint = xbtune_core::env::permission_hint("bluez");
                format!("Bluetooth scan failed: {e}\n{hint}")
            });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.scanning = Some(rx);
    }
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.heading("Connect to the ECM");
        ui.label(
            RichText::new(
                "Ignition ON, engine off for the first connection. XBTune only reads from the ECM.",
            )
            .color(p.muted),
        );
        ui.add_space(8.0);

        if crate::remote_sync::is_guest(app) {
            let owner = app.remote.peer().unwrap_or_else(|| "the bike owner".into());
            theme::card(ui).stroke(egui::Stroke::new(1.0, p.accent)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("Remote session with {owner}")).font(theme::strong_font(15.0)));
                ui.label(
                    RichText::new("You see their bike. To connect to a bike here, end the remote session first.")
                        .color(p.muted),
                );
            });
            ui.add_space(8.0);
        }

        let missing: Vec<_> = app.plugs.iter().filter(|s| s.connected == Some(false)).cloned().collect();
        if !missing.is_empty() {
            theme::card(ui).stroke(egui::Stroke::new(1.0, p.warn)).show(ui, |ui| {
                ui.label(
                    RichText::new("Snap permissions needed").font(theme::strong_font(15.0)).color(p.warn),
                );
                ui.label(
                    "XBTune can't use these until you allow them. Run in a terminal, then press Check again:",
                );
                for s in &missing {
                    ui.horizontal(|ui| {
                        let cmd = s.connect_command();
                        ui.code(&cmd);
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(cmd.clone());
                        }
                        ui.label(RichText::new(s.purpose).color(p.muted));
                    });
                }
                if ui.button("Check again").clicked() {
                    app.plugs = xbtune_core::env::plug_states();
                }
            });
            ui.add_space(8.0);
        }

        // ECM type first: it applies to every Connect button below,
        // "Last used" included.
        ecm_type(app, ui, &p);
        ui.add_space(8.0);
        let race = app.connect.race;

        if let Some(last) = app.settings.last.clone().map(|t| t.with_race(race)) {
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Last used").color(p.muted).small());
                        ui.label(RichText::new(last.label()).font(theme::strong_font(15.0)));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let busy = matches!(
                            app.link.state,
                            Some(
                                LinkState::Connecting { .. }
                                    | LinkState::Live
                                    | LinkState::Reconnecting { .. }
                            )
                        );
                        if ui.add_enabled(!busy, egui::Button::new("Connect")).clicked() {
                            app.start(last.clone());
                        }
                        if ui.checkbox(&mut app.settings.auto_connect, "Connect on start").changed() {
                            app.save_settings();
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }

        // Bluetooth
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Bluetooth (BuellTooth)").font(theme::strong_font(16.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if app.connect.scanning.is_some() {
                        ui.spinner();
                        ui.label("Scanning…");
                    } else {
                        if ui.button("Scan").on_hover_text("Discover nearby devices for 8 seconds").clicked()
                        {
                            let ctx = ui.ctx().clone();
                            app.connect.scan(&app.rt, &ctx, 8);
                        }
                        if ui.button("Paired devices").clicked() {
                            let ctx = ui.ctx().clone();
                            app.connect.scan(&app.rt, &ctx, 0);
                        }
                    }
                });
            });
            if let Some(e) = &app.connect.scan_error {
                ui.colored_label(p.bad, e);
            }
            let mut chosen = None;
            if app.connect.devices.is_empty() && app.connect.scanning.is_none() {
                ui.label(
                    RichText::new("Press Scan with the ignition on. BuellTooth shows up as an SPP device.")
                        .color(p.muted),
                );
            }
            egui::Grid::new("bt").num_columns(4).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
                for d in &app.connect.devices {
                    ui.label(RichText::new(d.name.clone().unwrap_or_else(|| "(no name)".into())).strong());
                    ui.label(RichText::new(d.address.to_string()).color(p.muted));
                    ui.horizontal(|ui| {
                        if d.has_spp {
                            crate::gauge::chip(ui, "serial (SPP)", true);
                        }
                        if d.paired {
                            crate::gauge::chip(ui, "paired", false);
                        }
                        if let Some(r) = d.rssi {
                            ui.label(RichText::new(format!("{r} dBm")).small().color(p.muted));
                        }
                    });
                    if ui.button("Connect").clicked() {
                        chosen = Some(SavedTarget::Bluetooth {
                            address: d.address.to_string(),
                            channel: bluetooth::DEFAULT_CHANNEL,
                            name: d.name.clone(),
                            race,
                        });
                    }
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            ui.collapsing("Enter an address", |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut app.connect.manual_address)
                            .hint_text("XX:XX:XX:XX:XX:XX")
                            .desired_width(170.0),
                    );
                    ui.label("Channel");
                    ui.add(egui::DragValue::new(&mut app.connect.manual_channel).range(1..=30));
                    let valid = xbtune_link::parse_bt_address(app.connect.manual_address.trim()).is_ok();
                    if ui.add_enabled(valid, egui::Button::new("Connect")).clicked() {
                        chosen = Some(SavedTarget::Bluetooth {
                            address: app.connect.manual_address.trim().to_uppercase(),
                            channel: app.connect.manual_channel,
                            name: None,
                            race,
                        });
                    }
                });
                ui.label(
                    RichText::new("Pair first if needed: bluetoothctl pair <address> (PIN usually 1234).")
                        .small()
                        .color(p.muted),
                );
            });
            if let Some(t) = chosen {
                app.start(t);
            }
        });
        ui.add_space(8.0);

        // USB serial
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("USB serial cable").font(theme::strong_font(16.0)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        app.connect.ports = serial::list_ports();
                    }
                });
            });
            if app.connect.ports.is_empty() {
                ui.label(
                    RichText::new("No USB serial adapters found (/dev/ttyUSB*, /dev/ttyACM*).")
                        .color(p.muted),
                );
            }
            let mut chosen = None;
            for port in &app.connect.ports {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&port.path).strong());
                    ui.label(RichText::new(&port.description).color(p.muted));
                    if ui.button("Connect").clicked() {
                        chosen = Some(SavedTarget::Serial { path: port.path.clone(), race });
                    }
                });
            }
            if let Some(t) = chosen {
                app.start(t);
            }
        });
        ui.add_space(8.0);
        wideband(app, ui, &p);
        ui.add_space(8.0);

        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Demo").font(theme::strong_font(16.0)));
                    ui.label(
                        RichText::new(
                            if race {
                                "A simulated factory race B2RIB with made-up engine data, for trying XBTune without the bike."
                            } else {
                                "A simulated BUEIB with made-up engine data, for trying XBTune without the bike."
                            },
                        )
                        .color(p.muted),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Start demo").clicked() {
                        app.start(SavedTarget::Demo { race });
                    }
                });
            });
        });
    });
}

/// An optional wideband controller, read beside the ECM from the next
/// connect on.
fn wideband(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    use crate::settings::WidebandPref;
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Wideband O2 (optional)").font(theme::strong_font(16.0)));
        ui.label(
            RichText::new(
                "An Innovate LC-2 (or another Innovate MTS device) on its own USB serial cable, plugged into \
                 the controller's serial OUT. Its AFR shows on the dashboard, goes into data logs and lets \
                 auto-tune aim for a target AFR. Not yet tried with a real controller: please report how it goes.",
            )
            .small()
            .color(p.muted),
        );
        let before = app.settings.wideband.clone();
        let mut pick = before.clone();
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(&mut pick, None, "None");
            for port in &app.connect.ports {
                let this = Some(WidebandPref::Serial { path: port.path.clone() });
                ui.radio_value(&mut pick, this, &port.path).on_hover_text(&port.description);
            }
            if let Some(WidebandPref::Serial { path }) = &before
                && !app.connect.ports.iter().any(|q| &q.path == path)
            {
                ui.radio_value(&mut pick, before.clone(), format!("{path} (not plugged in)"));
            }
            ui.radio_value(&mut pick, Some(WidebandPref::Demo), "Simulated LC-2");
        });
        if pick != before {
            // A first wideband gets a dashboard gauge.
            let afr = xbtune_core::wideband::AFR.to_string();
            if before.is_none() && pick.is_some() && !app.settings.gauges.contains(&afr) {
                app.settings.gauges.push(afr);
            }
            app.settings.wideband = pick;
            app.save_settings();
        }
        let ecm_port = match &app.settings.last {
            Some(SavedTarget::Serial { path, .. }) => Some(path.clone()),
            _ => None,
        };
        if let (Some(WidebandPref::Serial { path }), Some(ecm)) = (&app.settings.wideband, &ecm_port)
            && path == ecm
        {
            ui.colored_label(p.warn, "That's the ECM's port. The wideband needs a cable of its own.");
        }
        let running = app.link.target.is_some()
            && app.link.state.as_ref().is_some_and(|s| !matches!(s, LinkState::Stopped { .. }));
        let reading = app.link.wideband.as_ref().map(|(l, _)| l.clone());
        if running && reading != wanted_label(app) {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Applies from the next connect.").color(p.muted));
                if ui.button("Reconnect now").clicked()
                    && let Some(t) = app.link.target.clone()
                {
                    app.start(t);
                }
            });
        }
    });
}

/// The session label of the chosen wideband (as `WbLink` names it).
fn wanted_label(app: &XbApp) -> Option<String> {
    use crate::settings::WidebandPref;
    Some(match app.settings.wideband.as_ref()? {
        WidebandPref::Demo => "simulated Innovate LC-2".to_string(),
        WidebandPref::Serial { path } => xbtune_core::wideband::WbLink::serial_label(path),
    })
}

/// Stock or factory race: EcmDroid's protocol choice (3c189be). A factory
/// race ECM answers only on address 0x55 at 19200 baud, so it has to be
/// chosen before connecting; P&A race ECMs talk like stock ones.
fn ecm_type(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("ECM type").font(theme::strong_font(16.0)));
            ui.add_space(12.0);
            ui.radio_value(&mut app.connect.race, false, "Stock (or P&A race)");
            ui.radio_value(&mut app.connect.race, true, "Factory race");
        });
        let text = if app.connect.race {
            "Factory race ECMs have RACE USE ONLY engraved on the case. They talk at 19200 baud: \
             a USB cable is set up for you, but a Bluetooth adapter has to be configured for \
             19200 baud first (a BuellTooth is normally 9600)."
        } else {
            "Almost every ECM. P&A race ECMs (RACE USE ONLY printed on the case) count as stock here; \
             only an engraved marking means factory race."
        };
        ui.label(RichText::new(text).small().color(p.muted));
    });
}
