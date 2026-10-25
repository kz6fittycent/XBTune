// SPDX-License-Identifier: GPL-3.0-or-later
//! The Charts view: selected channels over time, one stacked plot each,
//! with linked time axes.

use egui::{RichText, Ui};
use egui_plot::{Line, Plot, PlotPoints};
use xbtune_core::live::{format_value, units};
use xbtune_defs::RtKind;

use crate::app::XbApp;
use crate::theme::{self, Palette};

const WINDOWS: &[(f64, &str)] =
    &[(10.0, "10 s"), (30.0, "30 s"), (60.0, "1 min"), (120.0, "2 min"), (300.0, "5 min"), (900.0, "15 min")];

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    if app.link.identity.is_none() {
        crate::table::not_connected(app, ui);
        return;
    }
    let p = Palette::for_visuals(ui.visuals());
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.heading("Charts");
        ui.add_space(12.0);
        for (secs, label) in WINDOWS {
            if ui.selectable_label(app.settings.bike.chart_seconds == *secs, *label).clicked() {
                app.settings.bike.chart_seconds = *secs;
                changed = true;
            }
        }
        ui.separator();
        let paused = app.chart_paused.is_some();
        let label = if paused {
            crate::icons::with(crate::icons::PLAY, "Resume")
        } else {
            crate::icons::with(crate::icons::PAUSE, "Pause")
        };
        if ui.button(label).clicked() {
            app.chart_paused = if paused { None } else { app.history.latest().map(|s| s.t) };
        }
        ui.menu_button("Channels", |ui| {
            ui.set_max_height(460.0);
            egui::ScrollArea::vertical().show(ui, |ui| {
                for c in app.link.channels.iter().filter(|c| c.kind == RtKind::Scalar) {
                    let mut on = app.settings.bike.chart_channels.contains(&c.name);
                    if ui.checkbox(&mut on, format!("{}  ({})", c.label(), c.name)).changed() {
                        if on {
                            app.settings.bike.chart_channels.push(c.name.clone());
                        } else {
                            app.settings.bike.chart_channels.retain(|n| n != &c.name);
                        }
                        changed = true;
                    }
                }
            });
        });
    });
    if changed {
        app.save_settings();
    }
    ui.add_space(6.0);

    let end = app.chart_paused.or_else(|| app.history.latest().map(|s| s.t)).unwrap_or(0.0);
    let start = end - app.settings.bike.chart_seconds;
    let vars: Vec<_> =
        app.settings.bike.chart_channels.iter().filter_map(|n| app.link.channel(n).cloned()).collect();
    if vars.is_empty() {
        ui.label(RichText::new("Pick channels to chart from the Channels menu.").color(p.muted));
        return;
    }
    let height = ((ui.available_height() - 8.0 * vars.len() as f32) / vars.len() as f32).clamp(110.0, 260.0);
    let link_group = ui.id().with("time");
    egui::ScrollArea::vertical().show(ui, |ui| {
        for (i, v) in vars.iter().enumerate() {
            let color = p.series[i % p.series.len()];
            let pts: Vec<[f64; 2]> = app
                .history
                .samples
                .iter()
                .filter(|s| s.t >= start && s.t <= end)
                .filter_map(|s| s.value(v).map(|y| [s.t - end, y]))
                .collect();
            let current = pts.last().map(|p| format_value(v, p[1])).unwrap_or_else(|| "–".into());
            theme::card(ui).inner_margin(10).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(v.label()).font(theme::strong_font(14.0)).color(color));
                    ui.label(RichText::new(format!("{current} {}", units(v))).font(theme::strong_font(14.0)));
                });
                Plot::new(("chart", &v.name))
                    .height(height)
                    .link_axis(link_group, [true, false])
                    .link_cursor(link_group, [true, false])
                    .default_x_bounds(-app.settings.bike.chart_seconds, 0.0)
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .allow_boxed_zoom(false)
                    .auto_bounds([false, true])
                    .y_axis_min_width(48.0)
                    .x_axis_formatter(|m, _| format!("{:.0}s", m.value))
                    .show(ui, |plot| {
                        plot.set_plot_bounds_x(-app.settings.bike.chart_seconds..=0.0);
                        plot.line(Line::new(v.label(), PlotPoints::from(pts)).color(color).width(2.0));
                    });
            });
            ui.add_space(4.0);
        }
    });
}
