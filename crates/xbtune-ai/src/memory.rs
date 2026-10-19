// SPDX-License-Identifier: GPL-3.0-or-later
//! What the advisor remembers between conversations (owner's request,
//! 2026-10-19: "it sucks to have to tell it repeatedly what's going on").
//!
//! - **The bike** (`crate::bike`): model, year, mods and notes per ECM ID,
//!   sent with every request.
//! - **Recent conversations**: the questions and the advisor's last answer
//!   from the previous few conversations about the same ECM, read back from
//!   the journal files, shortened.
//!
//! Both are the user's files, readable and editable; nothing else is kept.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// How many earlier conversations go with a request, and how much of each.
pub const RECENT_CONVERSATIONS: usize = 3;
const QUESTION_CHARS: usize = 240;
const ANSWER_CHARS: usize = 500;

fn shorten(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        return s.to_string();
    }
    let cut: String = s.chars().take(n).collect();
    format!("{}…", cut.trim_end())
}

/// The question part of a stored user message (without the data block).
fn question_of(text: &str) -> &str {
    text.rsplit_once("\nQuestion: ").map_or(text, |(_, q)| q)
}

/// The ECM ID in a stored request's data block, if any.
fn ecm_of(text: &str) -> Option<String> {
    let data = text.split_once("--- Data from XBTune (JSON) ---\n")?.1.split_once("\n--- End of data ---")?.0;
    let v: Value = serde_json::from_str(data).ok()?;
    Some(v.pointer("/ecm/id")?.as_str()?.to_string())
}

/// One earlier conversation, read back from its journal.
#[derive(Debug, Clone, PartialEq)]
pub struct Past {
    pub when: String,
    pub map: Option<String>,
    pub questions: Vec<String>,
    pub last_answer: String,
}

fn read_journal(path: &Path) -> Option<(String, Past)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut ecm = None;
    let mut map = None;
    let mut questions = Vec::new();
    let mut last_answer = String::new();
    let mut when = String::new();
    for line in text.lines() {
        let Ok(e) = serde_json::from_str::<Value>(line) else { continue };
        match e.get("kind").and_then(Value::as_str) {
            Some("request") => {
                if when.is_empty() {
                    when = e.get("t").and_then(Value::as_str).unwrap_or_default().to_string();
                }
                // The last message of the body is the new question.
                let msgs = e.pointer("/body/messages").and_then(Value::as_array);
                let last = msgs.and_then(|m| m.last()).and_then(|m| m.get("content")).and_then(Value::as_str);
                if let Some(t) = last {
                    if ecm.is_none() {
                        ecm = ecm_of(t);
                        map = t
                            .split_once("\"map\":{")
                            .and_then(|(_, r)| r.split_once("\"name\":\""))
                            .and_then(|(_, r)| r.split_once('"'))
                            .map(|(n, _)| n.to_string());
                    }
                    questions.push(shorten(question_of(t), QUESTION_CHARS));
                }
            }
            Some("reply") => {
                if let Some(t) = e.get("text").and_then(Value::as_str) {
                    last_answer = shorten(t, ANSWER_CHARS);
                }
            }
            _ => {}
        }
    }
    (!questions.is_empty()).then(|| (ecm.unwrap_or_default(), Past { when, map, questions, last_answer }))
}

/// The most recent earlier conversations about `ecm_id`, newest first,
/// leaving out `current` (the conversation being continued).
pub fn recent(files: &Path, ecm_id: &str, current: Option<&Path>) -> Vec<Past> {
    let dir = files.join("AI");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut paths: Vec<PathBuf> =
        rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect();
    // File names are timestamps: newest last.
    paths.sort();
    paths
        .iter()
        .rev()
        .filter(|p| Some(p.as_path()) != current)
        .filter_map(|p| read_journal(p))
        .filter(|(ecm, _)| ecm == ecm_id)
        .map(|(_, p)| p)
        .take(RECENT_CONVERSATIONS)
        .collect()
}

/// The memory part of the data block: the bike profile and the recap.
pub fn block(bike: Value, recent: &[Past]) -> Value {
    json!({
        "bike": bike,
        "earlier_conversations": recent
            .iter()
            .map(|p| json!({"when": p.when, "map": p.map, "questions": p.questions, "advisor_last_answer": p.last_answer}))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Prepared, Reply};
    use crate::journal::Journal;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xbtune-ai-mem-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn earlier_conversations_come_back_from_the_journal() {
        let files = dir("recent");
        let msg = |q: &str, ecm: &str| {
            let data = json!({"ecm": {"id": ecm}, "map": {"name": "Tab_Fuel_Front"}});
            crate::context::user_message(q, &data)
        };
        for (i, (ecm, q)) in [("BUEIB", "Where is it rich?"), ("BUEGB", "Other bike"), ("BUEIB", "And now?")]
            .iter()
            .enumerate()
        {
            let j = Journal { path: files.join("AI").join(format!("20261019-15000{i}.jsonl")) };
            let body = json!({"messages": [{"role": "system", "content": "s"}, {"role": "user", "content": msg(q, ecm)}]});
            let p = Prepared {
                url: String::new(),
                body: body.to_string(),
                host: "localhost".into(),
                local: true,
                approx_tokens: 1,
                model: "m".into(),
            };
            j.request(&p).unwrap();
            j.reply(&Ok(Reply { text: format!("answer {i}"), ..Default::default() })).unwrap();
        }
        let r = recent(&files, "BUEIB", None);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].questions, vec!["And now?"]);
        assert_eq!(r[0].last_answer, "answer 2");
        assert_eq!(r[1].map.as_deref(), Some("Tab_Fuel_Front"));
        // The conversation being continued isn't repeated.
        let cur = files.join("AI").join("20261019-150002.jsonl");
        assert_eq!(recent(&files, "BUEIB", Some(&cur)).len(), 1);
        let b = block(json!({"bike": "not set"}), &r);
        assert_eq!(b["bike"]["bike"], "not set");
        assert_eq!(b["earlier_conversations"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&files);
    }
}
