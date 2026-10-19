// SPDX-License-Identifier: GPL-3.0-or-later
//! The user's record of each conversation: `<files>/AI/<time>.jsonl`, one
//! line per request and per answer (docs/AI.md §5). The API key is never in
//! it (it travels in a header). Safe to delete.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{Value, json};

use crate::client::{Prepared, Reply};

pub struct Journal {
    pub path: PathBuf,
}

impl Journal {
    /// A new conversation file in `<files>/AI/` (created on first write).
    pub fn new(files: &Path) -> Self {
        let stamp = xbtune_core::time::file_stamp(SystemTime::now());
        Self { path: files.join("AI").join(format!("{stamp}.jsonl")) }
    }

    fn append(&self, mut entry: Value) -> std::io::Result<()> {
        entry["t"] = json!(xbtune_core::time::rfc3339(SystemTime::now()));
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{}", serde_json::to_string(&entry).expect("JSON values serialise"))
    }

    pub fn request(&self, p: &Prepared) -> std::io::Result<()> {
        let body: Value = serde_json::from_str(&p.body).unwrap_or(Value::String(p.body.clone()));
        self.append(json!({
            "kind": "request",
            "prompt_version": crate::context::PROMPT_VERSION,
            "host": p.host,
            "model": p.model,
            "body": body,
        }))
    }

    pub fn reply(&self, r: &Result<Reply, crate::client::AiError>) -> std::io::Result<()> {
        self.append(match r {
            Ok(r) => json!({
                "kind": "reply",
                "model": r.model,
                "text": r.text,
                "input_tokens": r.input_tokens,
                "output_tokens": r.output_tokens,
                "stop": r.stop,
            }),
            Err(e) => json!({"kind": "error", "error": e.to_string()}),
        })
    }
}
