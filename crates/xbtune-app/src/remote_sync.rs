// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote tuning, app side (docs/REMOTE.md stages R2-R5). The host shares
//! its live data, the open tune and logs; the tuner (guest) proposes edits
//! and asks for burns, tests, resets and recordings. Everything that
//! touches the ECM still runs in the host's own session through the
//! host's normal dialogs (KICKOFF §6); a tuner-started action needs the
//! host's permission and then waits out a countdown the host can stop
//! (REMOTE.md 2.1). `remote_view.rs` draws all of it.

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use sha2::{Digest, Sha256};
use xbtune_core::image::{Image, Meta, diff_ranges};
use xbtune_core::live::{Sample, channels};
use xbtune_core::session::{Command as SessionCommand, Identity, LinkState};
use xbtune_proto::Function;
use xbtune_remote::msg::{self, B64, FrameMsg, ImageMsg, Msg, PatchMsg, ReqState, RequestKind};
use xbtune_remote::session::Command;

use crate::app::{LinkView, View, XbApp};

/// How long the host has to stop a tuner-started action.
pub const COUNTDOWN: Duration = Duration::from_secs(10);
const FRAME_BATCH: Duration = Duration::from_millis(250);
const INFO_EVERY: Duration = Duration::from_secs(1);

/// What the host lets the tuner do directly (all off at the start of every
/// session, REMOTE.md 2.1), and whether the tuner may save a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Perms {
    pub edit: bool,
    pub burn: bool,
    pub tests: bool,
    pub allow_save: bool,
}

impl Default for Perms {
    fn default() -> Self {
        Self { edit: false, burn: false, tests: false, allow_save: true }
    }
}

pub struct Proposal {
    pub id: u64,
    pub label: String,
    pub patches: Vec<(usize, Vec<u8>)>,
    /// What changes, in human units.
    pub lines: Vec<String>,
}

