// SPDX-License-Identifier: GPL-3.0-or-later
//! The Dashboard: configurable gauges, engine flags and current errors.

use egui::{RichText, Ui};
use xbtune_core::live::{format_value, units};
use xbtune_defs::RtKind;

use crate::app::XbApp;
use crate::gauge::{chip, gauge, range_for};
use crate::settings::Settings;
use crate::theme::{self, Palette};

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    if app.link.identity.is_none() {
        crate::table::not_connected(app, ui);
        return;
    }
    let latest = app.history.latest().cloned();
    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Dashboard");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reset gauges").clicked() {
                    app.settings.bike.gauges = Settings::default().bike.gauges;
                    app.save_settings();
                }
                if ui.button("Add gauge").clicked() {
                    app.settings.bike.gauges.push("RPM".into());
                    app.save_settings();
                }
                ui.label(RichText::new("Right-click a gauge to change or remove it").small().color(p.muted));
            });
        });
        ui.add_space(6.0);

        // Each card adds its margin and stroke; cards are item_spacing apart.
        let width = ui.available_width();
        let gap = ui.spacing().item_spacing.x;
        let overhead = 2.0 * 6.0 + 2.0;
        let fit = |n: f32| (width - gap * (n - 1.0)) / n - overhead;
        let per_row = (1..=8).rev().find(|&n| fit(n as f32) >= 160.0).unwrap_or(1);
        let size = fit(per_row as f32).min(240.0);
        let mut change: Option<(usize, Option<String>)> = None;
        for (row, names) in app.settings.bike.gauges.chunks(per_row).enumerate() {
            ui.horizontal(|ui| {
                for (col, name) in names.iter().enumerate() {
                    let index = row * per_row + col;
                    let var = app.link.channel(name).cloned();
                    let r = theme::card(ui).inner_margin(6).show(ui, |ui| match &var {
                        Some(v) => {
                            let value = latest.as_ref().and_then(|s| s.value(v));
                            let text = value.map(|x| format_value(v, x)).unwrap_or_else(|| "–".into());
                            gauge(ui, size, v.label(), &units(v), &text, value, range_for(v))
                        }
                        None => gauge(
                            ui,
                            size,
                            name,
                            "not on this ECM",
                            "–",
                            None,
                            crate::gauge::Range { min: 0.0, max: 1.0, warn: None },
                        ),
                    });
                    r.inner.context_menu(|ui| {
                        ui.set_max_height(420.0);
                        if ui.button("Remove gauge").clicked() {
                            change = Some((index, None));
                            ui.close();
                        }
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for c in app.link.channels.iter().filter(|c| c.kind == RtKind::Scalar) {
                                if ui
                                    .selectable_label(c.name == *name, format!("{}  ({})", c.label(), c.name))
                                    .clicked()
                                {
                                    change = Some((index, Some(c.name.clone())));
                                    ui.close();
                                }
                            }
                        });
                    });
                }
            });
            ui.add_space(4.0);
        }
        if let Some((i, new)) = change {
            match new {
                Some(n) => app.settings.bike.gauges[i] = n,
                None => {
                    app.settings.bike.gauges.remove(i);
                }
            }
            app.save_settings();
        }

        ui.add_space(10.0);
        let Some(sample) = &latest else { return };
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Inputs and outputs").font(theme::strong_font(15.0)));
            // Bit sets whose bits the definitions name (e.g. DOut, DIn).
            for v in
                app.link.channels.iter().filter(|v| v.kind == RtKind::Bitfield && !v.varname.contains("Diag"))
            {
                let Some(bits) = &v.bits else { continue };
                if bits.bits.iter().all(|b| b.name.starts_with(&v.varname)) {
                    continue;
                }
                let byte = sample.value(v).unwrap_or(0.0) as u8;
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(v.label()).color(p.muted));
                    for b in bits.bits.iter().filter(|b| !b.name.starts_with(&v.varname)) {
                        chip(ui, &b.name, byte & (1 << b.nr) != 0);
                    }
                });
            }
            ui.add_space(4.0);
            // Flag bytes without named bits: show the bits, don't guess.
            ui.horizontal_wrapped(|ui| {
                for v in app.link.channels.iter().filter(|v| {
                    v.kind == RtKind::Bitfield
                        && v.bits
                            .as_ref()
                            .is_none_or(|b| b.bits.iter().all(|x| x.name.starts_with(&v.varname)))
                }) {
                    let byte = sample.value(v).unwrap_or(0.0) as u8;
                    ui.add(
                        egui::Label::new(RichText::new(format!("{} {byte:08b}", v.name)).color(p.muted))
                            .extend(),
                    );
                }
            });
            ui.label(
                RichText::new(
                    "Bit meanings for the Flags bytes are not in the definitions, so they are shown raw.",
                )
                .small()
                .color(p.muted),
            );
        });
        ui.add_space(8.0);
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Current trouble codes").font(theme::strong_font(15.0)));
            let mut any = false;
            ui.horizontal_wrapped(|ui| {
                for v in app.link.channels.iter().filter(|v| v.varname.starts_with("CDiag")) {
                    let Some(bits) = &v.bits else { continue };
                    let byte = sample.value(v).unwrap_or(0.0) as u8;
                    for b in bits.bits.iter().filter(|b| byte & (1 << b.nr) != 0) {
                        any = true;
                        let code = b.dtc.as_deref().map(|d| format!("{d} ")).unwrap_or_default();
                        ui.colored_label(
                            p.bad,
                            format!("{code}{}", if b.name.is_empty() { "?" } else { &b.name }),
                        );
                    }
                }
            });
            if !any {
                ui.label(RichText::new("None").color(p.good));
            }
            ui.label(
                RichText::new("Stored codes, descriptions and clearing come with diagnostics (phase 4).")
                    .small()
                    .color(p.muted),
            );
        });
    });
}
