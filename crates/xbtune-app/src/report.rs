// SPDX-License-Identifier: GPL-3.0-or-later
//! The app log, crash capture and "Report a problem".
//!
//! Everything lives in `<Dirs.data>/app-log/`:
//! - `app.log` (+ `app.log.1`): the tracing log, rotated at `MAX_LOG_BYTES`.
//! - `crash.json`: the first panic of a session (message, place,
//!   fingerprint, backtrace). Written by the panic hook.
//! - `fatal.log`: a line per fatal signal (SIGSEGV in a GL driver, abort…),
//!   written from the signal handler, which can't do more than `write`.
//! - `pending/`: reports that couldn't be sent, retried on later launches.
//!
//! A report is sent only when the user clicks Send: from "Report a
//! problem", or from the dialog offered after a crash (on the next launch,
//! or at once when the app survived it, e.g. a panic in a background task).
//! It goes to a relay (`relay/bug-report/`), which files it as an issue in
//! the XBTune GitHub repo, so the user needs no GitHub account and the app
//! holds no token. Home-folder paths and Bluetooth addresses are masked
//! first. A crash's fingerprint lets the relay add repeats of the same bug
//! to one issue.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

const RELAY_URL: &str = "https://xbtune-bug-report.jimmy-tigert.workers.dev/report";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LOG_BYTES: u64 = 2_000_000;
/// Log text sent with a report: the newest part. The relay trims to
/// GitHub's size limit itself; this keeps the upload reasonable.
const MAX_LOG_CHARS: usize = 45_000;
const MAX_BACKTRACE_CHARS: usize = 12_000;
pub const MAX_DESCRIPTION_CHARS: usize = 5000;
pub const MAX_CONTACT_CHARS: usize = 200;
const PENDING_MAX_FILES: usize = 5;
const PENDING_MAX_AGE: Duration = Duration::from_secs(30 * 86_400);

/// What the default filter writes to the log file (RUST_LOG still sets
/// the terminal's). sctk-adwaita (the GNOME Wayland title bar) warns at
/// every start when one side of the button layout is empty (buttons on the
/// left: "close,minimize,maximize:"); harmless.
const FILE_FILTER: &str = "info,zbus=error,eframe=warn,egui=warn,egui_glow=warn,winit=warn,\
                           calloop=warn,sctk=warn,sctk_adwaita::buttons=error,ureq=warn,rustls=warn";

pub fn log_dir() -> Option<PathBuf> {
    xbtune_core::env::Dirs::detect().map(|d| d.data.join("app-log"))
}

/// `XBTUNE_REPORT_URL` overrides the relay (tests, a staging relay).
fn relay_url() -> String {
    std::env::var("XBTUNE_REPORT_URL").unwrap_or_else(|_| RELAY_URL.into())
}

pub fn version() -> String {
    // The snap's version carries the git hash ("0.1.0+git8a55efa").
    std::env::var("SNAP_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").into())
}

/// "Ubuntu 24.04.3 LTS · x86_64 · snap rev 12".
pub fn platform() -> String {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
        })
        .unwrap_or_else(|| std::env::consts::OS.into());
    let mut s = format!("{os} · {}", std::env::consts::ARCH);
    if let Ok(rev) = std::env::var("SNAP_REVISION") {
        s.push_str(&format!(" · snap rev {rev}"));
    }
    // Inside the snap /etc/os-release is the base's; the host's is here.
    if xbtune_core::env::in_snap()
        && let Ok(t) = std::fs::read_to_string("/var/lib/snapd/hostfs/usr/lib/os-release")
        && let Some(host) =
            t.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string()))
    {
        s = format!("{host} (host) · {s}");
    }
    s
}

// ---------------------------------------------------------------------------
// Startup: log file, panic hook, fatal signals.

/// The log file, rotated when it passes `MAX_LOG_BYTES`.
struct LogFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> Self {
        let mut f = Self { path, file: None, written: 0 };
        if std::fs::metadata(&f.path).is_ok_and(|m| m.len() > MAX_LOG_BYTES / 2) {
            f.rotate();
        }
        f.reopen();
        f
    }

    fn rotate(&mut self) {
        let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
    }

    fn reopen(&mut self) {
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
        self.written = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
    }

    fn write(&mut self, buf: &[u8]) {
        if self.written > MAX_LOG_BYTES {
            self.rotate();
            self.reopen();
        }
        if let Some(f) = &mut self.file
            && f.write_all(buf).is_ok()
        {
            self.written += buf.len() as u64;
        }
    }
}

