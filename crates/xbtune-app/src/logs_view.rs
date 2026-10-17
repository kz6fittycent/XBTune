// SPDX-License-Identifier: GPL-3.0-or-later
//! The Logs view: open recorded logs, see their statistics and anomalies,
//! chart any channel over the whole log, and play a log back through the
//! Dashboard, Live data, Charts and the live point on the fuel maps, as if
//! it were live. Playback only runs while no ECM is connected.
//!
//! The loaded logs also feed the fuel-map overlays and the auto-tune in
//! the Tune view (`crate::autotune_view`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use egui::{RichText, Ui};
use egui_plot::{Line, Plot, PlotPoints, VLine};
use xbtune_core::live::{format_value, units};
use xbtune_core::logfile::Log;
use xbtune_core::logstats::{self, Anomaly, O2Stats};
use xbtune_core::session::Identity;
use xbtune_defs::RtKind;

use crate::app::{LinkView, View, XbApp};
use crate::theme::{self, Palette};

/// A loaded log and its summary.
pub struct Loaded {
    pub log: Arc<Log>,
    pub o2: O2Stats,
    pub anomalies: Vec<Anomaly>,
}

pub struct Playback {
    pub log: usize,
    /// Playhead, in the log's own seconds.
    pub t: f64,
    pub playing: bool,
    pub speed: f64,
    last: Instant,
    /// First sample not yet in the history.
    next: usize,
}

pub struct LogsUi {
    pub logs: Vec<Loaded>,
    pub selected: usize,
    pub play: Option<Playback>,
    /// Channels charted in this view.
    pub chart: Vec<String>,
    /// Open-file picker folder, when open.
    picker: Option<PathBuf>,
    /// Bumped whenever the set of logs changes (overlay/auto-tune caches).
    pub generation: u64,
}

impl Default for LogsUi {
    fn default() -> Self {
        Self {
            logs: Vec::new(),
            selected: 0,
            play: None,
            chart: ["RPM", "TPD", "O2", "EGO Corr.", "CLT"].map(String::from).to_vec(),
            picker: None,
            generation: 0,
        }
    }
}

impl LogsUi {
    pub fn playing_name(&self) -> Option<String> {
        self.play.as_ref().and_then(|p| self.logs.get(p.log)).map(|l| l.log.name())
    }
}

fn logs_dir() -> PathBuf {
    xbtune_core::env::Dirs::detect().map(|d| d.logs()).unwrap_or_else(|| PathBuf::from("."))
}

/// Write log `i` as an .msl next to the other logs (never over a file).
fn export_msl(app: &mut XbApp, i: usize) {
    let log = app.logs.logs[i].log.clone();
    let dir = logs_dir();
    let stem = log.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "log".into());
    let path = xbtune_core::time::unique_path(&dir, &stem, ".msl");
    let r = std::fs::create_dir_all(&dir)
        .and_then(|_| std::fs::File::create(&path))
        .and_then(|f| xbtune_core::logformats::write_msl(&log, std::io::BufWriter::new(f)));
    match r {
        Ok(()) => {
            tracing::info!(path = %path.display(), "log exported as .msl");
            app.say(format!("Saved {}", path.display()), false);
        }
        Err(e) => app.say(format!("Couldn't save {}: {e}", path.display()), true),
    }
}

/// Open a log file and add it (once).
pub fn add(app: &mut XbApp, path: &Path) {
    if app.logs.logs.iter().any(|l| l.log.path == path) {
        return;
    }
    match Log::open(path, &app.catalog) {
        Ok(log) => {
            let midpoint = app
                .tune
                .as_ref()
                .and_then(|t| t.value("KO2_Midpoint").ok())
                .and_then(|v| match v {
                    xbtune_defs::Value::Number(n) => Some(n),
                    _ => None,
                })
                .unwrap_or(0.49);
            let o2 = logstats::o2_stats(&log, midpoint);
            let anomalies = logstats::anomalies(&log);
            tracing::info!(path = %path.display(), samples = log.samples.len(), "log opened");
            app.logs.logs.push(Loaded { log: Arc::new(log), o2, anomalies });
            app.logs.selected = app.logs.logs.len() - 1;
            app.logs.generation += 1;
        }
        Err(e) => app.say(format!("Couldn't open the log: {e}"), true),
    }
}

