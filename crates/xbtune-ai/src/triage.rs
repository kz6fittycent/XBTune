// SPDX-License-Identifier: GPL-3.0-or-later
//! "Check with the AI first" in Report a problem (docs/AI.md §6a, A4): the
//! same masked report the user would send goes to the user's own model,
//! with XBTune's known local problems and the facts XBTune checked itself
//! (snap permissions, the link state). The model names likely local causes
//! and writes a short summary for the developer, which the user may add to
//! the report. Nothing goes to the developer without the user's Send.

use crate::client::{Role, Turn};

pub const TRIAGE_PROMPT: &str = "\
You help users of XBTune, a Linux app (a snap) that talks to Buell motorcycle ECMs over a \
Bluetooth adapter (usually a BuellTooth) or a USB serial cable. A user is about to send a bug \
report. Read it and its app log, and the facts XBTune checked, and tell them whether the cause \
is likely on their side (and how to fix it) before it goes to the developer.

Known local problems and their fixes:
- Snap permissions not connected (bluez for Bluetooth, raw-usb for USB cables, removable-media \
for USB sticks): run `sudo snap connect xbtune:<plug>`, then restart XBTune.
- Bluetooth adapter not paired or trusted: pair it once in the system's Bluetooth settings.
- 'Device or resource busy' (EBUSY) on connect: the previous Bluetooth link hasn't timed out yet \
(often right after switching the ignition off and on). Wait about 20 seconds; XBTune reconnects \
by itself.
- 'Host is down': Bluetooth is off on the computer.
- No answer from the ECM: ignition off, kill switch not at RUN, adapter not on the diagnostic \
connector, or 'Factory race' chosen as the ECM type for a stock ECM (use Stock).
- Many timeouts, NAKs or garbled replies: weak battery (below about 11.5 V), the laptop too far \
from the adapter, or riding vibration; a few are normal while riding.
- USB cable not found or 'permission denied': wrong port, cable unplugged, or raw-usb not connected.
- Wideband shows no data: controller not powered, or the cable is on its serial IN instead of OUT.
- 'locked on real hardware': tests and writes are switched off until the user unlocks them in \
Settings -> Safety locks (on purpose).
- AI advisor answers ignoring the question: a local model server's context window is too small.

Answer in two parts, plainly and briefly:
Likely local causes: what in the report points to one of the problems above (or 'none found'), \
and the exact steps to try.
Summary for the developer: one to three sentences on what happened, for the report (no personal \
details).
Don't invent causes the report doesn't support; if it looks like a real bug in XBTune, say so.";

/// The question: the facts XBTune checked, then the report as it would be sent.
pub fn turns(report_json: &str, facts: &[String]) -> Vec<Turn> {
    let facts = if facts.is_empty() { "none".to_string() } else { facts.join("\n- ") };
    vec![Turn {
        role: Role::User,
        text: format!(
            "Facts XBTune checked:\n- {facts}\n\n--- The report (JSON) ---\n{report_json}\n--- End ---"
        ),
    }]
}

/// The "Summary for the developer" part of an answer, if there is one.
pub fn summary(answer: &str) -> Option<String> {
    let lower = answer.to_ascii_lowercase();
    let i = lower.find("summary for the developer")?;
    let rest = answer[i..].split_once(':').map(|(_, r)| r)?;
    let s = rest.trim().trim_matches('*').trim();
    (!s.is_empty()).then(|| s.chars().take(600).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_part() {
        let a = "**Likely local causes:** none found.\n\n**Summary for the developer:** The app froze when opening Logs.";
        assert_eq!(summary(a).as_deref(), Some("The app froze when opening Logs."));
        assert_eq!(summary("no sections"), None);
        let t = turns("{}", &["snap plug bluez: not connected".into()]);
        assert!(t[0].text.contains("bluez: not connected"));
    }
}
