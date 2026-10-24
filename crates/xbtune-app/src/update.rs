// SPDX-License-Identifier: GPL-3.0-or-later
//! "A new version is available": outside the snap (snapd updates that),
//! XBTune asks GitHub once a day for the latest stable release and shows a
//! banner with a link to its page, where the download is. It never
//! downloads or installs anything itself. Owner's call (2026-10-24): a
//! link to the release page, not winget.

use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::XbApp;
use crate::theme::{self, Palette};

const LATEST: &str = "https://api.github.com/repos/kz6fittycent/XBTune/releases/latest";
/// All releases, for "see what's new" when the API gives no page.
pub const RELEASES_PAGE: &str = "https://github.com/kz6fittycent/XBTune/releases";
/// At most one check a day.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// A stable release on GitHub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// "0.1.2" (no leading "v").
    pub version: String,
    /// The release's page (notes and the downloads).
    pub url: String,
}

/// Background check and what it found.
#[derive(Default)]
pub struct Updater {
    rx: Option<mpsc::Receiver<Result<Release, String>>>,
    /// Closed with × for this run.
    hidden: bool,
}

/// True where XBTune checks for updates itself: not in the snap.
pub fn applies() -> bool {
    !xbtune_core::env::in_snap()
}

/// `"v0.1.10"` -> `[0, 1, 10]`; anything after the numbers is ignored.
fn parse(v: &str) -> Option<[u64; 3]> {
    let core = v.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+', ' ']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let out = [parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0)];
    Some(out)
}

/// True if `latest` is a newer version than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse(latest), parse(current)), (Some(l), Some(c)) if l > c)
}

/// The fields we use from GitHub's "latest release" answer.
fn parse_latest(json: &str) -> Result<Release, String> {
    #[derive(Deserialize)]
    struct Gh {
        tag_name: String,
        html_url: Option<String>,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }
    let gh: Gh = serde_json::from_str(json).map_err(|e| format!("unexpected answer: {e}"))?;
    if gh.draft || gh.prerelease {
        return Err("latest release is not a stable one".into());
    }
    let version = gh.tag_name.trim_start_matches(['v', 'V']).to_string();
    if parse(&version).is_none() {
        return Err(format!("tag '{}' is not a version", gh.tag_name));
    }
    Ok(Release { version, url: gh.html_url.unwrap_or_else(|| RELEASES_PAGE.into()) })
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// At start: check if it's on and a day has passed since the last check.
pub fn start(app: &mut XbApp) {
    if !applies() || !app.settings.update_check || app.is_screenshot() {
        return;
    }
    if now_secs().saturating_sub(app.settings.update_checked_at) < EVERY.as_secs() {
        return;
    }
    check_now(app);
}

/// Ask GitHub now (Settings -> "Check now", or the daily check).
pub fn check_now(app: &mut XbApp) {
    let (tx, rx) = mpsc::channel();
    let ctx = app.egui_ctx().clone();
    let spawned = std::thread::Builder::new().name("xbtune-update".into()).spawn(move || {
        let r = crate::report::agent()
            .get(LATEST)
            .header("Accept", "application/vnd.github+json")
            .call()
            .map_err(|e| e.to_string())
            .and_then(|mut resp| match resp.status().as_u16() {
                200 => resp.body_mut().read_to_string().map_err(|e| e.to_string()),
                404 => Err("no stable release yet".into()),
                s => Err(format!("HTTP {s}")),
            })
            .and_then(|body| parse_latest(&body));
        let _ = tx.send(r);
        ctx.request_repaint();
    });
    if spawned.is_ok() {
        app.update.rx = Some(rx);
    }
}

/// Collect a finished check. Called every frame.
pub fn poll(app: &mut XbApp) {
    let Some(rx) = &app.update.rx else { return };
    match rx.try_recv() {
        Ok(r) => {
            app.update.rx = None;
            app.settings.update_checked_at = now_secs();
            match r {
                Ok(rel) => {
                    tracing::info!(latest = %rel.version, "update check");
                    app.settings.update_latest = Some(rel);
                }
                // Silent: logged only.
                Err(e) => tracing::info!("update check failed: {e}"),
            }
            app.save_settings();
        }
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => app.update.rx = None,
    }
}

pub fn checking(app: &XbApp) -> bool {
    app.update.rx.is_some()
}

/// The release to tell the user about, if any.
pub fn offer(app: &XbApp) -> Option<&Release> {
    let rel = app.settings.update_latest.as_ref()?;
    (applies()
        && app.settings.update_check
        && is_newer(&rel.version, env!("CARGO_PKG_VERSION"))
        && app.settings.update_skip != rel.version)
        .then_some(rel)
}

/// The banner above every view.
pub fn banner(app: &mut XbApp, ui: &mut Ui) {
    if app.update.hidden {
        return;
    }
    let Some(rel) = offer(app).cloned() else { return };
    let p = Palette::for_visuals(ui.visuals());
    theme::card(ui).stroke(egui::Stroke::new(1.0, p.accent)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("XBTune {} is available", rel.version)).font(theme::strong_font(15.0)),
            );
            ui.label(RichText::new(format!("You have {}.", env!("CARGO_PKG_VERSION"))).color(p.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button(crate::icons::CLOSE)
                    .on_hover_text("Hide until XBTune starts again")
                    .clicked()
                {
                    app.update.hidden = true;
                }
                if ui.button("Skip this version").clicked() {
                    app.settings.update_skip = rel.version.clone();
                    app.save_settings();
                }
                if ui
                    .add(theme::primary(crate::icons::with(crate::icons::DOWNLOAD, "Download")))
                    .on_hover_text(&rel.url)
                    .clicked()
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&rel.url));
                }
            });
        });
    });
    ui.add_space(8.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("v0.1.2", "0.1.1"));
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(!is_newer("0.1.0", "0.1.1"));
        assert!(!is_newer("nightly", "0.1.1"));
        // The snap's "0.1.1+git..." and a "-windows-test" suffix compare by numbers.
        assert!(!is_newer("0.1.1-windows-test.1", "0.1.1+git7c31d49"));
    }

    #[test]
    fn reads_githubs_answer() {
        let r = parse_latest(
            r#"{"tag_name":"v0.1.2","html_url":"https://github.com/kz6fittycent/XBTune/releases/tag/v0.1.2",
                "draft":false,"prerelease":false,"assets":[]}"#,
        )
        .unwrap();
        assert_eq!(r.version, "0.1.2");
        assert!(r.url.ends_with("/releases/tag/v0.1.2"));
        assert!(parse_latest(r#"{"tag_name":"v0.2.0","prerelease":true}"#).is_err());
        assert!(parse_latest(r#"{"tag_name":"latest"}"#).is_err());
        assert_eq!(parse_latest(r#"{"tag_name":"0.3.0"}"#).unwrap().url, RELEASES_PAGE);
    }
}