pub struct HostRequest {
    pub id: u64,
    pub kind: RequestKind,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CountAction {
    Function(Function),
    Burn,
}

pub struct Countdown {
    pub id: u64,
    pub what: String,
    pub until: Instant,
    pub action: CountAction,
    note: String,
}

/// The request the host is acting on.
pub struct Active {
    pub id: u64,
    pub kind: RequestKind,
    pub direct: bool,
    /// The ECM action actually started (so closing a dialog after this
    /// isn't a decline).
    started: bool,
    note: String,
    /// Direct burn: the review went to the tuner.
    reviewed: bool,
}

struct Shared {
    share: u32,
    seq: u64,
    /// The tune's bytes as opened: a new tune in the Tune view = new share.
    key: Vec<u8>,
    snapshot: Vec<u8>,
    journal: Option<std::fs::File>,
}

#[derive(Default)]
pub struct HostSide {
    pub perms: Perms,
    /// When the Burn permission was last switched on (for burn.txt).
    burn_granted: Option<SystemTime>,
    frames: Vec<FrameMsg>,
    last_flush: Option<Instant>,
    last_info: Option<Instant>,
    sent_identity: Option<Option<String>>,
    sent_perms: Option<Perms>,
    shared: Option<Shared>,
    next_share: u32,
    pub proposals: Vec<Proposal>,
    pub requests: VecDeque<HostRequest>,
    pub active: Option<Active>,
    pub countdown: Option<Countdown>,
    next_log: u64,
}

pub struct GuestRequest {
    pub id: u64,
    pub what: String,
    pub state: Option<ReqState>,
    pub text: String,
}

struct IncomingLog {
    name: String,
    size: u64,
    sha256: String,
    data: Vec<u8>,
    next: u32,
}

type Patches = Vec<(usize, Vec<u8>)>;

#[derive(Default)]
pub struct GuestSide {
    pub version: Option<String>,
    pub host_recording: bool,
    share: Option<u32>,
    image: Option<ImageMsg>,
    /// Our Tune's opened bytes while it's the shared tune.
    key: Vec<u8>,
    /// The host's working copy as we last heard it.
    truth: Vec<u8>,
    /// Our edits the host hasn't confirmed yet.
    pending: Vec<(u64, String, Patches)>,
    snapshot: Vec<u8>,
    pub following: bool,
    pub allow_save: bool,
    pub tune_name: String,
    pub perms: Perms,
    next_id: u64,
    pub requests: Vec<GuestRequest>,
    /// A direct burn's review: (request, lines, "I've checked" ticked).
    pub review: Option<(u64, Vec<String>, bool)>,
    /// The "burn / AFV reset" request dialog: (AFV?, note).
    pub ask_burn: Option<(bool, String)>,
    logs: HashMap<u64, IncomingLog>,
}

fn send(app: &XbApp, m: Msg) {
    if let Some(s) = app.remote.session() {
        s.send(Command::Send(Box::new(m)));
    }
}

fn peer(app: &XbApp) -> String {
    app.remote.peer().unwrap_or_else(|| "The other side".into())
}

pub fn is_guest(app: &XbApp) -> bool {
    app.remote.active() && !app.remote.hosting()
}

pub fn host_paired(app: &XbApp) -> bool {
    app.remote.active() && app.remote.hosting() && app.remote.paired()
}

/// The tuner is viewing a host's bike (its live data and buttons are the host's).
pub fn guest_live(app: &XbApp) -> bool {
    is_guest(app) && app.remote.guest.version.is_some()
}

fn patches_msg(p: &[(usize, Vec<u8>)]) -> Vec<PatchMsg> {
    p.iter().map(|(o, b)| PatchMsg { offset: *o, bytes: B64(b.clone()) }).collect()
}

fn diff(a: &[u8], b: &[u8]) -> Patches {
    diff_ranges(a, b).into_iter().map(|(s, e)| (s, b[s..e].to_vec())).collect()
}

fn apply(bytes: &mut [u8], patches: &[(usize, Vec<u8>)]) -> bool {
    for (o, b) in patches {
        let Some(end) = o.checked_add(b.len()).filter(|e| *e <= bytes.len()) else { return false };
        bytes[*o..end].copy_from_slice(b);
    }
    true
}

pub fn describe(kind: &RequestKind) -> String {
    match kind {
        RequestKind::Burn => "burn the tune to the ECM".into(),
        RequestKind::AfvReset => "reset AFV".into(),
        RequestKind::Function { name } => match xbtune_core::diag::function_by_name(name) {
            Some(Function::ClearCodes) => "clear the stored trouble codes".into(),
            Some(Function::TpsReset) => "reset the TPS".into(),
            Some(f) => format!("run the {} test", f.label().to_lowercase()),
            None => format!("run '{name}'"),
        },
        RequestKind::Record { start: true } => "start recording a log".into(),
        RequestKind::Record { start: false } => "stop recording".into(),
    }
}

// ---------------------------------------------------------------- events

/// Paired (first time or after a drop): (re)send our side's state.
pub fn on_paired(app: &mut XbApp) {
    if app.remote.hosting() {
        let h = &mut app.remote.host;
        h.sent_identity = None;
        h.sent_perms = None;
        h.last_info = None;
        // Resend the whole tune: cheaper than replaying edits, and the
        // tuner's unconfirmed proposals are absolute bytes (resent, idempotent).
        if let Some(s) = &mut h.shared {
            s.key.clear();
        }
    } else {
        // The tuner's own bike link is off while joined (REMOTE.md 2).
        crate::logs_view::stop_playback(app);
        if app.session_active() {
            app.stop();
        }
    }
}

/// The other side dropped out or the session ended.
pub fn on_gone(app: &mut XbApp, ended: bool) {
    if app.remote.hosting() {
        let h = &mut app.remote.host;
        if let Some(c) = h.countdown.take() {
            app.say(format!("Stopped: {} (the tuner dropped out)", c.what), true);
        }
        if ended {
            app.remote.host = HostSide::default();
        }
        return;
    }
    if app.link.remote.is_some() {
        if ended {
            app.link = LinkView::default();
            app.history = Default::default();
        } else {
            app.link.state = Some(LinkState::Reconnecting {
                reason: "the bike owner dropped out".into(),
                delay: Duration::from_secs(1),
            });
        }
    }
    if ended {
        let blocked = save_blocked(app);
        app.remote.guest = GuestSide::default();
        if blocked {
            app.tune = None;
            app.say("The shared tune was closed: the bike owner didn't allow saving a copy.", false);
        }
    }
}

pub fn on_message(app: &mut XbApp, m: Msg) {
    if app.remote.hosting() { host_message(app, m) } else { guest_message(app, m) }
}

/// Every frame, while a session runs.
pub fn tick(app: &mut XbApp) {
    if !app.remote.paired() {
        return;
    }
    if app.remote.hosting() {
        host_tick(app);
    } else {
        guest_tick(app);
    }
}

// ---------------------------------------------------------------- host

/// A live sample from the host's own session.
pub fn on_sample(app: &mut XbApp, s: &Sample) {
    if host_paired(app) {
        let h = &mut app.remote.host;
        if h.frames.len() < 64 {
            h.frames.push(FrameMsg { t: s.t, raw: B64(s.frame.clone()) });
        }
    }
}

fn host_tick(app: &mut XbApp) {
    let now = Instant::now();
    // R2: live data.
    let version = if app.link.live() { app.link.identity.as_ref().map(|i| i.version.clone()) } else { None };
    if app.remote.host.sent_identity.as_ref() != Some(&version) {
        app.remote.host.sent_identity = Some(version.clone());
        send(app, Msg::Identity { version });
    }
    if app.remote.host.last_flush.is_none_or(|t| now - t >= FRAME_BATCH) {
        app.remote.host.last_flush = Some(now);
        let frames = std::mem::take(&mut app.remote.host.frames);
        if !frames.is_empty() {
            send(app, Msg::Frames { frames });
        }
    }
    if app.remote.host.last_info.is_none_or(|t| now - t >= INFO_EVERY) {
        app.remote.host.last_info = Some(now);
        let state = match &app.link.state {
            Some(LinkState::Live) => "live",
            Some(LinkState::Connecting { .. }) => "connecting",
            Some(LinkState::Reconnecting { .. }) => "reconnecting",
            _ => "stopped",
        };
        let error_pct = app.link.stats.as_ref().map_or(0.0, |s| s.recent_error_rate() * 100.0);
        send(
            app,
            Msg::LinkInfo {
                state: state.into(),
                hz: app.link.hz,
                error_pct,
                recording: app.link.log.is_some(),
            },
        );
    }
    // Burn needs the host's own write unlock.
    if app.remote.host.perms.burn && !app.writes_permitted() {
        app.remote.host.perms.burn = false;
    }
    let perms = app.remote.host.perms;
    if app.remote.host.sent_perms != Some(perms) {
        app.remote.host.sent_perms = Some(perms);
        send(app, Msg::Permissions { edit: perms.edit, burn: perms.burn, tests: perms.tests });
        cancel_if_not_allowed(app);
    }
    host_tune(app);
    host_active(app);
    if let Some(c) = &app.remote.host.countdown
        && now >= c.until
    {
        let c = app.remote.host.countdown.take().expect("checked");
        run_countdown(app, c);
    }
}

/// Switch a permission (host's Remote panel).
pub fn set_perms(app: &mut XbApp, p: Perms) {
    if p.burn && !app.remote.host.perms.burn {
        app.remote.host.burn_granted = Some(SystemTime::now());
    }
    tracing::info!(edit = p.edit, burn = p.burn, tests = p.tests, save = p.allow_save, "remote permissions");
    let save_changed = p.allow_save != app.remote.host.perms.allow_save;
    app.remote.host.perms = p;
    if save_changed && let Some(s) = &mut app.remote.host.shared {
        s.key.clear(); // resend the tune with the new allow_save
    }
}

fn cancel_if_not_allowed(app: &mut XbApp) {
    let perms = app.remote.host.perms;
    let Some(c) = &app.remote.host.countdown else { return };
    let allowed = match c.action {
        CountAction::Function(_) => perms.tests,
        CountAction::Burn => perms.burn,
    };
    if !allowed {
        let c = app.remote.host.countdown.take().expect("checked");
        state(app, c.id, ReqState::Stopped, "permission withdrawn");
        if c.action == CountAction::Burn {
            app.burn = crate::burn_view::BurnUi::Closed;
            app.remote.host.active = None;
        }
    }
}

fn state(app: &XbApp, id: u64, s: ReqState, text: impl Into<String>) {
    send(app, Msg::RequestState { id, state: s, text: text.into() });
}

fn journal_dir(app: &XbApp) -> Option<PathBuf> {
    let room = app.remote.code().map(|c| c.room).unwrap_or_default();
    let base = xbtune_core::env::Dirs::detect()?.remote();
    Some(xbtune_core::time::unique_path(
        &base,
        &format!("{}_room{room}", xbtune_core::time::file_stamp(SystemTime::now())),
        "",
    ))
}

/// Journal: `start.xbt` and one JSON line per applied edit, flushed before
/// the edit is sent (REMOTE.md 4.7).
fn start_journal(app: &XbApp, img: &Image) -> Option<std::fs::File> {
    let dir = journal_dir(app)?;
    let r = (|| -> std::io::Result<std::fs::File> {
        std::fs::create_dir_all(&dir)?;
        img.save(&dir.join("start.xbt"), &app.catalog).map_err(std::io::Error::other)?;
        std::fs::File::create(dir.join("journal.jsonl"))
    })();
    match r {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!(error = %e, dir = %dir.display(), "remote journal not written");
            None
        }
    }
}