fn identity(log: &Log) -> Identity {
    Identity { version: log.version.clone(), ecm: log.ecm.clone(), definition: log.definition.clone() }
}

/// Play log `i` through the live views (disconnects first).
pub fn start_playback(app: &mut XbApp, i: usize) {
    let Some(l) = app.logs.logs.get(i) else { return };
    let log = l.log.clone();
    app.stop();
    app.link =
        LinkView { identity: Some(identity(&log)), channels: log.channels.clone(), ..Default::default() };
    let t0 = log.samples.first().map_or(0.0, |s| s.t);
    app.logs.play =
        Some(Playback { log: i, t: t0, playing: true, speed: 1.0, last: Instant::now(), next: 0 });
    seek(app, t0);
}

pub fn stop_playback(app: &mut XbApp) {
    if app.logs.play.take().is_some() {
        app.link = LinkView::default();
        app.history = Default::default();
    }
}

/// Move the playhead: the history becomes everything up to `t`.
pub fn seek(app: &mut XbApp, t: f64) {
    let Some(pb) = &mut app.logs.play else { return };
    let Some(l) = app.logs.logs.get(pb.log) else { return };
    let samples = &l.log.samples;
    pb.t = t;
    pb.next = samples.partition_point(|s| s.t <= t);
    app.history = Default::default();
    for s in &samples[..pb.next] {
        app.history.push(s.clone());
    }
}

/// Advance playback; called every frame.
pub fn tick(app: &mut XbApp) {
    let Some(pb) = &mut app.logs.play else { return };
    let now = Instant::now();
    let dt = now.duration_since(pb.last).as_secs_f64();
    pb.last = now;
    if !pb.playing {
        return;
    }
    let Some(l) = app.logs.logs.get(pb.log) else { return };
    let samples = &l.log.samples;
    pb.t += dt * pb.speed;
    while pb.next < samples.len() && samples[pb.next].t <= pb.t {
        app.history.push(samples[pb.next].clone());
        pb.next += 1;
    }
    if pb.next >= samples.len() {
        pb.playing = false;
    }
    app.egui_ctx().request_repaint_after(std::time::Duration::from_millis(50));
}