#[derive(Clone)]
struct LogWriter(Arc<Mutex<LogFile>>);

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut f) = self.0.lock() {
            f.write(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Set up logging (terminal + file) and crash capture. Call first in main.
/// Returns a crash left by the previous session, if any; its files are
/// removed once the user has answered (`forget_crash`).
pub fn init() -> Option<Crash> {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{EnvFilter, fmt};

    let dir = log_dir();
    let previous = dir.as_deref().and_then(previous_crash);
    let file_layer = dir.as_ref().and_then(|d| std::fs::create_dir_all(d).ok().map(|_| d)).map(|d| {
        let w = LogWriter(Arc::new(Mutex::new(LogFile::open(d.join("app.log")))));
        fmt::layer().with_ansi(false).with_writer(move || w.clone()).with_filter(EnvFilter::new(FILE_FILTER))
    });
    let term_layer = fmt::layer().with_writer(std::io::stderr).with_filter(
        EnvFilter::try_from_default_env()
            // zbus warns when confinement blocks the accessibility bus; harmless.
            .unwrap_or_else(|_| EnvFilter::new("warn,zbus=error,sctk_adwaita::buttons=error")),
    );
    tracing_subscriber::registry().with(term_layer).with(file_layer).init();
    tracing::info!("=== XBTune {} starting · {}", version(), platform());

    if let Some(d) = &dir {
        let _ = DIR.set(d.clone());
        install_signal_handlers(d);
    }
    install_panic_hook();
    previous
}

/// Test hook: `XBTUNE_CRASH_TEST=panic|thread|segv|abort xbtune` crashes on
/// purpose, to check crash capture and the report dialog. `thread` panics
/// a background thread, so the app keeps running and offers a report at once.
pub fn crash_test(kind: Option<&str>) {
    match kind {
        Some("panic") => panic!("XBTUNE_CRASH_TEST: deliberate panic"),
        Some("thread") => {
            let _ = std::thread::Builder::new().name("crash-test".into()).spawn(|| {
                std::thread::sleep(Duration::from_secs(3));
                panic!("XBTUNE_CRASH_TEST: deliberate panic in a background thread");
            });
        }
        // SAFETY: deliberately faults; only ever run by hand to test the
        // signal handler.
        Some("segv") => unsafe {
            std::ptr::null::<u8>().read_volatile();
        },
        Some("abort") => std::process::abort(),
        _ => {}
    }
}

/// Call after the window closes normally.
pub fn clean_exit() {
    tracing::info!("=== XBTune closed normally");
}

static DIR: OnceLock<PathBuf> = OnceLock::new();
/// A crash this session, for the dialog while the app is still running.
static NEW_CRASH: Mutex<Option<Crash>> = Mutex::new(None);
static CRASHED: AtomicBool = AtomicBool::new(false);

/// What a crash report needs. Saved as `crash.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Crash {
    /// "panic in tune_view::show" or "fatal signal SIGSEGV".
    pub summary: String,
    /// The panic message (masked), empty for a signal.
    pub message: String,
    pub fingerprint: String,
    pub version: String,
    /// Seconds since the Unix epoch.
    pub when: u64,
    pub details: String,
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let crash = describe_panic(&message, &location, &thread, &backtrace);
        // Only the first panic of a session is kept: later ones are often
        // knock-on effects (a poisoned lock).
        if !CRASHED.swap(true, Ordering::SeqCst) {
            if let Some(d) = DIR.get()
                && let Ok(json) = serde_json::to_string_pretty(&crash)
            {
                let _ = std::fs::write(d.join("crash.json"), json);
            }
            if let Ok(mut c) = NEW_CRASH.lock() {
                *c = Some(crash.clone());
            }
        }
        tracing::error!(
            "Uncaught {} [fp-{}]: {}\n{}",
            crash.summary,
            crash.fingerprint,
            crash.message,
            location
        );
        default(info);
    }));
}

/// A crash recorded this session that hasn't been shown yet.
pub fn take_new_crash() -> Option<Crash> {
    NEW_CRASH.lock().ok()?.take()
}