fn journal(file: &mut Option<std::fs::File>, seq: u64, by: &str, label: &str, patches: &[(usize, Vec<u8>)]) {
    let Some(f) = file else { return };
    let line = serde_json::json!({
        "seq": seq, "by": by, "label": label,
        "time": xbtune_core::time::rfc3339(SystemTime::now()),
        "patches": patches.iter().map(|(o, b)| serde_json::json!({"offset": o, "bytes": msg::b64_encode(b)})).collect::<Vec<_>>(),
    });
    if writeln!(f, "{line}").and_then(|_| f.sync_data()).is_err() {
        tracing::warn!("remote journal write failed");
    }
}

/// Rebuild a journal folder's tune: `start.xbt` plus every edit.
pub fn rebuild_journal(
    dir: &std::path::Path,
    catalog: &xbtune_defs::Catalog,
) -> Result<(Image, usize), String> {
    let mut img = Image::load(&dir.join("start.xbt"), None, catalog).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(dir.join("journal.jsonl")).map_err(|e| e.to_string())?;
    let mut n = 0;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { break }; // torn last line
        for p in v["patches"].as_array().into_iter().flatten() {
            let (Some(o), Some(b)) = (p["offset"].as_u64(), p["bytes"].as_str()) else { continue };
            let bytes = msg::b64_decode(b)?;
            if !apply(&mut img.bytes, &[(o as usize, bytes)]) {
                return Err("journal edit outside the image".into());
            }
        }
        n += 1;
    }
    Ok((img, n))
}

/// The most recent journal folder, if any.
pub fn last_journal() -> Option<PathBuf> {
    let base = xbtune_core::env::Dirs::detect()?.remote();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| std::fs::metadata(p.join("journal.jsonl")).is_ok_and(|m| m.len() > 0))
        .collect();
    dirs.sort();
    dirs.pop()
}

fn host_tune(app: &mut XbApp) {
    let Some(t) = &app.tune else {
        if app.remote.host.shared.take().is_some() {
            send(app, Msg::TuneClosed);
        }
        return;
    };
    let same = app.remote.host.shared.as_ref().is_some_and(|s| s.key == t.opened_bytes());
    if !same {
        let resend = app.remote.host.shared.as_ref().is_some_and(|s| s.key.is_empty());
        let (share, seq, journal) = match app.remote.host.shared.take() {
            // Resend after a reconnect or a save-permission change: same share.
            Some(s) if resend => (s.share, s.seq, s.journal),
            _ => {
                app.remote.host.next_share += 1;
                (app.remote.host.next_share, 0, start_journal(app, &t.image))
            }
        };
        let name = t
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unsaved tune".into());
        let image = ImageMsg {
            ecm_id: t.image.ecm_id.clone(),
            ecm_version: t.image.ecm_version.clone(),
            page_zero: t.image.page_zero,
            bytes: B64(t.image.bytes.clone()),
        };
        let allow_save = app.remote.host.perms.allow_save;
        app.remote.host.shared = Some(Shared {
            share,
            seq,
            key: t.opened_bytes().to_vec(),
            snapshot: t.image.bytes.clone(),
            journal,
        });
        if !resend {
            app.remote.host.proposals.clear();
        }
        send(app, Msg::Tune { share, seq, name, image, allow_save });
        return;
    }
    // The host's own edits (typing, undo, paste, auto-tune...).
    let s = app.remote.host.shared.as_mut().expect("same share");
    if t.image.bytes != s.snapshot {
        let patches = diff(&s.snapshot, &t.image.bytes);
        s.snapshot = t.image.bytes.clone();
        s.seq += 1;
        let label = t.undo_label().unwrap_or("edit").to_string();
        let name = app.settings.remote_name.clone();
        let by = if name.trim().is_empty() { "Bike owner".to_string() } else { name };
        journal(&mut s.journal, s.seq, &by, &label, &patches);
        let m = Msg::Applied {
            share: s.share,
            seq: s.seq,
            by,
            proposal: None,
            label,
            patches: patches_msg(&patches),
        };
        send(app, m);
    }
}

/// Apply a tuner's proposal to the host's working copy (accepted, or
/// under the Edit permission).
fn host_apply(app: &mut XbApp, id: u64, label: &str, patches: &[(usize, Vec<u8>)]) -> Result<(), String> {
    let by = peer(app);
    let t = app.tune.as_mut().ok_or("no tune open")?;
    t.apply_patches(format!("{by}: {label}"), patches).map_err(|e| e.to_string())?;
    let s = app.remote.host.shared.as_mut().ok_or("no tune shared")?;
    s.snapshot = t.image.bytes.clone();
    s.seq += 1;
    journal(&mut s.journal, s.seq, &by, label, patches);
    let m = Msg::Applied {
        share: s.share,
        seq: s.seq,
        by,
        proposal: Some(id),
        label: label.into(),
        patches: patches_msg(patches),
    };
    send(app, m);
    Ok(())
}