pub fn show(app: &mut XbApp, ui: &mut Ui) {
    let p = Palette::for_visuals(ui.visuals());
    ui.horizontal(|ui| {
        ui.heading("Logs");
        ui.add_space(12.0);
        if ui.button("Open log…").clicked() {
            app.logs.picker = Some(logs_dir());
        }
        if !app.logs.logs.is_empty() {
            let tune_ok = app.tune.is_some();
            let b = ui.add_enabled(tune_ok, egui::Button::new("Auto-tune from these logs…"));
            if !tune_ok {
                b.on_disabled_hover_text("Open the tune to correct in the Tune view first");
            } else if b.clicked() {
                app.view = View::Tune;
                crate::autotune_view::open(app);
            }
        }
    });
    if app.logs.logs.is_empty() {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No logs open").font(theme::strong_font(18.0)));
            ui.label(
                RichText::new("Record a log with ● Record while connected, then open it here.")
                    .color(p.muted),
            );
        });
        picker(app, ui);
        return;
    }
    ui.add_space(6.0);

    // Loaded logs.
    let mut close = None;
    let mut play = None;
    let mut share: Option<PathBuf> = None;
    let mut export = None;
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("logs").num_columns(5).spacing([14.0, 4.0]).show(ui, |ui| {
            for (i, l) in app.logs.logs.iter().enumerate() {
                if ui.selectable_label(app.logs.selected == i, l.log.name()).clicked() {
                    app.logs.selected = i;
                }
                ui.label(RichText::new(l.log.version.clone()).color(p.muted));
                ui.label(format!("{:.0} s · {:.1} Hz", l.log.duration(), l.log.rate()));
                let playing = app.logs.play.as_ref().is_some_and(|pb| pb.log == i);
                let can = app.link.state.is_none() || playing || app.logs.play.is_some();
                let b = ui.add_enabled(can && !playing, egui::Button::new("Play").small());
                if !can {
                    b.on_disabled_hover_text("Disconnect from the ECM to play a log");
                } else if b.clicked() {
                    play = Some(i);
                }
                if crate::remote_sync::host_paired(app)
                    && ui
                        .small_button("Send to tuner")
                        .on_hover_text("Send this log to your remote tuner")
                        .clicked()
                {
                    share = Some(l.log.path.clone());
                }
                if ui
                    .small_button("Export .msl")
                    .on_hover_text("Save a copy for MegaLogViewer in the Logs folder")
                    .clicked()
                {
                    export = Some(i);
                }
                if ui.small_button("Close").clicked() {
                    close = Some(i);
                }
                ui.end_row();
            }
        });
    });
    if let Some(i) = play {
        start_playback(app, i);
    }
    if let Some(p) = share {
        crate::remote_sync::share_log(app, &p);
    }
    if let Some(i) = export {
        export_msl(app, i);
    }
    if let Some(i) = close {
        if app.logs.play.as_ref().is_some_and(|pb| pb.log == i) {
            stop_playback(app);
        }
        app.logs.logs.remove(i);
        app.logs.selected = app.logs.selected.min(app.logs.logs.len().saturating_sub(1));
        if let Some(pb) = &mut app.logs.play
            && pb.log > i
        {
            pb.log -= 1;
        }
        app.logs.generation += 1;
        if app.logs.logs.is_empty() {
            return;
        }
    }
    ui.add_space(6.0);
    playback_bar(app, ui, &p);
    ui.add_space(6.0);

    let Some(l) = app.logs.logs.get(app.logs.selected) else { return };
    let log = l.log.clone();
    let (o2, anomalies) = (l.o2.clone(), l.anomalies.clone());
    egui::ScrollArea::vertical().show(ui, |ui| {
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(log.name()).font(theme::strong_font(16.0)));
            let mut line = format!(
                "{} samples over {:.0} s ({:.1} Hz) · {} link event(s)",
                log.samples.len(),
                log.duration(),
                log.rate(),
                log.events.len()
            );
            if let Some(s) = &log.started {
                line = format!("Started {s} · {line}");
            }
            ui.label(RichText::new(line).color(p.muted));
            if log.source != xbtune_core::logfile::LogSource::Xbtune || log.skipped > 0 {
                let mut s = log.source.describe().to_string();
                if log.skipped > 0 {
                    s.push_str(&format!(" · {} unreadable record(s) left out", log.skipped));
                }
                if log.source == xbtune_core::logfile::LogSource::Msl {
                    s.push_str(&format!(" · {} of the ECM's channels in the file", log.channels.len()));
                }
                ui.label(RichText::new(s).color(p.muted));
            }
            if let Some(n) = &log.note {
                ui.label(format!("Note: {n}"));
            }
            let v = |x: Option<f64>| x.map_or("–".into(), |v| format!("{v:.2} V"));
            ui.label(format!(
                "O2 (narrowband): {:.0}% rich · {:.0}% lean · {:.0}% sensor error · average rich {} · lean {}",
                o2.rich_pct,
                o2.lean_pct,
                o2.error_pct,
                v(o2.avg_rich_v),
                v(o2.avg_lean_v)
            ));
            if anomalies.is_empty() {
                ui.label(RichText::new("No anomalies").color(p.good));
            } else {
                egui::CollapsingHeader::new(
                    RichText::new(format!("{} anomalies (click one to jump there)", anomalies.len())).color(p.warn),
                )
                .id_salt(("anomalies", app.logs.selected))
                .show(ui, |ui| {
                    for a in &anomalies {
                        if ui.selectable_label(false, format!("{:>7.1} s  {}", a.t, a.text)).clicked() {
                            jump(app, a.t);
                        }
                    }
                });
            }
        });
        ui.add_space(6.0);
        charts(app, ui, &p, &log);
    });
    picker(app, ui);
}