/// The previous session's crash: a panic (`crash.json`) or a fatal signal
/// (`fatal.log`).
fn previous_crash(dir: &Path) -> Option<Crash> {
    if let Ok(text) = std::fs::read_to_string(dir.join("crash.json"))
        && let Ok(c) = serde_json::from_str::<Crash>(&text)
    {
        return Some(c);
    }
    let fatal = std::fs::read_to_string(dir.join("fatal.log")).ok()?;
    let line = fatal.lines().rev().find(|l| !l.trim().is_empty())?;
    let signal = line.trim().strip_prefix("fatal signal ").unwrap_or(line.trim()).to_string();
    let when = std::fs::metadata(dir.join("fatal.log"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(Crash {
        summary: format!("fatal signal {signal}"),
        message: String::new(),
        fingerprint: fingerprint(&format!("signal|{signal}")),
        version: String::from("(previous session)"),
        when,
        details: "The app was stopped by a fatal signal; the log above shows what it was doing.".into(),
    })
}

/// The user has answered the crash dialog: don't offer it again.
pub fn forget_crash() {
    if let Some(d) = log_dir() {
        let _ = std::fs::remove_file(d.join("crash.json"));
        let _ = std::fs::remove_file(d.join("fatal.log"));
    }
}

fn describe_panic(message: &str, location: &str, thread: &str, backtrace: &str) -> Crash {
    let place = own_frame(backtrace);
    let file = location.rsplit(['/', '\\']).next().unwrap_or(location);
    let file = file.split(':').next().unwrap_or(file);
    let summary = match &place {
        Some(f) => format!("panic in {f}"),
        None if !file.is_empty() => format!("panic in {file}"),
        None => "panic".into(),
    };
    // Type of place, not line numbers or the message's values, so the same
    // bug from different versions and users matches.
    let key = match &place {
        Some(f) => format!("panic|{f}"),
        None => {
            format!("panic|{file}|{}", message.chars().filter(|c| !c.is_ascii_digit()).collect::<String>())
        }
    };
    let mut details = format!("thread '{thread}' panicked at {}:\n{}\n\n", mask(location), mask(message));
    let bt = mask(backtrace);
    details.push_str(&bt[..floor_char(&bt, MAX_BACKTRACE_CHARS)]);
    Crash {
        summary,
        message: mask(message).chars().take(300).collect(),
        fingerprint: fingerprint(&key),
        version: version(),
        when: SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        details,
    }
}

/// The innermost frame in XBTune's own code ("tune_view::show"), skipping
/// the panic machinery and this module. A panic inside a library is filed
/// under the XBTune code that called it.
fn own_frame(backtrace: &str) -> Option<String> {
    for line in backtrace.lines() {
        let line = line.trim_start();
        // Frame lines are "12: path::to::function", locations "at file:line".
        let Some((n, name)) = line.split_once(": ") else { continue };
        if n.parse::<u32>().is_err() {
            continue;
        }
        let name = strip_hash(name.trim());
        let ours = name.starts_with("xbtune") || name.starts_with("<xbtune");
        if !ours || name.contains("::report::") {
            continue;
        }
        let name = clean_name(name);
        let parts: Vec<&str> = name.split("::").collect();
        let tail = parts[parts.len().saturating_sub(2)..].join("::");
        return Some(tail);
    }
    None
}

/// Drops closures ("{{closure}}", "{closure#0}") and generic arguments, so
/// "xbtune::app::run::<T>::{closure#0}" -> "xbtune::app::run".
fn clean_name(name: &str) -> String {
    // "<xbtune::app::XbApp as eframe::App>::ui" -> "xbtune::app::XbApp::ui".
    let owned;
    let name = match name.strip_prefix('<').and_then(|r| Some((r, r.find(" as ")?, r.rfind(">::")?))) {
        Some((r, a, c)) if a < c => {
            owned = format!("{}{}", &r[..a], &r[c + 1..]);
            &owned
        }
        _ => name,
    };
    let mut out = String::new();
    let mut depth = 0;
    for c in name.chars() {
        match c {
            '<' | '{' => depth += 1,
            '>' | '}' => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    let mut out = out.replace("::::", "::");
    while out.ends_with("::") {
        out.truncate(out.len() - 2);
    }
    out
}

/// "xbtune::app::show::h1a2b3c4d5e6f7a8b" -> "xbtune::app::show".
fn strip_hash(name: &str) -> &str {
    match name.rsplit_once("::h") {
        Some((head, hash)) if hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()) => head,
        _ => name,
    }
}

/// 12 hex digits of FNV-1a 64: stable across Rust versions (unlike
/// `DefaultHasher`), and the relay only needs it to match reports.
pub fn fingerprint(key: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:012x}", h >> 16)
}

fn floor_char(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    (0..=max).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
}

/// The fd the signal handler writes to (-1: none).
static FATAL_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

const FATAL_SIGNALS: [(libc::c_int, &[u8]); 5] = [
    (libc::SIGSEGV, b"fatal signal SIGSEGV\n"),
    (libc::SIGBUS, b"fatal signal SIGBUS\n"),
    (libc::SIGILL, b"fatal signal SIGILL\n"),
    (libc::SIGFPE, b"fatal signal SIGFPE\n"),
    (libc::SIGABRT, b"fatal signal SIGABRT\n"),
];

/// The handlers Rust's runtime (stack-overflow detection) or a library had
/// installed, called after ours.
static PREVIOUS: OnceLock<Vec<(libc::c_int, libc::sigaction)>> = OnceLock::new();

fn install_signal_handlers(dir: &Path) {
    use std::os::fd::IntoRawFd;
    let Ok(f) = OpenOptions::new().create(true).append(true).open(dir.join("fatal.log")) else { return };
    FATAL_FD.store(f.into_raw_fd(), Ordering::SeqCst);
    let mut previous = Vec::new();
    for (sig, _) in FATAL_SIGNALS {
        // SAFETY: a zeroed sigaction is valid; the handler only uses
        // async-signal-safe calls (write, sigaction, raise) and data that
        // never changes after this point.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = fatal_handler as *const () as usize;
            // SA_ONSTACK: a stack overflow must run the handler on the
            // alternate stack that Rust sets up for each thread.
            action.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigemptyset(&mut action.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, &action, &mut old) == 0 {
                previous.push((sig, old));
            }
        }
    }
    let _ = PREVIOUS.set(previous);
}

extern "C" fn fatal_handler(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    let fd = FATAL_FD.load(Ordering::SeqCst);
    if fd >= 0
        && let Some((_, msg)) = FATAL_SIGNALS.iter().find(|(s, _)| *s == sig)
    {
        // SAFETY: write(2) is async-signal-safe; msg is a static buffer.
        unsafe {
            libc::write(fd, msg.as_ptr().cast(), msg.len());
        }
    }
    let previous = PREVIOUS.get().and_then(|p| p.iter().find(|(s, _)| *s == sig)).map(|(_, a)| *a);
    // SAFETY: restores the earlier handler (or the default) and hands the
    // signal on; all calls are async-signal-safe.
    unsafe {
        match previous {
            Some(old) if old.sa_sigaction != libc::SIG_DFL && old.sa_sigaction != libc::SIG_IGN => {
                libc::sigaction(sig, &old, std::ptr::null_mut());
                if old.sa_flags & libc::SA_SIGINFO != 0 {
                    let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                        std::mem::transmute(old.sa_sigaction);
                    f(sig, info, ctx);
                } else {
                    let f: extern "C" fn(libc::c_int) = std::mem::transmute(old.sa_sigaction);
                    f(sig);
                }
                // Rust's handler returns for a fault it doesn't own after
                // restoring the default; the faulting instruction runs
                // again and the default action ends the process.
            }
            _ => {
                let mut dfl: libc::sigaction = std::mem::zeroed();
                dfl.sa_sigaction = libc::SIG_DFL;
                libc::sigaction(sig, &dfl, std::ptr::null_mut());
                libc::raise(sig);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Building and sending reports.

/// The JSON the relay expects (`relay/bug-report/worker.js`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    pub version: String,
    pub platform: String,
    /// "BUEIB310 12-11-03 (BUEIB, DDFI-2)" when connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ecm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub log: String,
}

/// A report: the user's words, what's known about the app and the bike,
/// a crash (if this report is about one) and the newest part of the log.
pub fn build(description: &str, contact: &str, ecm: Option<String>, crash: Option<&Crash>) -> Report {
    let mut log = read_log(MAX_LOG_CHARS);
    if let Some(c) = crash {
        log.push_str("\n--- crash ---\n");
        log.push_str(&c.details);
    }
    let contact = contact.trim();
    Report {
        description: description.trim().chars().take(MAX_DESCRIPTION_CHARS).collect(),
        contact: (!contact.is_empty()).then(|| contact.chars().take(MAX_CONTACT_CHARS).collect()),
        version: version(),
        platform: platform(),
        ecm,
        error: crash.map(|c| {
            let e = if c.message.is_empty() {
                c.summary.clone()
            } else {
                format!("{}: {}", c.summary, c.message)
            };
            e.chars().take(200).collect()
        }),
        fingerprint: crash.map(|c| c.fingerprint.clone()),
        log: mask(&log),
    }
}

/// The newest `max` characters of `app.log.1` + `app.log`.
fn read_log(max: usize) -> String {
    let Some(d) = log_dir() else { return String::new() };
    let mut text = std::fs::read_to_string(d.join("app.log.1")).unwrap_or_default();
    text.push_str(&std::fs::read_to_string(d.join("app.log")).unwrap_or_default());
    if text.len() > max {
        let mut start = text.len() - max;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        // Start at a whole line.
        if let Some(nl) = text[start..].find('\n') {
            start += nl + 1;
        }
        text.drain(..start);
    }
    text
}

/// Masks what identifies the user: home-folder paths (they contain the
/// account name) become "~", and Bluetooth addresses become
/// "XX:XX:XX:XX:XX:XX".
pub fn mask(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("/home/") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        let end = after
            .find(|c: char| c == '/' || c.is_whitespace() || "\"'`)]".contains(c))
            .unwrap_or(after.len());
        out.push('~');
        rest = &after[end..];
    }
    out.push_str(rest);
    let out = mask_mac(&out);
    // The AI advisor's API key, should it ever appear in a log.
    match xbtune_ai::AiConfig::path()
        .and_then(|p| xbtune_ai::AiConfig::load(&p).ok())
        .and_then(|c| c.secret())
    {
        Some(k) => out.replace(&k, "[API key]"),
        None => out,
    }
}

fn mask_mac(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < b.len() {
        if i + 17 <= b.len() && is_mac(&b[i..i + 17]) && (i == 0 || !b[i - 1].is_ascii_hexdigit()) {
            out.push_str("XX:XX:XX:XX:XX:XX");
            i += 17;
            continue;
        }
        let ch = text[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_mac(s: &[u8]) -> bool {
    s.iter().enumerate().all(|(i, &c)| if i % 3 == 2 { c == b':' } else { c.is_ascii_hexdigit() })
}

/// How a send went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Filed as a new issue.
    Sent,
    /// Added to an existing issue for the same crash.
    Duplicate,
    /// The relay refused the report itself; retrying won't help.
    Rejected(String),
    /// Offline, timed out, or the relay is busy: worth retrying later.
    Failed(String),
}

pub fn classify(status: u16, body: &str) -> Outcome {
    let json: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let error = || {
        json.as_ref()
            .and_then(|j| j.get("error"))
            .and_then(|e| e.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("HTTP {status}"))
    };
    match status {
        200 | 201 => {
            if json.as_ref().and_then(|j| j.get("duplicate")).and_then(|d| d.as_bool()) == Some(true) {
                Outcome::Duplicate
            } else {
                Outcome::Sent
            }
        }
        400 | 404 | 405 | 413 => Outcome::Rejected(error()),
        _ => Outcome::Failed(error()),
    }
}

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};
    // The crypto provider for rustls (ureq is built without its own).
    let _ = rustls::crypto::ring::default_provider().install_default();
    ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .user_agent(format!("XBTune/{}", version()))
        .tls_config(
            TlsConfig::builder()
                .provider(TlsProvider::Rustls)
                // The system's CA certificates (the snap's base has them).
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}

/// Send one report (blocking; call off the UI thread).
pub fn send_blocking(report: &Report) -> Outcome {
    send_to(&relay_url(), report)
}

fn send_to(url: &str, report: &Report) -> Outcome {
    match agent().post(url).send_json(report) {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let body = resp.body_mut().read_to_string().unwrap_or_default();
            classify(status, &body)
        }
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

/// Send on a background thread; the result arrives on the receiver. A
/// failed send is saved for a later launch (`Outcome::Failed` then means
/// "saved"; if saving fails too it becomes `Rejected`).
pub fn send(report: Report, repaint: impl Fn() + Send + 'static) -> mpsc::Receiver<Outcome> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("bug-report".into())
        .spawn(move || {
            let mut outcome = send_blocking(&report);
            match &outcome {
                Outcome::Failed(e) => {
                    tracing::warn!(error = %e, "report not sent; saving it to retry later");
                    if save_pending(&report).is_none() {
                        outcome = Outcome::Rejected(format!("{e} (and it couldn't be saved)"));
                    }
                }
                Outcome::Rejected(e) => tracing::warn!(error = %e, "relay refused the report"),
                Outcome::Sent | Outcome::Duplicate => tracing::info!("report sent"),
            }
            let _ = tx.send(outcome);
            repaint();
        })
        .expect("spawn report thread");
    rx
}

fn pending_dir() -> Option<PathBuf> {
    log_dir().map(|d| d.join("pending"))
}

fn save_pending(report: &Report) -> Option<PathBuf> {
    let dir = pending_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
    let path = dir.join(format!("{stamp:024}.json"));
    std::fs::write(&path, serde_json::to_vec(report).ok()?).ok()?;
    let all = pending_reports(&dir);
    for old in &all[..all.len().saturating_sub(PENDING_MAX_FILES)] {
        let _ = std::fs::remove_file(old);
    }
    Some(path)
}

/// Saved reports, oldest first; expired ones are deleted.
fn pending_reports(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter(|p| {
            let old = std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > PENDING_MAX_AGE);
            if old {
                let _ = std::fs::remove_file(p);
            }
            !old
        })
        .collect();
    out.sort();
    out
}

/// Retry saved reports, one at a time, on a background thread. Stops at the
/// first one that still fails (still offline).
pub fn send_pending_later(delay: Duration) {
    let Some(dir) = pending_dir() else { return };
    if pending_reports(&dir).is_empty() {
        return;
    }
    let _ = std::thread::Builder::new().name("bug-report-retry".into()).spawn(move || {
        std::thread::sleep(delay);
        for path in pending_reports(&dir) {
            let report = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Report>(&b).ok());
            let Some(report) = report else {
                let _ = std::fs::remove_file(&path);
                continue;
            };
            match send_blocking(&report) {
                Outcome::Failed(e) => {
                    tracing::info!(error = %e, "saved report still can't be sent; will retry next launch");
                    return;
                }
                o => {
                    tracing::info!(outcome = ?o, "saved report sent");
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_hides_home_and_bt_addresses() {
        let t = "opened /home/jane/snap/xbtune/common/data/a.xbt; target rfcomm:00:11:22:AA:BB:CC ch 1";
        assert_eq!(mask(t), "opened ~/snap/xbtune/common/data/a.xbt; target rfcomm:XX:XX:XX:XX:XX:XX ch 1");
        assert_eq!(mask("\"/home/jane\" ok"), "\"~\" ok");
        // Times and hex dumps aren't addresses.
        assert_eq!(mask("12:30:45 frame 01:02:03"), "12:30:45 frame 01:02:03");
    }

    #[test]
    fn own_frame_skips_std_and_this_module() {
        let bt = "   0: std::backtrace_rs::backtrace::libunwind::trace
             at /rustc/abc/library/std/src/../../backtrace/src/backtrace/libunwind.rs:117:9
   1: xbtune::report::install_panic_hook::{{closure}}::h0123456789abcdef
   2: std::panicking::rust_panic_with_hook
   3: core::panicking::panic_bounds_check::h0011223344556677
   4: xbtune::tune_view::show::{{closure}}::hfedcba9876543210
             at ./crates/xbtune-app/src/tune_view.rs:120:5
   5: xbtune::app::XbApp::ui::h1111111111111111";
        assert_eq!(own_frame(bt).as_deref(), Some("tune_view::show"));
        // Newer symbol format.
        let bt = "   0: xbtune::report::install_panic_hook::{closure#0}
   6: xbtune_core::session::run::<xbtune_link::BoxIo>::{closure#0}::{closure#1}";
        assert_eq!(own_frame(bt).as_deref(), Some("session::run"));
        let bt = "   9: <xbtune::app::XbApp as eframe::epi::App>::ui::{closure#2}";
        assert_eq!(own_frame(bt).as_deref(), Some("XbApp::ui"));
        assert_eq!(own_frame("   0: std::rt::lang_start"), None);
    }

    #[test]
    fn same_place_same_fingerprint() {
        let bt = "  3: xbtune_core::tune::Tune::set::h0123456789abcdef";
        let a = describe_panic(
            "index out of bounds: the len is 3 but the index is 7",
            "src/tune.rs:10:5",
            "main",
            bt,
        );
        let b = describe_panic(
            "index out of bounds: the len is 9 but the index is 12",
            "src/tune.rs:14:5",
            "main",
            bt,
        );
        assert_eq!(a.summary, "panic in Tune::set");
        assert_eq!(a.fingerprint, b.fingerprint);
        assert_eq!(a.fingerprint.len(), 12);
        // No frame of ours: file + message without its numbers.
        let c = describe_panic("bad value 3", "/home/jane/x/src/lib.rs:1:1", "t", "");
        let d = describe_panic("bad value 4", "/home/jane/x/src/lib.rs:9:1", "t", "");
        assert_eq!(c.summary, "panic in lib.rs");
        assert_eq!(c.fingerprint, d.fingerprint);
        assert!(!c.details.contains("jane"));
    }

    #[test]
    fn classify_relay_answers() {
        assert_eq!(classify(201, r#"{"issue":4,"duplicate":false}"#), Outcome::Sent);
        assert_eq!(classify(200, r#"{"issue":4,"duplicate":true}"#), Outcome::Duplicate);
        assert_eq!(
            classify(400, r#"{"error":"description too long"}"#),
            Outcome::Rejected("description too long".into())
        );
        assert!(matches!(classify(429, "{}"), Outcome::Failed(_)));
        assert!(matches!(classify(502, "not json"), Outcome::Failed(_)));
    }

    /// One request to a local stand-in for the relay; returns what it got.
    fn fake_relay(status: &'static str, body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/report", listener.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut r = BufReader::new(sock.try_clone().unwrap());
            let mut len = 0;
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
            }
            let mut got = vec![0; len];
            r.read_exact(&mut got).unwrap();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).unwrap();
            String::from_utf8(got).unwrap()
        });
        (url, h)
    }

    #[test]
    fn send_posts_json_and_reads_answer() {
        let r = build("test", "me@example.com", None, None);
        let (url, h) = fake_relay("201 Created", r#"{"issue":7,"duplicate":false}"#);
        assert_eq!(send_to(&url, &r), Outcome::Sent);
        let got: Report = serde_json::from_str(&h.join().unwrap()).unwrap();
        assert_eq!(got, r);
        let (url, h) = fake_relay("200 OK", r#"{"issue":7,"duplicate":true}"#);
        assert_eq!(send_to(&url, &r), Outcome::Duplicate);
        h.join().unwrap();
        // Nobody listening: worth retrying later.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(matches!(send_to(&format!("http://127.0.0.1:{port}/report"), &r), Outcome::Failed(_)));
    }

    /// TLS with the system's certificates works (needs the internet).
    #[test]
    #[ignore = "network"]
    fn tls_reaches_github() {
        let resp = agent().get("https://api.github.com/zen").call().expect("TLS to api.github.com");
        assert!(resp.status().is_success() || resp.status().as_u16() == 403);
    }

    #[test]
    fn report_json_shape() {
        let crash = Crash {
            summary: "panic in app::start".into(),
            message: "boom".into(),
            fingerprint: "0123456789ab".into(),
            version: "0.1.0".into(),
            when: 0,
            details: "trace at /home/jane/x".into(),
        };
        let r = build("  it broke \n", "", Some("BUEIB310 12-11-03".into()), Some(&crash));
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["description"], "it broke");
        assert!(v.get("contact").is_none());
        assert_eq!(v["error"], "panic in app::start: boom");
        assert_eq!(v["fingerprint"], "0123456789ab");
        assert!(v["log"].as_str().unwrap().contains("trace at ~/x"));
    }
}