pub fn accept(app: &mut XbApp, id: u64) {
    let Some(i) = app.remote.host.proposals.iter().position(|p| p.id == id) else { return };
    let p = app.remote.host.proposals.remove(i);
    if let Err(e) = host_apply(app, p.id, &p.label, &p.patches) {
        send(app, Msg::Rejected { id, reason: e.clone() });
        app.say(format!("Couldn't apply the edit: {e}"), true);
    }
}

pub fn reject(app: &mut XbApp, id: u64) {
    if let Some(i) = app.remote.host.proposals.iter().position(|p| p.id == id) {
        app.remote.host.proposals.remove(i);
        send(app, Msg::Rejected { id, reason: format!("{} turned it down", host_name(app)) });
    }
}

fn host_name(app: &XbApp) -> String {
    let n = app.settings.remote_name.trim();
    if n.is_empty() { "The bike owner".into() } else { n.into() }
}

fn proposal_lines(app: &XbApp, patches: &[(usize, Vec<u8>)]) -> Vec<String> {
    let Some(t) = &app.tune else { return Vec::new() };
    let mut after = t.image.bytes.clone();
    if !apply(&mut after, patches) {
        return vec!["(outside the tune)".into()];
    }
    xbtune_core::burn::changes(&t.def, &t.image.bytes, &after, None, t.image.page_zero)
        .into_iter()
        .map(|c| format!("{}: {} -> {}", c.label, c.ecm, c.tune))
        .collect()
}

fn host_message(app: &mut XbApp, m: Msg) {
    match m {
        Msg::Propose { share, id, label, patches } => {
            let label = msg::clean_line(&label, 80);
            let patches: Patches = patches.into_iter().map(|p| (p.offset, p.bytes.0)).collect();
            let current = app.remote.host.shared.as_ref().map(|s| s.share);
            let len = app.tune.as_ref().map_or(0, |t| t.image.bytes.len());
            let total: usize = patches.iter().map(|(_, b)| b.len()).sum();
            if current != Some(share) {
                send(
                    app,
                    Msg::Rejected { id, reason: "the bike owner has a different tune open now".into() },
                );
            } else if total > len
                || patches.iter().any(|(o, b)| o.checked_add(b.len()).is_none_or(|e| e > len))
            {
                send(app, Msg::Rejected { id, reason: "edit outside the tune".into() });
            } else if app.remote.host.perms.edit {
                if let Err(e) = host_apply(app, id, &label, &patches) {
                    send(app, Msg::Rejected { id, reason: e });
                }
            } else if !app.remote.host.proposals.iter().any(|p| p.id == id) {
                let lines = proposal_lines(app, &patches);
                app.say(format!("{} proposes an edit: {label}", peer(app)), false);
                app.remote.host.proposals.push(Proposal { id, label, patches, lines });
            }
        }
        Msg::Request { id, kind, direct, note } => {
            host_request(app, id, kind, direct, msg::clean_line(&note, msg::MAX_NOTE))
        }
        Msg::Confirm { id } => {
            let ok = app.remote.host.active.as_ref().is_some_and(|a| a.id == id && a.direct && a.reviewed);
            if ok && app.remote.host.perms.burn && matches!(app.burn, crate::burn_view::BurnUi::Review { .. })
            {
                let n = crate::burn_view::chosen_count(app);
                let note = app.remote.host.active.as_ref().map(|a| a.note.clone()).unwrap_or_default();
                let afv = app.remote.host.active.as_ref().is_some_and(|a| a.kind == RequestKind::AfvReset);
                let what =
                    if afv { "reset AFV".to_string() } else { format!("burn {n} change(s) to the ECM") };
                tracing::info!(id, %what, "remote countdown started");
                app.remote.host.countdown = Some(Countdown {
                    id,
                    what,
                    until: Instant::now() + COUNTDOWN,
                    action: CountAction::Burn,
                    note,
                });
                state(app, id, ReqState::Countdown, "");
            } else if ok {
                state(app, id, ReqState::Failed, "the burn is no longer ready or allowed");
            }
        }
        Msg::Cancel { id } => {
            app.remote.host.requests.retain(|r| r.id != id);
            if app.remote.host.countdown.as_ref().is_some_and(|c| c.id == id) {
                app.remote.host.countdown = None;
            }
            if let Some(a) = &app.remote.host.active
                && a.id == id
                && !a.started
            {
                app.remote.host.active = None;
                if matches!(
                    app.burn,
                    crate::burn_view::BurnUi::Review { .. } | crate::burn_view::BurnUi::Preparing { .. }
                ) {
                    app.burn = crate::burn_view::BurnUi::Closed;
                }
            }
            state(app, id, ReqState::Stopped, "cancelled by the tuner");
        }
        _ => {}
    }
}