/// Jump the playhead of the selected log to `t` (starting playback paused).
fn jump(app: &mut XbApp, t: f64) {
    let i = app.logs.selected;
    if app.logs.play.as_ref().is_none_or(|pb| pb.log != i) {
        if app.link.state.is_some() {
            return;
        }
        start_playback(app, i);
    }
    if let Some(pb) = &mut app.logs.play {
        pb.playing = false;
    }
    seek(app, t);
}

fn playback_bar(app: &mut XbApp, ui: &mut Ui, p: &Palette) {
    let Some(pb) = &app.logs.play else {
        ui.label(
            RichText::new("Play a log to watch it in the Dashboard, Live data, Charts and on the fuel maps.")
                .color(p.muted),
        );
        return;
    };
    let Some(l) = app.logs.logs.get(pb.log) else { return };
    let (t0, t1) = (l.log.samples.first().map_or(0.0, |s| s.t), l.log.samples.last().map_or(0.0, |s| s.t));
    let name = l.log.name();
    let (mut t, mut playing, mut speed) = (pb.t, pb.playing, pb.speed);
    let mut seek_to = None;
    let mut stop = false;
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Playing {name}")).strong());
            if ui.button(if playing { "Pause" } else { "Play" }).clicked() {
                if !playing && t >= t1 {
                    seek_to = Some(t0);
                }
                playing = !playing;
            }
            for s in [1.0, 2.0, 5.0, 10.0] {
                if ui.selectable_label(speed == s, format!("{s}×")).clicked() {
                    speed = s;
                }
            }
            if ui.button("Stop playback").clicked() {
                stop = true;
            }
            ui.label(RichText::new(format!("{:.1} / {:.1} s", t - t0, t1 - t0)).color(p.muted));
        });
        ui.spacing_mut().slider_width = (ui.available_width() - 20.0).max(100.0);
        let r = ui.add(egui::Slider::new(&mut t, t0..=t1.max(t0 + 0.001)).show_value(false));
        if r.changed() {
            seek_to = Some(t);
        }
        ui.horizontal(|ui| {
            for (v, label) in
                [(View::Dashboard, "Dashboard"), (View::Data, "Live data"), (View::Charts, "Charts")]
            {
                if ui.small_button(format!("Open {label}")).clicked() {
                    app.view = v;
                }
            }
            if ui.small_button("Open Tune (live point)").clicked() {
                app.view = View::Tune;
            }
        });
    });
    if stop {
        stop_playback(app);
        return;
    }
    if let Some(pb) = &mut app.logs.play {
        pb.playing = playing;
        pb.speed = speed;
    }
    if let Some(t) = seek_to {
        seek(app, t);
    }
}

