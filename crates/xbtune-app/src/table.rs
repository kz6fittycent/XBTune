// SPDX-License-Identifier: GPL-3.0-or-later
//! The Live data view: every runtime channel the definition exposes.

use egui::{RichText, Ui};
use xbtune_core::live::{format_value, set_flags, units};

use crate::app::{View, XbApp};
use crate::theme::{self, Palette};

/// Placeholder for views that need a connection.
pub fn not_connected(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("No live data").font(theme::strong_font(20.0)));
        ui.label(
            RichText::new("Connect to the ECM, or start the demo, to see live data here.").color(p.muted),
        );
        ui.add_space(10.0);
        if ui.button("Go to Connect").clicked() {
            app.view = View::Connect;
        }
    });
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if app.link.identity.is_none() {
        not_connected(app, ui);
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let filter_id = ui.id().with("filter");
    let mut filter: String = ui.data_mut(|d| d.get_temp(filter_id)).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.heading("Live data");
        ui.add_space(12.0);
        ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Filter channels").desired_width(220.0));
        if let Some(id) = &app.link.identity {
            ui.label(
                RichText::new(format!("{} channels · {}", app.link.channels.len(), id.ecm.id)).color(p.muted),
            );
        }
    });
    ui.data_mut(|d| d.insert_temp(filter_id, filter.clone()));
    ui.add_space(6.0);
    let latest = app.history.latest().cloned();
    let f = filter.to_lowercase();
    theme::card(ui).show(ui, |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("vars").num_columns(5).spacing([18.0, 6.0]).striped(true).show(ui, |ui| {
                for h in ["Channel", "Name", "Value", "Units", "Flags set / raw"] {
                    ui.label(RichText::new(h).color(p.muted).small());
                }
                ui.end_row();
                for v in &app.link.channels {
                    if !f.is_empty()
                        && !v.label().to_lowercase().contains(&f)
                        && !v.name.to_lowercase().contains(&f)
                        && !v.varname.to_lowercase().contains(&f)
                    {
                        continue;
                    }
                    ui.label(v.label()).on_hover_text(v.names.description.clone().unwrap_or_default());
                    ui.label(RichText::new(&v.name).color(p.muted));
                    let value = latest.as_ref().and_then(|s| s.value(v));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(value.map(|x| format_value(v, x)).unwrap_or_else(|| "–".into()))
                                .font(theme::strong_font(14.0)),
                        );
                    });
                    ui.label(units(v));
                    let raw = latest.as_ref().and_then(|s| v.raw(&s.frame));
                    let extra = match (value, &v.bits) {
                        (Some(x), Some(_)) => set_flags(v, x).join(", "),
                        _ => raw.map(|r| format!("raw {r}")).unwrap_or_default(),
                    };
                    ui.label(RichText::new(extra).small().color(p.muted));
                    ui.end_row();
                }
            });
        });
    });
}