fn host_request(app: &mut XbApp, id: u64, kind: RequestKind, direct: bool, note: String) {
    let function = match &kind {
        RequestKind::Function { name } => match xbtune_core::diag::function_by_name(name) {
            Some(f) => Some(f),
            None => return state(app, id, ReqState::Failed, format!("unknown function '{name}'")),
        },
        _ => None,
    };
    let what = describe(&kind);
    let p = app.remote.host.perms;
    let allowed = direct
        && match &kind {
            RequestKind::Function { .. } => p.tests,
            RequestKind::Burn | RequestKind::AfvReset => p.burn && app.writes_permitted(),
            RequestKind::Record { .. } => false,
        };
    if !allowed {
        tracing::info!(id, %what, "remote request queued for review");
        app.say(format!("{} asks you to {what}", peer(app)), false);
        app.remote.host.requests.push_back(HostRequest { id, kind, note });
        let text = if direct { "no permission for that now; sent for review" } else { "" };
        return state(app, id, ReqState::Shown, text);
    }
    let busy = app.remote.host.countdown.is_some()
        || app.remote.host.active.is_some()
        || app.diag.running.is_some()
        || app.burn.is_open();
    if busy {
        return state(app, id, ReqState::Failed, "the bike owner's XBTune is busy with something else");
    }
    if !app.link.live() {
        return state(app, id, ReqState::Failed, "the bike owner isn't connected to the bike");
    }
    tracing::info!(id, %what, "remote direct request");
    match (function, &kind) {
        (Some(f), _) => {
            tracing::info!(id, %what, "remote countdown started");
            app.remote.host.countdown = Some(Countdown {
                id,
                what,
                until: Instant::now() + COUNTDOWN,
                action: CountAction::Function(f),
                note,
            });
            state(app, id, ReqState::Countdown, "");
        }
        (None, RequestKind::Burn | RequestKind::AfvReset) => {
            let mode = match burn_mode(app, &kind) {
                Ok(m) => m,
                Err(e) => return state(app, id, ReqState::Failed, e),
            };
            app.remote.host.active =
                Some(Active { id, kind, direct: true, started: false, note, reviewed: false });
            crate::burn_view::start(app, mode);
            state(app, id, ReqState::Preparing, "making a verified backup");
        }
        _ => {}
    }
}

fn burn_mode(app: &XbApp, kind: &RequestKind) -> Result<crate::burn_view::Mode, String> {
    match kind {
        RequestKind::AfvReset => Ok(crate::burn_view::Mode::Afv),
        _ => {
            let t = app.tune.as_ref().ok_or("no tune is open on the bike owner's side")?;
            Ok(crate::burn_view::Mode::Tune {
                tune: Box::new(t.image.clone()),
                opened: t.opened_bytes().to_vec(),
            })
        }
    }
}

/// Host: review a queued request in the normal dialog (or allow a recording).
pub fn review(app: &mut XbApp, id: u64) {
    let Some(i) = app.remote.host.requests.iter().position(|r| r.id == id) else { return };
    let r = app.remote.host.requests.remove(i).expect("found");
    match &r.kind {
        RequestKind::Record { start } => {
            if *start {
                app.start_log();
            } else {
                app.send(SessionCommand::StopLog);
            }
            state(app, id, ReqState::Done, if *start { "recording" } else { "recording stopped" });
        }
        RequestKind::Function { name } => {
            let Some(f) = xbtune_core::diag::function_by_name(name) else { return };
            app.diag.confirm = Some(f);
            app.diag.safe = false;
            app.view = if f == Function::TpsReset { View::Resets } else { View::Diagnostics };
            app.remote.host.active = Some(Active {
                id,
                kind: r.kind.clone(),
                direct: false,
                started: false,
                note: r.note,
                reviewed: false,
            });
        }
        RequestKind::Burn | RequestKind::AfvReset => match burn_mode(app, &r.kind) {
            Ok(mode) => {
                app.view = if r.kind == RequestKind::AfvReset { View::Resets } else { View::Tune };
                app.remote.host.active = Some(Active {
                    id,
                    kind: r.kind.clone(),
                    direct: false,
                    started: false,
                    note: r.note,
                    reviewed: false,
                });
                crate::burn_view::start(app, mode);
            }
            Err(e) => state(app, id, ReqState::Failed, e),
        },
    }
}

pub fn decline(app: &mut XbApp, id: u64) {
    app.remote.host.requests.retain(|r| r.id != id);
    state(app, id, ReqState::Declined, format!("{} said no", host_name(app)));
}

pub fn stop_countdown(app: &mut XbApp) {
    if let Some(c) = app.remote.host.countdown.take() {
        tracing::info!(id = c.id, what = %c.what, "remote countdown stopped by the host");
        state(app, c.id, ReqState::Stopped, format!("{} stopped it", host_name(app)));
        if c.action == CountAction::Burn {
            app.burn = crate::burn_view::BurnUi::Closed;
            app.remote.host.active = None;
        }
    }
}

fn run_countdown(app: &mut XbApp, c: Countdown) {
    tracing::info!(id = c.id, what = %c.what, "remote countdown over: running");
    match c.action {
        CountAction::Function(f) => {
            if app.diag.running.is_some() || !app.link.live() {
                return state(app, c.id, ReqState::Failed, "the link or the ECM is busy");
            }
            app.diag.running = Some((f, None));
            app.diag.last = None;
            app.send(SessionCommand::Function(f));
            app.remote.host.active = Some(Active {
                id: c.id,
                kind: RequestKind::Function { name: f.label().into() },
                direct: true,
                started: true,
                note: c.note,
                reviewed: false,
            });
            state(app, c.id, ReqState::Running, "");
        }
        CountAction::Burn => {
            let granted = app.remote.host.burn_granted.map(crate::remote_view::hhmm).unwrap_or_default();
            let mut note = format!("burned remotely by {} (permission granted {granted})", peer(app));
            if !c.note.is_empty() {
                note.push_str(": ");
                note.push_str(&c.note);
            }
            if crate::burn_view::confirm_remote(app, note) {
                if let Some(a) = &mut app.remote.host.active {
                    a.started = true;
                }
                state(app, c.id, ReqState::Running, "writing");
            } else {
                app.remote.host.active = None;
                state(app, c.id, ReqState::Failed, "the burn wasn't ready any more");
            }
        }
    }
}

/// Watch the dialogs for the active request: started, or closed without.
fn host_active(app: &mut XbApp) {
    let Some(a) = &mut app.remote.host.active else { return };
    let (id, started) = (a.id, a.started);
    let function = matches!(a.kind, RequestKind::Function { .. });
    if function {
        if !started && app.diag.running.is_some() {
            a.started = true;
            state(app, id, ReqState::Running, "");
        } else if !started && app.diag.confirm.is_none() {
            app.remote.host.active = None;
            state(app, id, ReqState::Declined, format!("{} cancelled it", host_name(app)));
        }
    } else if !started {
        match app.burn {
            crate::burn_view::BurnUi::Burning { .. } => {
                a.started = true;
                state(app, id, ReqState::Running, "writing");
            }
            crate::burn_view::BurnUi::Closed => {
                app.remote.host.active = None;
                app.remote.host.countdown = None;
                state(app, id, ReqState::Declined, format!("{} closed the burn", host_name(app)));
            }
            crate::burn_view::BurnUi::Failed { ref failure, .. } => {
                let text = failure.message.clone();
                app.remote.host.active = None;
                app.remote.host.countdown = None;
                state(app, id, ReqState::Failed, text);
            }
            _ => {}
        }
    }
}