fn charts(app: &mut XbApp, ui: &mut Ui, p: &Palette, log: &Log) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Charts").font(theme::strong_font(15.0)));
        ui.menu_button("Channels", |ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for c in log.channels.iter().filter(|c| c.kind == RtKind::Scalar) {
                    let mut on = app.logs.chart.contains(&c.name);
                    if ui.checkbox(&mut on, format!("{} ({})", c.label(), c.name)).changed() {
                        if on {
                            app.logs.chart.push(c.name.clone());
                        } else {
                            app.logs.chart.retain(|n| n != &c.name);
                        }
                    }
                }
            });
        });
        ui.label(RichText::new("Click a chart to move the playhead there.").color(p.muted));
    });
    let playhead = app
        .logs
        .play
        .as_ref()
        .filter(|pb| app.logs.logs.get(pb.log).is_some_and(|l| l.log.path == log.path))
        .map(|pb| pb.t);
    let group = ui.id().with(("log-time", log.path.clone()));
    let mut clicked_at = None;
    let names = app.logs.chart.clone();
    for (i, name) in names.iter().enumerate() {
        let Some(v) = log.channel(name) else { continue };
        let color = p.series[i % p.series.len()];
        let pts: Vec<[f64; 2]> = log.samples.iter().filter_map(|s| s.value(v).map(|y| [s.t, y])).collect();
        let at = playhead
            .and_then(|t| log.samples.iter().rev().find(|s| s.t <= t))
            .and_then(|s| s.value(v))
            .map(|y| format!("{} {}", format_value(v, y), units(v)))
            .unwrap_or_default();
        theme::card(ui).inner_margin(8).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(v.label()).strong().color(color));
                ui.label(at);
            });
            let r = Plot::new(("log-chart", name.clone(), log.path.clone()))
                .height(110.0)
                .link_axis(group, [true, false])
                .link_cursor(group, [true, false])
                .allow_scroll(false)
                .allow_boxed_zoom(false)
                .y_axis_min_width(48.0)
                .x_axis_formatter(|m, _| format!("{:.0}s", m.value))
                .show(ui, |plot| {
                    plot.line(Line::new(v.label(), PlotPoints::from(pts)).color(color).width(1.5));
                    if let Some(t) = playhead {
                        plot.vline(VLine::new("playhead", t).color(egui::Color32::WHITE).width(1.5));
                    }
                    plot.pointer_coordinate()
                });
            if r.response.clicked()
                && let Some(c) = r.inner
            {
                clicked_at = Some(c.x);
            }
        });
        ui.add_space(4.0);
    }
    if let Some(t) = clicked_at {
        jump(app, t);
    }
}

/// Pick log files to open (several, one click each).
fn picker(app: &mut XbApp, ui: &mut Ui) {
    let Some(dir) = app.logs.picker.clone() else { return };
    let p = Palette::for_visuals(ui.visuals());
    let mut dir_next = None;
    let mut open = None;
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("log-picker")).show(ui.ctx(), |ui| {
        ui.set_width(540.0);
        ui.label(RichText::new("Open logs").font(theme::strong_font(18.0)));
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Logs").clicked() {
                dir_next = Some(logs_dir());
            }
            if ui.small_button("Home").clicked()
                && let Some(h) = xbtune_core::env::real_home()
            {
                dir_next = Some(h);
            }
            if ui.small_button("Up").clicked()
                && let Some(parent) = dir.parent()
            {
                dir_next = Some(parent.to_path_buf());
            }
            ui.label(RichText::new(dir.display().to_string()).small().color(p.muted));
        });
        let mut entries: Vec<(bool, PathBuf)> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
                    .filter(|p| {
                        p.is_dir()
                            || p.extension().is_some_and(|e| {
                                ["csv", "bin", "msl"].iter().any(|x| e.eq_ignore_ascii_case(x))
                            })
                    })
                    .map(|p| (p.is_dir(), p))
                    .collect()
            })
            .unwrap_or_default();
        // Folders first, then the newest logs first.
        entries.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            if entries.is_empty() {
                ui.label(
                    RichText::new("No folders or logs (.csv, EcmDroid .bin, .msl) here.").color(p.muted),
                );
            }
            for (is_dir, path) in &entries {
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let loaded = app.logs.logs.iter().any(|l| &l.log.path == path);
                let label = if *is_dir {
                    format!("{name}/")
                } else if loaded {
                    format!("{name}  (open)")
                } else {
                    name
                };
                if ui.selectable_label(false, label).clicked() {
                    if *is_dir {
                        dir_next = Some(path.clone());
                    } else {
                        open = Some(path.clone());
                    }
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Click logs to open them; open several to tune from all of them.")
                    .color(p.muted),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Done").clicked() {
                    close = true;
                }
            });
        });
    });
    if let Some(path) = open {
        add(app, &path);
    }
    if close || modal.should_close() {
        app.logs.picker = None;
    } else if let Some(d) = dir_next {
        app.logs.picker = Some(d);
    }
}
