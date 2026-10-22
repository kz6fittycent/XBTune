// SPDX-License-Identifier: GPL-3.0-or-later
//! The warning about engine damage, shared by the app (accepted once at
//! first start, and again whenever `VERSION` goes up) and the CLI (printed
//! before burns and with tuning proposals).

/// Bump when the text changes in substance, so the app asks again.
pub const VERSION: u32 = 2;

pub const TITLE: &str = "Tuning can damage your engine";

/// The full text, one paragraph per entry.
pub const POINTS: &[&str] = &[
    "A bad tune can destroy an engine. Too little fuel (running lean) or too much spark advance \
     overheats the pistons and valves and can cause detonation; the damage can happen in seconds \
     under load, often without warning.",
    "XBTune's suggestions (auto-tune, the AFV fold) are only estimates from your logs, and a \
     remote tuner's edits are someone else's judgement. Either can be wrong. Check every change \
     before you write it to the ECM, change a little at a time, and watch the engine afterwards.",
    "Keep a backup of the ECM's original tune somewhere safe. XBTune saves one before every \
     write, but you are responsible for what you write and when you ride.",
    "Modifying the engine's tune may break emissions or road-use rules where you live, and may \
     void a warranty.",
    "XBTune is free software and comes with NO WARRANTY, to the extent permitted by law \
     (GNU GPL v3, sections 15 and 16). You use it at your own risk.",
];

/// Shown after the second point only while the AI advisor is visible
/// (`xbtune_ai::enabled`). Bump `VERSION` if it is shown to everyone again.
pub const AI_POINT: &str = "The AI advisor is a language model: it can be confidently wrong about your \
                            engine. Treat what it says as a suggestion to check against the numbers, \
                            never as a safe setting.";

/// One line for places where the full text was already accepted.
pub const SHORT: &str = "A bad tune can damage the engine. Check every change before writing it; you are \
                         responsible for what you write.";

/// One line shown with the AI advisor.
pub const AI: &str = "AI answers can be wrong. Check them against the numbers; you decide what to change.";

/// The full text for a terminal.
pub fn text() -> String {
    let mut s = format!("{TITLE}\n");
    for p in POINTS {
        s.push_str("\n- ");
        s.push_str(p);
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn text_has_every_point() {
        let t = super::text();
        assert!(t.starts_with(super::TITLE));
        assert_eq!(t.matches("\n- ").count(), super::POINTS.len());
        assert!(t.contains("NO WARRANTY"));
    }
}