/// From burn_view: the backup is made and the review is ready.
pub fn on_burn_prepared(app: &mut XbApp) {
    let Some(a) = &app.remote.host.active else { return };
    if !a.direct || a.reviewed {
        return;
    }
    let id = a.id;
    let lines = crate::burn_view::review_lines(app);
    if let Some(a) = &mut app.remote.host.active {
        a.reviewed = true;
    }
    send(app, Msg::BurnReview { id, lines });
    state(app, id, ReqState::Shown, "review the changes");
}

/// From burn_view: a burn finished.
pub fn on_burn_done(app: &mut XbApp, ok: bool, text: &str) {
    if app
        .remote
        .host
        .active
        .as_ref()
        .is_some_and(|a| !matches!(a.kind, RequestKind::Function { .. }) && a.started)
    {
        let a = app.remote.host.active.take().expect("checked");
        state(app, a.id, if ok { ReqState::Done } else { ReqState::Failed }, text);
    }
}

/// From the session: a test or reset finished.
pub fn on_function_done(app: &mut XbApp, ok: bool, text: String) {
    if app
        .remote
        .host
        .active
        .as_ref()
        .is_some_and(|a| matches!(a.kind, RequestKind::Function { .. }) && a.started)
    {
        let a = app.remote.host.active.take().expect("checked");
        state(app, a.id, if ok { ReqState::Done } else { ReqState::Failed }, text);
    }
}

/// R5: send a log file to the tuner.
pub fn share_log(app: &mut XbApp, path: &std::path::Path) {
    let data = match std::fs::read(path) {
        Ok(d) if d.len() as u64 <= msg::MAX_LOG => d,
        Ok(_) => return app.say("That log is too big to send (64 MB at most).", true),
        Err(e) => return app.say(format!("Couldn't read the log: {e}"), true),
    };
    app.remote.host.next_log += 1;
    let id = app.remote.host.next_log;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "log.csv".into());
    let sha256 = Sha256::digest(&data).iter().map(|b| format!("{b:02x}")).collect();
    send(app, Msg::LogOffer { id, name: name.clone(), size: data.len() as u64, sha256 });
    for (index, chunk) in data.chunks(msg::LOG_CHUNK).enumerate() {
        send(app, Msg::LogChunk { id, index: index as u32, data: B64(chunk.to_vec()) });
    }
    send(app, Msg::LogDone { id });
    tracing::info!(%name, bytes = data.len(), "remote log shared");
    app.say(format!("Sent {name} to {}", peer(app)), false);
}

// ---------------------------------------------------------------- guest

fn guest_message(app: &mut XbApp, m: Msg) {
    match m {
        Msg::Identity { version } => guest_identity(app, version),
        Msg::Frames { frames } => {
            let len = app.link.identity.as_ref().map(|i| i.ecm.runtime_len);
            for f in frames {
                if Some(f.raw.0.len()) == len {
                    app.history.push(Sample { t: f.t, wall: SystemTime::now(), frame: f.raw.0 });
                }
            }
        }
        Msg::LinkInfo { state, hz, recording, .. } => {
            app.link.hz = hz;
            app.remote.guest.host_recording = recording;
            if app.link.identity.is_some() {
                app.link.state = Some(match state.as_str() {
                    "live" => LinkState::Live,
                    "reconnecting" | "connecting" => LinkState::Reconnecting {
                        reason: "the bike owner's link to the bike".into(),
                        delay: Duration::from_secs(1),
                    },
                    _ => LinkState::Stopped { reason: None },
                });
            }
        }
        Msg::Tune { share, seq: _, name, image, allow_save } => {
            guest_tune(app, share, msg::clean_line(&name, 80), image, allow_save)
        }
        Msg::TuneClosed => {
            if app.remote.guest.following {
                app.say(format!("{} closed the shared tune.", peer(app)), false);
            }
            app.remote.guest.following = false;
            app.remote.guest.share = None;
        }
        Msg::Applied { share, proposal, patches, .. } => {
            guest_capture(app);
            let g = &mut app.remote.guest;
            if g.share != Some(share) {
                return;
            }
            let patches: Patches = patches.into_iter().map(|p| (p.offset, p.bytes.0)).collect();
            if !apply(&mut g.truth, &patches) {
                return;
            }
            if let Some(id) = proposal {
                g.pending.retain(|(p, _, _)| *p != id);
            }
            guest_refresh(app);
        }
        Msg::Rejected { id, reason } => {
            guest_capture(app);
            let g = &mut app.remote.guest;
            let label = g.pending.iter().find(|(p, _, _)| *p == id).map(|(_, l, _)| l.clone());
            g.pending.retain(|(p, _, _)| *p != id);
            guest_refresh(app);
            let reason = msg::clean_line(&reason, 200);
            app.say(format!("Your edit '{}' wasn't applied: {reason}", label.unwrap_or_default()), true);
        }
        Msg::Permissions { edit, burn, tests } => {
            let g = &mut app.remote.guest.perms;
            let before = *g;
            g.edit = edit;
            g.burn = burn;
            g.tests = tests;
            if before.edit != edit || before.burn != burn || before.tests != tests {
                let mut can = Vec::new();
                if edit {
                    can.push("edit the tune directly");
                }
                if burn {
                    can.push("burn");
                }
                if tests {
                    can.push("run tests and resets");
                }
                let text = if can.is_empty() {
                    format!("{} will review what you ask for.", peer(app))
                } else {
                    format!("{} lets you {}.", peer(app), can.join(", "))
                };
                app.say(text, false);
            }
        }
        Msg::RequestState { id, state, text } => {
            let text = msg::clean_line(&text, 200);
            let who = peer(app);
            let Some(r) = app.remote.guest.requests.iter_mut().find(|r| r.id == id) else { return };
            r.state = Some(state);
            r.text = text.clone();
            let what = r.what.clone();
            if matches!(state, ReqState::Stopped | ReqState::Declined | ReqState::Failed) {
                if app.remote.guest.review.as_ref().is_some_and(|(rid, _, _)| *rid == id) {
                    app.remote.guest.review = None;
                }
                app.say(format!("{who}'s bike: {what}: {}", state_text(state, &text)), true);
            } else if state == ReqState::Done {
                app.say(
                    format!(
                        "{who}'s bike: {what}: done{}",
                        if text.is_empty() { String::new() } else { format!(" ({text})") }
                    ),
                    false,
                );
            }
        }
        Msg::BurnReview { id, lines } => {
            let lines = lines.iter().take(400).map(|l| msg::clean_line(l, 200)).collect();
            app.remote.guest.review = Some((id, lines, false));
        }
        Msg::LogOffer { id, name, size, sha256 } => {
            if size > msg::MAX_LOG || app.remote.guest.logs.len() > 4 {
                return;
            }
            // A file name only, nothing that walks out of the folder.
            let name: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' })
                .take(80)
                .collect();
            let name = name.trim_start_matches('.').to_string();
            let name = if name.is_empty() { "shared.csv".into() } else { name };
            app.remote.guest.logs.insert(id, IncomingLog { name, size, sha256, data: Vec::new(), next: 0 });
        }
        Msg::LogChunk { id, index, data } => {
            let g = &mut app.remote.guest;
            let Some(l) = g.logs.get_mut(&id) else { return };
            if index != l.next || l.data.len() as u64 + data.0.len() as u64 > l.size {
                g.logs.remove(&id);
                return;
            }
            l.data.extend_from_slice(&data.0);
            l.next += 1;
        }
        Msg::LogDone { id } => {
            let Some(l) = app.remote.guest.logs.remove(&id) else { return };
            let sha: String = Sha256::digest(&l.data).iter().map(|b| format!("{b:02x}")).collect();
            if l.data.len() as u64 != l.size || sha != l.sha256 {
                return app
                    .say(format!("A shared log ({}) arrived damaged; ask for it again.", l.name), true);
            }
            let Some(dir) = xbtune_core::env::Dirs::detect().map(|d| d.remote().join("Logs")) else { return };
            let stem = l.name.strip_suffix(".csv").unwrap_or(&l.name).to_string();
            let path = xbtune_core::time::unique_path(&dir, &stem, ".csv");
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &l.data)) {
                Ok(()) => {
                    crate::logs_view::add(app, &path);
                    app.say(
                        format!("{} sent a log; it's open in Logs ({})", peer(app), path.display()),
                        false,
                    );
                }
                Err(e) => app.say(format!("Couldn't save the shared log: {e}"), true),
            }
        }
        _ => {}
    }
}

pub fn state_text(s: ReqState, text: &str) -> String {
    let base = match s {
        ReqState::Shown => "waiting for the bike owner",
        ReqState::Declined => "declined",
        ReqState::Countdown => "starting in 10 s (the bike owner can stop it)",
        ReqState::Stopped => "stopped",
        ReqState::Preparing => "preparing",
        ReqState::Running => "running",
        ReqState::Done => "done",
        ReqState::Failed => "failed",
    };
    if text.is_empty() { base.into() } else { format!("{base}: {text}") }
}

fn guest_identity(app: &mut XbApp, version: Option<String>) {
    let who = peer(app);
    app.remote.guest.version = version.clone();
    app.history = Default::default();
    let identity = version.and_then(|v| {
        let ecm = app.catalog.lookup(&v)?.clone();
        let definition = app.catalog.definition(&ecm)?.clone();
        Some(Identity { version: v, ecm, definition })
    });
    match identity {
        Some(id) => {
            app.link = LinkView {
                channels: channels(&id.definition),
                identity: Some(id),
                state: Some(LinkState::Live),
                remote: Some(who.clone()),
                ..Default::default()
            };
            if app.view == View::Remote {
                app.say(format!("Showing {who}'s bike live: Dashboard, Live data, Charts, Tune."), false);
            }
        }
        None => {
            app.remote.guest.version = None;
            app.link = LinkView { remote: Some(who), ..Default::default() };
        }
    }
}

fn guest_tune(app: &mut XbApp, share: u32, name: String, image: ImageMsg, allow_save: bool) {
    guest_capture(app);
    let who = peer(app);
    let same_share = app.remote.guest.share == Some(share);
    let img = Image {
        ecm_id: image.ecm_id.clone(),
        ecm_version: image.ecm_version.clone(),
        bytes: image.bytes.0.clone(),
        page_zero: image.page_zero,
        meta: Meta { notes: format!("Shared by {who} (remote tuning): {name}"), ..Default::default() },
    };
    let t = match xbtune_core::tune::Tune::new(img, &app.catalog) {
        Ok(t) => t,
        Err(e) => return app.say(format!("Couldn't open {who}'s tune: {e}"), true),
    };
    if !app.remote.guest.following || !same_share {
        app.save_recovery_now();
    }
    let g = &mut app.remote.guest;
    if !same_share {
        g.pending.clear();
    }
    g.share = Some(share);
    g.key = t.opened_bytes().to_vec();
    g.truth = image.bytes.0.clone();
    g.image = Some(image);
    g.following = true;
    g.allow_save = allow_save;
    g.tune_name = name.clone();
    app.tune = Some(t);
    app.tune_ui.status = None;
    guest_refresh(app);
    // Resend what the host hasn't confirmed (absolute bytes: idempotent).
    for (id, label, patches) in app.remote.guest.pending.clone() {
        send(app, Msg::Propose { share, id, label, patches: patches_msg(&patches) });
    }
    if !same_share {
        app.say(format!("{who} shared a tune: {name} (Tune view)"), false);
    }
}

/// Show the host's copy plus our unconfirmed edits.
fn guest_refresh(app: &mut XbApp) {
    let g = &mut app.remote.guest;
    let mut shown = g.truth.clone();
    for (_, _, p) in &g.pending {
        apply(&mut shown, p);
    }
    g.snapshot = shown.clone();
    if g.following
        && let Some(t) = &mut app.tune
        && t.opened_bytes() == g.key
    {
        t.replace_bytes(shown);
    }
}

fn guest_tick(app: &mut XbApp) {
    guest_capture(app);
}

/// Propose the tuner's local edits not sent yet. Called every frame and
/// before anything replaces the tuner's copy (a resent tune after a
/// reconnect, an applied or rejected edit), so an edit made while the
/// link was down is never overwritten.
fn guest_capture(app: &mut XbApp) {
    let g = &app.remote.guest;
    if !g.following {
        return;
    }
    let Some(t) = &app.tune else {
        app.remote.guest.following = false;
        return;
    };
    if t.opened_bytes() != g.key {
        app.remote.guest.following = false;
        app.say(
            format!(
                "You opened another tune; your edits no longer go to {}. Remote -> Back to the shared tune.",
                peer(app)
            ),
            false,
        );
        return;
    }
    if t.image.bytes != g.snapshot {
        let patches = diff(&g.snapshot, &t.image.bytes);
        let label = t.undo_label().unwrap_or("edit").to_string();
        let bytes = t.image.bytes.clone();
        let g = &mut app.remote.guest;
        g.next_id += 1;
        let (id, share) = (g.next_id, g.share.unwrap_or(0));
        g.pending.push((id, label.clone(), patches.clone()));
        g.snapshot = bytes;
        send(app, Msg::Propose { share, id, label, patches: patches_msg(&patches) });
    }
}

/// Guest: reopen the shared tune after opening something else.
pub fn back_to_shared(app: &mut XbApp) {
    let Some(image) = app.remote.guest.image.clone() else { return };
    let Some(share) = app.remote.guest.share else { return };
    let name = app.remote.guest.tune_name.clone();
    let allow = app.remote.guest.allow_save;
    let truth = app.remote.guest.truth.clone();
    guest_tune(app, share, name, ImageMsg { bytes: B64(truth), ..image }, allow);
}

/// Guest: send a request (direct if the host gave permission for it).
pub fn request(app: &mut XbApp, kind: RequestKind, note: String) {
    let p = app.remote.guest.perms;
    let direct = match &kind {
        RequestKind::Function { .. } => p.tests,
        RequestKind::Burn | RequestKind::AfvReset => p.burn,
        RequestKind::Record { .. } => false,
    };
    let g = &mut app.remote.guest;
    g.next_id += 1;
    let id = g.next_id;
    let what = describe(&kind);
    g.requests.push(GuestRequest { id, what: what.clone(), state: None, text: String::new() });
    if g.requests.len() > 30 {
        g.requests.remove(0);
    }
    tracing::info!(id, %what, direct, "remote request sent");
    send(app, Msg::Request { id, kind, direct, note: note.chars().take(msg::MAX_NOTE).collect() });
    let who = peer(app);
    app.say(
        if direct {
            format!("Sent to {who}: {what}. It starts after a 10 s countdown on their side.")
        } else {
            format!("Asked {who} to {what}.")
        },
        false,
    );
}

pub fn confirm_review(app: &mut XbApp) {
    if let Some((id, _, true)) = app.remote.guest.review.take() {
        send(app, Msg::Confirm { id });
    }
}

pub fn cancel_request(app: &mut XbApp, id: u64) {
    if app.remote.guest.review.as_ref().is_some_and(|(r, _, _)| *r == id) {
        app.remote.guest.review = None;
    }
    send(app, Msg::Cancel { id });
}

/// Tune view title for the tuner: whose tune this is.
pub fn shared_label(app: &XbApp) -> Option<String> {
    let g = &app.remote.guest;
    let shared = is_guest(app)
        && !g.key.is_empty()
        && app.tune.as_ref().is_some_and(|t| t.opened_bytes() == g.key && t.path.is_none());
    shared.then(|| format!("{} (shared by {})", g.tune_name, peer(app)))
}

/// Tune view: is the open tune the bike owner's shared tune, with saving
/// a copy not allowed?
pub fn save_blocked(app: &XbApp) -> bool {
    let g = &app.remote.guest;
    is_guest(app)
        && !g.allow_save
        && !g.key.is_empty()
        && app.tune.as_ref().is_some_and(|t| t.opened_bytes() == g.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_rebuilds_the_edited_tune() {
        let c = xbtune_defs::Catalog::builtin().unwrap();
        let dir = std::env::temp_dir().join(format!("xbtune-journal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = Image::from_raw(c.lookup("BUEIB").unwrap(), xbtune_sim::BUEIB_EEPROM).unwrap();
        img.save(&dir.join("start.xbt"), &c).unwrap();
        let mut f = Some(std::fs::File::create(dir.join("journal.jsonl")).unwrap());
        journal(&mut f, 1, "Owner", "rev limit", &[(10, vec![1, 2])]);
        journal(&mut f, 2, "Tuner", "fuel cell", &[(11, vec![9]), (300, vec![7, 7, 7])]);
        drop(f);
        // A torn last line (crash mid-write) is ignored.
        std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join("journal.jsonl"))
            .unwrap()
            .write_all(b"{\"seq\":3,\"patc")
            .unwrap();
        let (out, n) = rebuild_journal(&dir, &c).unwrap();
        assert_eq!(n, 2);
        let mut want = img.bytes.clone();
        want[10] = 1;
        want[11] = 9;
        want[300..303].copy_from_slice(&[7, 7, 7]);
        assert_eq!(out.bytes, want);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn patches_diff_and_apply_round_trip() {
        let a = vec![0u8; 64];
        let mut b = a.clone();
        b[3] = 1;
        b[4] = 2;
        b[40] = 9;
        let p = diff(&a, &b);
        assert_eq!(p, vec![(3, vec![1, 2]), (40, vec![9])]);
        let mut c = a.clone();
        assert!(apply(&mut c, &p));
        assert_eq!(c, b);
        assert!(!apply(&mut c, &[(63, vec![1, 2])]));
        assert!(!apply(&mut c, &[(usize::MAX, vec![1])]));
    }
}
